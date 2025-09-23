use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use axum::{Form, Router};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use schemars::schema::{InstanceType, RootSchema, Schema, SingleOrVec};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::Configuration;
use crate::config_repo::{ConfigRepository, ConfigTag};

const SAFE_PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.');

#[derive(Clone)]
struct AppState {
    repo: ConfigRepository,
    schema: Arc<RootSchema>,
    restart: RestartTrigger,
}

#[derive(Clone)]
pub struct RestartTrigger {
    cancel: CancellationToken,
}

impl RestartTrigger {
    pub fn new(cancel: CancellationToken) -> Self {
        Self { cancel }
    }

    pub fn request_restart(&self, reason: &str) {
        tracing::info!(reason, "configuration server requested restart");
        self.cancel.cancel();
    }
}

pub fn spawn(
    repo: ConfigRepository,
    schema: RootSchema,
    cancel: CancellationToken,
    bind_addr: SocketAddr,
) -> JoinHandle<()> {
    let state = AppState {
        repo,
        schema: Arc::new(schema),
        restart: RestartTrigger::new(cancel.clone()),
    };
    let app = Router::new()
        .route("/", get(list_configs))
        .route("/configs/:tag/edit", get(edit_config_page))
        .route("/configs/:tag/activate", post(activate_config))
        .route("/configs/:tag/delete", post(delete_config))
        .route("/configs/:tag/save", post(save_config))
        .with_state(state);

    tokio::spawn(async move {
        tracing::info!(%bind_addr, "starting configuration web server");
        match TcpListener::bind(bind_addr).await {
            Ok(listener) => {
                let shutdown = cancel.clone();
                if let Err(err) = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async move {
                    shutdown.cancelled().await;
                })
                .await
                {
                    tracing::error!(error = %err, "configuration web server failed");
                }
            }
            Err(err) => {
                tracing::error!(error = %err, %bind_addr, "failed to bind configuration web server");
            }
        }
    })
}

async fn list_configs(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
) -> Result<Html<String>, (StatusCode, Html<String>)> {
    ensure_local(addr)?;
    let tags = state
        .repo
        .list_tags()
        .await
        .map_err(internal_error("failed to list configuration tags"))?;
    let active = state
        .repo
        .detect_active_tag()
        .await
        .map_err(internal_error("failed to determine active configuration"))?;
    let schema_html = render_schema_table(&state.schema);
    let body = render_index(&tags, active.as_deref(), &schema_html);
    Ok(Html(layout(&body)))
}

async fn edit_config_page(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Path(tag): Path<String>,
) -> Result<Html<String>, (StatusCode, Html<String>)> {
    ensure_local(addr)?;
    validate_tag(&tag)?;
    let content = state
        .repo
        .load_tag_yaml(&tag)
        .await
        .map_err(internal_error("failed to load configuration"))?;
    let page = render_edit(&tag, &content, None);
    Ok(Html(layout(&page)))
}

#[derive(Deserialize)]
struct SaveForm {
    content: String,
}

async fn save_config(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Path(tag): Path<String>,
    Form(form): Form<SaveForm>,
) -> Result<impl IntoResponse, (StatusCode, Html<String>)> {
    ensure_local(addr)?;
    validate_tag(&tag)?;
    let parsed: Configuration = match serde_yaml::from_str(&form.content) {
        Ok(cfg) => cfg,
        Err(err) => {
            let page = render_edit(
                &tag,
                &form.content,
                Some(&format!("Failed to parse YAML: {err}")),
            );
            return Err((StatusCode::BAD_REQUEST, Html(layout(&page))));
        }
    };
    let validated = match parsed.validated() {
        Ok(cfg) => cfg,
        Err(err) => {
            let page = render_edit(
                &tag,
                &form.content,
                Some(&format!("Configuration validation failed: {err}")),
            );
            return Err((StatusCode::BAD_REQUEST, Html(layout(&page))));
        }
    };
    let canonical = serde_yaml::to_string(&validated)
        .map_err(|err| internal_error("failed to serialize configuration")(err.into()))?;
    state
        .repo
        .commit_and_tag(&tag, &canonical)
        .await
        .map_err(internal_error("failed to save configuration"))?;
    state
        .restart
        .request_restart("configuration saved and committed");
    Ok(Redirect::to("/"))
}

async fn activate_config(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Path(tag): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Html<String>)> {
    ensure_local(addr)?;
    validate_tag(&tag)?;
    state
        .repo
        .make_active(&tag)
        .await
        .map_err(internal_error("failed to activate configuration"))?;
    state.restart.request_restart("configuration activated");
    Ok(Redirect::to("/"))
}

async fn delete_config(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Path(tag): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Html<String>)> {
    ensure_local(addr)?;
    validate_tag(&tag)?;
    state
        .repo
        .delete_tag(&tag)
        .await
        .map_err(internal_error("failed to delete configuration"))?;
    Ok(Redirect::to("/"))
}

fn ensure_local(addr: SocketAddr) -> Result<(), (StatusCode, Html<String>)> {
    if is_local(addr.ip()) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Html(layout("<h2>Access denied</h2><p>This interface is only available on the local network.</p>")),
        ))
    }
}

fn is_local(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
    }
}

fn validate_tag(tag: &str) -> Result<(), (StatusCode, Html<String>)> {
    if tag
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            Html(layout(&format!(
                "<h2>Invalid configuration tag</h2><p>Tag '{tag}' contains unsupported characters.</p>"
            ))),
        ))
    }
}

fn internal_error(msg: &'static str) -> impl Fn(anyhow::Error) -> (StatusCode, Html<String>) {
    move |err| {
        tracing::error!(error = ?err, "{msg}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html(layout(&format!(
                "<h2>Something went wrong</h2><p>{msg}</p>"
            ))),
        )
    }
}

fn render_index(tags: &[ConfigTag], active: Option<&str>, schema_table: &str) -> String {
    let mut body = String::new();
    body.push_str("<h1>Photo Frame Configuration</h1>");
    body.push_str("<p>Manage saved configuration snapshots. <strong>Make Active</strong> will restart the Photo Frame to reload settings.</p>");
    if let Some(active) = active {
        writeln!(
            &mut body,
            "<p class=\"active\">Currently active configuration: <strong>{}</strong></p>",
            escape_html(active)
        )
        .ok();
    }
    if tags.is_empty() {
        body.push_str("<p>No configuration tags were found. Create a tag in the configuration repository to get started.</p>");
    } else {
        body.push_str("<table class=\"configs\"><thead><tr><th>Name</th><th>Summary</th><th>Actions</th></tr></thead><tbody>");
        for tag in tags {
            let encoded = utf8_percent_encode(&tag.name, SAFE_PATH_SEGMENT).to_string();
            let is_active = active.map(|a| a == tag.name.as_str()).unwrap_or(false);
            body.push_str("<tr>");
            body.push_str("<td>");
            if is_active {
                body.push_str(&format!(
                    "<span class=\"tag active\">{}</span>",
                    escape_html(&tag.name)
                ));
            } else {
                body.push_str(&format!(
                    "<span class=\"tag\">{}</span>",
                    escape_html(&tag.name)
                ));
            }
            body.push_str("</td>");
            body.push_str("<td>");
            if let Some(message) = &tag.message {
                body.push_str(&escape_html(message));
            } else {
                body.push_str("&mdash;");
            }
            body.push_str("</td>");
            body.push_str("<td class=\"actions\">");
            body.push_str(&format!(
                "<form method=\"get\" action=\"/configs/{}/edit\"><button type=\"submit\">Edit</button></form>",
                encoded
            ));
            body.push_str(&format!(
                "<form method=\"post\" action=\"/configs/{}/activate\"><button type=\"submit\">Make Active</button></form>",
                encoded
            ));
            body.push_str(&format!(
                "<form method=\"post\" action=\"/configs/{}/delete\" onsubmit=\"return confirm('Delete configuration {}?');\"><button type=\"submit\" class=\"danger\">Delete</button></form>",
                encoded,
                escape_html(&tag.name)
            ));
            body.push_str("</td></tr>");
        }
        body.push_str("</tbody></table>");
    }
    body.push_str("<section class=\"schema\"><h2>Configuration Reference</h2>");
    body.push_str(schema_table);
    body.push_str("</section>");
    body
}

fn render_edit(tag: &str, content: &str, message: Option<&str>) -> String {
    let mut body = String::new();
    let encoded = utf8_percent_encode(tag, SAFE_PATH_SEGMENT).to_string();
    writeln!(
        &mut body,
        "<h1>Edit configuration: {}</h1>",
        escape_html(tag)
    )
    .ok();
    if let Some(msg) = message {
        body.push_str(&format!("<p class=\"error\">{}</p>", escape_html(msg)));
    }
    body.push_str(&format!(
        "<form method=\"post\" action=\"/configs/{}/save\">",
        encoded
    ));
    body.push_str("<textarea name=\"content\" rows=\"32\">");
    body.push_str(&escape_html(content));
    body.push_str("</textarea>");
    body.push_str("<div class=\"form-actions\"><button type=\"submit\">Save</button> <a class=\"secondary\" href=\"/\">Cancel</a></div>");
    body.push_str("<p class=\"note\">Saving will validate the configuration, commit it to the configuration repository, and restart the Photo Frame.</p>");
    body.push_str("</form>");
    body
}

fn layout(body: &str) -> String {
    format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>Photo Frame Configuration</title><style>{}</style></head><body><main>{}</main></body></html>",
        styles(),
        body
    )
}

fn styles() -> &'static str {
    "body { font-family: sans-serif; margin: 0; padding: 0; background: #f5f5f5; color: #222; }\nmain { max-width: 960px; margin: 0 auto; padding: 24px; background: #fff; min-height: 100vh; box-sizing: border-box; }\nh1, h2 { margin-top: 0; }\ntable { width: 100%; border-collapse: collapse; margin-top: 16px; }\nth, td { border-bottom: 1px solid #ddd; padding: 8px; text-align: left; vertical-align: middle; }\ntr:hover { background: #fafafa; }\nform { display: inline-block; margin: 0 4px; }\nform button { padding: 6px 12px; font-size: 0.95rem; border-radius: 4px; border: 1px solid #1976d2; background: #2196f3; color: #fff; cursor: pointer; }\nform button:hover { background: #1e88e5; }\nform button.danger { border-color: #b71c1c; background: #d32f2f; }\nform button.danger:hover { background: #c62828; }\ntextarea { width: 100%; box-sizing: border-box; font-family: monospace; font-size: 0.95rem; padding: 12px; margin-top: 12px; border-radius: 6px; border: 1px solid #ccc; background: #fdfdfd; }\n.form-actions { margin-top: 12px; }\n.form-actions .secondary { margin-left: 12px; text-decoration: none; color: #1976d2; }\n.note { font-size: 0.9rem; color: #555; }\n.error { background: #ffebee; color: #b71c1c; padding: 12px; border-radius: 4px; }\n.active { color: #2e7d32; }\n.tag { font-weight: 600; }\n.schema table { margin-top: 12px; }\n.schema td, .schema th { border-bottom: 1px solid #eee; padding: 6px; font-size: 0.95rem; }\n.schema tr:nth-child(odd) { background: #fafafa; }\n.schema .type { font-family: monospace; color: #37474f; }"
}

struct FieldDoc {
    path: String,
    type_label: String,
    description: Option<String>,
    enum_values: Vec<String>,
}

fn render_schema_table(schema: &RootSchema) -> String {
    let mut docs = Vec::new();
    let root_schema = Schema::Object(schema.schema.clone());
    collect_fields(&root_schema, schema, "", &mut docs);
    if docs.is_empty() {
        return "<p>No schema information available.</p>".to_string();
    }
    let mut out = String::new();
    out.push_str(
        "<table><thead><tr><th>Key</th><th>Type</th><th>Description</th></tr></thead><tbody>",
    );
    for doc in docs {
        out.push_str("<tr>");
        out.push_str(&format!("<td><code>{}</code></td>", escape_html(&doc.path)));
        out.push_str(&format!(
            "<td class=\"type\">{}</td>",
            escape_html(&doc.type_label)
        ));
        let mut desc = doc.description.unwrap_or_default();
        if !doc.enum_values.is_empty() {
            if !desc.is_empty() {
                desc.push(' ');
            }
            desc.push_str("Options: ");
            desc.push_str(&doc.enum_values.join(", "));
        }
        if desc.is_empty() {
            desc.push_str("&mdash;");
        }
        out.push_str(&format!("<td>{}</td>", escape_html(&desc)));
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table>");
    out
}

fn collect_fields(schema: &Schema, root: &RootSchema, path: &str, out: &mut Vec<FieldDoc>) {
    let schema = deref_schema(schema, root);
    match schema {
        Schema::Bool(true) | Schema::Bool(false) => {}
        Schema::Object(obj) => {
            let type_label = instance_type_label(obj.instance_type.as_ref());
            let description = obj
                .metadata
                .as_ref()
                .and_then(|meta| meta.description.clone())
                .filter(|d| !d.is_empty());
            let enum_values = obj
                .enum_values
                .as_ref()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if !path.is_empty() {
                out.push(FieldDoc {
                    path: path.to_string(),
                    type_label,
                    description,
                    enum_values,
                });
            }
            if let Some(validation) = &obj.object {
                for (key, subschema) in &validation.properties {
                    let new_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    collect_fields(subschema, root, &new_path, out);
                }
                if let Some(additional) = &validation.additional_properties {
                    let new_path = if path.is_empty() {
                        "*".to_string()
                    } else {
                        format!("{path}.*")
                    };
                    collect_fields(additional.as_ref(), root, &new_path, out);
                }
            }
        }
    }
}

fn deref_schema<'a>(schema: &'a Schema, root: &'a RootSchema) -> &'a Schema {
    if let Schema::Object(obj) = schema {
        if let Some(reference) = &obj.reference {
            if let Some(name) = reference.strip_prefix("#/definitions/") {
                if let Some(next) = root.definitions.get(name) {
                    return deref_schema(next, root);
                }
            }
        }
    }
    schema
}

fn instance_type_label(instance: Option<&SingleOrVec<InstanceType>>) -> String {
    match instance {
        None => "any".to_string(),
        Some(SingleOrVec::Single(single)) => instance_name(single),
        Some(SingleOrVec::Vec(list)) => list
            .iter()
            .map(instance_name)
            .collect::<Vec<_>>()
            .join(" | "),
    }
}

fn instance_name(instance: &InstanceType) -> String {
    match instance {
        InstanceType::Null => "null".into(),
        InstanceType::Boolean => "bool".into(),
        InstanceType::Object => "object".into(),
        InstanceType::Array => "array".into(),
        InstanceType::Number => "number".into(),
        InstanceType::Integer => "integer".into(),
        InstanceType::String => "string".into(),
    }
}

fn escape_html(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '&' => escaped.push_str("&amp;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}
