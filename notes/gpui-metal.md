# GPUI and Metal

- GPUI's Blade/Metal path can submit commands with unretained texture references. Dropping an image immediately after a frame can destroy a texture the GPU is still sampling.
- Retire removed `RenderImage`s only after two `on_next_frame` callbacks, then call `window.drop_image`.
- The rapid-zoom freeze happened when several visible pages were replaced together. A longer debounce reduced its frequency but did not fix the texture-lifetime bug.
- `request_animation_frame` requires an active paint context and can panic from an input handler. Queue `on_next_frame`, call `cx.notify`, and chain later ticks from the callback.
- GPUI's macOS upload path expects BGRA. `RgbaImage` is only the owned four-byte carrier here; do not swap PDFium's native channels.
- Paint a tile's bleed bitmap through a `ContentMask` matching its core. Mask text highlights to the page too.
- Apply precise trackpad deltas directly. Frame-based smoothing is useful for keyboard scrolling.
- AppKit magnification coordinates are bottom-left based; GPUI coordinates are top-left based, so invert y.
- The AppKit event monitor retains its block and token. A process-lifetime monitor is acceptable for this single-window app.
