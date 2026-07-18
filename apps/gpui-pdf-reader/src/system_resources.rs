//! Small platform boundary for host resource policy inputs.

use key_workspace_core::SystemResources;

#[cfg(any(not(target_os = "macos"), test))]
const GIB: u64 = 1024 * 1024 * 1024;

/// Detects stable, cheap process-wide capacity signals at startup.
///
/// Activity remains dynamic in the workspace registry. Platform notification
/// adapters can refresh this snapshot later without changing participants.
pub(crate) fn detect() -> SystemResources {
    let logical_cpus = std::thread::available_parallelism()
        .map_or(1, |parallelism| parallelism.get())
        .min(usize::from(u16::MAX)) as u16;
    let (physical_memory_bytes, low_power_mode) = platform_capacity();
    SystemResources {
        physical_memory_bytes,
        logical_cpus,
        low_power_mode,
    }
}

#[cfg(target_os = "macos")]
fn platform_capacity() -> (u64, bool) {
    use objc2_foundation::NSProcessInfo;

    let process = NSProcessInfo::processInfo();
    (process.physicalMemory(), process.isLowPowerModeEnabled())
}

#[cfg(not(target_os = "macos"))]
fn platform_capacity() -> (u64, bool) {
    // Conservative fallback until other platform adapters are developed.
    (8 * GIB, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detected_capacity_is_always_usable() {
        let resources = detect();
        assert!(resources.physical_memory_bytes >= 2 * GIB);
        assert!(resources.logical_cpus >= 1);
    }
}
