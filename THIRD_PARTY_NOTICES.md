# Third-party notices

This file records the important third-party components and the license audit
boundary for GPUI PDF Reader. The referenced license files are authoritative.
Keep them with redistributed source and binary bundles.

## Rust dependency graph

The supported `aarch64-apple-darwin` normal/build graph currently contains 451
unique packages. Every active package declares MIT, Apache-2.0, or a
more-permissive option: BSD-2-Clause, BSD-3-Clause, ISC, Zlib, Unicode-3.0,
CC0-1.0, MIT-0, Unlicense, BSL-1.0, or Apache-2.0 with LLVM exception. No active
package declares GPL, LGPL, AGPL, MPL, EPL, CDDL, or SSPL, and none lacks
license metadata.

The full lock also contains target-inactive `r-efi` packages whose expression
is `MIT OR Apache-2.0 OR LGPL-2.1-or-later`; GPUI PDF Reader uses the
MIT/Apache option, and those packages are not selected by the supported macOS
build.

Key direct components include:

| Component | License used/available |
|---|---|
| `gpui` 0.2.2 and Zed support crates | Apache-2.0 |
| `pdfium-render` 0.9.2 plus GPUI PDF Reader tile patch | MIT (upstream also offers Apache-2.0) |
| `image` 0.25 | MIT OR Apache-2.0 |
| `block2` | MIT |
| `objc2-app-kit` and Objective-C support crates | MIT OR Apache-2.0 OR Zlib |

Exact versions and sources are in `Cargo.lock`. Some dependency licenses
require preserving their copyright or notice text. A release bundle must
therefore include a complete generated license bundle for the active Cargo
graph; this summary and `Cargo.lock` alone are not that bundle.

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
