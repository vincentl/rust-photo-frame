//! Minimal shutdown coordination façade for future signal wiring.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Shared shutdown flag (set on SIGINT/SIGTERM when wired).
#[derive(Debug, Clone)]
pub struct ShutdownFlag(Arc<AtomicBool>);

impl ShutdownFlag {
    /// Create a new, unset flag.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// Set the flag.
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Check if the flag is set.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl Default for ShutdownFlag {
    fn default() -> Self {
        Self::new()
    }
}
