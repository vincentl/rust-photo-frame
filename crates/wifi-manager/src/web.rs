use crate::config::Config;
use crate::qr;
use crate::status::{
    AttemptRecord, ProvisionRequest, now_rfc3339, read_last_attempt, redact_ssid,
    write_last_attempt, write_request,
};
use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Form, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use rand::Rng;
use rand::distr::Alphanumeric;
use serde::Deserialize;
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::signal;
use tracing::{info, warn};

#[derive(Clone)]
struct UiState {
    config: Arc<Config>,
}

pub async fn run_ui(config: Config) -> Result<()> {
    let state = UiState {
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
        use tokio::signal::unix::{SignalKind, signal};
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
    let qr_available = qr::qr_path(&state.config).exists();
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
    match queue_submission(&state.config, &form).await {
        Ok(message) => Html(success_page(&message)).into_response(),
        Err(err) => {
            warn!(error = ?err, "wifi submission failed");
            let display =
                "We could not queue those settings. Check the password and try again.".to_string();
            let _ = write_last_attempt(
                &state.config,
                &AttemptRecord {
                    timestamp: now_rfc3339().unwrap_or_else(|_| "unknown".to_string()),
                    status: "error".to_string(),
                    message: display.clone(),
                    ssid: redact_ssid(&ssid),
                    attempt_id: None,
                    error: Some(err.to_string()),
                },
            );
            Html(error_page(&display)).into_response()
        }
    }
}

async fn queue_submission(config: &Config, form: &WifiForm) -> Result<String> {
    validate_ssid(&form.ssid)?;
    validate_password(&form.password)?;

    let attempt_id = generate_attempt_id();
    let timestamp = now_rfc3339()?;
    let request = ProvisionRequest {
        attempt_id: attempt_id.clone(),
        timestamp: timestamp.clone(),
        ssid: form.ssid.trim().to_string(),
        password: form.password.clone(),
    };

    write_request(config, &request)?;

    let message = format!(
        "Queued credentials for {}. The frame is applying them now…",
        redact_ssid(&form.ssid)
    );
    write_last_attempt(
        config,
        &AttemptRecord {
            timestamp,
            status: "queued".to_string(),
            message: message.clone(),
            ssid: redact_ssid(&form.ssid),
            attempt_id: Some(attempt_id),
            error: None,
        },
    )?;

    Ok(message)
}

async fn status_page(State(state): State<UiState>) -> Html<String> {
    Html(render_status_html(&state.config))
}

async fn status_json(State(state): State<UiState>) -> Response {
    match read_last_attempt(&state.config) {
        Ok(Some(attempt)) => Json(attempt).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no status available").into_response(),
        Err(err) => {
            warn!(error = ?err, "failed to read last attempt");
            (StatusCode::NOT_FOUND, "no status available").into_response()
        }
    }
}

async fn serve_qr(State(state): State<UiState>) -> Response {
    match fs::read(qr::qr_path(&state.config)) {
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

fn render_status_html(config: &Config) -> String {
    match read_last_attempt(config) {
        Ok(Some(record)) => format!(
            "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta http-equiv='refresh' content='5'><title>Connection Status</title><style>{}</style></head><body><main><section class='status'><h1>Connection Status</h1><p><strong>Status:</strong> {}</p><p>{}</p><p>Last network: {}</p><p class='back'><a href='/'>Return to setup</a></p></section></main></body></html>",
            styles(),
            record.status,
            record.message,
            record.ssid
        ),
        _ => format!(
            "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta http-equiv='refresh' content='5'><title>No status</title><style>{}</style></head><body><main><section class='status'><h1>No status yet</h1><p>Submit credentials to see progress.</p><p class='back'><a href='/'>Return to setup</a></p></section></main></body></html>",
            styles()
        ),
    }
}

fn success_page(message: &str) -> String {
    format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta http-equiv='refresh' content='3;url=/status'><title>Connecting…</title><style>{}</style></head><body><main><section class='status'><h1>Connecting…</h1><p>{}</p><p>The frame is applying your credentials. This page will refresh with live status.</p></section></main></body></html>",
        styles(),
        message
    )
}

fn error_page(message: &str) -> String {
    format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><title>Submission error</title><style>{}</style></head><body><main><section class='status error'><h1>Check and try again</h1><p>{}</p><p class='back'><a href='/'>Back to form</a></p></section></main></body></html>",
        styles(),
        message
    )
}

fn validate_ssid(ssid: &str) -> Result<()> {
    let len = ssid.trim().len();
    if (1..=32).contains(&len) {
        Ok(())
    } else {
        anyhow::bail!("SSID must be between 1 and 32 characters")
    }
}

fn validate_password(password: &str) -> Result<()> {
    let len = password.chars().count();
    if (8..=63).contains(&len) {
        Ok(())
    } else {
        anyhow::bail!("Password must be between 8 and 63 characters")
    }
}

fn generate_attempt_id() -> String {
    let suffix: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();
    format!("attempt-{}", suffix.to_lowercase())
}

#[derive(Deserialize)]
struct WifiForm {
    ssid: String,
    password: String,
}

fn styles() -> &'static str {
    "body{font-family:'Inter',system-ui,sans-serif;margin:0;background:#0b1d26;color:#f7f9fb;}main{max-width:720px;margin:0 auto;padding:3rem 1.5rem;}section.hero{background:#132b3a;padding:2rem;border-radius:18px;margin-bottom:2rem;box-shadow:0 20px 45px rgba(0,0,0,0.25);}section.hero h1{margin-top:0;font-size:2rem;}section.hero p{line-height:1.6;}section.hero .qr{display:block;margin:1.5rem auto;width:220px;height:220px;background:#fff;padding:12px;border-radius:12px;box-shadow:0 10px 20px rgba(0,0,0,0.2);}section.form{background:#132b3a;padding:2rem;border-radius:18px;box-shadow:0 20px 45px rgba(0,0,0,0.25);}section.form form{display:flex;flex-direction:column;gap:1rem;}label{display:flex;flex-direction:column;font-weight:600;}input{margin-top:0.4rem;padding:0.75rem;border-radius:12px;border:none;background:#0b1d26;color:#f7f9fb;font-size:1rem;}button{padding:0.85rem;border:none;border-radius:14px;font-size:1.05rem;font-weight:700;background:linear-gradient(135deg,#4cc9f0,#4361ee);color:#fff;cursor:pointer;box-shadow:0 14px 28px rgba(67,97,238,0.35);}button:hover{filter:brightness(1.05);}p.status-link{text-align:center;margin-top:1.5rem;}p.status-link a{color:#4cc9f0;text-decoration:none;font-weight:600;}section.status{background:#132b3a;padding:2rem;border-radius:18px;box-shadow:0 20px 45px rgba(0,0,0,0.25);}section.status.error{border:2px solid #ef476f;}section.status h1{margin-top:0;font-size:1.8rem;}section.status p{line-height:1.6;}p.back a{color:#4cc9f0;text-decoration:none;font-weight:600;}@media (max-width:600px){main{padding:2rem 1rem;}section.hero,section.form,section.status{padding:1.5rem;}}"
}

#[cfg(test)]
mod tests {
    use super::{generate_attempt_id, validate_password, validate_ssid};

    #[test]
    fn submission_validators_reject_invalid_inputs() {
        assert!(validate_ssid("").is_err());
        assert!(validate_password("short").is_err());
        assert!(validate_password("12345678").is_ok());
    }

    #[test]
    fn attempt_id_prefix_is_stable() {
        let id = generate_attempt_id();
        assert!(id.starts_with("attempt-"));
        assert_eq!(id.len(), "attempt-".len() + 8);
    }
}
