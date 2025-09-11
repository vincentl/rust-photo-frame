use anyhow::{Result, bail};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    pub photo_paths: Vec<PathBuf>,
}

impl Config {
    pub fn from_yaml(s: &str) -> Result<Self> {
        Ok(serde_yaml::from_str(s)?)
    }

    pub fn validate(&self) -> Result<()> {
        if self.photo_paths.is_empty() {
            bail!("photo-paths is empty");
        }
        for p in &self.photo_paths {
            if !p.is_dir() {
                bail!("photo path is not a directory: {}", p.display());
            }
        }
        Ok(())
    }
}
