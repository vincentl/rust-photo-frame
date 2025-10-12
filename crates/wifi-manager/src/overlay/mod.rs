pub mod ui;

use crate::config::{Config, OverlayConfig};
use crate::hotspot;
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::ErrorKind;
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
        command.args(args);
        command.arg("--ssid").arg(&request.ssid);
        command.arg("--password-file").arg(&request.password_file);
        command.arg("--ui-url").arg(&request.ui_url);
        if let Some(title) = &request.title {
            command.arg("--title").arg(title);
        }
        command.env("WINIT_APP_ID", &self.config.overlay_app_id);
        if let Ok(socket) = self.ensure_sway_socket_env() {
            command.env("SWAYSOCK", socket);
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

    fn prune_exited(&mut self) -> Result<()> {
        if let Some(child) = &mut self.child {
            if let Some(status) = child.try_wait()? {
                debug!(?status, "wifi overlay process exited");
                self.child = None;
            }
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
        if let Ok(true) = self.overlay_present().await {
            debug!(app_id = %overlay_app, "overlay window present after command");
        }
        Ok(())
    }

    async fn overlay_present(&self) -> Result<bool> {
        let app_id = self.config.overlay_app_id.clone();
        self.ensure_sway_socket_env()
            .context("failed to configure sway IPC environment")?;
        Ok(tokio::task::spawn_blocking(move || -> Result<bool> {
            let mut conn = Connection::new().context("failed to connect to sway IPC")?;
            let tree = conn.get_tree().context("failed to query sway tree")?;
            Ok(find_app(&tree, &app_id))
        })
        .await??)
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
        Ok(socket)
    }

    fn locate_sway_socket(&self) -> Result<PathBuf> {
        if let Some(path) = &self.config.sway_socket {
            return Ok(path.clone());
        }

        if let Some(path) = std::env::var_os("SWAYSOCK") {
            return Ok(PathBuf::from(path));
        }

        for dir in self.runtime_dirs() {
            if let Some(socket) = find_socket_in_dir(&dir)? {
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
        if !path.is_file() {
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
