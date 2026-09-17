<div align="center">

<img src="web/unicell-logo.svg" width="72" height="72" alt="UniCell" />

# UniCell

**A local spreadsheet for organizing data, calculating formulas and delivering workbooks.**

**English** | [简体中文](README.zh-CN.md)

[Website](https://omnidoc.top/) · [Live app](https://unicell.unidoc.top/) · [Quick start](#quick-start) · [Documentation](#documentation) · [Benchmarks](benchmarks/README.md)

[![CI](https://github.com/OmniDocX/unicell/actions/workflows/ci.yml/badge.svg)](https://github.com/OmniDocX/unicell/actions/workflows/ci.yml)
[![License: PolyForm Noncommercial](https://img.shields.io/badge/license-PolyForm_Noncommercial-315EFB?style=flat-square)](LICENSE)
[![GitHub issues](https://img.shields.io/github/issues/OmniDocX/unicell?style=flat-square)](https://github.com/OmniDocX/unicell/issues)

</div>

UniCell brings familiar spreadsheet workflows to your browser: open Excel or CSV files, edit multiple sheets, calculate formulas, format data and save workbooks locally. A Rust service handles calculation and file processing, while the browser provides editing, data tools and print controls.

Developed in China as part of OmniDoc, with IronCalc for calculation and KaTeX for math typesetting. Third-party components are attributed independently.

![UniCell spreadsheet editor](docs/images/editor.png)

<sub>Actual public local edition with synthetic data, formulas, number formats and calculated totals.</sub>

## Highlights

- **Everyday editing** — Multiple sheets, cell and range operations, row/column sizing, merges, rich text, undo and redo.
- **Formulas and recalculation** — IronCalc evaluates formula dependencies. Bounded SUM ranges are optimized, with reproducible benchmarks included.
- **Data organization** — Sort, filter, freeze panes, apply conditional formatting and validation, and use supported what-if operations.
- **File round trips** — Import and export XLSX/XLSM, CSV, UDOC and UniCell HTML for continued editing and local archiving.
- **Visual content and print** — Work with images, SVG, charts and supported Office objects; configure pages and print through the browser.
- **AI automation** — Configure U AI or use local MCP to read data, write formulas and format ranges.

## Quick start

Install Git, a stable Rust toolchain supporting edition 2024, and a modern browser:

```sh
git clone https://github.com/OmniDocX/unicell.git
cd unicell
cargo run --release --locked --manifest-path server/Cargo.toml
```

Open **http://127.0.0.1:8143**. Append `-- --port=8145` to use a different port. Run from the repository root or `server/`.

Windows requires Visual Studio C++ build tools. Linux requires a C/C++ toolchain, pkg-config and OpenSSL development packages.

## AI and developer integration

**U AI**: copy `.env.example` to `.env.local` and configure a Chat Completions-compatible provider. Basic editing works independently of AI; selected context is sent to the provider when AI is enabled. [Configuration](docs/LOCAL_AI.md).

**MCP**: the local `/mcp` endpoint exposes five tools within the current cookie-based workbook session. [Protocol and examples](docs/MCP.md).

| Tool | Purpose |
| --- | --- |
| `workbook_info` | Inspect the workbook and its sheets |
| `read_cell` / `read_range` | Read a cell or range |
| `write_cell` | Write values, text and formulas |
| `format_range` | Apply range formatting |

## Performance

<!-- BENCHMARK:START -->
The bounded-range SUM optimization reduced median import plus calculation for a 10,000-row CSV from **20.217 s to 0.661 s — approximately 31× faster**. Every iteration checked all 20,000 formulas and results.

| Operation | Workload | Median |
| --- | --- | --- |
| CSV import + calculation | 10,000 rows / 20,000 formulas | **660.92 ms** |
| Batch edit + recalculation | 10,000 rows / 20,000 formulas | **120.68 ms** |

2026-09-16 · Windows 10 · Intel i7-1165G7 · 31.7 GiB · Rust release · 2 warmups / 7 measurements.

[Full results, raw observations and reproduction](benchmarks/README.md) — Synthetic local workloads; browser rendering is excluded. Other office products were not timed.
<!-- BENCHMARK:END -->

## Edition and file compatibility

This repository provides the single-user local edition, without R2, cloud storage, collaborative editing or centralized accounts. Save active workbooks to files: in-memory state expires on server restart or after eight hours of session inactivity.

Supported macro parts can be retained in XLSM, but VBA is not executed. CSV exports display values. See [feature coverage](docs/FEATURES.md) for editing and preservation of pivots, SmartArt, external connections and other advanced objects.

## Documentation

| Guide | What it covers |
| --- | --- |
| [Feature coverage](docs/FEATURES.md) | Editing, formats and advanced objects |
| [U AI](docs/LOCAL_AI.md) | Model configuration and data handling |
| [MCP](docs/MCP.md) | Local protocol and call examples |
| [Fonts](FONT_LOADING.md) | Local fonts and rendering |
| [Third-party notices](THIRD_PARTY_NOTICES.md) | Dependency origins and licenses |

## Development

```sh
cargo test --locked --manifest-path server/Cargo.toml
python -m unittest discover -s tools -p "test_*.py"
python benchmarks/verify_results.py
```

With the server running, use `python tools/local_smoke.py --url http://127.0.0.1:8143` to check the local API.

## OmniDoc and community

[OmniDoc website](https://omnidoc.top/) · [UniPPT](https://github.com/OmniDocX/UniPPT) · [UniCell](https://github.com/OmniDocX/unicell) · [vecmeta](https://github.com/OmniDocX/vecmeta)

Share reproducible bugs and feature requests through [GitHub Issues](https://github.com/OmniDocX/unicell/issues). Contributions to features, format compatibility and documentation are welcome.

Microsoft 365 (Office 365) and WPS Office inform our office workflows; ONLYOFFICE and Univer are reference projects in the public office ecosystem. See [project positioning and capabilities](docs/COMPARISON.md).

## License and commercial licensing

First-party code uses [PolyForm Noncommercial 1.0.0](LICENSE). Noncommercial and specified institutional uses are free under its terms. Commercial uses outside those permissions require a [paid commercial license](docs/COMMERCIAL_LICENSE.md) and written authorization.

**Commercial contact: [cc@omnidoc.top](mailto:cc@omnidoc.top) · WeChat: 13184071590**

This is a source-available license, not an OSI-approved open-source license. Third-party terms and valid earlier grants remain independent. See [license scope](docs/LICENSING.md).
