<h1 align="center">vecmeta</h1>
<p align="center"><strong>A native Rust SVG ↔ EMF vector conversion engine</strong></p>
<p align="center">An OmniDoc component for bidirectional vector conversion, a shared scene representation, and structured compatibility reporting.</p>
<p align="center"><a href="https://github.com/OmniDocX/vecmeta/actions/workflows/verify.yml"><img alt="Verify" src="https://github.com/OmniDocX/vecmeta/actions/workflows/verify.yml/badge.svg"></a> <img alt="Rust" src="https://img.shields.io/badge/Core-Rust-222222"> <a href="LICENSE"><img alt="Non-commercial license" src="https://img.shields.io/badge/License-Non--Commercial%20%2B%20Commercial-orange"></a></p>

<p align="center"><a href="README.md">简体中文</a> · <strong>English</strong></p>
<p align="center"><a href="https://github.com/OmniDocX/vecmeta">Source</a> · <a href="https://omnidoc.top/">OmniDoc</a> · <a href="https://unippt.unidoc.top/">UniPPT</a> · <a href="https://pic.unidoc.top/">UniPic</a> · <a href="docs/COMMERCIAL_LICENSE.md">Commercial licensing</a></p>

---

## Overview

vecmeta is OmniDoc's independently developed SVG and EMF conversion engine, distributed as Rust libraries and the `emfsvg` command-line tool. A shared vector intermediate representation connects both formats for document editors, presentation engines, vector asset processing, and round-trip validation.

EMF record serialization, primitive playback, SVG parsing, and vector output are implemented in Rust. Conversion does not require PowerPoint, LibreOffice, or the C libemf2svg implementation. Text can be converted to vector glyph outlines, which retain vector geometry but not text-editing semantics.

| Capability | Technical scope |
| --- | --- |
| **Native EMF reader/writer** | Records, state, transforms, pens, brushes, paths, and primitives |
| **Bidirectional SVG ↔ EMF** | One Scene IR for integration and inspection |
| **Glyph outlines** | fontdb / ttf-parser convert text into filled vector paths |
| **Gradients and transparency** | Recover linear gradients and alpha using embedded EMF+ paint information in the engine's output |
| **Source encapsulation** | `--lossless` embeds original bytes or SVG text for exact reverse recovery |
| **CSS and references** | Resolve supported styles, inheritance, specificity, and `<use>` |
| **Compatibility reports** | Return warnings for skipped content and embedded-source recovery status for conversion assessment |

## Contents

[Quick start](#quick-start) · [CLI](#command-line) · [Rust API](#rust-integration) · [Fidelity](#fidelity-and-support) · [OmniDoc Products](#omnidoc-products) · [License](#license-and-commercial-use)

## Quick start

The build environment requires Rust and Cargo. Continuous integration uses Rust 1.88. Text-to-outline conversion requires the corresponding fonts to be installed in the runtime environment.

```sh
git clone https://github.com/OmniDocX/vecmeta.git
cd vecmeta
cargo build --release --locked
cargo test --workspace --locked
cargo run --release -p emfsvg-cli -- --help
```

The binary is `target/release/emfsvg` (`emfsvg.exe` on Windows). Dependencies are managed by Cargo; no Office installation or C conversion library is required.

## Command line

```sh
# EMF → SVG
cargo run --release -p emfsvg-cli -- to-svg drawing.emf -o drawing.svg

# SVG → EMF
cargo run --release -p emfsvg-cli -- to-emf drawing.svg -o drawing.emf

# Embed the original source for exact reverse recovery
cargo run --release -p emfsvg-cli -- to-emf drawing.svg -o archive.emf --lossless

# Inspect round-trip geometry
cargo run --release -p emfsvg-cli -- roundtrip drawing.emf --tolerance 1e-6
```

`--verbose` reports skipped records. `--scale` caps SVG-to-EMF precision. Consult `--help` for the complete argument contract.

## Rust integration

Use the workspace's `emf2svg` and `svg2emf` packages:

```rust
use emf2svg::{emf_to_svg_with, Emf2SvgOptions};
use svg2emf::{svg_to_emf, EmitOptions};

let source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><circle cx="50" cy="50" r="25"/></svg>"#;
let emf = svg_to_emf(source, EmitOptions {
    lossless: true,
    ..Default::default()
}).expect("SVG to EMF");
let recovered = emf_to_svg_with(&emf, Emf2SvgOptions { lossless: true }).expect("EMF to SVG");
assert_eq!(source, recovered);
```

`emf_to_svg_with_report` and `svg_to_emf_with_report` return conversion output, warnings, and embedded-source recovery status. Existing basic conversion APIs remain compatible. UniPPT uses an independently versioned component snapshot; integrations must follow that snapshot's API definitions.

| Crate | Responsibility |
| --- | --- |
| `vector-ir` | Scene, elements, paths, matrices, colors, paint |
| `emf-core` | EMF records, binary reading/writing, embedded paint |
| `glyph2path` | Font lookup and glyph outlines |
| `emf2svg` | EMF playback → Scene → SVG |
| `svg2emf` | SVG parsing → Scene → EMF |
| `emfsvg-cli` | CLI and round-trip regressions |

## Fidelity and support

### Geometry consistency and source recovery

1. **Vector geometry:** coordinates, transforms, and styles are checked within supported primitives and normalized representations. Integer quantization and arc-to-Bézier conversion may introduce representational differences.
2. **Exact source recovery:** `--lossless` embeds original data in the target format for exact recovery during reverse conversion. This mechanism does not guarantee consistent third-party rendering or automatically merge changes to intermediate geometry into the embedded original.

### Format support and limitations

- Common paths, basic shapes, transforms, fill/stroke, linear gradients, and supported SVG CSS.
- Bitmaps, filters, clipping, and other unsupported vector semantics may be skipped; inspect warnings.
- Radial-gradient geometry is approximated linearly. Classic EMF viewers may show an average gradient color and ignore alpha.
- Text depends on installed fonts; comprehensive shaping, bidi, and all font features are not implemented.
- Pixel-level rendering consistency in third-party Office applications and browsers requires independent validation.

## Tests and distribution

`cargo test --workspace --locked` runs library tests and self-contained SVG/EMF round trips. Four external-corpus tests are marked `ignored` and require explicit execution. The public distribution excludes separately licensed historical test assets.

For an authorized external corpus, set the environment variable to a directory containing `emf/`, `emf-corrupted/`, and `emf-ea/`:

```powershell
$env:VECMETA_CORPUS_DIR = "C:\path\to\tests\resources"
cargo test -p emfsvg-cli --test roundtrip -- --ignored
```

See [provenance and distribution boundaries](docs/PROVENANCE.md). Third-party asset rights are separate from first-party source licensing.

## OmniDoc Products

OmniDoc provides products for document processing, content authoring, presentations, spreadsheets, email management, and image editing, together with format-conversion components.

| Product / component | Business focus | Official website / source |
| --- | --- | --- |
| OmniDoc | Main site, document services, identity | [omnidoc.top](https://omnidoc.top/) |
| UniDoc | Document authoring and editing | [app.unidoc.top](https://app.unidoc.top/) |
| UniPPT | Native presentations and AI orchestration | [unippt.unidoc.top](https://unippt.unidoc.top/) · [Source](https://github.com/OmniDocX/UniPPT) |
| UniCell | Browser spreadsheets | [unicell.unidoc.top](https://unicell.unidoc.top/) |
| UniMail | Multi-account email, calendars, contacts, and AI email assistance | [unimail.omnidoc.top](https://unimail.omnidoc.top/) |
| UniPic | Image and vector editing | [pic.unidoc.top](https://pic.unidoc.top/) |
| vecmeta | Native Rust SVG ↔ EMF engine | [Source and documentation](https://github.com/OmniDocX/vecmeta) |
| PolyglotPDF | Multilingual ebook and PDF translation | [Source](https://github.com/OmniDocX/PolyglotPDF) |

Deployment, account, and licensing requirements are defined in each product's official documentation. vecmeta is distributed as Rust libraries and a command-line tool.

## License and commercial use

First-party code and documentation use the unmodified [PolyForm Noncommercial 1.0.0](LICENSE). Uses permitted by that license are free. Commercial uses outside its permitted purposes require a separate paid commercial license: contact us to apply, agree on fees and obtain written authorization before use. The standard license's institutional permissions remain fully applicable. This is a source-available license, not an OSI-approved open-source license. Third-party terms and valid earlier grants remain unchanged.

[License scope and permitted uses](docs/LICENSING.md) · [Commercial licensing and application](docs/COMMERCIAL_LICENSE.md).
