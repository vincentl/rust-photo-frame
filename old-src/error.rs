//! Shared error type for the Photoframe library.

/// Crate error type
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// One or more configured directories are missing or invalid.
    #[error("invalid photo directory: {0}")]
    BadDir(String),

    /// No images found after scanning or filtering.
    #[error("no images found in configured directories")]
    EmptyScan,

    /// Wrapper for std IO errors.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Configuration parse error (YAML via `serde_yaml`).
    #[error(transparent)]
    Config(#[from] serde_yaml::Error),

    /// Rendering/backend error bubbled up from the viewer.
    #[error(transparent)]
    Render(#[from] anyhow::Error),
}
