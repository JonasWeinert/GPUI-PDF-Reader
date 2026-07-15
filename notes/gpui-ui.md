# GPUI UI notes

- A transparent macOS `TitlebarOptions` lets the app toolbar and traffic lights share one surface. Reserve the traffic-light width and mark only empty toolbar regions as `WindowControlArea::Drag`.
- Keep custom titlebar setup behind `cfg(target_os = "macos")`; other platforms should retain native decorations until their own chrome is implemented.
- `TOOLBAR_HEIGHT` is geometry, not just styling. Viewport sizing, content offsets, pointer mapping, and sidebar-anchor E2E all depend on it.
- GPUI has no first-class disabled button. A disabled control must omit `on_click`, lower opacity, and avoid a pointer cursor; dimming an active handler is misleading.
- Use `FocusHandle::is_focused(window)` before moving a cloned handle into an input canvas to style focused text fields and editors.
- `uniform_list` rows have fixed heights. Put spacing on an outer row and the border/background on an inner card so clipping stays deterministic.
- A custom editor toolbar can hover over a text surface with `absolute`, explicit top spacing, and `shadow_sm`; leave enough body inset so the pill never covers the first line.
- At the 700 px minimum width, selection actions take priority. Hide lower-priority global toggles temporarily rather than clipping controls or replacing labels with unclear glyphs.
- `Window::on_window_should_close` is synchronous: return `false`, open one guarded async prompt, then call `remove_window()` only after confirmation. Handle the app's Quit action at the reader root; `on_app_quit` runs too late to cancel termination.
- For native visual QA, identify the exact app window by PID/title with `CGWindowListCopyWindowInfo`, then capture that window ID only. Never use a full-screen capture when other documents may be visible.
