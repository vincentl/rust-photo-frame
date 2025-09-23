use anyhow::{anyhow, Context, Result};
use html_escape::encode_text;
use if_addrs::get_if_addrs;
use rand::{distributions::Alphanumeric, Rng};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{sleep, Duration, Instant};
use tracing::{debug, info, warn};

mod ui;

#[derive(Clone, Debug)]
pub struct SetupScreenInfo {
    pub hotspot_ssid: String,
    pub hotspot_password: String,
    pub access_urls: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum WifiSetupStatus {
    StartingHotspot,
    WaitingForCredentials,
    ApplyingCredentials { ssid: String },
    ConnectionFailed { ssid: String, message: String },
    Connected { ssid: String },
}

#[derive(Clone, Debug)]
struct JoinRequest {
    ssid: String,
    password: String,
}

struct AppState {
    join_tx: mpsc::Sender<JoinRequest>,
    status: watch::Sender<WifiSetupStatus>,
    info: SetupScreenInfo,
}

pub async fn ensure_wifi_connected() -> Result<bool> {
    if current_ssid().await?.is_some() {
        info!("wifi already connected; continuing normal startup");
        return Ok(true);
    }

    info!("no wifi connection detected; entering setup mode");
    run_setup_flow().await?;
    Ok(false)
}

async fn current_ssid() -> Result<Option<String>> {
    let output = Command::new("iwgetid")
        .arg("-r")
        .output()
        .await
        .context("failed to run iwgetid")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        debug!("iwgetid returned non-zero: {}", stderr.trim());
        return Ok(None);
    }
    let ssid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if ssid.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ssid))
    }
}

async fn run_setup_flow() -> Result<()> {
    let hotspot_ssid = format!(
        "Frame-Setup-{}",
        rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(4)
            .map(char::from)
            .collect::<String>()
    );
    let hotspot_password = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(10)
        .map(char::from)
        .collect::<String>();

    if let Err(err) = enable_hotspot(&hotspot_ssid, &hotspot_password).await {
        warn!("failed to enable hotspot automatically: {err:?}");
    }

    let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], 8080)))
        .await
        .context("failed to bind wifi setup listener")?;

    let access_urls = discover_access_urls(8080);
    info!(?access_urls, "wifi setup server running");

    let screen_info = SetupScreenInfo {
        hotspot_ssid: hotspot_ssid.clone(),
        hotspot_password: hotspot_password.clone(),
        access_urls: access_urls.clone(),
    };

    let (status_tx, status_rx) = watch::channel(WifiSetupStatus::StartingHotspot);
    let (join_tx, join_rx) = mpsc::channel::<JoinRequest>(8);
    let (outcome_tx, outcome_rx) = oneshot::channel();

    let app_state = Arc::new(AppState {
        join_tx: join_tx.clone(),
        status: status_tx.clone(),
        info: screen_info.clone(),
    });

    let server_task = tokio::spawn({
        let state = app_state.clone();
        async move {
            if let Err(err) = run_http_server(listener, state).await {
                warn!("wifi setup server stopped unexpectedly: {err:?}");
            }
        }
    });

    let controller_task = tokio::spawn(async move {
        run_controller(
            join_rx,
            status_tx,
            hotspot_ssid.clone(),
            hotspot_password.clone(),
            outcome_tx,
        )
        .await;
    });

    let (ui_status_tx, ui_status_rx) = std::sync::mpsc::channel();
    let (ui_ctrl_tx, ui_ctrl_rx) = std::sync::mpsc::channel();

    let mut status_rx_for_ui = status_rx.clone();
    tokio::spawn(async move {
        loop {
            let value = status_rx_for_ui.borrow().clone();
            if ui_status_tx.send(value.clone()).is_err() {
                break;
            }
            if status_rx_for_ui.changed().await.is_err() {
                break;
            }
        }
    });

    let ui_thread = std::thread::spawn(move || {
        if let Err(err) = ui::run(screen_info, ui_status_rx, ui_ctrl_rx) {
            warn!("wifi setup UI exited with error: {err:?}");
        }
    });

    let outcome = outcome_rx
        .await
        .unwrap_or(WifiSetupStatus::ConnectionFailed {
            ssid: String::new(),
            message: "setup interrupted".to_string(),
        });

    if let WifiSetupStatus::Connected { ssid } = &outcome {
        info!(ssid, "wifi connected; preparing to restart application");
        sleep(Duration::from_secs(3)).await;
        let _ = disable_hotspot().await;
        let _ = ui_ctrl_tx.send(ui::UiControl::Exit);
    } else {
        warn!("wifi setup controller exited unexpectedly; leaving UI running");
    }

    let _ = ui_ctrl_tx.send(ui::UiControl::Exit);
    let _ = ui_thread.join();

    server_task.abort();
    let _ = controller_task.await;

    if let WifiSetupStatus::Connected { ssid: _ } = outcome {
        if let Err(err) = restart_network_services().await {
            warn!("failed to restart network services: {err:?}");
        }
        if let Err(err) = restart_application_service().await {
            warn!("failed to request app restart: {err:?}");
        }
    }

    Ok(())
}

async fn run_controller(
    mut join_rx: mpsc::Receiver<JoinRequest>,
    status_tx: watch::Sender<WifiSetupStatus>,
    hotspot_ssid: String,
    hotspot_password: String,
    outcome_tx: oneshot::Sender<WifiSetupStatus>,
) {
    let _ = status_tx.send(WifiSetupStatus::WaitingForCredentials);

    while let Some(req) = join_rx.recv().await {
        let JoinRequest { ssid, password } = req;
        let _ = status_tx.send(WifiSetupStatus::ApplyingCredentials { ssid: ssid.clone() });

        match apply_credentials(&ssid, &password).await {
            Ok(()) => {
                info!(target = %ssid, "credentials applied; waiting for wifi connection");
                if wait_for_connection(&ssid, Duration::from_secs(45)).await {
                    let _ = status_tx.send(WifiSetupStatus::Connected { ssid: ssid.clone() });
                    let _ = outcome_tx.send(WifiSetupStatus::Connected { ssid });
                    return;
                }
                let _ = status_tx.send(WifiSetupStatus::ConnectionFailed {
                    ssid: ssid.clone(),
                    message: "Timed out waiting for connection".to_string(),
                });
            }
            Err(err) => {
                let _ = status_tx.send(WifiSetupStatus::ConnectionFailed {
                    ssid: ssid.clone(),
                    message: err.to_string(),
                });
            }
        }

        if let Err(err) = enable_hotspot(&hotspot_ssid, &hotspot_password).await {
            warn!("failed to ensure hotspot remains active: {err:?}");
        }
        let _ = status_tx.send(WifiSetupStatus::WaitingForCredentials);
    }

    let _ = outcome_tx.send(WifiSetupStatus::ConnectionFailed {
        ssid: String::new(),
        message: "Setup channel closed".to_string(),
    });
}

async fn wait_for_connection(target_ssid: &str, max_wait: Duration) -> bool {
    let deadline = Instant::now() + max_wait;
    loop {
        match current_ssid().await {
            Ok(Some(current)) if current == target_ssid => return true,
            Ok(Some(current)) => debug!(
                current,
                "connected to unexpected network; continuing to wait"
            ),
            Ok(None) => {}
            Err(err) => warn!("failed to check wifi status: {err:?}"),
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_secs(5)).await;
    }
}

async fn run_http_server(listener: TcpListener, state: Arc<AppState>) -> Result<()> {
    use axum::extract::State;
    use axum::response::Html;
    use axum::routing::get;
    use axum::{Form, Router};
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct JoinForm {
        ssid: String,
        password: String,
    }

    async fn get_index(State(state): State<Arc<AppState>>) -> Html<String> {
        let status = state.status.borrow().clone();
        Html(render_setup_page(&state.info, &status))
    }

    async fn post_join(
        State(state): State<Arc<AppState>>,
        Form(form): Form<JoinForm>,
    ) -> Html<String> {
        let req = JoinRequest {
            ssid: form.ssid.trim().to_string(),
            password: form.password.trim().to_string(),
        };
        if req.ssid.is_empty() {
            let status = WifiSetupStatus::ConnectionFailed {
                ssid: String::new(),
                message: "SSID is required".to_string(),
            };
            let _ = state.status.send(status.clone());
            return Html(render_setup_page(&state.info, &status));
        }
        if state.join_tx.try_send(req).is_err() {
            warn!("dropping join request; controller busy");
        }
        let status = state.status.borrow().clone();
        Html(render_setup_page(&state.info, &status))
    }

    let app = Router::new()
        .route("/", get(get_index).post(post_join))
        .with_state(state);

    axum::serve(listener, app)
        .await
        .context("wifi setup server terminated")
}

fn render_setup_page(info: &SetupScreenInfo, status: &WifiSetupStatus) -> String {
    let mut status_msg = match status {
        WifiSetupStatus::StartingHotspot => "Preparing hotspot...".to_string(),
        WifiSetupStatus::WaitingForCredentials => "Ready for Wi-Fi details.".to_string(),
        WifiSetupStatus::ApplyingCredentials { ssid } => {
            format!("Connecting to '{ssid}'...")
        }
        WifiSetupStatus::ConnectionFailed { ssid, message } => {
            if ssid.is_empty() {
                format!("Setup error: {message}")
            } else {
                format!("Failed to connect to '{ssid}': {message}")
            }
        }
        WifiSetupStatus::Connected { ssid } => {
            format!("Connected to '{ssid}'. Restarting frame...")
        }
    };
    if status_msg.is_empty() {
        status_msg = "Ready.".to_string();
    }

    let access_list = if info.access_urls.is_empty() {
        "<li>http://192.168.4.1:8080</li>".to_string()
    } else {
        info.access_urls
            .iter()
            .map(|url| format!("<li>{}</li>", url))
            .collect::<Vec<_>>()
            .join("")
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>Frame Wi-Fi Setup</title>
<style>
body {{ font-family: sans-serif; margin: 2rem; background: #f5f5f5; color: #222; }}
main {{ max-width: 30rem; margin: 0 auto; background: white; padding: 2rem; border-radius: 1rem; box-shadow: 0 0.5rem 2rem rgba(0,0,0,0.1); }}
form {{ display: flex; flex-direction: column; gap: 1rem; }}
label {{ display: flex; flex-direction: column; font-weight: 600; }}
input {{ padding: 0.75rem; border-radius: 0.5rem; border: 1px solid #ccc; font-size: 1rem; }}
button {{ padding: 0.75rem; font-size: 1rem; border-radius: 0.5rem; border: none; background: #0069c0; color: white; cursor: pointer; }}
.status {{ margin-bottom: 1rem; font-weight: 600; }}
</style>
</head>
<body>
<main>
<h1>Wi-Fi Setup</h1>
<p>1. Connect to hotspot <strong>{ssid}</strong> with password <strong>{password}</strong>.</p>
<p>2. Visit one of these URLs once connected:</p>
<ul>{access_list}</ul>
<p>3. Enter your Wi-Fi network below.</p>
<p class="status">{status_msg}</p>
<form method="post" action="/">
<label>Wi-Fi Network
<input name="ssid" placeholder="Network name" required /></label>
<label>Password
<input name="password" placeholder="Password" type="password" /></label>
<button type="submit">Join</button>
</form>
</main>
</body>
</html>"#,
        ssid = encode_text(&info.hotspot_ssid),
        password = encode_text(&info.hotspot_password),
        access_list = access_list,
        status_msg = encode_text(&status_msg),
    )
}

fn discover_access_urls(port: u16) -> Vec<String> {
    match get_if_addrs() {
        Ok(ifaces) => ifaces
            .into_iter()
            .filter(|iface| !iface.is_loopback())
            .filter_map(|iface| match iface.addr.ip() {
                std::net::IpAddr::V4(v4) => Some(format!("http://{}:{}", v4, port)),
                _ => None,
            })
            .collect(),
        Err(err) => {
            warn!("failed to enumerate interfaces: {err:?}");
            Vec::new()
        }
    }
}

async fn enable_hotspot(ssid: &str, password: &str) -> Result<()> {
    let status = Command::new("nmcli")
        .args([
            "device",
            "wifi",
            "hotspot",
            "ifname",
            "wlan0",
            "con-name",
            "frame-setup",
            "ssid",
            ssid,
            "band",
            "bg",
            "password",
            password,
        ])
        .status()
        .await?;
    if !status.success() {
        return Err(anyhow!("nmcli hotspot command failed"));
    }
    Ok(())
}

async fn disable_hotspot() -> Result<()> {
    let status = Command::new("nmcli")
        .args(["connection", "down", "frame-setup"])
        .status()
        .await?;
    if !status.success() {
        warn!("failed to disable hotspot connection");
    }
    Ok(())
}

async fn apply_credentials(ssid: &str, password: &str) -> Result<()> {
    let path = std::env::var("WPA_SUPPLICANT_CONF")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/etc/wpa_supplicant/wpa_supplicant.conf"));
    let existing = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let marker = format!("ssid=\"{ssid}\"");
    let mut updated_lines = Vec::new();
    let mut inside_network = false;
    let mut inside_target = false;
    let mut psk_written = false;

    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("network={") {
            inside_network = true;
            inside_target = false;
            psk_written = false;
        } else if inside_network && trimmed == "}" {
            if inside_target && !psk_written {
                updated_lines.push(format!("    psk=\"{}\"", password));
            }
            inside_network = false;
            inside_target = false;
            psk_written = false;
        }

        if inside_network && trimmed == marker {
            inside_target = true;
        }

        if inside_target && trimmed.starts_with("psk=") {
            updated_lines.push(format!("    psk=\"{}\"", password));
            psk_written = true;
            continue;
        }

        updated_lines.push(line.to_string());
    }

    if !existing.contains(&marker) {
        if !updated_lines
            .last()
            .map(|l| l.trim().is_empty())
            .unwrap_or(true)
        {
            updated_lines.push(String::new());
        }
        updated_lines.push("network={".to_string());
        updated_lines.push(format!("    ssid=\"{}\"", ssid));
        updated_lines.push(format!("    psk=\"{}\"", password));
        updated_lines.push("}".to_string());
    }

    let mut updated = updated_lines.join("\n");
    if !updated.ends_with('\n') {
        updated.push('\n');
    }

    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, updated)
        .await
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    tokio::fs::rename(&tmp, &path)
        .await
        .with_context(|| format!("failed to replace {}", path.display()))?;

    let status = Command::new("wpa_cli")
        .args(["-i", "wlan0", "reconfigure"])
        .status()
        .await
        .context("failed to reconfigure wpa_supplicant")?;
    if !status.success() {
        warn!("wpa_cli reconfigure returned non-zero");
    }

    Ok(())
}

async fn restart_network_services() -> Result<()> {
    let status = Command::new("systemctl")
        .args(["restart", "dhcpcd.service"])
        .status()
        .await
        .context("failed to restart dhcpcd")?;
    if !status.success() {
        warn!("systemctl restart dhcpcd.service failed");
    }
    Ok(())
}

async fn restart_application_service() -> Result<()> {
    let status = Command::new("systemctl")
        .args(["restart", "rust-photo-frame.service"])
        .status()
        .await
        .context("failed to restart rust-photo-frame service")?;
    if !status.success() {
        warn!("systemctl restart rust-photo-frame.service failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_page_contains_urls() {
        let info = SetupScreenInfo {
            hotspot_ssid: "Frame-Setup".to_string(),
            hotspot_password: "password".to_string(),
            access_urls: vec!["http://192.168.4.1:8080".to_string()],
        };
        let html = render_setup_page(&info, &WifiSetupStatus::WaitingForCredentials);
        assert!(html.contains("Frame-Setup"));
        assert!(html.contains("http://192.168.4.1:8080"));
    }
}
