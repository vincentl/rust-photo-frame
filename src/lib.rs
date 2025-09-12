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
/// Library error type used across modules.
pub mod error;
/// Small façade for graceful shutdown coordination.
pub mod events;
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

/// Run the slideshow using the existing render path.
///
/// This function serves as a stable public entrypoint from `main.rs`.
///
/// # Errors
/// Propagates rendering or IO errors as [`Error`].
pub fn run_slideshow(buffer: &mut PhotoBuffer, display: &DisplayOptions) -> Result<(), Error> {
    // Keep the coupling to the rendering module behind this small shim,
    // so future render backends can swap in.
    render::viewer::run_slideshow(buffer, display.delay_ms).map_err(Error::Render)
}

// Keep render private to the library; only the `run_slideshow` API is public.
mod render {
    pub mod viewer {
        use crate::buffer::PhotoBuffer;

        /// Adapter to call the existing viewer implementation.
        ///
        /// NOTE: Replace this stub with the call into your
        /// actual `render::viewer::run_slideshow` function.
        /// The signature assumed here is `(buffer, delay_ms) -> anyhow::Result<()>`.
        #[allow(
            clippy::unnecessary_wraps,
            clippy::needless_pass_by_ref_mut,
            clippy::missing_const_for_fn
        )]
        pub fn run_slideshow(_buffer: &mut PhotoBuffer, _delay_ms: u64) -> anyhow::Result<()> {
            // Replace this with your real rendering invocation.
            Ok(())
        }
    }
}
