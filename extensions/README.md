# Extensions

GPUI PDF Reader uses a pre-stable, capability-based extension contract. An
installable package is either declarative data or a WebAssembly Component Model
guest. Local packages cannot load native Rust code, construct GPUI elements,
open files, create sockets, or call PDFium directly.

The standard app accepts a `.keyext` archive or a development directory with:

```text
manifest.toml
ui.json             # declarative or host-rendered Wasm UI, when declared
component.wasm      # only for a wasm_component entrypoint
assets/             # only bounded, declared package assets
```

Use File → Install or Update Extension. The Extensions panel slides to an
in-app review page showing identity, license, source, and every required
permission before the package is installed and enabled. It does not use a
second system confirmation dialog. Safe mode starts the reader with
third-party packages disabled:

```sh
GPUI_PDF_READER_SAFE_MODE=1 cargo run --locked
```

The contract is intentionally `0.1`. Packages must declare the compatible host
and extension API range. See `notes/extension-api-policy.md` before depending on
it outside this workspace.

## Settings and Tools entry

Non-sensitive manifest settings are rendered by the host on the extension's
detail page. The current controls cover booleans, bounded strings, bounded
integers and numbers, and declared choices. Values are validated against the
manifest, persisted atomically, and delivered to an active runtime as a
`settings_changed` lifecycle event. Extensions do not construct these GPUI
controls and sensitive settings never enter this form.

Every active local extension gets one Tools → Extensions entry. To make that
entry run a command, contribute one enabled direct command to the reserved
`tools.extensions` menu slot. If no such command exists, the entry opens the
extension's detail and settings page. A contributed side/settings view opens
in its own floating panel; it is not embedded in the extension manager.

## Reference packages

- `reference-theme-pack` has no executable code. It proves bounded state,
  settings, nested menus, commands, and host-rendered declarative UI.
- `reference-document-statistics` is a no-WASI Component Model guest. It proves
  the same lifecycle and UI path under fuel, deadline, memory, stack, queue,
  input, and output limits while consuming bounded document summaries.
- `reference-adversarial-loop` is a hostile no-WASI test guest. It proves that
  repeated infinite event handlers exhaust fuel and suspend only the package.
- `reference-native-escape` is rejected during preview because installable
  packages cannot declare trusted native adapters.

Rebuild all checked-in packages reproducibly with:

```sh
cargo run -p key-extension-wasm --example build_reference_extensions
```

The reference packages are unsigned local-development fixtures. The app marks
their publisher as unverified; it never infers trust from an extension ID or
filename. A future registry must add canonical signing and revocation without
weakening the local-install boundary.

## API layers

- `key-extension-api`: runtime-neutral manifests, events, effects, state, and
  declarative contribution data.
- `key-extension-host`: lifecycle, dependency/capability negotiation,
  permissions, quotas, rollback, and effect arbitration.
- `key-extension-gpui`: trusted rendering of bounded extension data.
- `key-extension-package`: traversal-safe, size-bounded package loading.
- `key-extension-wasm`: optional no-WASI Component Model runtime.
- `key-pdf-extension-api`: separate generation-scoped PDF capabilities.

The semantic contracts contain no GPUI, PDFium, Wasmtime, filesystem path,
socket, or application types. Read `notes/installable-extension-architecture.md`
and `notes/extension-threat-model.md` for the complete model.
