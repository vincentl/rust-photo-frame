//! Configuration types and helpers for Photoframe.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// Top-level configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Directories to search for photos.
    #[serde(rename = "photo-paths")]
    photo_paths: Vec<PathBuf>,

    /// Scanning options.
    #[serde(default)]
    scan: ScanConfig,

    /// Display options.
    #[serde(default)]
    display: DisplayConfig,
}

impl Config {
    /// Validate that all photo paths exist and are directories.
    ///
    /// # Errors
    /// Returns [`Error::BadDir`] if any configured path is missing or not a directory.
    pub fn validate(&self) -> Result<(), Error> {
        let mut bad: Vec<PathBuf> = Vec::new();
        for p in &self.photo_paths {
            if !p.exists() || !p.is_dir() {
                bad.push(p.clone());
            }
        }
        if bad.is_empty() {
            Ok(())
        } else {
            let joined = bad
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ");
            Err(Error::BadDir(joined))
        }
    }

    /// Borrow the configured photo directories.
    #[must_use]
    pub fn photo_paths(&self) -> &[PathBuf] {
        &self.photo_paths
    }

    /// Borrow the scan configuration block.
    #[must_use]
    pub const fn scan(&self) -> &ScanConfig {
        &self.scan
    }

    /// Borrow the display configuration block.
    #[must_use]
    pub const fn display(&self) -> &DisplayConfig {
        &self.display
    }
}

/// Scanning configuration block.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanConfig {
    /// Recurse into subdirectories (default: true).
    pub recursive: Option<bool>,
    /// Optional maximum recursion depth (0/None = unlimited).
    pub max_depth: Option<usize>,
}

/// Display configuration block.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// Delay between images in milliseconds (default: 5000).
    #[serde(rename = "delay-ms")]
    pub delay_ms: Option<u64>,
}

/// Parse YAML text into [`Config`] and validate it.
///
/// # Errors
/// Returns YAML parse errors or validation failures.
pub fn from_yaml_str(s: &str) -> Result<Config, Error> {
    let cfg: Config = serde_yaml::from_str(s)?;
    cfg.validate()?;
    Ok(cfg)
}

/// Read a YAML file and load a validated [`Config`].
///
/// # Errors
/// Returns IO/YAML/validation errors.
pub fn from_yaml_file(path: &Path) -> Result<Config, Error> {
    let text = std::fs::read_to_string(path)?;
    from_yaml_str(&text)
}
