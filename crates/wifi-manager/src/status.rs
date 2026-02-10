use crate::config::Config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ProvisionRequest {
    pub attempt_id: String,
    pub timestamp: String,
    pub ssid: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub timestamp: String,
    pub status: String,
    pub message: String,
    pub ssid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStateRecord {
    pub timestamp: String,
    pub state: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
}

pub fn request_path(config: &Config) -> PathBuf {
    config.var_dir.join("wifi-request.json")
}

pub fn last_attempt_path(config: &Config) -> PathBuf {
    config.var_dir.join("wifi-last.json")
}

pub fn runtime_state_path(config: &Config) -> PathBuf {
    config.var_dir.join("wifi-state.json")
}

pub fn now_rfc3339() -> Result<String> {
    Ok(OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?)
}

pub fn write_request(config: &Config, request: &ProvisionRequest) -> Result<()> {
    write_json_with_mode(&request_path(config), request, 0o600)
}

pub fn read_request(config: &Config) -> Result<Option<ProvisionRequest>> {
    let path = request_path(config);
    read_json_optional(&path)
}

pub fn remove_request(config: &Config) -> Result<()> {
    let path = request_path(config);
    match fs::remove_file(&path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

pub fn write_last_attempt(config: &Config, record: &AttemptRecord) -> Result<()> {
    write_json_with_mode(&last_attempt_path(config), record, 0o644)
}

pub fn read_last_attempt(config: &Config) -> Result<Option<AttemptRecord>> {
    read_json_optional(&last_attempt_path(config))
}

pub fn write_runtime_state(config: &Config, record: &RuntimeStateRecord) -> Result<()> {
    write_json_with_mode(&runtime_state_path(config), record, 0o644)
}

#[cfg(test)]
pub fn read_runtime_state(config: &Config) -> Result<Option<RuntimeStateRecord>> {
    read_json_optional(&runtime_state_path(config))
}

pub fn redact_ssid(ssid: &str) -> String {
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

fn write_json_with_mode<T: Serialize>(path: &Path, value: &T, mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent dir at {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(value)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(&json)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn read_json_optional<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    let data = match fs::read(path) {
        Ok(value) => value,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let value = serde_json::from_slice::<T>(&data)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::{
        AttemptRecord, ProvisionRequest, RuntimeStateRecord, read_last_attempt, read_request,
        read_runtime_state, redact_ssid, remove_request, write_last_attempt, write_request,
        write_runtime_state,
    };
    use crate::config::Config;
    use tempfile::tempdir;

    #[test]
    fn redact_ssid_masks_prefix() {
        assert_eq!(redact_ssid("abc"), "***");
        assert_eq!(redact_ssid("home-network"), "***ork");
    }

    #[test]
    fn request_round_trip_and_remove() {
        let tmp = tempdir().expect("tempdir");
        let mut cfg: Config = serde_yaml::from_str("{}").expect("parse");
        cfg.var_dir = tmp.path().to_path_buf();
        let request = ProvisionRequest {
            attempt_id: "a1".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            ssid: "Home".to_string(),
            password: "supersecret".to_string(),
        };
        write_request(&cfg, &request).expect("write");
        let read_back = read_request(&cfg).expect("read").expect("present");
        assert_eq!(read_back.attempt_id, request.attempt_id);
        remove_request(&cfg).expect("remove");
        assert!(read_request(&cfg).expect("read none").is_none());
    }

    #[test]
    fn attempt_and_state_round_trip() {
        let tmp = tempdir().expect("tempdir");
        let mut cfg: Config = serde_yaml::from_str("{}").expect("parse");
        cfg.var_dir = tmp.path().to_path_buf();

        let attempt = AttemptRecord {
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            status: "queued".to_string(),
            message: "queued".to_string(),
            ssid: "***ome".to_string(),
            attempt_id: Some("a1".to_string()),
            error: None,
        };
        write_last_attempt(&cfg, &attempt).expect("write attempt");
        assert_eq!(
            read_last_attempt(&cfg)
                .expect("read")
                .expect("present")
                .status,
            "queued"
        );

        let state = RuntimeStateRecord {
            timestamp: "2026-01-01T00:00:01Z".to_string(),
            state: "RecoveryHotspotActive".to_string(),
            reason: "link-lost".to_string(),
            attempt_id: Some("a1".to_string()),
        };
        write_runtime_state(&cfg, &state).expect("write state");
        assert_eq!(
            read_runtime_state(&cfg)
                .expect("read")
                .expect("present")
                .state,
            "RecoveryHotspotActive"
        );
    }
}
