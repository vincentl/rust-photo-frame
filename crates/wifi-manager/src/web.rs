use crate::config::Config;
use crate::hotspot;
use crate::nm;
use crate::qr;
use anyhow::{Context, Result};
use axum::extract::{Form, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio::signal;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

#[derive(Clone)]
struct UiState {
    config: Arc<Config>,
    last_attempt: PathBuf,
    qr_image: PathBuf,
}

pub async fn run_ui(config: Config) -> Result<()> {
    let state = UiState {
        qr_image: qr::qr_path(&config),
        last_attempt: last_attempt_path(&config),
        config: Arc::new(config),
    };

    let router = Router::new()
        .route("/", get(render_form))
        .route("/submit", post(handle_submit))
        .route("/status", get(status_page))
        .route("/status.json", get(status_json))
        .route("/qr.png", get(serve_qr))
        .with_state(state.clone());

    let addr = SocketAddr::new(state.config.ui.bind_address.parse()?, state.config.ui.port);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind UI listener on {addr}"))?;
    info!(?addr, "UI server listening");

    axum::serve(listener, router.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("ui server exited")?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            term.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

async fn render_form(State(state): State<UiState>) -> Html<String> {
    let qr_available = state.qr_image.exists();
    let qr_tag = if qr_available {
        "<img src=\"/qr.png\" alt=\"QR code\" class=\"qr\">"
    } else {
        "<p class=\"qr-missing\">QR code is generating…</p>"
    };
    let body = format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'>\
<title>Photo Frame Wi-Fi Setup</title><style>{}</style></head><body><main><section class='hero'><h1>Photo Frame Wi-Fi Recovery</h1><p>Connect to the hotspot <strong>{}</strong> using the password shown on the frame. Scan the QR code first, then fill in your network details.</p>{}</section><section class='form'><form method='post' action='/submit'><label>Wi-Fi Name (SSID)<input name='ssid' required maxlength='32'></label><label>Password<input name='password' type='password' minlength='8' maxlength='63' required></label><button type='submit'>Connect</button></form><p class='status-link'><a href='/status'>View connection status</a></p></section></main></body></html>",
        styles(),
        state.config.hotspot.ssid,
        qr_tag
    );
    Html(body)
}

async fn handle_submit(State(state): State<UiState>, Form(form): Form<WifiForm>) -> Response {
    let ssid = form.ssid.clone();
    match process_submission(&state, &form).await {
        Ok(message) => Html(success_page(&message)).into_response(),
        Err(err) => {
            warn!(error = ?err, "wifi submission failed");
            let display =
                "We could not apply those settings. Check the password and try again.".to_string();
            if let Err(write_err) =
                write_last_attempt(&state, &ssid, "error", &display, Some(err.to_string()))
            {
                warn!(error = ?write_err, "failed to persist error state");
            }
            Html(error_page(&display)).into_response()
        }
    }
}

async fn process_submission(state: &UiState, form: &WifiForm) -> Result<String> {
    validate_ssid(&form.ssid)?;
    let connection_id =
        nm::add_or_update_wifi(&state.config.interface, &form.ssid, &form.password).await?;
    hotspot::deactivate(&state.config).await?;
    if let Err(err) = nm::activate_connection(&connection_id).await {
        warn!(error = ?err, "failed to activate connection immediately");
    }
    let message = format!("Attempting connection to {}…", redact_ssid(&form.ssid));
    write_last_attempt(&state, &form.ssid, "connecting", &message, None)?;
    let monitor_state = state.clone();
    let ssid = form.ssid.clone();
    tokio::spawn(async move {
        monitor_connection(monitor_state, ssid).await;
    });
    Ok(message)
}

async fn status_page(State(state): State<UiState>) -> Html<String> {
    Html(render_status_html(&state))
}

async fn status_json(State(state): State<UiState>) -> Response {
    let result = read_last_attempt(&state);
    match result {
        Ok(attempt) => Json(attempt).into_response(),
        Err(err) => {
            warn!(error = ?err, "failed to read last attempt");
            (StatusCode::NOT_FOUND, "no status available").into_response()
        }
    }
}

async fn monitor_connection(state: UiState, ssid: String) {
    for _ in 0..12 {
        match nm::device_connected(&state.config.interface).await {
            Ok(true) => match nm::gateway_reachable(&state.config.interface).await {
                Ok(true) => {
                    let message = "Frame is back online.".to_string();
                    if let Err(err) = write_last_attempt(&state, &ssid, "connected", &message, None)
                    {
                        warn!(error = ?err, "failed to mark connection as successful");
                    }
                    return;
                }
                Ok(false) => {}
                Err(err) => warn!(error = ?err, "gateway probe failed while monitoring"),
            },
            Ok(false) => {}
            Err(err) => warn!(error = ?err, "device connectivity check failed while monitoring"),
        }
        sleep(Duration::from_secs(5)).await;
    }
    let message =
        "Unable to confirm connection. Double-check the password and try again.".to_string();
    if let Err(err) = write_last_attempt(&state, &ssid, "error", &message, None) {
        warn!(error = ?err, "failed to record connection timeout");
    }
}

async fn serve_qr(State(state): State<UiState>) -> Response {
    match fs::read(&state.qr_image) {
        Ok(bytes) => {
            let mut resp = Response::new(bytes.into());
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("image/png"),
            );
            resp
        }
        Err(_) => (StatusCode::NOT_FOUND, "QR not ready").into_response(),
    }
}

fn validate_ssid(ssid: &str) -> Result<()> {
    let len = ssid.trim().len();
    if (1..=32).contains(&len) {
        Ok(())
    } else {
        anyhow::bail!("SSID must be between 1 and 32 characters");
    }
}

fn redact_ssid(ssid: &str) -> String {
    let len = ssid.chars().count();
    if len <= 3 {
        "***".to_string()
    } else {
        format!(
            "***{}",
            ssid.chars()
                .rev()
                .take(3)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        )
    }
}

fn write_last_attempt(
    state: &UiState,
    ssid: &str,
    status: &str,
    message: &str,
    error: Option<String>,
) -> Result<()> {
    let record = AttemptRecord {
        timestamp: OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)?,
        status: status.to_string(),
        message: message.to_string(),
        ssid: redact_ssid(ssid),
        error,
    };
    let json = serde_json::to_vec_pretty(&record)?;
    fs::create_dir_all(state.config.var_dir.clone())
        .with_context(|| format!("failed to create {}", state.config.var_dir.display()))?;
    fs::write(&state.last_attempt, json)
        .with_context(|| format!("failed to write {}", state.last_attempt.display()))?;
    Ok(())
}

fn read_last_attempt(state: &UiState) -> Result<AttemptRecord> {
    let data = fs::read(&state.last_attempt)?;
    let record: AttemptRecord = serde_json::from_slice(&data)?;
    Ok(record)
}

fn render_status_html(state: &UiState) -> String {
    match read_last_attempt(state) {
        Ok(record) => format!("<!doctype html><html lang='en'><head><meta charset='utf-8'><meta http-equiv='refresh' content='5'><title>Connection Status</title><style>{}</style></head><body><main><section class='status'><h1>Connection Status</h1><p><strong>Status:</strong> {}</p><p>{}</p><p>Last network: {}</p><p class='back'><a href='/'>Return to setup</a></p></section></main></body></html>", styles(), record.status, record.message, record.ssid),
        Err(_) => format!("<!doctype html><html lang='en'><head><meta charset='utf-8'><meta http-equiv='refresh' content='5'><title>No status</title><style>{}</style></head><body><main><section class='status'><h1>No status yet</h1><p>Submit credentials to see progress.</p><p class='back'><a href='/'>Return to setup</a></p></section></main></body></html>", styles()),
    }
}

fn success_page(message: &str) -> String {
    format!("<!doctype html><html lang='en'><head><meta charset='utf-8'><meta http-equiv='refresh' content='5;url=/status'><title>Connecting…</title><style>{}</style></head><body><main><section class='status'><h1>Connecting…</h1><p>{}</p><p>The frame is applying your credentials. This page will refresh with live status.</p></section></main></body></html>", styles(), message)
}

fn error_page(message: &str) -> String {
    format!("<!doctype html><html lang='en'><head><meta charset='utf-8'><title>Submission error</title><style>{}</style></head><body><main><section class='status error'><h1>Check and try again</h1><p>{}</p><p class='back'><a href='/'>Back to form</a></p></section></main></body></html>", styles(), message)
}

#[derive(Deserialize)]
struct WifiForm {
    ssid: String,
    password: String,
}

#[derive(Serialize, Deserialize)]
struct AttemptRecord {
    timestamp: String,
    status: String,
    message: String,
    ssid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn last_attempt_path(config: &Config) -> PathBuf {
    config.var_dir.join("wifi-last.json")
}

fn styles() -> &'static str {
    "body{font-family:'Inter',system-ui,sans-serif;margin:0;background:#0b1d26;color:#f7f9fb;}main{max-width:720px;margin:0 auto;padding:3rem 1.5rem;}section.hero{background:#132b3a;padding:2rem;border-radius:18px;margin-bottom:2rem;box-shadow:0 20px 45px rgba(0,0,0,0.25);}section.hero h1{margin-top:0;font-size:2rem;}section.hero p{line-height:1.6;}section.hero .qr{display:block;margin:1.5rem auto;width:220px;height:220px;background:#fff;padding:12px;border-radius:12px;box-shadow:0 10px 20px rgba(0,0,0,0.2);}section.form{background:#132b3a;padding:2rem;border-radius:18px;box-shadow:0 20px 45px rgba(0,0,0,0.25);}section.form form{display:flex;flex-direction:column;gap:1rem;}label{display:flex;flex-direction:column;font-weight:600;}input{margin-top:0.4rem;padding:0.75rem;border-radius:12px;border:none;background:#0b1d26;color:#f7f9fb;font-size:1rem;}button{padding:0.85rem;border:none;border-radius:14px;font-size:1.05rem;font-weight:700;background:linear-gradient(135deg,#4cc9f0,#4361ee);color:#fff;cursor:pointer;box-shadow:0 14px 28px rgba(67,97,238,0.35);}button:hover{filter:brightness(1.05);}p.status-link{text-align:center;margin-top:1.5rem;}p.status-link a{color:#4cc9f0;text-decoration:none;font-weight:600;}section.status{background:#132b3a;padding:2rem;border-radius:18px;box-shadow:0 20px 45px rgba(0,0,0,0.25);}section.status.error{border:2px solid #ef476f;}section.status h1{margin-top:0;font-size:1.8rem;}section.status p{line-height:1.6;}p.back a{color:#4cc9f0;text-decoration:none;font-weight:600;}@media (max-width:600px){main{padding:2rem 1rem;}section.hero,section.form,section.status{padding:1.5rem;}}"
}
