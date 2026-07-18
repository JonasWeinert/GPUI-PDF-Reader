# Installable extension architecture

## Purpose

Key should support installable extensions without executing arbitrary native
code. GPUI PDF Reader should ship as the official default bundle while other
developers can remove bundled features or add features such as redaction,
alternate navigation, document analysis, or new metadata providers.

Installability is part of the architecture from the start. It does not mean
that every current feature must immediately become an external extension.
Trusted, performance-sensitive functionality may remain native while using the
same commands, events, capabilities, and UI contribution model.

## Core decision

Use a hybrid extension model:

- A manifest describes identity, compatibility, permissions, dependencies,
  capabilities, contributions, limits, and settings.
- A constrained declarative format describes simple UI and composition.
- An optional WebAssembly component implements non-trivial logic.
- The host renders all UI with trusted GPUI components.
- Extensions interact only through typed, versioned host capabilities.
- Native built-ins and WebAssembly extensions adapt to the same semantic API.

Do not invent a general-purpose extension language. A small declarative format
is useful for UI and composition, but loops, algorithms, parsing, networking,
async work, and complex state belong in sandboxed WebAssembly.

## Package format

An installable package can use a single archive such as `.keyext`:

```text
example.keyext
  manifest.toml
  component.wasm       # optional
  ui.json              # optional
  assets/
    icons/
    images/
    themes/
  signature            # required for store distribution
```

Development packages may be loaded unpacked. Store-distributed packages must
be immutable, content-addressed, and signed. The exact archive and signature
formats require a separate security decision before implementation.

The manifest should contain at least:

- Stable reverse-domain extension ID.
- Name, version, publisher, description, and license.
- Extension API version range.
- Entry world/component and optional declarative UI entry.
- Required and optional capabilities.
- Required extension dependencies and version ranges.
- UI contribution declarations.
- Settings schema.
- Storage and cache requirements.
- Requested network scope.
- Minimum host and platform requirements.
- Package hashes and signature information.

Official application builds must retain the permissive-license policy. The
installer must show the license and prevent non-approved extensions from being
included in official distributions. Whether users may locally install other
licenses is a separate product-policy decision.

## Runtime foundation

Use the WebAssembly Component Model rather than raw WebAssembly exports.
WebAssembly Interface Types, or WIT, define typed interfaces and worlds. A
world describes what the extension imports from the host and exports back.
Resources provide opaque handles for documents, sessions, selections, tasks,
and stored objects without exposing host memory or implementation details.

WIT is the interface definition language, not the extension programming
language. Authors can use any supported source language that compiles to a
compatible component.

The runtime implementation should be replaceable behind an internal
`ExtensionRuntime` interface. Wasmtime is the likely first implementation, but
the public extension contract must not expose Wasmtime-specific types.

WASI should not be granted wholesale. Extensions receive only explicitly
linked interfaces. No filesystem, environment, clock, random, socket, process,
or HTTP access exists unless the host supplies a corresponding capability.

## Layering

```text
Application shell
  Extension package manager
  Permission and capability manager
  Declarative UI renderer
  Native extension adapter
  WebAssembly component runtime
  Typed command and event bridge
    PDF runtime
    Editor runtime
    Storage providers
    Safe HTTP provider
    OS and application services
```

### Host platform

The host platform owns:

- Application and window lifecycle.
- Task scheduling and cancellation.
- File picker and user-granted file handles.
- Clipboard and browser opening.
- Preferences, settings, logging, and diagnostics.
- Optional safe HTTP service.
- Extension installation, activation, suspension, upgrade, and removal.
- Permission prompts and capability resolution.
- UI slots, focus, z-order, keyboard routing, and panel ownership.

It must not expose raw GPUI windows or unrestricted OS APIs.

### PDF runtime

The PDF runtime owns:

- Document identity and session lifecycle.
- Page metadata and document coordinate systems.
- Rendering demand, caching, and result delivery.
- Text extraction and spatial indexes.
- Links, outlines, and navigation destinations.
- Scroll, zoom, selection, and jump primitives.
- Validated annotations and optional mutation operations.
- Engine capability discovery.

`key-pdfium` implements PDF engine operations. Extensions never receive raw
PDFium pointers, documents, pages, bitmaps, or bindings.

### Extension SDK

The SDK defines:

- Lifecycle interfaces.
- Commands and effects.
- Events and subscriptions.
- Immutable state snapshots.
- Capability handles.
- UI contribution types.
- Document overlay and hit-test protocols.
- Namespaced storage and settings.
- Diagnostics and structured errors.
- API compatibility and feature negotiation.

### Product composition

The standalone reader selects a default feature bundle. A future Key app may
select another bundle and inject different storage, navigation, and permission
policies. Product composition must not require modifying extension code.

## Extension kinds

### Declarative extensions

Contain no executable code. Suitable for:

- Theme packs.
- Toolbar and menu compositions.
- Forms and settings panels.
- Static or data-bound side panels.
- Simple command wiring.
- View presets.

The declarative language should remain deliberately limited:

- No general loops or recursion.
- Bounded conditionals and list mapping.
- No arbitrary expressions with side effects.
- No direct file, network, process, or clock access.
- All commands and state bindings must be typed and declared.

### WebAssembly extensions

Contain sandboxed logic for:

- Parsing and analysis.
- Async metadata and network workflows.
- Stateful tools.
- Search or indexing strategies.
- Document automation.
- Complex event handling.

### Native built-ins

Trusted code shipped with the application for functionality that needs tight,
high-frequency integration. It uses a native adapter implementing the same
logical extension protocol.

Likely initial native functionality includes the PDF surface, tiling, scrolling,
zooming, basic text extraction, and possibly selection painting. Features can
move across the boundary later if measurement and tooling justify it.

## Typed capability model

Extensions import semantic capabilities instead of generic system access.
Initial capability families may include:

```text
key:extension/lifecycle
key:commands
key:settings
key:storage
key:http
key:ui/contributions
key:ui/notifications
key:pdf/document
key:pdf/text
key:pdf/selection
key:pdf/navigation
key:pdf/overlays
key:pdf/annotations
key:pdf/mutations
key:editor/document
```

Capabilities must be narrow and independently grantable. Examples:

- `pdf.document.metadata`, not a generic document pointer.
- `pdf.text.read-page`, not direct access to the text cache.
- `http.domains(["api.openalex.org"])`, not raw sockets.
- `storage.document-namespace`, not arbitrary filesystem access.
- `pdf.mutations.redact`, not every PDFium editing API.

The host resolves required capabilities before activation. Missing required
capabilities disable the extension with an explanation. Missing optional
capabilities are reported through feature negotiation.

An extension manifest should describe both requirements and provisions. This
allows one extension to provide a service used by another, subject to a checked
acyclic dependency graph.

## Permissions

Permissions describe user-sensitive capability use. Installation and first use
should clearly explain them:

```text
This extension requests:
  Read text from open documents
  Add document overlays
  Add a side panel
  Connect to api.example.com

It does not receive:
  General filesystem access
  Raw network sockets
  Other open documents
  PDFium or GPUI handles
```

Network scopes should support:

- No network.
- Fixed declared domains.
- User-approved additional domains.
- Public internet, reserved for exceptional cases.

The safe HTTP host must retain redirect, DNS, private-address, timeout, body,
content-type, concurrency, and cache limits. Permissions do not replace these
technical protections.

Document access should be scoped to the active document unless the user grants
multi-document access. File access should use explicit user-selected handles or
namespaced extension storage.

WebAssembly isolation does not prevent misuse of granted data. An extension
with document-text and HTTP permissions can exfiltrate document text. Permission
design, publisher trust, review, and user consent remain essential.

## UI contribution model

Extensions do not construct arbitrary GPUI elements. They return a constrained,
versioned UI tree that the host maps to trusted themed components.

Possible nodes include:

```text
column
row
stack
text
styled-text
markdown
button
icon-button
toggle
select
text-field
list
tabs
badge
divider
image
progress
spacer
```

Each node has bounded properties. Arbitrary shaders, canvases, fonts, layout
callbacks, native views, scripts, HTML, and CSS are not accepted initially.

The host owns:

- Theme resolution.
- Accessibility labels and roles.
- Keyboard focus and traversal.
- Hover and pressed states.
- Text selection policy.
- Clipping and rounded corners.
- Animation limits.
- Panel placement and occlusion.
- Z-order and overlay priority.
- Responsive layout constraints.

Useful contribution slots include:

- Window menu.
- Command palette.
- Top toolbar.
- PDF floating toolbar.
- Selection context pill.
- Document overlay.
- Hover card.
- Side panel.
- Context menu.
- Status area.
- Settings panel.

Slots have typed contracts. An extension cannot invent a new global z-index or
place an unbounded surface over the entire application.

The host reserves `tools.extensions` for one optional direct trigger per active
extension. If an extension contributes an enabled command there, its
Tools → Extensions entry invokes that command. Otherwise the entry opens the
host-owned detail/settings page. Nested or additional extension commands remain
available through their normal contribution surfaces; they do not create an
unbounded application menu.

Install review, permissions, and manifest settings are host UI. Extension-owned
side/settings contributions are different: the host places them in a separate
bounded floating panel. This keeps package administration usable even when an
extension's own UI is unavailable or malformed.

## State and effect model

Use an event/update/view model:

```text
Host event
  Extension update
    New extension state
    Requested effects
      Host validates and executes effects
        Result event returned later
```

Extensions never directly mutate reader state. They consume immutable snapshots
and request typed effects.

Examples of snapshots:

- `DocumentSnapshot`.
- `ViewportSnapshot`.
- `SelectionSnapshot`.
- `AnnotationSnapshot`.
- `ThemeSnapshot`.
- `CapabilitySnapshot`.

Examples of effects:

- Jump to a document position.
- Request page text.
- Add or update an annotation.
- Open or close an extension panel.
- Request a bounded HTTP operation.
- Read or write namespaced storage.
- Copy text.
- Open a browser URL.
- Request a supported document mutation.
- Show a notification or confirmation.

Every effect is validated by the host even if it comes from a bundled extension.

## Native and WebAssembly adapters

```text
ExtensionHost
  NativeExtensionAdapter
  WasmExtensionAdapter
```

Both adapters expose the same manifest, subscriptions, snapshots, events,
effects, UI contributions, errors, and diagnostics. The host should not give
native built-ins a separate semantic API unless a measured performance path
requires it.

Performance-critical native code may use an internal fast path, but its output
must still enter shared arbitration for commands, panels, overlays, and focus.
This keeps the public extension path first-class.

## Execution and resource limits

Each WebAssembly extension receives a separate runtime store. Disabling or
unloading an extension drops its store and resources.

Limits should include:

- Component and package byte size.
- Linear memory and table growth.
- Instance, memory, table, and resource counts.
- WebAssembly stack size.
- Fuel or instruction budget per invocation.
- Epoch or wall-clock deadline.
- Periodic async yielding.
- Maximum host calls per invocation and time window.
- Maximum event and effect batch size.
- Maximum strings, lists, records, and response bytes.
- UI node count, nesting depth, list length, and asset dimensions.
- Storage and cache quotas.
- HTTP concurrency, redirects, body size, and rate.
- Overlay geometry and paint-operation budgets.

Runtime memory limits do not cover all host allocations caused by an extension.
Every host interface must independently bound inputs, outputs, retained handles,
queued work, and cache entries.

Extension execution must not block GPUI painting. High-frequency events are
coalesced, and expensive logic runs through cancellable background tasks. UI
trees or validated patches are delivered back to the main thread.

## Loop and re-entrancy protection

Static validation can reject:

- Extension dependency cycles.
- Declarative UI cycles.
- Invalid state bindings.
- Direct command aliases that form obvious cycles.
- Recursive component composition beyond a fixed depth.

Static validation cannot prove that arbitrary extension logic terminates.
Runtime limits handle infinite computation.

Event loops require host-level controls. Every event and effect chain should
carry a cause ID. The dispatcher should enforce:

- No synchronous re-entry into the same extension.
- Self-generated events are delivered on a later tick.
- Maximum dispatch depth.
- Maximum events and effects per tick.
- Identical effect coalescing.
- Bounded retries and backoff.
- Cancellation when a document generation changes.
- Temporary suspension after repeated budget violations.
- User-visible safe-mode disablement after persistent failure.

An extension should never receive an unbounded stream of raw pointer movement
or scroll packets by default. It explicitly subscribes to coalesced states such
as `viewport-changed`, `viewport-settled`, or `selection-changed`.

## Installation validation

Before execution, validate:

- Archive paths, sizes, compression ratios, and hashes.
- Manifest schema and canonical extension ID.
- Signature and publisher trust where required.
- License policy.
- API version compatibility.
- WIT world and import/export types.
- Imported capabilities against manifest requests.
- WASI imports against explicit grants.
- Extension dependency versions and acyclicity.
- Component, initial memory, table, and stack limits.
- Declarative UI schema, node limits, bindings, and command IDs.
- Asset formats, dimensions, and decoded byte limits.
- Settings and persisted-state schemas.

After static validation, instantiate with a very small initialization budget.
Activation failure must not prevent the application from starting.

## Lifecycle

Suggested lifecycle events:

- Installed.
- Validated.
- Activated.
- Application ready.
- Document opening/opened/closing/closed.
- Suspended.
- Resumed.
- Settings changed.
- Upgrading.
- Unloading.

Extension tasks, document handles, UI contributions, and caches are scoped to a
generation. Document close or extension disable cancels work and invalidates all
handles.

An application safe mode must start without third-party extensions. A failed
extension must be individually disableable without editing configuration files.

## Storage and migrations

Extensions receive namespaced storage rather than paths:

- Application settings.
- Document-scoped state.
- Ephemeral document cache.
- Optional user-visible exported data.

Storage has quotas, typed errors, atomic operations, and schema versions.
Upgrades declare migrations. The host should snapshot or transactionally migrate
state so a failed upgrade can roll back.

The standalone reader may map document state to adjacent sidecars. The future
Key app may map the same semantic store to its database. Extension code must not
depend on either representation.

## Redaction example

A redaction extension illustrates why semantic capabilities matter.

Frontend responsibilities:

- Let the user select text or rectangular regions.
- Paint a preview overlay.
- Show pending redactions and confirmation UI.
- Request application of validated redactions.

Backend responsibilities:

- Resolve selections into document geometry.
- Ask whether the active engine supports true content redaction.
- Submit a transactional mutation request.
- Require Save As or another explicit persistence policy.
- Return a result and invalidate rendered/text caches as needed.

The extension must not receive the PDFium page object. The host exposes a narrow
`pdf.mutations.redact` operation only after the engine implementation and tests
prove that underlying text and graphics are actually removed. A black rectangle
overlay must never be presented as secure redaction.

## Versioning and compatibility

- Version WIT packages and declarative UI schemas independently.
- Manifests specify compatible version ranges.
- Capabilities have stable names and explicit versions.
- Additive compatible changes should not require recompilation where possible.
- Breaking contracts require a new interface version or world.
- The host may provide limited compatibility adapters for older extension APIs.
- Extensions use feature negotiation rather than host-version conditionals.
- Persisted state schemas are extension-owned and independently versioned.

Contract fixtures should include older extension versions to test compatibility.

## Distribution and trust

Possible installation sources:

- Bundled official extensions.
- Local development directories.
- User-selected `.keyext` files.
- A future signed extension registry.

The registry should record publisher identity, package hashes, permissions,
license, supported API versions, review status, and security advisories.

Signing proves publisher and package integrity; it does not prove benign
behavior. Permission review, runtime isolation, store policy, and revocation are
still required.

## Testing

Maintain reference extensions covering:

- Declarative UI only.
- Backend-only Wasm logic.
- UI plus async backend work.
- Permission denial.
- Missing optional capability.
- Version mismatch.
- Dependency cycle.
- Infinite computation.
- Event feedback loop.
- Memory growth.
- Excessive UI output.
- Oversized asset and HTTP response.
- Document close during work.
- Upgrade and storage migration failure.
- Trap, timeout, and safe-mode recovery.

Test supported product bundles rather than every theoretical extension
combination:

- Minimal standalone reader.
- Standard standalone reader.
- Reader plus a reference third-party extension.
- Future full Key application.

Fuzz manifest parsing, package extraction, WIT boundary values, declarative UI,
effect validation, and extension state migrations.

## Initial delivery sequence

1. Define the manifest, capability taxonomy, command/effect model, and package
   validation rules without executing extensions.
2. Define versioned WIT lifecycle, UI, storage, command, and read-only PDF
   interfaces.
3. Implement the host lifecycle, capability, event, effect, quota, and safe-mode
   machinery and prove it with one trusted native feature using the semantic
   protocol.
4. Implement the constrained host-rendered UI tree and declarative extension
   path, then prove it with a theme-pack reference extension.
5. Implement a replaceable WebAssembly component runtime with strict limits,
   no default WASI capabilities, and the same protocol as the native adapter.
6. Build a Wasm document-statistics extension using read-only text access and a
   side panel.
7. Build a TOC alternative to validate dynamic UI, navigation, and overlays.
8. Revise the capability contract from the pilots before declaring any portion
   stable.
9. Expose annotation and safe HTTP capabilities only after the first read-only
   contract is stable.
10. Design and test document mutations separately before exposing redaction or
    other write operations.

This order intentionally proves the semantic host without a sandbox runtime,
then proves constrained UI, and only then adds WebAssembly execution. It keeps
runtime implementation choices from shaping the public API prematurely.

## Decisions still required

- Exact `.keyext` archive and canonicalization format.
- Signature scheme, publisher trust, and revocation mechanism.
- Wasmtime versus another component runtime after a focused prototype.
- Supported Component Model and WASI versions at first release.
- Declarative UI serialization and patch protocol.
- Whether third-party extensions may use licenses outside the official app's
  permissive-only distribution policy.
- Domain approval UX and whether permissions can be granted per document.
- Extension registry scope and review process.
- Whether bundled extensions ship natively, as Wasm, or selectively as both.
- Which PDF capabilities form the stable read-only v1 API.
- Whether true document mutation is an initial or later extension API.

These decisions should be resolved through small prototypes and threat-model
reviews before freezing a public extension API.
