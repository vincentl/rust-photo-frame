use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use evdev::{Device, EventStream, InputEventKind, Key};
use thiserror::Error;
use tokio::process::Command;
use tokio::time::{self, Instant};

use crate::config::ButtonConfig;

const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum ButtonTaskError {
    #[error("unknown key code: {0}")]
    UnknownKey(String),
}

pub async fn spawn_button_task(
    cfg: ButtonConfig,
    mut shutdown_signal: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    if !cfg.enabled {
        tracing::info!("GPIO button task disabled via configuration");
        return Ok(());
    }

    let target_key = parse_key(&cfg.key_code)?;

    let open_fut = open_button_device(&cfg, target_key);
    tokio::pin!(open_fut);

    let mut device = tokio::select! {
        result = &mut open_fut => result.context("open power button input device")?,
        _ = &mut shutdown_signal => {
            tracing::info!("Shutdown requested before power button input device was ready");
            return Ok(());
        }
    };

    if cfg.grab_device {
        if let Err(err) = device.grab() {
            tracing::warn!("Failed to grab input device: {err}");
        }
    }

    let mut stream = device.into_event_stream().context("event stream")?;
    let mut output = cfg.output_name.clone();
    if output.is_none() && cfg.use_wlr_randr {
        output = detect_output().await;
    }
    let mut last_known_state: Option<bool> = None;

    loop {
        tokio::select! {
            _ = &mut shutdown_signal => {
                tracing::info!("Button task shutting down");
                break;
            }
            event = stream.next_event() => {
                let event = match event {
                    Ok(event) => event,
                    Err(err) => {
                        tracing::warn!("Input stream error: {err}");
                        continue;
                    }
                };

                if let InputEventKind::Key(key) = event.kind() {
                    if key == target_key && event.value() == 1 {
                        handle_key_down(
                            &cfg,
                            &mut stream,
                            &mut output,
                            &mut last_known_state,
                            target_key,
                        )
                        .await;
                    }
                }
            }
        }
    }

    Ok(())
}

async fn handle_key_down(
    cfg: &ButtonConfig,
    stream: &mut EventStream,
    output: &mut Option<String>,
    last_known_state: &mut Option<bool>,
    target_key: Key,
) {
    let start = Instant::now();
    let long_press = time::sleep(Duration::from_millis(cfg.long_threshold_ms));
    tokio::pin!(long_press);

    tokio::select! {
        _ = &mut long_press => {
            tracing::info!(
                "Long press >= {}ms → shutdown",
                cfg.long_threshold_ms
            );
            if let Err(err) = run_shutdown(cfg).await {
                tracing::warn!("Failed to execute shutdown command: {err:?}");
            }
            if let Err(err) = wait_for_key_up(stream, target_key).await {
                tracing::warn!("Failed to drain key release after shutdown trigger: {err:?}");
            }
        }
        event = stream.next_event() => {
            match event {
                Ok(event) => {
                    if let InputEventKind::Key(key) = event.kind() {
                        if key == target_key && event.value() == 0 {
                            let elapsed_ms = start.elapsed().as_millis() as u64;
                            if elapsed_ms < cfg.short_max_ms {
                                tracing::info!("Short press {}ms → toggle screen", elapsed_ms);
                                if let Err(err) = toggle_screen(output, last_known_state, cfg).await {
                                    tracing::warn!("Failed to toggle display: {err:?}");
                                }
                            } else {
                                tracing::info!("Dead-zone press {}ms (ignored)", elapsed_ms);
                            }
                        }
                    }
                }
                Err(err) => tracing::warn!("Error awaiting key release: {err}"),
            }
        }
    }
}

async fn wait_for_key_up(stream: &mut EventStream, target_key: Key) -> Result<()> {
    loop {
        let event = stream.next_event().await?;
        if let InputEventKind::Key(key) = event.kind() {
            if key == target_key && event.value() == 0 {
                return Ok(());
            }
        }
    }
}

fn parse_key(code: &str) -> Result<Key> {
    Key::from_str(code).map_err(|_| ButtonTaskError::UnknownKey(code.to_string()).into())
}

async fn open_button_device(cfg: &ButtonConfig, target_key: Key) -> Result<Device> {
    let mut delay = INITIAL_RETRY_DELAY;
    loop {
        match try_open_device(cfg, target_key) {
            Ok(device) => return Ok(device),
            Err(err) => {
                tracing::warn!(
                    "Power button input device unavailable: {err:?}; retrying in {}s",
                    delay.as_secs()
                );
                time::sleep(delay).await;
                delay = (delay * 2).min(MAX_RETRY_DELAY);
            }
        }
    }
}

fn try_open_device(cfg: &ButtonConfig, target_key: Key) -> Result<Device> {
    if let Some(path) = cfg.device_path.as_ref() {
        return Device::open(path).with_context(|| format!("open {}", path.display()));
    }

    for (path, device) in evdev::enumerate() {
        if device_matches(&device, target_key) {
            tracing::info!("Using input device {}", path.display());
            return Device::open(&path).with_context(|| format!("open {}", path.display()));
        }
    }

    Err(anyhow!("no compatible power button input device found"))
}

fn device_matches(device: &Device, target_key: Key) -> bool {
    let name = device.name().unwrap_or("").to_ascii_lowercase();

    if !name_matches(&name) {
        return false;
    }

    device
        .supported_keys()
        .map(|keys| keys.contains(target_key))
        .unwrap_or(false)
}

fn name_matches(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }

    trimmed.contains("pwr_button")
        || (trimmed.contains("power") && trimmed.contains("button"))
        || trimmed.contains("shutdown")
        || (trimmed.contains("gpio") && trimmed.contains("key"))
}

async fn toggle_screen(
    output: &mut Option<String>,
    last_known_state: &mut Option<bool>,
    cfg: &ButtonConfig,
) -> Result<()> {
    if !cfg.use_wlr_randr && !cfg.use_vcgencmd_fallback {
        tracing::info!("Display toggle requested but no control mechanisms are enabled");
        return Ok(());
    }

    if cfg.use_wlr_randr && output.is_none() {
        if let Some(name) = cfg.output_name.clone() {
            *output = Some(name);
        } else {
            *output = detect_output().await;
        }
    }

    let output_name = output.as_deref();
    let fallback_state = last_known_state.as_ref().copied().unwrap_or(true);
    let current_state = match screen_is_on(output_name, cfg).await {
        Ok(state) => {
            *last_known_state = Some(state);
            state
        }
        Err(err) => {
            tracing::warn!("Failed to query display state: {err:?}");
            fallback_state
        }
    };

    if current_state {
        if let Err(err) = turn_off(output_name, cfg).await {
            return Err(err);
        }
        *last_known_state = Some(false);
    } else {
        if let Err(err) = turn_on(output_name, cfg).await {
            return Err(err);
        }
        *last_known_state = Some(true);
    }

    Ok(())
}

async fn detect_output() -> Option<String> {
    match query_wlr_outputs().await {
        Ok(outputs) => outputs
            .into_iter()
            .find(|info| info.connected.unwrap_or(true))
            .map(|info| info.name),
        Err(err) => {
            tracing::warn!("Failed to detect display output: {err:?}");
            None
        }
    }
}

async fn screen_is_on(output_name: Option<&str>, cfg: &ButtonConfig) -> Result<bool> {
    if cfg.use_wlr_randr {
        if let Some(name) = output_name {
            if let Some(state) = query_wlr_output_state(name).await? {
                return Ok(state);
            }
        } else if let Some(name) = detect_output().await {
            if let Some(state) = query_wlr_output_state(&name).await? {
                return Ok(state);
            }
        }
    }

    if cfg.use_vcgencmd_fallback {
        if let Some(state) = query_vcgencmd_state().await? {
            return Ok(state);
        }
    }

    Ok(true)
}

async fn turn_off(output_name: Option<&str>, cfg: &ButtonConfig) -> Result<()> {
    let mut last_error: Option<anyhow::Error> = None;

    if cfg.use_wlr_randr {
        if let Some(name) = output_name {
            match run_wlr_randr(&["--output", name, "--off"]).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    tracing::warn!("wlr-randr failed to power off {name}: {err:?}");
                    last_error = Some(err);
                }
            }
        } else {
            tracing::warn!("No output name available for wlr-randr --off command");
        }
    }

    if cfg.use_vcgencmd_fallback {
        match run_vcgencmd(false).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                tracing::warn!("vcgencmd display_power 0 failed: {err:?}");
                last_error = Some(err);
            }
        }
    }

    if let Some(err) = last_error {
        Err(err)
    } else {
        Err(anyhow!("No display control mechanisms succeeded"))
    }
}

async fn turn_on(output_name: Option<&str>, cfg: &ButtonConfig) -> Result<()> {
    let mut last_error: Option<anyhow::Error> = None;

    if cfg.use_wlr_randr {
        if let Some(name) = output_name {
            match run_wlr_randr(&["--output", name, "--on"]).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    tracing::warn!("wlr-randr failed to power on {name}: {err:?}");
                    last_error = Some(err);
                }
            }
        } else {
            tracing::warn!("No output name available for wlr-randr --on command");
        }
    }

    if cfg.use_vcgencmd_fallback {
        match run_vcgencmd(true).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                tracing::warn!("vcgencmd display_power 1 failed: {err:?}");
                last_error = Some(err);
            }
        }
    }

    if let Some(err) = last_error {
        Err(err)
    } else {
        Err(anyhow!("No display control mechanisms succeeded"))
    }
}

async fn run_wlr_randr(args: &[&str]) -> Result<()> {
    let output = Command::new("wlr-randr")
        .args(args)
        .output()
        .await
        .context("spawn wlr-randr")?;

    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!("wlr-randr exited with status {}", output.status))
    }
}

async fn run_vcgencmd(power_on: bool) -> Result<()> {
    let value = if power_on { "1" } else { "0" };
    let output = Command::new("/usr/bin/vcgencmd")
        .arg("display_power")
        .arg(value)
        .output()
        .await
        .context("spawn vcgencmd")?;

    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "vcgencmd display_power {value} exited with status {}",
            output.status
        ))
    }
}

async fn run_shutdown(cfg: &ButtonConfig) -> Result<()> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(cfg.shutdown_command.as_str())
        .status()
        .await
        .context("spawn shutdown command")?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("shutdown command exited with status {status}"))
    }
}

#[derive(Debug)]
struct WlrOutputInfo {
    name: String,
    connected: Option<bool>,
    enabled: Option<bool>,
    has_current_mode: bool,
}

async fn query_wlr_outputs() -> Result<Vec<WlrOutputInfo>> {
    let output = Command::new("wlr-randr")
        .output()
        .await
        .context("spawn wlr-randr")?;

    if !output.status.success() {
        return Err(anyhow!("wlr-randr exited with status {}", output.status));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_wlr_outputs(&stdout))
}

fn parse_wlr_outputs(stdout: &str) -> Vec<WlrOutputInfo> {
    let mut outputs = Vec::new();
    let mut current: Option<WlrOutputInfo> = None;

    for line in stdout.lines() {
        if let Some(info) = parse_header_line(line) {
            if let Some(prev) = current.take() {
                outputs.push(prev);
            }
            current = Some(info);
            continue;
        }

        if line.trim().is_empty() {
            continue;
        }

        if let Some(info) = current.as_mut() {
            parse_detail_line(info, line.trim());
        }
    }

    if let Some(info) = current {
        outputs.push(info);
    }

    outputs
}

fn parse_header_line(line: &str) -> Option<WlrOutputInfo> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix("Output") {
        let name = rest.trim_start_matches(':').trim();
        if !name.is_empty() {
            return Some(WlrOutputInfo {
                name: name.to_string(),
                connected: None,
                enabled: None,
                has_current_mode: false,
            });
        }
    }

    if !line.starts_with(' ') {
        let mut parts = trimmed.split_whitespace();
        if let Some(name) = parts.next() {
            let mut info = WlrOutputInfo {
                name: name.trim_matches('"').to_string(),
                connected: None,
                enabled: None,
                has_current_mode: false,
            };

            for token in parts {
                let lower = token.to_ascii_lowercase();
                if lower.contains("connected") {
                    info.connected = Some(true);
                } else if lower.contains("disconnected") {
                    info.connected = Some(false);
                }
            }

            return Some(info);
        }
    }

    None
}

fn parse_detail_line(info: &mut WlrOutputInfo, line: &str) {
    let lower = line.to_ascii_lowercase();

    if lower.starts_with("enabled:") {
        if let Some(value) = line.split(':').nth(1) {
            let value = value.trim().to_ascii_lowercase();
            if value.starts_with('y') || value == "on" || value == "true" {
                info.enabled = Some(true);
            } else if value.starts_with('n') || value == "off" || value == "false" {
                info.enabled = Some(false);
            }
        }
    } else if lower.starts_with("current mode") {
        info.has_current_mode = true;
    } else if lower.contains("connected") {
        if lower.contains("disconnected") {
            info.connected = Some(false);
        } else {
            info.connected = Some(true);
        }
    }
}

async fn query_wlr_output_state(name: &str) -> Result<Option<bool>> {
    let outputs = query_wlr_outputs().await?;
    for info in outputs {
        if info.name == name {
            if let Some(enabled) = info.enabled {
                return Ok(Some(enabled));
            }
            if let Some(connected) = info.connected {
                if !connected {
                    return Ok(Some(false));
                }
            }
            if info.has_current_mode {
                return Ok(Some(true));
            }
            return Ok(None);
        }
    }
    Ok(None)
}

async fn query_vcgencmd_state() -> Result<Option<bool>> {
    let output = Command::new("/usr/bin/vcgencmd")
        .arg("display_power")
        .output()
        .await
        .context("spawn vcgencmd")?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.split('=').nth(1) {
            match rest.trim() {
                "1" => return Ok(Some(true)),
                "0" => return Ok(Some(false)),
                _ => {}
            }
        }
    }

    Ok(None)
}
