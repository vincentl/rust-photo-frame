use once_cell::sync::Lazy;
use std::sync::Once;
use tracing_subscriber::EnvFilter;

static INIT: Once = Once::new();
static FILTER: Lazy<EnvFilter> =
    Lazy::new(|| EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")));

pub fn init() {
    INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(FILTER.clone())
            .with_target(true)
            .with_thread_ids(false)
            .with_level(true)
            .init();
    });
}
