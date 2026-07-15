# Scheduling and zoom

- Debouncing new zoom renders is insufficient by itself because the worker otherwise continues the old multi-tile queue.
- On the first event in a zoom burst, replace demand with an empty viewport. Submit the settled viewport after the 150ms debounce.
- Schedule visible tiles first, explicit copy text second, automatic visible-page text third, and prefetch tiles last.
- A new viewport replaces queued demand; it does not append to it.
- Leave an in-flight tile pending while PDFium renders. Drain new commands afterwards and publish only if the exact key is still demanded for the current generation.
- Discard stale failures as well as stale successes. A canceled render error must not put the reader into an error state.
- Keep the UI's complete desired viewport signature stable across completions. Shrinking demand after each tile can race and reinsert completed work.
- Protect current visible tiles from cache eviction even when the nominal cache target is exceeded.
- Keep old-scale tiles until all exact visible replacements for that page exist to avoid blank flashes.
- Cursor-anchored zoom saves a normalized page anchor before rebuilding layout and restores that content point afterwards.
- Reject non-finite input and clamp one accelerated wheel packet's zoom contribution.
