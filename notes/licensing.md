# Licensing

- Allow MIT, Apache-2.0, BSD, ISC, Zlib, Unicode, CC0, and similarly permissive terms. Exclude GPL, LGPL, AGPL, MPL, EPL, CDDL, and SSPL.
- Audit the target-active normal/build graph, full lockfile, vendored source, binary dependencies, fixtures, fonts, and retained notices separately.
- Cargo license expressions containing `OR` need human review. Explicitly elect the permissive branch where available.
- Target-inactive packages still appear in `Cargo.lock`; record why they are inactive and which permissive `OR` branch applies.
- Cargo metadata does not cover native PDFium or its bundled third parties.
- PDFium's ICU notice mentions GPL Autoconf helper files. Those files are not present or linked; a GPL string in a retained notice is not GPL code in the product.
- Inspect the shipped dylib's dynamic dependencies. The audited arm64 build links only Apple frameworks and `libSystem`.
- Keep PDFium's complete `licenses/` directory. A short notice inventory is not the distribution license bundle.
- Pin PDFium downloads by architecture and SHA-256. Reinspect the Intel binary before an Intel or universal release.
- GPUI's graph introduced MPL-only utilities even on dormant or target-inactive paths. The local compatibility crates are small, independently written MIT implementations, not copied forks.
- Remove test assets outside the policy even if they never ship. The deterministic fixture uses PDF core fonts and no external font file.
- Strip Cargo tree's repeated-package marker before counting unique packages.
