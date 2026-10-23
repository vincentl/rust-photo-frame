pub mod ui;

use crate::config::{Config, OverlayConfig};
use crate::hotspot;
use crate::qr;
use crate::viewer_client;
use anyhow::{Context, Result, bail};
use config_model::control::{DEFAULT_OVERLAY_TTL_SEC, Request, WifiOverlay};
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use swayipc::{Connection, Error as SwayError, Node};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tracing::{debug, info, warn};

#[derive(Clone, Debug)]
pub struct OverlayRequest {
    pub ssid: String,
    pub password_file: PathBuf,
    pub ui_url: String,
    pub title: Option<String>,
}

/// Keep "Wi-Fi" on one line in the viewer's overlay. The viewer shapes the
/// headline with a Unicode line breaker, which treats the hyphen as a break
/// opportunity, so a tight column wrapped "Wi-" / "Fi" (seen on the frame,
/// 2026-10-22). U+2060 WORD JOINER on both sides of the hyphen forbids the
/// break and is zero-width and invisible to the shaper; a non-breaking hyphen
/// glyph (U+2011) might be missing from the font. The window UI keeps the
/// plain text: its layout engine does not hide format characters.
fn unbreakable_wifi(text: &str) -> String {
    text.replace("Wi-Fi", "Wi\u{2060}-\u{2060}Fi")
}

/// The overlay's default copy; the window (`ui.rs`) shows the same words.
pub const DEFAULT_TITLE: &str = "Reconnect to Wi-Fi";
pub const BODY: &str = "Use another device to restore connectivity.";
pub const FOOTER: &str = "The slideshow resumes automatically after the frame reconnects.";

/// How often the viewer's lease is renewed while the overlay is up. The
/// lease (`DEFAULT_OVERLAY_TTL_SEC`, 30 s) outlasts two missed refreshes, so
/// the overlay survives a slow viewer, comes back within this interval after
/// a viewer restart, and disappears within the lease if wifi-manager dies.
const VIEWER_REFRESH: Duration = Duration::from_secs(10);

/// Who draws the overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// The viewer, as a scene (both display stacks). Nothing to hand off:
    /// the viewer keeps running and draws the overlay itself.
    Viewer,
    /// The sway window (the fallback when the viewer does not answer).
    Window,
}

impl OverlayRequest {
    /// What the viewer is sent: the facts plus the prose wifi-manager owns.
    /// Reads the current hotspot password.
    pub fn viewer_content(&self) -> Result<WifiOverlay> {
        let raw = fs::read_to_string(&self.password_file).with_context(|| {
            format!(
                "failed to read hotspot password at {}",
                self.password_file.display()
            )
        })?;
        let password = raw.trim().to_string();
        if password.is_empty() {
            bail!(
                "hotspot password file {} is empty",
                self.password_file.display()
            );
        }
        Ok(WifiOverlay {
            ttl_sec: DEFAULT_OVERLAY_TTL_SEC,
            headline: unbreakable_wifi(
                &self
                    .title
                    .clone()
                    .unwrap_or_else(|| DEFAULT_TITLE.to_string()),
            ),
            body: BODY.to_string(),
            join_qr: qr::wifi_join_payload(&self.ssid, &password),
            ssid: self.ssid.clone(),
            password,
            portal_url: self.ui_url.clone(),
            footer: FOOTER.to_string(),
        })
    }

    pub fn from_config(config: &Config) -> Self {
        let ui_url = format!("http://{}:{}/", config.hotspot.ipv4_addr, config.ui.port);
        Self {
            ssid: config.hotspot.ssid.clone(),
            password_file: hotspot::hotspot_password_path(config),
            ui_url,
            title: None,
        }
    }
}

pub struct OverlayController {
    config: OverlayConfig,
    child: Option<Child>,
    /// Every request to the viewer goes through this, one at a time.
    link: ViewerLink,
    /// Renews the viewer's lease while the overlay is up (Viewer backend).
    refresher: Option<JoinHandle<()>>,
    /// Who drew the overlay last; kept after `hide` so recovery can tell
    /// whether the photo app was handed off.
    backend: Option<Backend>,
}

/// The overlay's conversation with the viewer. Every request (ping, show,
/// hide) runs under one lock, held from connect until the reply has been
/// read. The viewer replies only after it has queued the command, so the
/// viewer applies them in the order they were sent. A hide therefore waits
/// for a show already in flight, which `abort()` on the refresher cannot
/// guarantee: the send runs on a blocking thread, and aborting the task does
/// not stop it (review L1).
#[derive(Clone)]
struct ViewerLink {
    socket: PathBuf,
    state: std::sync::Arc<tokio::sync::Mutex<LinkState>>,
}

#[derive(Default)]
struct LinkState {
    /// The overlay is wanted; false after a hide, so a late refresh sends
    /// nothing.
    active: bool,
    /// A show has been sent since the last hide. The viewer may draw it
    /// even if its reply never arrived (a timeout), so a hide follows.
    offered: bool,
}

impl ViewerLink {
    fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            state: Default::default(),
        }
    }

    async fn activate(&self) {
        self.state.lock().await.active = true;
    }

    /// Offer the overlay. The content carries the hotspot password, so a
    /// password-free `ping` goes first every time, and the show follows
    /// only if the viewer answered it. A viewer from before the reply
    /// protocol answers nothing and logs payloads it cannot parse; it gets
    /// the ping, never the password. Pinging every time, not only after a
    /// failure, also covers a viewer replaced by an older one between two
    /// refreshes (review L7).
    async fn offer(&self, content: &WifiOverlay) -> viewer_client::Outcome {
        let mut state = self.state.lock().await;
        if !state.active {
            return viewer_client::Outcome::NotAnswered("the overlay was hidden".to_string());
        }
        let probe = viewer_client::send(self.socket.clone(), Request::Ping).await;
        if !matches!(probe, viewer_client::Outcome::Answered(_)) {
            return probe;
        }
        state.offered = true;
        viewer_client::send(
            self.socket.clone(),
            Request::WifiOverlayShow(content.clone()),
        )
        .await
    }

    /// Withdraw the overlay: no more offers, and a hide if a show was ever
    /// sent. `None` when there was nothing to hide.
    async fn withdraw(&self) -> Option<viewer_client::Outcome> {
        let mut state = self.state.lock().await;
        state.active = false;
        if !std::mem::take(&mut state.offered) {
            return None;
        }
        Some(viewer_client::send(self.socket.clone(), Request::WifiOverlayHide).await)
    }
}

impl OverlayController {
    pub fn new(config: OverlayConfig) -> Self {
        let link = ViewerLink::new(config.control_socket());
        Self {
            config,
            child: None,
            link,
            refresher: None,
            backend: None,
        }
    }

    /// Who drew (or is drawing) the overlay; `None` before the first show.
    pub fn backend(&self) -> Option<Backend> {
        self.backend
    }

    /// Show the overlay, preferring the viewer: if it accepts
    /// `wifi-overlay-show`, it draws the overlay as a scene and a refresher
    /// keeps its lease alive. Otherwise the sway window is used as before.
    /// If neither works (KMS while the viewer restarts: no sway there), the
    /// refresher keeps offering the overlay to the viewer until it answers.
    pub async fn show(&mut self, request: &OverlayRequest) -> Result<()> {
        let content = request.viewer_content()?;
        self.stop_refresher().await;
        self.link.activate().await;
        let outcome = self.link.offer(&content).await;
        if outcome.accepted() {
            info!("wifi overlay drawn by the viewer");
            // A window left from an earlier fallback would sit on top.
            if self.child.is_some() {
                self.stop_overlay_only().await?;
            }
            self.backend = Some(Backend::Viewer);
            self.start_refresher(content);
            return Ok(());
        }
        info!(
            reason = %outcome.reason(),
            "the viewer did not take the wifi overlay; using the sway window"
        );
        match self.show_window(request).await {
            Ok(()) => {
                self.backend = Some(Backend::Window);
                Ok(())
            }
            Err(err) => {
                warn!(
                    error = ?err,
                    "no sway window either; offering the overlay to the viewer every {}s",
                    VIEWER_REFRESH.as_secs()
                );
                self.backend = Some(Backend::Viewer);
                self.start_refresher(content);
                Ok(())
            }
        }
    }

    fn start_refresher(&mut self, content: WifiOverlay) {
        let link = self.link.clone();
        self.refresher = Some(tokio::spawn(async move {
            let mut accepted = true;
            loop {
                sleep(VIEWER_REFRESH).await;
                let outcome = link.offer(&content).await;
                match (outcome.accepted(), accepted) {
                    (true, false) => info!("the viewer took the wifi overlay again"),
                    (false, true) => {
                        warn!(reason = %outcome.reason(), "the viewer did not renew the wifi overlay; retrying")
                    }
                    _ => {}
                }
                accepted = outcome.accepted();
            }
        }));
    }

    /// Stop renewing. Taken under the link's lock, so the refresher is never
    /// stopped half-way through a send.
    async fn stop_refresher(&mut self) {
        let _quiet = self.link.state.lock().await;
        if let Some(task) = self.refresher.take() {
            task.abort();
        }
    }

    async fn show_window(&mut self, request: &OverlayRequest) -> Result<()> {
        self.prune_exited()?;
        if self.child.is_some() {
            // Always refresh overlay content so any regenerated hotspot password
            // is re-read from disk and rendered immediately.
            self.stop_overlay_only().await?;
        }

        let command_parts = self.config.command.clone();
        let (program, args) = command_parts
            .split_first()
            .context("overlay command is empty; configure program and args")?;

        let mut command = Command::new(program);

        // Ensure sway IPC environment and runtime dir are exported for child processes.
        let socket = self.ensure_sway_socket_env()?;
        command.env("SWAYSOCK", &socket);
        if let Some(runtime_dir) = socket.parent() {
            self.ensure_runtime_dir_env(runtime_dir);
        }

        // When launching via swaymsg, construct a single shell-safe command line so
        // arguments like --title with spaces survive the shell.
        if program_basename(program) == "swaymsg" {
            let exe =
                std::env::current_exe().context("failed to determine current executable path")?;
            let mut parts: Vec<String> = Vec::new();
            // Prefix with env to inject app_id inside the Sway session
            parts.push("env".to_string());
            parts.push(format!("WINIT_APP_ID={}", self.config.overlay_app_id));
            parts.push(exe.display().to_string());
            parts.push("overlay".to_string());
            parts.push("--ssid".to_string());
            parts.push(request.ssid.clone());
            parts.push("--password-file".to_string());
            parts.push(request.password_file.display().to_string());
            parts.push("--ui-url".to_string());
            parts.push(request.ui_url.clone());
            if let Some(title) = &request.title {
                parts.push("--title".to_string());
                parts.push(title.clone());
            }
            let cmdline = parts
                .into_iter()
                .map(|s| shell_escape(&s))
                .collect::<Vec<_>>()
                .join(" ");
            command.args(args);
            command.arg("exec");
            command.arg(cmdline);
        } else {
            // Direct spawn path; ensure Wayland env is sensible for winit
            command.args(args);
            command.arg("--ssid").arg(&request.ssid);
            command.arg("--password-file").arg(&request.password_file);
            command.arg("--ui-url").arg(&request.ui_url);
            if let Some(title) = &request.title {
                command.arg("--title").arg(title);
            }
            command.env("WINIT_APP_ID", &self.config.overlay_app_id);
            // Default to wayland-0 if WAYLAND_DISPLAY is not set
            if std::env::var_os("WAYLAND_DISPLAY").is_none() {
                command.env("WAYLAND_DISPLAY", "wayland-0");
            }
        }

        command.stdout(Stdio::null());
        command.stderr(Stdio::null());

        info!(command = ?self.config.command, "launching wifi overlay");
        let child = command
            .spawn()
            .context("failed to spawn wifi overlay process")?;
        self.child = Some(child);

        sleep(Duration::from_millis(250)).await;
        if let Err(err) = self.focus_overlay().await {
            warn!(error = ?err, "failed to focus overlay window");
        }
        Ok(())
    }

    async fn stop_overlay_only(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            if let Some(pid) = child.id() {
                debug!(pid, "restarting wifi overlay process");
            }
            child.start_kill().ok();
            let _ = child.wait().await;
        }
        if let Err(err) = self
            .run_commands(vec![format!(
                "[app_id=\"{}\"] kill",
                self.config.overlay_app_id
            )])
            .await
        {
            debug!(error = ?err, "failed to kill existing overlay window");
        }
        Ok(())
    }

    /// Take the overlay down wherever it is: the viewer's scene (best
    /// effort; a viewer that is not running has nothing to hide) and any
    /// window.
    pub async fn hide(&mut self) -> Result<()> {
        // Whatever the backend: a show the viewer never acknowledged (a
        // timeout before falling back to the window) may still be drawn.
        if let Some(outcome) = self.link.withdraw().await
            && !outcome.accepted()
        {
            debug!(reason = %outcome.reason(), "viewer did not acknowledge the overlay hide");
        }
        self.stop_refresher().await;
        self.hide_window().await
    }

    async fn hide_window(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            if let Some(pid) = child.id() {
                debug!(pid, "stopping wifi overlay process");
            }
            child.start_kill().ok();
            let _ = child.wait().await;
        }
        if let Err(err) = self
            .run_commands(vec![format!(
                "[app_id=\"{}\"] kill",
                self.config.overlay_app_id
            )])
            .await
        {
            debug!(error = ?err, "failed to kill overlay window");
        }
        if let Err(err) = self
            .run_commands(vec![
                format!("[app_id=\"{}\"] focus", self.config.photo_app_id),
                format!(
                    "[app_id=\"{}\"] fullscreen enable",
                    self.config.photo_app_id
                ),
            ])
            .await
        {
            debug!(error = ?err, "failed to restore photo frame focus");
        }
        Ok(())
    }

    pub async fn kill_app(&self, app_id: &str) -> Result<()> {
        self.run_commands(vec![format!("[app_id=\"{app_id}\"] kill")])
            .await
    }

    pub async fn launch_app(&self, app_id: &str, launch_command: &[String]) -> Result<()> {
        if launch_command.is_empty() {
            bail!("photo app launch-command is empty");
        }
        let mut parts = Vec::with_capacity(launch_command.len() + 2);
        parts.push("env".to_string());
        parts.push(format!("WINIT_APP_ID={app_id}"));
        parts.extend(launch_command.iter().cloned());
        let cmdline = parts
            .into_iter()
            .map(|part| shell_escape(&part))
            .collect::<Vec<_>>()
            .join(" ");
        self.run_commands(vec![format!("exec {cmdline}")]).await
    }

    pub async fn app_present(&self, app_id: &str) -> Result<bool> {
        let app = app_id.to_string();
        self.ensure_sway_socket_env()
            .context("failed to configure sway IPC environment")?;
        tokio::task::spawn_blocking(move || -> Result<bool> {
            let mut conn = Connection::new().context("failed to connect to sway IPC")?;
            let tree = conn.get_tree().context("failed to query sway tree")?;
            Ok(find_app(&tree, &app))
        })
        .await?
    }

    fn prune_exited(&mut self) -> Result<()> {
        if let Some(child) = &mut self.child
            && let Some(status) = child.try_wait()?
        {
            debug!(?status, "wifi overlay process exited");
            self.child = None;
        }
        Ok(())
    }

    async fn focus_overlay(&self) -> Result<()> {
        self.run_commands(vec![
            format!("[app_id=\"{}\"] focus", self.config.overlay_app_id),
            format!(
                "[app_id=\"{}\"] fullscreen enable",
                self.config.overlay_app_id
            ),
        ])
        .await
    }

    async fn run_commands(&self, commands: Vec<String>) -> Result<()> {
        let overlay_app = self.config.overlay_app_id.clone();
        self.ensure_sway_socket_env()
            .context("failed to configure sway IPC environment")?;

        tokio::task::spawn_blocking(move || {
            let mut conn = Connection::new().context("failed to connect to sway IPC")?;
            for command in commands {
                let results = conn
                    .run_command(&command)
                    .with_context(|| format!("failed to run sway command: {command}"))?;
                for (idx, outcome) in results.into_iter().enumerate() {
                    if let Err(error) = outcome {
                        log_sway_error(&command, idx, &error);
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        })
        .await??;

        // Give sway a moment to apply focus before returning so subsequent calls
        // (like hide) see the expected tree state.
        sleep(Duration::from_millis(50)).await;
        // Extra debug log to help trace overlay presence when troubleshooting.
        if let Ok(true) = self.app_present(&overlay_app).await {
            debug!(app_id = %overlay_app, "overlay window present after command");
        }
        Ok(())
    }
}

impl OverlayController {
    fn ensure_sway_socket_env(&self) -> Result<PathBuf> {
        let socket = self.locate_sway_socket()?;
        let current = std::env::var_os("SWAYSOCK");
        let needs_update = current
            .as_ref()
            .map(|value| Path::new(value) != socket.as_path())
            .unwrap_or(true);
        if needs_update {
            debug!(path = %socket.display(), "configuring sway IPC socket");
            set_env_path("SWAYSOCK", &socket);
        }
        // Also ensure XDG_RUNTIME_DIR is aligned with the socket location (for Wayland)
        if let Some(runtime_dir) = socket.parent() {
            self.ensure_runtime_dir_env(runtime_dir);
        }
        Ok(socket)
    }

    fn ensure_runtime_dir_env(&self, dir: &Path) {
        let current = std::env::var_os("XDG_RUNTIME_DIR");
        let needs_update = current
            .as_ref()
            .map(|value| Path::new(value) != dir)
            .unwrap_or(true);
        if needs_update {
            debug!(path = %dir.display(), "configuring XDG_RUNTIME_DIR for overlay");
            set_env_path("XDG_RUNTIME_DIR", dir);
        }
    }

    fn locate_sway_socket(&self) -> Result<PathBuf> {
        if let Some(path) = &self.config.sway_socket {
            if is_live_sway_socket(path) {
                return Ok(path.clone());
            }
            bail!("configured sway socket is unavailable: {}", path.display());
        }

        if let Some(path) = std::env::var_os("SWAYSOCK") {
            let p = PathBuf::from(path);
            if is_live_sway_socket(&p) {
                return Ok(p);
            }
        }

        // Prefer socket tied to the running sway PID for the current user
        let runtime_dirs = self.runtime_dirs();
        for dir in &runtime_dirs {
            if let Some(uid) = owner_uid(dir)
                && let Some(pid) = find_sway_pid(uid)
            {
                let candidate = dir.join(format!("sway-ipc.{uid}.{pid}.sock"));
                if is_live_sway_socket(&candidate) {
                    return Ok(candidate);
                }
            }
        }

        // Fallback: scan runtime dirs for any sway-ipc.*.sock
        for dir in &runtime_dirs {
            if let Some(socket) = find_socket_in_dir(dir)? {
                return Ok(socket);
            }
        }

        bail!("failed to locate sway IPC socket; set overlay.sway-socket or export SWAYSOCK");
    }

    fn runtime_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            dirs.push(PathBuf::from(dir));
        }
        let uid = unsafe { libc::getuid() };
        dirs.push(PathBuf::from(format!("/run/user/{uid}")));
        dirs
    }
}

fn log_sway_error(command: &str, index: usize, error: &SwayError) {
    match error {
        SwayError::CommandFailed(message) => {
            debug!(command, index, message = %message, "sway command reported failure");
        }
        SwayError::CommandParse(message) => {
            debug!(command, index, message = %message, "sway command parse failure");
        }
        other => {
            debug!(command, index, error = ?other, "sway command failed");
        }
    }
}

fn find_app(node: &Node, app_id: &str) -> bool {
    if node.app_id.as_deref() == Some(app_id) {
        return true;
    }
    node.nodes.iter().any(|child| find_app(child, app_id))
        || node
            .floating_nodes
            .iter()
            .any(|child| find_app(child, app_id))
}

fn find_socket_in_dir(dir: &Path) -> Result<Option<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read sway runtime dir at {}", dir.display()));
        }
    };

    for entry in entries {
        let entry = entry.with_context(|| {
            format!(
                "failed to inspect sway runtime dir entry in {}",
                dir.display()
            )
        })?;
        let path = entry.path();
        // Accept only UNIX sockets
        if !is_socket(&path) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if name.starts_with("sway-ipc.") && name.ends_with(".sock") && is_live_sway_socket(&path) {
            return Ok(Some(path));
        }
    }

    Ok(None)
}

fn set_env_path(key: &str, value: &Path) {
    // SAFETY: `key` has no interior nulls, and `value` comes from filesystem paths,
    // which likewise cannot contain interior null bytes on Unix platforms.
    unsafe {
        std::env::set_var(key, value);
    }
}

pub fn overlay_request(config: &Config) -> OverlayRequest {
    OverlayRequest {
        title: Some(DEFAULT_TITLE.to_string()),
        ..OverlayRequest::from_config(config)
    }
}

fn program_basename(program: &str) -> &str {
    Path::new(program)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(program)
}

fn is_socket(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(md) => md.file_type().is_socket(),
        Err(_) => false,
    }
}

fn is_live_sway_socket(path: &Path) -> bool {
    if !is_socket(path) {
        return false;
    }
    UnixStream::connect(path).is_ok()
}

fn owner_uid(dir: &Path) -> Option<u32> {
    fs::metadata(dir).ok().map(|m| m.uid())
}

fn find_sway_pid(uid: u32) -> Option<u32> {
    // Scan /proc for a process named "sway" owned by the provided uid
    let proc = Path::new("/proc");
    let Ok(entries) = fs::read_dir(proc) else {
        return None;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let pid: u32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let status_path = entry.path().join("status");
        let Ok(data) = fs::read_to_string(status_path) else {
            continue;
        };
        let mut proc_name = None;
        let mut proc_uid = None;
        for line in data.lines() {
            if let Some(stripped) = line.strip_prefix("Name:\t") {
                proc_name = Some(stripped.trim());
            } else if let Some(stripped) = line.strip_prefix("Uid:\t") {
                // Format: Uid:\tReal\tEffective\tSavedSet\tFilesystem
                let mut parts = stripped.split_whitespace();
                if let Some(real) = parts.next()
                    && let Ok(v) = real.parse::<u32>()
                {
                    proc_uid = Some(v);
                }
            }
            if proc_name.is_some() && proc_uid.is_some() {
                break;
            }
        }
        if proc_name == Some("sway") && proc_uid == Some(uid) {
            return Some(pid);
        }
    }
    None
}

fn shell_escape(arg: &str) -> String {
    // Safe characters for shell token without quoting
    fn is_safe(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@' | '%' | '+')
    }
    if arg.chars().all(is_safe) {
        arg.to_string()
    } else {
        // Single-quote, escaping internal single quotes
        let escaped = arg.replace('\'', "'\\''");
        format!("'{}'", escaped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_model::control::Reply;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    fn request(dir: &Path, password: &str) -> OverlayRequest {
        let password_file = dir.join("hotspot-password.txt");
        fs::write(&password_file, password).expect("write password");
        OverlayRequest {
            ssid: "Frame;Setup".to_string(),
            password_file,
            ui_url: "http://192.168.4.1:8080/".to_string(),
            title: None,
        }
    }

    #[test]
    fn viewer_content_carries_the_escaped_join_payload() {
        let dir = tempfile::tempdir().unwrap();
        let content = request(dir.path(), "apple-river-stone\n")
            .viewer_content()
            .unwrap();
        assert_eq!(content.password, "apple-river-stone");
        assert_eq!(
            content.join_qr,
            "WIFI:T:WPA;S:Frame\\;Setup;P:apple-river-stone;;"
        );
        assert_eq!(content.headline, unbreakable_wifi(DEFAULT_TITLE));
        assert_eq!(content.portal_url, "http://192.168.4.1:8080/");
        assert!(content.validate().is_ok());
        assert!(request(dir.path(), "  \n").viewer_content().is_err());
    }

    /// A fake viewer that accepts every request and records it.
    fn accepting_viewer(
        connections: usize,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        std::thread::JoinHandle<Vec<String>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..connections {
                let (mut stream, _) = listener.accept().unwrap();
                let mut body = String::new();
                stream.read_to_string(&mut body).unwrap();
                stream.write_all(Reply::ok().to_line().as_bytes()).unwrap();
                seen.push(body);
            }
            seen
        });
        (dir, path, handle)
    }

    #[tokio::test]
    async fn an_answering_viewer_draws_the_overlay_and_no_window_is_spawned() {
        let (dir, socket, viewer) = accepting_viewer(3);
        let mut controller = OverlayController::new(OverlayConfig {
            // Would fail loudly if the window path ran.
            command: vec!["/nonexistent/overlay-window".to_string()],
            control_socket: Some(socket),
            ..OverlayConfig::default()
        });
        controller
            .show(&request(dir.path(), "apple-river-stone"))
            .await
            .expect("show");
        assert_eq!(controller.backend(), Some(Backend::Viewer));
        assert!(controller.child.is_none(), "no window process");
        assert!(controller.refresher.is_some(), "the lease is renewed");

        controller.hide().await.expect("hide");
        assert!(controller.refresher.is_none());
        assert_eq!(
            controller.backend(),
            Some(Backend::Viewer),
            "kept for the handoff decision"
        );
        let seen = viewer.join().unwrap();
        assert_eq!(
            seen[0], r#"{"command":"ping"}"#,
            "the password goes only to a viewer that answered"
        );
        assert!(
            seen[1].contains("\"command\":\"wifi-overlay-show\""),
            "{}",
            seen[1]
        );
        assert_eq!(seen[2], r#"{"command":"wifi-overlay-hide"}"#);
    }

    #[tokio::test]
    async fn without_a_viewer_or_a_window_the_viewer_is_offered_it_again() {
        let dir = tempfile::tempdir().unwrap();
        let missing_sway = dir.path().join("no-sway.sock");
        let mut controller = OverlayController::new(OverlayConfig {
            command: vec!["swaymsg".to_string()],
            sway_socket: Some(missing_sway),
            control_socket: Some(dir.path().join("no-viewer.sock")),
            ..OverlayConfig::default()
        });
        controller
            .show(&request(dir.path(), "apple-river-stone"))
            .await
            .expect("show never fails on a missing backend");
        assert_eq!(controller.backend(), Some(Backend::Viewer));
        assert!(controller.refresher.is_some());
        controller.stop_refresher().await;
    }

    /// A viewer from before the reply protocol reads and closes without an
    /// answer. It gets the ping and never the overlay, which carries the
    /// password and which such a viewer would log as an unparsable payload.
    #[tokio::test]
    async fn a_viewer_that_never_answers_never_receives_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let old_viewer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut body = String::new();
            stream.read_to_string(&mut body).unwrap();
            body // and no reply
        });
        let content = request(dir.path(), "apple-river-stone")
            .viewer_content()
            .unwrap();
        let link = ViewerLink::new(path);
        link.activate().await;
        let outcome = link.offer(&content).await;
        assert!(!outcome.accepted());
        assert_eq!(old_viewer.join().unwrap(), r#"{"command":"ping"}"#);
        // Nothing was offered, so a hide has nothing to send.
        assert!(link.withdraw().await.is_none());
    }

    /// A fake viewer that serves each connection on its own thread, like
    /// the real one, and records the order in which it *applies* commands.
    /// It applies a show only after `show_delay` (a slow forward), a ping
    /// or a hide at once, then replies ok.
    fn concurrent_viewer(
        show_delay: std::time::Duration,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let applied = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = applied.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let log = log.clone();
                std::thread::spawn(move || {
                    let mut body = String::new();
                    stream.read_to_string(&mut body).unwrap();
                    let command =
                        config_model::control::command_name(body.as_bytes()).unwrap_or_default();
                    if command == "wifi-overlay-show" {
                        std::thread::sleep(show_delay);
                    }
                    log.lock().unwrap().push(command);
                    let _ = stream.write_all(Reply::ok().to_line().as_bytes());
                });
            }
        });
        (dir, path, applied)
    }

    /// Review L1: a hide issued while a refresh is in flight is applied
    /// after that refresh, never before it (which would leave the overlay,
    /// password included, on screen until its lease ran out). And once
    /// hidden, a late refresh sends nothing.
    #[tokio::test]
    async fn a_hide_during_a_refresh_is_applied_after_it() {
        let (dir, path, applied) = concurrent_viewer(std::time::Duration::from_millis(300));
        let content = request(dir.path(), "apple-river-stone")
            .viewer_content()
            .unwrap();
        let link = ViewerLink::new(path);
        link.activate().await;
        let refresh = {
            let link = link.clone();
            let content = content.clone();
            tokio::spawn(async move { link.offer(&content).await })
        };
        // Let the refresh reach its slow show, then hide.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let hidden = link.withdraw().await.expect("a show was offered");
        assert!(hidden.accepted());
        assert!(refresh.await.unwrap().accepted());
        let order: Vec<_> = applied
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c != "ping")
            .cloned()
            .collect();
        assert_eq!(order, vec!["wifi-overlay-show", "wifi-overlay-hide"]);

        let late = link.offer(&content).await;
        assert!(!late.accepted(), "no offer after a hide");
        assert_eq!(
            applied.lock().unwrap().len(),
            3,
            "the late refresh sent nothing"
        );
    }

    /// Review L7: every show is preceded by its own ping, so a viewer
    /// replaced between two refreshes is checked again.
    #[tokio::test]
    async fn every_offer_pings_first() {
        let (dir, path, applied) = concurrent_viewer(std::time::Duration::ZERO);
        let content = request(dir.path(), "apple-river-stone")
            .viewer_content()
            .unwrap();
        let link = ViewerLink::new(path);
        link.activate().await;
        assert!(link.offer(&content).await.accepted());
        assert!(link.offer(&content).await.accepted());
        assert_eq!(
            *applied.lock().unwrap(),
            vec!["ping", "wifi-overlay-show", "ping", "wifi-overlay-show"]
        );
    }
}

#[cfg(test)]
mod headline_tests {
    use super::*;

    #[test]
    fn wifi_in_the_viewer_headline_cannot_break_at_the_hyphen() {
        let joined = unbreakable_wifi(DEFAULT_TITLE);
        assert!(joined.contains("Wi\u{2060}-\u{2060}Fi"));
        assert!(!joined.contains("Wi-Fi"));
        assert_eq!(
            unbreakable_wifi("no network name here"),
            "no network name here"
        );
    }
}
