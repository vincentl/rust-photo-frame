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
/// Render backend (provided by the app) with `viewer::run_slideshow`.
pub mod render;
/// Directory scanning utilities and filters.
pub mod scan;

use std::path::PathBuf;

use buffer::PhotoBuffer;
use config::{Config, DisplayConfig, ScanConfig};
use error::Error;
use scan::{ScanOptions, scan_with_options};

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
    let scan_cfg: &ScanConfig = cfg.scan();
    let opts = ScanOptions {
        recursive: scan_cfg.recursive.unwrap_or(true),
        max_depth: scan_cfg.max_depth,
        exts: None, // use default image extensions
    };
    scan_with_options(cfg.photo_paths(), &opts)
}

/// Build a circular photo buffer from discovered photos.
///
/// This validates emptiness and returns a buffer ready for iteration.
///
/// # Errors
/// Returns [`Error::EmptyScan`] if the input list is empty.
pub fn build_buffer(photos: Vec<PathBuf>) -> Result<PhotoBuffer, Error> {
    PhotoBuffer::from_vec(photos)
}

/// Run the slideshow using the render backend in `crate::render::viewer`.
///
/// Steps:
/// 1. Validate paths: skip missing/corrupt images with warnings.
/// 2. Pass validated list and the configured delay to the viewer.
///
/// # Errors
/// - [`Error::EmptyScan`] if no decodable images remain after validation.
/// - [`Error::Render`] propagated from the viewer backend.
pub fn run_slideshow(buffer: &mut PhotoBuffer, display: &DisplayOptions) -> Result<(), Error> {
    // Borrow the discovered paths.
    let validated: Vec<PathBuf> = display::filter_valid_images(buffer.as_slice());
    if validated.is_empty() {
        return Err(Error::EmptyScan);
    }

    // Call your real viewer. Expected signature:
    //     render::viewer::run_slideshow(validated, display.delay_ms)
    //
    // If your current viewer takes only a `Vec<PathBuf>`, add an overload or
    // thread `delay_ms` through your event loop timing.
    crate::render::viewer::run_slideshow(&validated, display.delay_ms).map_err(Error::Render)
}
