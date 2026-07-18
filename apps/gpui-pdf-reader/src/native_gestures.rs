use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Copy, Debug)]
pub struct NativePinch {
    pub x: f32,
    /// Cocoa window coordinates have their origin at the bottom-left.
    pub cocoa_y: f32,
    pub delta: f32,
}

#[derive(Default)]
struct PinchBroadcast {
    subscribers: Mutex<Vec<flume::Sender<NativePinch>>>,
}

impl PinchBroadcast {
    fn subscribe(&self) -> flume::Receiver<NativePinch> {
        let (sender, receiver) = flume::unbounded();
        self.subscribers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(sender);
        receiver
    }

    fn publish(&self, pinch: NativePinch) {
        self.subscribers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|subscriber| subscriber.send(pinch).is_ok());
    }
}

static PINCH_BROADCAST: OnceLock<Arc<PinchBroadcast>> = OnceLock::new();

/// Subscribes one window to the process-wide native magnification monitor.
/// Readers still check GPUI activation before applying an event, so the one
/// AppKit event is routed only to its active PDF window.
pub fn subscribe_pinch_monitor() -> flume::Receiver<NativePinch> {
    PINCH_BROADCAST
        .get_or_init(|| {
            let broadcast = Arc::new(PinchBroadcast::default());
            install_platform_monitor(broadcast.clone());
            broadcast
        })
        .subscribe()
}

#[cfg(target_os = "macos")]
fn install_platform_monitor(broadcast: Arc<PinchBroadcast>) {
    use block2::RcBlock;
    use objc2_app_kit::{NSEvent, NSEventMask};
    use std::ptr::NonNull;

    let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let event_ref = unsafe { event.as_ref() };
        let location = event_ref.locationInWindow();
        broadcast.publish(NativePinch {
            x: location.x as f32,
            cocoa_y: location.y as f32,
            delta: event_ref.magnification() as f32,
        });
        event.as_ptr()
    });

    // AppKit owns the monitor for process lifetime. Unlike the old per-reader
    // path this executes exactly once, regardless of the number of windows.
    if let Some(monitor) = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::Magnify, &block)
    } {
        std::mem::forget(monitor);
    }
}

#[cfg(not(target_os = "macos"))]
fn install_platform_monitor(_: Arc<PinchBroadcast>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_reaches_every_live_window_and_prunes_closed_ones() {
        let broadcast = PinchBroadcast::default();
        let first = broadcast.subscribe();
        let second = broadcast.subscribe();
        let pinch = NativePinch {
            x: 10.0,
            cocoa_y: 20.0,
            delta: 0.25,
        };
        broadcast.publish(pinch);
        assert_eq!(first.recv().unwrap().delta, 0.25);
        assert_eq!(second.recv().unwrap().x, 10.0);

        drop(first);
        broadcast.publish(pinch);
        assert_eq!(second.recv().unwrap().cocoa_y, 20.0);
        assert_eq!(
            broadcast
                .subscribers
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len(),
            1
        );
    }
}
