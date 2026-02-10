pub mod ui;

use crate::config::{Config, OverlayConfig};
use crate::hotspot;
use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use swayipc::{Connection, Error as SwayError, Node};
use tokio::process::{Child, Command};
use tokio::time::sleep;
use tracing::{debug, info, warn};
use users::get_current_uid;

#[derive(Clone, Debug)]
pub struct OverlayRequest {
    pub ssid: String,
    pub password_file: PathBuf,
    pub ui_url: String,
    pub title: Option<String>,
}

impl OverlayRequest {
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
}

impl OverlayController {
    pub fn new(config: OverlayConfig) -> Self {
        Self {
            config,
            child: None,
        }
    }

    pub async fn show(&mut self, request: &OverlayRequest) -> Result<()> {
        self.prune_exited()?;
        if self.child.is_some() {
            self.focus_overlay().await?;
            return Ok(());
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

    pub async fn hide(&mut self) -> Result<()> {
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
            return Ok(path.clone());
        }

        if let Some(path) = std::env::var_os("SWAYSOCK") {
            let p = PathBuf::from(path);
            if is_socket(&p) {
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
                if is_socket(&candidate) {
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
        let uid = get_current_uid();
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
        if name.starts_with("sway-ipc.") && name.ends_with(".sock") {
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
        title: Some("Reconnect the photo frame to Wi-Fi".to_string()),
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
