//! Debug-only allocator and process-memory instrumentation for native QA.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOC_CALLS: AtomicU64 = AtomicU64::new(0);
static DEALLOC_CALLS: AtomicU64 = AtomicU64::new(0);
static REALLOC_CALLS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static DEALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_LIVE_BYTES: AtomicU64 = AtomicU64::new(0);

pub(crate) struct QaTrackingAllocator;

#[global_allocator]
static GLOBAL_ALLOCATOR: QaTrackingAllocator = QaTrackingAllocator;

fn add_live(bytes: usize) {
    let bytes = bytes as u64;
    let live = LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK_LIVE_BYTES.fetch_max(live, Ordering::Relaxed);
}

fn remove_live(bytes: usize) {
    let bytes = bytes as u64;
    let _ = LIVE_BYTES.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
        Some(live.saturating_sub(bytes))
    });
}

unsafe impl GlobalAlloc for QaTrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
            add_live(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
            add_live(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        DEALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        DEALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        remove_live(layout.size());
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let resized = unsafe { System.realloc(pointer, layout, new_size) };
        if !resized.is_null() {
            REALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
            if new_size >= layout.size() {
                let growth = new_size - layout.size();
                ALLOCATED_BYTES.fetch_add(growth as u64, Ordering::Relaxed);
                add_live(growth);
            } else {
                let shrink = layout.size() - new_size;
                DEALLOCATED_BYTES.fetch_add(shrink as u64, Ordering::Relaxed);
                remove_live(shrink);
            }
        }
        resized
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AllocatorSnapshot {
    pub(crate) alloc_calls: u64,
    pub(crate) dealloc_calls: u64,
    pub(crate) realloc_calls: u64,
    pub(crate) allocated_bytes: u64,
    pub(crate) deallocated_bytes: u64,
    pub(crate) live_bytes: u64,
    pub(crate) peak_live_bytes: u64,
}

pub(crate) fn allocator_snapshot() -> AllocatorSnapshot {
    AllocatorSnapshot {
        alloc_calls: ALLOC_CALLS.load(Ordering::Relaxed),
        dealloc_calls: DEALLOC_CALLS.load(Ordering::Relaxed),
        realloc_calls: REALLOC_CALLS.load(Ordering::Relaxed),
        allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
        deallocated_bytes: DEALLOCATED_BYTES.load(Ordering::Relaxed),
        live_bytes: LIVE_BYTES.load(Ordering::Relaxed),
        peak_live_bytes: PEAK_LIVE_BYTES.load(Ordering::Relaxed),
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ProcessMemorySnapshot {
    pub(crate) virtual_bytes: u64,
    pub(crate) resident_bytes: u64,
    pub(crate) peak_resident_bytes: u64,
}

#[cfg(target_os = "macos")]
pub(crate) fn process_memory_snapshot() -> Option<ProcessMemorySnapshot> {
    use mach2::kern_return::KERN_SUCCESS;
    use mach2::task::task_info;
    use mach2::task_info::{MACH_TASK_BASIC_INFO, task_info_t};
    use mach2::traps::mach_task_self;

    #[repr(C)]
    #[derive(Default)]
    struct TimeValue {
        seconds: i32,
        microseconds: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: TimeValue,
        system_time: TimeValue,
        policy: i32,
        suspend_count: i32,
    }

    let mut info = MachTaskBasicInfo::default();
    let mut count = (std::mem::size_of::<MachTaskBasicInfo>() / std::mem::size_of::<u32>()) as u32;
    let result = unsafe {
        task_info(
            mach_task_self(),
            MACH_TASK_BASIC_INFO,
            (&mut info as *mut MachTaskBasicInfo).cast::<i32>() as task_info_t,
            &mut count,
        )
    };
    (result == KERN_SUCCESS).then_some(ProcessMemorySnapshot {
        virtual_bytes: info.virtual_size,
        resident_bytes: info.resident_size,
        peak_resident_bytes: info.resident_size_max,
    })
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn process_memory_snapshot() -> Option<ProcessMemorySnapshot> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_counters_are_monotonic_around_owned_memory() {
        let before = allocator_snapshot();
        let memory = vec![0_u8; 16 * 1024];
        std::hint::black_box(&memory);
        let allocated = allocator_snapshot();
        assert!(allocated.alloc_calls >= before.alloc_calls);
        assert!(allocated.allocated_bytes >= before.allocated_bytes);
        assert!(allocated.peak_live_bytes >= before.peak_live_bytes);
        drop(memory);
        let released = allocator_snapshot();
        assert!(released.dealloc_calls >= allocated.dealloc_calls);
        assert!(released.deallocated_bytes >= allocated.deallocated_bytes);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn process_memory_snapshot_reports_current_residency() {
        let snapshot = process_memory_snapshot().expect("task_info should be available");
        assert!(snapshot.virtual_bytes >= snapshot.resident_bytes);
        assert!(snapshot.peak_resident_bytes >= snapshot.resident_bytes);
        assert!(snapshot.resident_bytes > 0);
    }
}
