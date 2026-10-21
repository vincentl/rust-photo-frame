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

/// The overlay's default copy; the window (`ui.rs`) shows the same words.
pub const DEFAULT_TITLE: &str = "Reconnect the photo frame to Wi-Fi";
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
            headline: self
                .title
                .clone()
                .unwrap_or_else(|| DEFAULT_TITLE.to_string()),
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
    /// Renews the viewer's lease while the overlay is up (Viewer backend).
    refresher: Option<JoinHandle<()>>,
    /// Who drew the overlay last; kept after `hide` so recovery can tell
    /// whether the photo app was handed off.
    backend: Option<Backend>,
}

impl OverlayController {
    pub fn new(config: OverlayConfig) -> Self {
        Self {
            config,
            child: None,
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
        self.stop_refresher();
        let outcome = viewer_client::send(
            self.config.control_socket.clone(),
            Request::WifiOverlayShow(content.clone()),
        )
        .await;
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
        let socket = self.config.control_socket.clone();
        self.refresher = Some(tokio::spawn(async move {
            let mut answered = true;
            loop {
                sleep(VIEWER_REFRESH).await;
                let outcome =
                    viewer_client::send(socket.clone(), Request::WifiOverlayShow(content.clone()))
                        .await;
                match (outcome.accepted(), answered) {
                    (true, false) => info!("the viewer took the wifi overlay again"),
                    (false, true) => {
                        warn!(reason = %outcome.reason(), "the viewer did not renew the wifi overlay; retrying")
                    }
                    _ => {}
                }
                answered = outcome.accepted();
            }
        }));
    }

    fn stop_refresher(&mut self) {
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
        self.stop_refresher();
        if self.backend == Some(Backend::Viewer) {
            let outcome =
                viewer_client::send(self.config.control_socket.clone(), Request::WifiOverlayHide)
                    .await;
            if !outcome.accepted() {
                debug!(reason = %outcome.reason(), "viewer did not acknowledge the overlay hide");
            }
        }
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
        assert_eq!(content.headline, DEFAULT_TITLE);
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
        let (dir, socket, viewer) = accepting_viewer(2);
        let mut controller = OverlayController::new(OverlayConfig {
            // Would fail loudly if the window path ran.
            command: vec!["/nonexistent/overlay-window".to_string()],
            control_socket: socket,
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
        assert!(
            seen[0].contains("\"command\":\"wifi-overlay-show\""),
            "{}",
            seen[0]
        );
        assert_eq!(seen[1], r#"{"command":"wifi-overlay-hide"}"#);
    }

    #[tokio::test]
    async fn without_a_viewer_or_a_window_the_viewer_is_offered_it_again() {
        let dir = tempfile::tempdir().unwrap();
        let missing_sway = dir.path().join("no-sway.sock");
        let mut controller = OverlayController::new(OverlayConfig {
            command: vec!["swaymsg".to_string()],
            sway_socket: Some(missing_sway),
            control_socket: dir.path().join("no-viewer.sock"),
            ..OverlayConfig::default()
        });
        controller
            .show(&request(dir.path(), "apple-river-stone"))
            .await
            .expect("show never fails on a missing backend");
        assert_eq!(controller.backend(), Some(Backend::Viewer));
        assert!(controller.refresher.is_some());
        controller.stop_refresher();
    }
}
