use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Configuration {
    /// Root directory to scan recursively for images.
    #[serde(alias = "photo_library_path")]
    pub photo_library_path: PathBuf,
    /// GPU render oversample factor relative to screen size (1.0 = native).
    pub oversample: f32,
    /// Cross-fade duration in milliseconds.
    pub fade_ms: u64,
    /// Time an image remains fully visible before starting a transition, in ms.
    pub dwell_ms: u64,
    /// How many images the viewer preloads/keeps pending.
    #[serde(rename = "viewer-preload-count")]
    pub preload_count: usize,
    /// Maximum number of concurrent image decodes in the loader.
    #[serde(rename = "loader-max-concurrent-decodes")]
    pub max_in_flight: usize,
}

impl Configuration {
    pub fn from_yaml_file(path: impl AsRef<Path>) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&s)?)
    }
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            photo_library_path: PathBuf::new(),
            oversample: 1.0,
            fade_ms: 400,
            dwell_ms: 2000,
            preload_count: 3,
            max_in_flight: 4,
        }
    }
}
