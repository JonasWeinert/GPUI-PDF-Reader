#[derive(Clone, Copy, Debug)]
pub struct NativePinch {
    pub x: f32,
    /// Cocoa window coordinates have their origin at the bottom-left.
    pub cocoa_y: f32,
    pub delta: f32,
}

#[cfg(target_os = "macos")]
pub fn install_pinch_monitor() -> std::sync::mpsc::Receiver<NativePinch> {
    use block2::RcBlock;
    use objc2_app_kit::{NSEvent, NSEventMask};
    use std::ptr::NonNull;
    use std::sync::mpsc;

    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let event_ref = unsafe { event.as_ref() };
        let location = event_ref.locationInWindow();
        let _ = sender.send(NativePinch {
            x: location.x as f32,
            cocoa_y: location.y as f32,
            delta: event_ref.magnification() as f32,
        });
        event.as_ptr()
    });

    // AppKit retains both the monitor token and a copy of the block. The app has
    // one reader window, so this monitor intentionally lives for the process.
    if let Some(monitor) = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::Magnify, &block)
    } {
        std::mem::forget(monitor);
    }
    receiver
}

#[cfg(not(target_os = "macos"))]
pub fn install_pinch_monitor() -> std::sync::mpsc::Receiver<NativePinch> {
    let (_sender, receiver) = std::sync::mpsc::channel();
    receiver
}
