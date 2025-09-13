#![deny(unsafe_code)]
#![warn(
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::cargo,
    clippy::nursery
)]

//! Photoframe library surface for Tier 1 goals.
//!
//! This crate exposes a small, cohesive API for:
//! - Loading and validating configuration.
//! - Scanning image directories with options.
//! - Managing a circular photo buffer.
//! - Running the display loop with a fixed delay.
//!
//! All fallible operations return [`Result<T, Error>`]. Library code never panics.

/// Circular slideshow buffer types and utilities.
pub mod buffer;
/// Configuration types and loading/validation helpers.
pub mod config;
/// Display utilities (validation, glue to the viewer).
pub mod display;
/// Library error type used across modules.
pub mod error;
/// Small façade for graceful shutdown coordination.
pub mod events;
/// Render backend provided by the binary.
pub mod render;
/// Directory scanning utilities and filters.
pub mod scan;

use config::{Config, DisplayConfig};
pub use error::Error;
use scan::scan;
use std::path::PathBuf;

/// Options to control the display loop.
#[derive(Debug, Clone, Copy)]
pub struct DisplayOptions {
    /// Delay between photos, in milliseconds.
    pub delay_ms: u64,
}

impl From<&DisplayConfig> for DisplayOptions {
    fn from(d: &DisplayConfig) -> Self {
        Self {
            delay_ms: d.delay_ms.unwrap_or(5_000),
        }
    }
}

/// Scan for photos according to configuration.
///
/// # Errors
/// Returns an error if any configured directory is invalid or if scanning fails.
pub fn scan_photos(cfg: &Config) -> Result<Vec<PathBuf>, Error> {
    scan(cfg.photo_paths()).map(|metas| metas.into_iter().map(|m| m.path).collect())
}
