use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Configuration {
    pub photo_library_path: PathBuf,
    pub photo_quarantine_path: PathBuf,
}

impl Configuration {
    pub fn from_yaml_file(path: impl AsRef<Path>) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&s)?)
    }
}
