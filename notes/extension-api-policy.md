# Extension API policy

## Status

The Rust and WIT extension contracts are pre-stable `0.1` APIs. They are
usable by bundled pilots and local development packages, but compatibility is
not promised across every commit until the pilots have exercised the surface.

The generic contract lives in `key-extension-api`; PDF capabilities are a
separate versioned layer in `key-pdf-extension-api`. GPUI, PDFium, Wasmtime,
filesystem paths, sockets, and application action types are not public
extension types.

## Versioning

- Manifests have their own schema version.
- The semantic extension API, host version, each capability, and each WIT
  package are versioned independently with semantic versions.
- Required capabilities use an explicit compatible range. Optional missing
  capabilities degrade the feature; required missing capabilities prevent
  activation with a structured diagnostic.
- Opaque handles are generation scoped and never valid across document close,
  replacement, extension reload, or host restart.
- Adding an optional field, event, effect, capability, or enum handling path is
  a compatible minor change only when old consumers can ignore it safely.
  Removing or changing meaning is a major change.

## Deprecation

Before a stable major version, incompatible changes require a migration note
and an older contract fixture where practical. After `1.0`, a deprecated
operation remains supported for at least one minor release train and emits a
diagnostic before removal in a new major version.

Compatibility adapters translate semantic values only. They must not weaken
permission checks, revive stale handles, expose implementation types, or give
older extensions authority unavailable to current ones.

## Package and state migration

- Validate and compile a replacement before unloading the current package.
- Snapshot or transactionally migrate extension-owned state.
- Failed activation or migration restores the prior package and state.
- Extension settings, document state, and ephemeral cache are independently
  namespaced and schema versioned.
- Removal must distinguish retaining data from destructive deletion. The
  current local package pilot owns no persistent extension data, so removal is
  non-destructive.

## Security support

Security fixes may disable a capability or reject a package even when its
declared version range would otherwise match. Diagnostics must explain the
incompatibility without exposing document data or secrets.

The project supports only the contract versions exercised by the current
standard bundle and checked compatibility fixtures. A public registry or
long-term support window requires a separate published support matrix,
publisher-key policy, revocation mechanism, and vulnerability reporting path.

## Review rule

Do not add a generic capability for one feature's convenience. First prove a
semantic operation with at least two credible consumers or a pilot that cannot
be expressed safely with existing snapshots, commands, effects, and slots.
Document data sensitivity, permission wording, cancellation, quotas, stale
handle behavior, and denial behavior with every new capability.
