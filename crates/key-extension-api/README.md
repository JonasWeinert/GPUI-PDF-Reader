# key-extension-api

Runtime-neutral, pre-stable contracts shared by Key extension hosts and
adapters. Public types contain no GPUI, PDFium, Wasmtime, filesystem, socket,
or operating-system implementation objects.

The Rust contract and its semantic WIT package are versioned together at
`0.1.0`. The WIT package lives at `wit/v0.1.0/extension-api.wit` and exposes
`key:extension-api/semantic-extension@0.1.0`. It types lifecycle, subscribed
events, immutable state updates, requested effects, capabilities, permissions,
storage, tasks, manifests, and host-rendered UI contributions.

Rust has recursive `DataValue`, `UiNode`, and `MenuItem` trees. WIT cannot
directly express recursive value types, so their WIT forms use bounded index
arenas. Hosts must validate the root and every child index, depth, node count,
string size, and payload size before retaining or dispatching a value. These
arenas are structural data; they do not expose host memory or resources.

`key-extension-wasm/wit/extension.wit` remains the compatibility transport for
the first local Wasm components. It carries the same versioned semantic events
and updates as bounded JSON bytes while typed component adapters are phased in.
It is not the source of truth for extension semantics. Capability interfaces
such as the PDF WIT package are linked separately and only after permission
and capability negotiation.

Contract tests parse the WIT package and exhaustively map Rust enum cases to
WIT cases. A Rust enum change cannot compile those tests until its WIT mapping
is explicitly updated; record, world, and typed guest-export shapes are also
checked.
