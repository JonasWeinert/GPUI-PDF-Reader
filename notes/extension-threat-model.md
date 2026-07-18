# Extension threat model

## Scope

This model covers the extension host, `.keyext` packages, declarative UI,
WebAssembly components, PDF read capabilities, and bounded HTTP used by GPUI
PDF Reader. It does not claim to sandbox PDFium itself or trusted native code.

The first public PDF capability set is read-only with respect to document
bytes. Navigation and transient overlays may change the reader view, but no
extension can save, replace, redact, or otherwise mutate a PDF.

## Trust levels

- The application shell and bundled native adapters are trusted code. A native
  adapter can compromise the process and therefore cannot be installed from a
  package.
- Declarative packages are untrusted data. The host validates their manifest,
  paths, sizes, contribution tree, commands, bindings, and permissions, then
  renders them using trusted GPUI components.
- WebAssembly packages are untrusted code. Each component gets a separate
  bounded store and only explicitly linked semantic host calls. No general
  WASI filesystem, socket, process, clock, or environment authority is linked.
- Locally selected development packages and unsigned `.keyext` archives are
  unverified. The permission prompt says so. A future registry must require a
  verifier backed by pinned publisher keys before showing a verified identity.
- PDF files, URLs, DNS answers, redirects, provider responses, images, JSON,
  extension archives, and extension publishers may all be malicious.

## Protected assets

- PDF contents, extracted text, selections, annotations, comments, and paths.
- User intent: navigation, copied data, opened URLs, and enabled features.
- Sidecar and future extension-owned persisted state.
- Application availability, frame latency, memory, disk, and network budgets.
- Host integrity, extension identity, permission decisions, and diagnostics.

## Boundaries and controls

### Package boundary

The package loader snapshots a package into immutable memory. It rejects path
traversal, absolute and duplicate paths, symlinks, special files, encrypted or
unsupported ZIP entries, undeclared files, malformed manifests, missing
entrypoints, invalid WebAssembly headers, decompression bombs, oversized files,
too many entries/assets, and structurally unbounded UI JSON. External packages
declaring native entrypoints are rejected by the application.

Package parsing does not grant authority. Compatibility, dependencies,
capabilities, and requested permissions are checked again by the host before
activation.

### Semantic host boundary

Native and WebAssembly adapters receive the same versioned commands, immutable
snapshots, events, effects, and capability identifiers. The host validates
effects even for bundled adapters. It bounds event size, dispatch depth, cause
chains, delayed self-events, batches, queues, retries, subscriptions, state,
and contribution output. Generation changes cancel work and make old handles
stale. One extension failing or trapping moves only that extension to a failed
state.

Safe mode is selected with `GPUI_PDF_READER_SAFE_MODE=1`. In safe mode the host
activates bundled packages only, so a broken third-party package cannot prevent
the reader from starting.

### WebAssembly boundary

The Component Model adapter validates components before activation and applies
component-size, linear-memory, table, resource, stack, fuel, epoch-deadline,
host-call, return-value, queue, and UI-patch limits. Extension callbacks do not
run in a GPUI paint or raw input path. Disabling or unloading a component drops
its instance and cancels generation-scoped work.

WebAssembly isolation does not make granted document text public or harmless.
A component granted text access can copy what it receives into its own allowed
state or encode it in a permitted network request. Permissions must therefore
describe data exposure, not merely implementation mechanics.

### PDF capability boundary

Extensions receive opaque document, page, overlay, and generation handles,
never PDFium or GPUI objects. Metadata, page metadata, text, selection,
viewport, outline, links, navigation, and overlay services are independently
versioned and grantable. Every request is validated for generation, bounds,
revision monotonicity, string/list/geometry limits, and capability scope.
Closing or replacing a document invalidates all handles and drops retained
snapshots and overlays.

Navigation resolves to the shared host-owned jump abstraction. The reader, not
the extension, owns scrolling, centering, text refinement, and transient focus
animation. Overlays are bounded data and cannot choose arbitrary global
z-order or construct native views.

### UI boundary

Extension UI is a bounded tree of approved nodes and host icon/theme tokens.
It cannot supply callbacks, CSS, shaders, native views, raw images, arbitrary
focus behavior, or unbounded animation. Menubar contributions target named
host slots and bounded nested submenus; the host constructs native menu items.
Panel ownership, clipping, keyboard focus, accessibility, and overlay priority
remain host controlled.

### Network boundary

The bounded HTTP client allows only configured schemes and exact hosts. It
resolves and pins public addresses, disables environment proxies and automatic
redirects, revalidates each redirect, rejects private/local/link-local targets
and HTTPS downgrade, limits methods, headers, content types and bytes, and
applies timeouts, cancellation, rate and concurrency budgets. Per-document
preview caches are purged when their session closes.

DNS and transport policy reduce SSRF and rebinding risk but do not make a
remote response trustworthy. Provider data remains size checked and parsed as
untrusted input; links are opened only through explicit user actions.

## Failure and recovery

- Invalid install or upgrade input leaves the previously active package intact.
- Permission denial leaves the package inactive and cannot be bypassed by an
  optional capability request.
- Disable, unload, removal, document close, and permission revocation cancel
  work and invalidate handles.
- Traps, fuel exhaustion, deadline expiry, oversized output, host-call floods,
  and feedback loops are contained and diagnosed per extension.
- The standard reader remains usable when installable extensions or scholarly
  networking are omitted at compile time.

## Explicit non-goals and residual risk

- PDFium runs inside the application process on a dedicated thread, not in an
  OS sandbox or helper process. A memory-safety flaw in PDFium is outside the
  extension sandbox. Moving PDF parsing/rendering into a hardened helper would
  require a separate design.
- Bundled native adapters are trusted and are not contained by Wasm limits.
- UI deception cannot be eliminated entirely. Host-owned permission prompts,
  verified-state wording, fixed slots, and consistent chrome reduce it.
- The local/development distribution channel is intentionally not a trusted
  registry. Do not infer publisher identity from an extension ID or filename.
- Document mutation, redaction, Save As, signing, and irreversible operations
  are outside the v1 API and require a separate threat model and approval.
- Resource quotas constrain denial of service but cannot prove termination or
  constant latency for every allowed workload.

## Review triggers

Repeat security review before adding document mutation, arbitrary network
hosts, persistent secrets, a public package registry, a new WIT major version,
native package loading, background startup activation, or any capability that
exposes raw filesystem or operating-system handles.
