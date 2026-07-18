# Third-party notices

This file records the important third-party components and the license audit
boundary for GPUI PDF Reader. The referenced license files are authoritative.
Keep them with redistributed source and binary bundles.

## Rust dependency graph

The default standard `aarch64-apple-darwin` normal/build graph currently
contains 752 unique package records; the minimal `--no-default-features` graph
contains 675. The repository guard currently checks 787 unique host and
cross-target normal/build/dev records across the workspace. Every selected
record has an MIT, Apache-2.0, or more-permissive license choice:
BSD-2-Clause, BSD-3-Clause, ISC, Zlib, Unicode-3.0, CC0-1.0, MIT-0, Unlicense,
BSL-1.0, NCSA, or Apache-2.0 with LLVM exception. None lacks license metadata.

Some multi-license declarations include a reciprocal alternative, including
`Apache-2.0 OR GPL-2.0-only` and
`MIT OR Apache-2.0 OR LGPL-2.1-or-later`. GPUI PDF Reader elects the explicit
Apache/MIT branch; no GPL/LGPL branch is used. Expressions that require a
reciprocal license with `AND` are rejected by the guard.

Key direct components include:

| Component | License used/available |
|---|---|
| `gpui` 0.2.2 and Zed support crates | Apache-2.0 |
| `gpui-component` and its icon assets 0.5.1 | Apache-2.0 |
| `pdfium-render` 0.9.2 plus GPUI PDF Reader tile patch | MIT (upstream also offers Apache-2.0) |
| `image` 0.25 | MIT OR Apache-2.0 |
| `wasmtime` 45 (standard bundle only) | Apache-2.0 WITH LLVM-exception |
| `zed-reqwest`/`reqwest` (standard scholarly bundle only) | MIT OR Apache-2.0 |
| `zip` 8 (installable package loader only) | MIT |
| `block2` | MIT |
| `objc2-app-kit` and Objective-C support crates | MIT OR Apache-2.0 OR Zlib |

Exact versions and sources are in `Cargo.lock`. Some dependency licenses
require preserving their copyright or notice text. A release bundle must
therefore include a complete generated license bundle for the active Cargo
graph; this summary and `Cargo.lock` alone are not that bundle.

The bundled named themes are the official gpui-component theme collection,
combined without changing their values. Their exact source revision and the
retained Apache-2.0 license are recorded in
[`assets/themes/README.md`](assets/themes/README.md).

## PDFium binary

GPUI PDF Reader pins Chromium PDFium build 7763 (Chromium 148.0.7763.0) from
Benoit Blanchon's `pdfium-binaries` packaging. The package-level MIT license is
[`vendor/pdfium/LICENSE`](vendor/pdfium/LICENSE). PDFium itself uses a
BSD-style license, retained in
[`vendor/pdfium/licenses/pdfium.txt`](vendor/pdfium/licenses/pdfium.txt).

The complete retained `vendor/pdfium/licenses/` directory includes notices for
Abseil (Apache-2.0), AGG, fast_float (MIT), FreeType (FreeType License), ICU and
Unicode data (Unicode-3.0), Little CMS (MIT), libjpeg-turbo (IJG/BSD/Zlib),
OpenJPEG (BSD-2-Clause), libpng, libtiff, LLVM libc (Apache-2.0 with LLVM
exception), simdutf (MIT), and zlib.

Portions of this software use FreeType Project code
([www.freetype.org](https://www.freetype.org)). All rights reserved.

`vendor/pdfium/licenses/icu.txt` reproduces upstream notices for ICU's source
distribution, including GPL-with-exception text for the Autoconf helper files
`aclocal.m4` and `config.guess`. Those helper source files are not present in
this repository and are not compiled or linked into `libpdfium.dylib`; the
wording in the retained notice is not GPL code in GPUI PDF Reader. The shipped
arm64 dylib links only Apple system frameworks and `libSystem`.

The Intel archive is checksum-pinned by `scripts/fetch-pdfium.sh`, but should
receive the same binary inspection before an Intel or universal release.

## pdfium-render and the tile extension

The vendored source at `vendor/pdfium-render-tile/` is `pdfium-render` 0.9.2 by
Alastair Carey plus a focused viewport-tile extension and regression fixtures.
GPUI PDF Reader elects the upstream MIT license option; see
`vendor/pdfium-render-tile/LICENSE-MIT` and `LICENSE.md`. The patch is offered
under the same terms. `TILE_PATCH.md` describes the unsafe boundary, geometry,
and full-versus-tiled verification.

## Local compatibility crates

`vendor/cbindgen-compat`, `vendor/option-ext-compat`, and
`vendor/dwrote-compat` are independently written MIT compatibility crates, not
forks of the similarly named upstream packages. Their individual `LICENSE`
files are included. `cbindgen-compat` supplies only a compile-time API that is
dormant under the selected Blade renderer; `dwrote-compat` is a Windows-only
placeholder and is not built on supported macOS targets.

## Reproducing the graph audit

Run:

```sh
sh scripts/audit-licenses.sh
```

or inspect another supported target explicitly:

```sh
sh scripts/audit-licenses.sh x86_64-apple-darwin
```

The script is a guard against missing metadata and prohibited license-family
identifiers. Final release review must still inspect compound `OR` expressions,
bundled native code, copyright notices, and the generated distribution bundle.
