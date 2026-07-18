# GPUI PDF Reader

GPUI PDF Reader is a fast, native PDF reader built with Rust, GPUI, and PDFium.
It is designed as a cross-platform application, with smooth navigation,
bounded high-resolution rendering, and a selectable text layer.

> GPUI PDF Reader is under active development. macOS is currently the only
> platform being actively developed, built, tested, and supported. Linux and
> Windows are intended targets, but the current source tree should not be
> considered functional or supported on either platform yet.

There are no signed or notarized binary releases yet. The current version is
best suited to contributors, testers, and people comfortable building Rust
software from source.

## Platform status

| Platform | Status |
|---|---|
| macOS, Apple silicon | Actively developed and tested |
| macOS, Intel | Source and PDFium fetch support; release validation still needed |
| Linux | Planned, not currently developed or supported |
| Windows | Planned, not currently developed or supported |

The core document, layout, scheduling, and text-selection code is written to
remain portable. Platform integration and the current GPUI dependency setup
are macOS-specific today.

## Highlights

- Open PDFs from the toolbar, `Command-O`, or the command line.
- Smooth continuous horizontal and vertical scrolling with a trackpad, mouse,
  or keyboard.
- Cursor-anchored pinch and `Command`/`Control`-wheel zoom from 20% to 500%,
  with Fit Width and 100% controls.
- Bounded high-resolution viewport tiles instead of oversized full-page
  bitmaps at high zoom.
- Selectable text, word selection, cross-page selection, Select All, and copy.
- Selection-anchored highlights in yellow, green, blue, pink, or purple.
- Markdown-backed comments with a WYSIWYG editor for bold, italic, inline
  code, bulleted lists, and numbered lists.
- Classic and Fluid reader views, selectable from the View menu. Classic keeps
  controls in the titlebar; Fluid uses intent-sensitive floating controls over
  the document.
- Thirty-seven bundled gpui-component themes, selectable from View → Theme,
  plus a system-following mode. All reader chrome and icons use shared semantic
  theme tokens. Dark themes use PDFium forced-color rendering for dark paper
  and readable text/vector content while preserving embedded image pixels.
  The PDF paper stays distinct from the workspace, and Fluid view provides a
  moon/sun control to show the original light PDF without changing app theme.
- Animated comments and search panels. Classic makes room for them; Fluid
  floats near-full-height panels over the PDF while extending horizontal reach
  so covered content remains accessible.
- Case-insensitive in-document search with on-page result highlights, a
  virtualized result list, and previous/next navigation.
- PDFium rendering for intrinsic page rotation, CropBox pages, annotations,
  and AcroForm appearances.
- Outline-aware navigation for PDFs with a table of contents: a subtle
  hierarchy rail follows reading position, reveals section detail on hover,
  and smoothly moves to explicit destinations or matched page headings when
  clicked.
- Clickable PDF links for URLs and in-document destinations. Internal jumps
  refine rough page destinations against nearby text and use the same centered,
  transient focus animation as search and outline navigation.
- Hover previews for links and scientific citations, including grouped
  references, OpenAlex/Semantic Scholar metadata, selectable citation details,
  abstract/TLDR tabs, DOI copy, open-access links, and bounded website images.
- A capability-based extension host with declarative and sandboxed WebAssembly
  packages, permission previews, lifecycle management, nested host-owned menu
  slots, and bounded host-rendered panels. Installable packages cannot load
  arbitrary native Rust or receive GPUI/PDFium handles.
- Latest-wins rendering and bounded caches to keep rapid scrolling and zooming
  responsive.

Forms are rendered for visual fidelity but are not interactive yet.

Highlights and comments are app-managed annotations. GPUI PDF Reader leaves
the PDF unchanged and stores them beside it in a versioned JSON sidecar named
`<document>.pdf.gpui-pdf-reader.json`. The reader validates the sidecar against
the PDF's file size, modification time, and page count before loading or
saving it. Keep the sidecar with the PDF when moving a document if you want to
retain its annotations.

## Build from source

You need:

- macOS
- Xcode Command Line Tools
- A current stable Rust toolchain

GPUI PDF Reader uses Rust edition 2024. A minimum supported Rust version has
not been declared yet.

The repository includes the audited Apple silicon PDFium binary used during
development. To install or refresh the pinned PDFium build for the current Mac
architecture, then run GPUI PDF Reader:

```sh
./scripts/fetch-pdfium.sh
cargo run --locked -- /path/to/document.pdf
```

For an optimized build:

```sh
cargo build --release --locked
./target/release/gpui-pdf-reader /path/to/document.pdf
```

The default `standard` bundle includes local installable extensions and bounded
scholarly networking. A smaller reader omits Wasmtime and all scholarly/network
providers while retaining the core PDF, text, search, comment, and annotation
experience:

```sh
cargo build --release --locked --no-default-features
```

The fetch script downloads Chromium PDFium build 7763, selects `mac-arm64` or
`mac-x64`, verifies a pinned SHA-256 digest, and retains the upstream notices.

An alternative matching-architecture PDFium library can be provided as a file
or directory:

```sh
PDFIUM_DYNAMIC_LIB_PATH=/path/to/libpdfium.dylib \
  cargo run --locked -- /path/to/document.pdf
```

Runtime lookup order is `PDFIUM_DYNAMIC_LIB_PATH`, the executable directory,
the executable's `../Resources` directory, `vendor/pdfium/lib`, and finally the
system library lookup path.

For local redistribution, place `libpdfium.dylib` beside the executable or in
`GPUI PDF Reader.app/Contents/Resources` and retain all project and dependency
notices. A local, unsigned application bundle can be assembled after either
release build:

```sh
sh scripts/package-macos-app.sh standard target/release/gpui-pdf-reader
# Or, after the --no-default-features build:
sh scripts/package-macos-app.sh minimal target/release/gpui-pdf-reader
```

The assembler places the executable, PDFium, theme provenance/assets, and a
feature-selected dependency inventory with retained notices in
`target/dist/<bundle>/GPUI PDF Reader.app`, then checks the runtime PDFium path,
Mach-O architectures, and dynamic-library assumptions. Run
`sh scripts/test-macos-bundle.sh` for a fast assembly smoke test without a full
reader build. Signing, notarization, and automatic updates have not been
implemented yet.

## Controls

| Action | Input |
|---|---|
| Open | Toolbar or `Command-O` |
| Switch layout | Classic View or Fluid View in the View menu |
| Scroll | Two-finger trackpad, mouse wheel, or both trackpad axes |
| Horizontal scroll | Native horizontal gesture or `Shift`-wheel |
| Pan | Middle-button drag |
| Zoom | Pinch, `Command`/`Control`-wheel, toolbar `−`/`+`, or `Command--` / `Command-=` |
| Actual size | `Command-0` |
| Fit width | Toolbar or View menu |
| Fine navigation | Arrow keys |
| Page navigation | `Page Up` / `Page Down`, `Shift-Space` / `Space` |
| First / last page | `Home` / `End` |
| Select text | Left drag; `Shift`-click extends; double-click selects a word |
| Select all / copy | `Command-A` / `Command-C` |
| Highlight selection | Choose one of the five color controls; Fluid also shows a selection pill |
| Add comment to selection | Toolbar or `Command-Option-M` |
| Search document | Toolbar or `Command-F` |
| Next / previous search result | `Command-G` / `Command-Shift-G` |
| Show / hide comments | Comments control in the toolbar |
| Install or update an extension | File → Install or Update Extension |
| Manage installed extensions | Tools → Extensions → Manage |

The comment editor displays formatted content directly while storing Markdown.
Its hovering formatting pill provides bold, italic, inline code, bulleted-list,
and numbered-list controls. Both views auto-save edits; `Escape` or Back returns
to the comments list. Classic keeps this workflow in its docked sidebar, while
Fluid animates between panes in its floating panel.

Keyboard scrolling is animated. Precise trackpad deltas are applied directly,
and zoom gestures preserve the document position beneath the pointer.

## Current limitations

- Only macOS is currently implemented and supported.
- Encrypted PDFs do not have a password prompt.
- Thumbnail navigation, PDF-embedded annotation editing, and interactive form
  filling are not implemented yet.
- Highlights and comments use a companion sidecar; they are not written into
  the PDF and are not interoperable with PDF annotation tools yet.
- There is no packaged, signed, or notarized application release.
- The extension API and local package format are pre-stable. Local packages are
  explicitly marked unverified; a signed registry and revocation service have
  not been implemented.
- Zoom is limited to 20–500%.
- PDFium's initial text-page loading call is synchronous. Later character
  extraction is cancellable and scheduled behind visible rendering.
- Automatic text indexing is limited to the nearest 16 visible pages at once.
  This is normally invisible, but can matter for PDFs with many unusually tiny
  pages on screen simultaneously.

## Development

The repository is a virtual Cargo workspace. The standalone app lives in
`apps/gpui-pdf-reader`; reusable editor, PDF domain/runtime/PDFium, shared UI,
storage, safe-network, and extension layers live in `crates/`. Reference and
first-party feature packages live in `extensions/`. No reusable crate imports
the standalone app shell.

Core crates contain no GPUI, PDFium, Wasmtime, network, or app-shell types.
`key-pdf-gpui` provides an embeddable viewport controller/entity adapter;
applications inject the PDF engine, annotation store, product chrome, and
optional capabilities. The semantic extension APIs likewise contain no GPUI,
PDFium, Wasmtime, filesystem-path, or socket types.

GPUI owns the window, input, layout, and GPU painting. PDFium rasterizes pages
and supplies character data for the text layer. All PDFium calls run on one
dedicated worker thread because document and form handles are not assumed to
be thread-safe.

Rendering uses 1024px tile cores with a 32px bleed gutter. Only the core is
painted; the gutter prevents PDFium edge culling and antialiasing seams,
including on intrinsically rotated pages. Tile allocation is capped at
1088×1088 BGRA pixels, and the GPU cache targets 48 tiles or 128 MiB while
protecting the exact visible working set.

Viewport requests replace stale queued work. Visible tiles run before text
extraction, document search, and prefetch work. PDFium rendering, text
extraction, and search all stay on the same worker thread. Zoom rendering is
debounced for 150ms, while a new zoom burst immediately cancels the previous
queued viewport. Stale successes and failures are both discarded.

Text coordinates are extracted at a stable precision independent of current
zoom and indexed in a bounded spatial grid. Copy streams uncached pages rather
than retaining the full document, and Select All stores only its endpoints.

Removed GPU images cross two frame callbacks before their textures are
released. This is required because GPUI's Metal renderer may still reference a
texture from an already submitted frame during rapid multi-page zoom.

Short investigation notes are kept in [`notes/`](notes/). The focused PDFium
tile extension is documented in
[`vendor/pdfium-render-tile/TILE_PATCH.md`](vendor/pdfium-render-tile/TILE_PATCH.md).
The workspace map is in [`notes/architecture.md`](notes/architecture.md), and
the installable package guide is in [`extensions/README.md`](extensions/README.md).

## Testing

Run the deterministic development suite with:

```sh
sh scripts/test.sh
```

It checks formatting, all workspace targets, Clippy with warnings denied, the
minimal bundle, architectural dependency boundaries, the explicit
permissive-license policy, and the focused tiled-versus-full PDFium pixel
regression. The PDFium regression covers portrait pages, intrinsic rotation,
CropBox geometry, annotations, and AcroForm appearances.

The native macOS E2E suite opens a real GPUI window and stresses rapid
Command-wheel zoom in both directions plus keyboard zoom across the render
debounce:

```sh
sh tests/e2e/macos_zoom.sh
```

The feature scenario creates all five highlight colors, types and formats a
multiword comment through GPUI's native input path, opens and closes both
sidebars while injecting live trackpad-style input, types a query into the
search field, navigates results, then relaunches the copied fixture to verify
the sidecar round trip:

```sh
sh tests/e2e/macos_features.sh
```

The Fluid scenario types and auto-saves a comment, clicks its highlighted text,
slides between the comment list and editor, opens a list item, searches the
document, verifies overlay-panel horizontal reach, and relaunches to check the
sidecar:

```sh
sh tests/e2e/macos_fluid.sh
```

The extension scenario installs and uses both reference packages, relaunches
to prove durable restoration, rejects a package that requests native code, and
contains a fuel-exhausting Component Model guest without taking down the app:

```sh
sh tests/e2e/macos_extensions.sh
```

Each E2E case has a hard watchdog and requires a quiet Ready state, all exact
visible tiles, bounded tile memory, and no panic or GPU/Metal fault in the app
or macOS logs. The feature scenario also measures document-anchor drift during
sidebar transitions. These scripts need a logged-in macOS GUI session but do
not require Accessibility permission.

The root integration fixture is `tests/fixtures/interaction.pdf`.

## Roadmap

Likely development areas include:

- Linux and Windows platform support
- Thumbnail navigation
- Password handling for encrypted PDFs
- Interactive forms
- A signed extension registry and stable extension API
- Packaged and signed application releases

The roadmap is directional rather than a release commitment.

## License

GPUI PDF Reader source is MIT licensed. Its supported dependency graph is
restricted to MIT, Apache-2.0, and more-permissive license choices; GPL, LGPL,
AGPL, MPL, and similar reciprocal-only dependencies are excluded by project
policy.

See [`LICENSE`](LICENSE) and
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md). PDFium's complete binary
notices are retained in [`vendor/pdfium/licenses/`](vendor/pdfium/licenses/),
and exact Rust dependency versions are locked in `Cargo.lock`.

`THIRD_PARTY_NOTICES.md` is an inventory and policy record, not a replacement
for the complete dependency license bundle required when distributing a
binary. The app-bundle assembler generates a standard- or minimal-specific
Rust dependency inventory, retains package-level notice files available in the
resolved sources, and includes the complete native PDFium and theme notices.
