use axum::extract::{Form, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::nm::{self, Connectivity, ScannedNetwork};

#[derive(Debug, Clone)]
pub struct AppState {
    ifname: String,
    hotspot_ip: String,
}

impl AppState {
    pub fn new(ifname: String, hotspot_ip: String) -> Self {
        Self { ifname, hotspot_ip }
    }

    pub fn wifi_ifname(&self) -> &str {
        &self.ifname
    }

    pub fn hotspot_ip(&self) -> &str {
        &self.hotspot_ip
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/apply", post(apply))
        .route("/scan", get(scan))
        .route("/api/status", get(status))
        .with_state(state)
}

async fn index(State(state): State<AppState>) -> Html<String> {
    Html(render_index(&state))
}

#[derive(Deserialize)]
struct ApplyForm {
    ssid: String,
    password: String,
}

async fn apply(
    State(state): State<AppState>,
    Form(form): Form<ApplyForm>,
) -> Result<Html<String>, (StatusCode, Html<String>)> {
    let ApplyForm { ssid, password } = form;
    let ssid_trimmed = ssid.trim();
    if ssid_trimmed.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Html(render_validation_error("SSID is required.")),
        ));
    }
    let password_trimmed = password.trim();
    if password_trimmed.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Html(render_validation_error("Password is required.")),
        ));
    }

    let result: Result<(), nm::NmError> = match nm::connection_for_ssid(ssid_trimmed) {
        Ok(Some(conn)) => {
            info!(ssid = ssid_trimmed, "updating known network");
            nm::modify_known_wifi(&conn.name, password_trimmed)
        }
        Ok(None) => {
            info!(ssid = ssid_trimmed, "creating new network");
            nm::create_new_wifi(ssid_trimmed, password_trimmed, state.wifi_ifname())
        }
        Err(err) => Err(err),
    };

    match result {
        Ok(()) => Ok(Html(render_connecting(ssid_trimmed))),
        Err(err) => {
            error!(?err, "failed to apply Wi-Fi settings");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(render_generic_error_page()),
            ))
        }
    }
}

async fn scan() -> Json<Vec<ScannedNetwork>> {
    match nm::scan_networks() {
        Ok(list) => Json(list),
        Err(err) => {
            warn!(?err, "scan failed");
            Json(Vec::new())
        }
    }
}

#[derive(Serialize)]
struct StatusResponse {
    connected: bool,
    ssid: Option<String>,
}

async fn status() -> Json<StatusResponse> {
    let connected = matches!(nm::connectivity(), Ok(Connectivity::Full));
    let ssid = match nm::active_ssid() {
        Ok(value) => value,
        Err(err) => {
            warn!(?err, "failed to read active connection");
            None
        }
    };
    Json(StatusResponse { connected, ssid })
}

fn render_index(state: &AppState) -> String {
    templates::INDEX.replace("{HOTSPOT_IP}", state.hotspot_ip())
}

fn render_connecting(ssid: &str) -> String {
    templates::CONNECTING.replace("{SSID}", &escape_html(ssid))
}

fn render_validation_error(message: &str) -> String {
    render_message_page("Invalid input", message)
}

fn render_generic_error_page() -> String {
    render_message_page(
        "Connection failed",
        "We could not apply those Wi-Fi settings. Please go back and try again.",
    )
}

fn render_message_page(title: &str, message: &str) -> String {
    templates::MESSAGE_PAGE
        .replace("{TITLE}", &escape_html(title))
        .replace("{BODY}", &escape_html(message))
}

fn escape_html(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

mod templates {
    pub(super) const INDEX: &str = r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>Wi-Fi Setup</title><style>body { font-family: Arial, sans-serif; background: #0b0c10; color: #f4f4f4; margin: 0; padding: 2rem; } main { max-width: 480px; margin: 0 auto; } h1 { margin-bottom: 0.5rem; } label { display: block; margin-top: 1rem; } input, button { width: 100%; padding: 0.75rem; margin-top: 0.25rem; border-radius: 0.5rem; border: none; } button { background: #45a29e; color: #0b0c10; font-weight: bold; cursor: pointer; } button:hover { background: #66fcf1; } .status { margin-top: 1.5rem; padding: 1rem; background: #1f2833; border-radius: 0.5rem; }</style></head><body><main><h1>Connect to your Wi-Fi</h1><p>Enter the Wi-Fi network and password to bring the frame online. The hotspot is reachable at <strong>http://{HOTSPOT_IP}/</strong>.</p><form method="post" action="/apply"><label for="ssid">Network name (SSID)</label><input id="ssid" name="ssid" list="ssid-options" required autofocus><datalist id="ssid-options"></datalist><label for="password">Password</label><input id="password" name="password" type="password" autocomplete="off" required><button type="submit">Connect</button></form><section class="status"><h2>Status</h2><p id="status-text">Checking&hellip;</p></section></main><script>async function refreshNetworks(){try{const res=await fetch('/scan');if(!res.ok)return;const networks=await res.json();const list=document.getElementById('ssid-options');list.innerHTML='';networks.forEach(n=>{const opt=document.createElement('option');opt.value=n.ssid;list.appendChild(opt);});}catch(e){console.error(e);}}async function refreshStatus(){try{const res=await fetch('/api/status');if(!res.ok)return;const data=await res.json();const text=document.getElementById('status-text');if(data.connected){const label=data.ssid?'Connected to '+data.ssid:'Connected';text.textContent=label;}else{text.textContent='Not connected';}}catch(e){console.error(e);}}refreshNetworks();refreshStatus();setInterval(refreshStatus,2000);setInterval(refreshNetworks,15000);</script></body></html>"#;

    pub(super) const CONNECTING: &str = r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><meta http-equiv="refresh" content="3"><title>Connecting</title><style>body { font-family: Arial, sans-serif; background: #0b0c10; color: #f4f4f4; display: flex; align-items: center; justify-content: center; height: 100vh; margin: 0; } div { max-width: 420px; text-align: center; } h1 { margin-bottom: 0.5rem; }</style></head><body><div><h1>Connecting…</h1><p>Attempting to join <strong>{SSID}</strong>. This page refreshes once the frame is online.</p><script>async function poll(){try{const res=await fetch('/api/status');if(res.ok){const data=await res.json();if(data.connected){window.location='/';}}}catch(e){console.error(e);}setTimeout(poll,2000);}poll();</script></div></body></html>"#;

    pub(super) const MESSAGE_PAGE: &str = r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>Wi-Fi Setup</title><style>body { font-family: Arial, sans-serif; background: #0b0c10; color: #f4f4f4; display: flex; align-items: center; justify-content: center; height: 100vh; margin: 0; } div { max-width: 420px; text-align: center; padding: 1.5rem; background: #1f2833; border-radius: 0.75rem; } a { color: #66fcf1; }</style></head><body><div><h1>{TITLE}</h1><p>{BODY}</p><p><a href="/">Return to form</a></p></div></body></html>"#;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_html_replaces_special_characters() {
        let input = "<ssid>&\"'";
        let escaped = escape_html(input);
        assert_eq!(escaped, "&lt;ssid&gt;&amp;&quot;&#39;");
    }

    #[test]
    fn connecting_page_escapes_ssid() {
        let html = render_connecting("My <SSID>");
        assert!(html.contains("My &lt;SSID&gt;"));
        assert!(!html.contains("My <SSID>"));
    }
}
