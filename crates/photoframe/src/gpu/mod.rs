pub mod debug_overlay;

/// Held by every test that creates a wgpu device, for as long as it uses
/// it: one device at a time per test binary. Lavapipe (the software Vulkan
/// in the Linux VM, developer/lima.md) crashes intermittently when tests
/// create and destroy devices on parallel test threads (review L6). Real
/// GPUs do not need it, and the cost is a few serialized tests.
#[cfg(test)]
static GPU_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A device for a test, with the lock that serializes GPU tests. Bind the
/// guard first (`let Some((_gpu, device, queue)) = ...`) so it is dropped
/// last, after the device. `None` when no adapter is available; the caller
/// skips the test.
#[cfg(test)]
pub(crate) fn test_device(
    label: &str,
) -> Option<(
    std::sync::MutexGuard<'static, ()>,
    wgpu::Device,
    wgpu::Queue,
)> {
    test_device_with(label, wgpu::Features::empty())
}

/// `test_device` with whichever of `features` the adapter offers enabled
/// (check `device.features()`).
#[cfg(test)]
pub(crate) fn test_device_with(
    label: &str,
    features: wgpu::Features,
) -> Option<(
    std::sync::MutexGuard<'static, ()>,
    wgpu::Device,
    wgpu::Queue,
)> {
    // A test that panicked while holding the lock does not make the device
    // any less usable for the next one.
    let guard = GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some(label),
        required_features: adapter.features() & features,
        required_limits: adapter.limits(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::default(),
    }))
    .ok()?;
    Some((guard, device, queue))
}
