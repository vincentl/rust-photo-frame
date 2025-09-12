use thiserror::Error;

/// Library error type for photoframe operations.
#[derive(Debug, Error)]
pub enum Error {
    /// One or more configured photo directories are invalid or unreadable.
    #[error("invalid photo directory: {0}")]
    BadDir(String),

    /// The scan completed but found no images.
    #[error("no images found in configured directories")]
    EmptyScan,

    /// Underlying IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// YAML/serde configuration error.
    #[error(transparent)]
    Config(#[from] serde_yaml::Error),

    /// Rendering/display error from downstream viewer.
    #[error("render error: {0}")]
    Render(anyhow::Error),
}
