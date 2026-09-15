# UniCell

**English** | [简体中文](README.zh-CN.md)

[OmniDoc](https://omnidoc.top/) · [GitHub](https://github.com/OmniDocX)

**A local spreadsheet application with a Rust calculation engine and browser interface.**

UniCell is an independently developed Chinese application project from OmniDoc. It provides local spreadsheet editing, formulas and document conversion. Its implementation incorporates third-party open-source components, including IronCalc and KaTeX; their origins and licenses are documented in [third-party notices](THIRD_PARTY_NOTICES.md).

## Project positioning

**Microsoft 365 (Office 365) and WPS Office** are the reference office products. Our ambition is to build the most complete China-developed office platform with publicly available source and reproducible engineering evidence. This is a development objective; see the [product comparison](docs/COMPARISON.md) for current scope and evidence. First-party code uses a non-commercial source license, detailed below.

## Performance

<!-- BENCHMARK:START -->
Measured on 2026-09-16: Windows 10, Intel Core i7-1165G7, 31.70 GiB RAM. Each case uses two warmups and seven measured iterations, executed sequentially.

| Operation | Size (rows) | Median ms | P95 ms |
| --- | ---: | ---: | ---: |
| CSV import + calculation | 10,000 | 20217.27 | 28130.84 |
| XLSX export | 10,000 | 452.73 | 557.95 |
| XLSX import + calculation | 10,000 | 17865.22 | 24569.10 |
| Batch edit + recalculation | 10,000 | 19081.38 | 23292.38 |

[All sizes, methodology and limitations](benchmarks/README.md) · [Raw observations](benchmarks/results/2026-09-16-windows-x64.json)

Timings exclude browser rendering. Office 365 and WPS were not timed in this campaign.
<!-- BENCHMARK:END -->

## Capabilities

| Area | Public local edition |
| --- | --- |
| Workbook editing | Cells, formulas, rich text, number formats, borders, merges, rows/columns, multiple sheets, undo/redo |
| Data operations | Sorting, filtering, frozen panes, conditional formatting, validation and supported what-if analysis |
| Document formats | XLSX/XLSM, CSV, UniDoc UDOC and UniCell HTML import/export |
| Embedded content | Images, SVG, charts and supported Office objects; preservation of selected complex OOXML parts |
| Local workflows | File saving, recent files, browser recovery copies, print preview and pagination |
| Automation | Optional configurable U AI and five local MCP tools |

The public edition excludes R2, cloud storage, shared links, collaborative editing, centralized accounts and hosted quotas.

## Quick start

Install a stable Rust toolchain with edition 2024 support and a modern browser. Windows builds require Visual Studio C++ Build Tools; Linux builds require a C/C++ toolchain, pkg-config and OpenSSL development libraries. Initial dependency downloads require network access.

```sh
git clone https://github.com/OmniDocX/unicell.git
cd unicell
cargo run --release --locked --manifest-path server/Cargo.toml
```

Open **http://127.0.0.1:8143**. Append `-- --port=8145` to select a different port. Run from the repository root or `server/`. The executable is `opencell-server`; the required `vecmeta/` source is included.

## Data lifecycle and compatibility

The service binds to `127.0.0.1` and separates workbooks by browser session. Active workbooks are primarily held in server memory. Restarting the server or leaving a session idle for eight hours invalidates that state. Save working documents explicitly; browser recovery copies are supplementary.

CSV exports display values from the current sheet, without retaining workbook formulas or formatting. XLSM macro parts may be retained, but VBA is not executed. Pivots, external connections, SmartArt and other advanced objects have preservation/editing limits. See [features and limitations](docs/FEATURES.md).

## Optional integrations

| Integration | Configuration |
| --- | --- |
| U AI | Copy `.env.example` to `.env.local`; configure a Chat Completions-compatible provider. See [local AI](docs/LOCAL_AI.md). Selected context is sent to that provider. |
| Office mathematics | `python -m pip install -r tools/requirements-math.txt`; select Python with `UNICELL_PYTHON`. Ordinary cell formulas do not require Python. |
| HTML object screenshots | Install Chrome, Edge or Chromium; optionally set `UNICELL_CHROMIUM`. |
| Fonts | System fonts by default; optional licensed local fonts. See [font loading](FONT_LOADING.md). |
| MCP | Local HTTP POST at `/mcp`, using a Cookie workbook session; information, cell/range read, cell write and range formatting. See [MCP](docs/MCP.md). |

## Architecture and verification

| Directory | Responsibility |
| --- | --- |
| `server` | Rust local service and patched IronCalc integration |
| `web` | Browser spreadsheet interface |
| `vecmeta` | Included SVG/EMF conversion components |
| `tools` | Math helpers, HTTP smoke tests and publication checks |
| `benchmarks` | Generated workbooks, performance harness and measured results |

```sh
cargo test --locked --manifest-path server/Cargo.toml
python -m unittest discover -s tools -p "test_*.py"
python tools/check_public_boundary.py
```

With the service running, execute `python tools/local_smoke.py --url http://127.0.0.1:8143`. The browser self-test entry is `/?test=auto`. The publication check reads indexed files. [Commercial licensing](docs/COMMERCIAL_LICENSE.md) · [License scope](docs/LICENSING.md).

## OmniDoc ecosystem

| Project | Purpose | Website / source |
| --- | --- | --- |
| OmniDoc | Main product portal | [omnidoc.top](https://omnidoc.top/) |
| UniDoc | Document authoring | [app.unidoc.top](https://app.unidoc.top/) |
| UniPPT | Presentations | [Editor](https://unippt.unidoc.top/) · [Source](https://github.com/OmniDocX/UniPPT) |
| UniCell | Spreadsheets | [Editor](https://unicell.unidoc.top/) · [Source](https://github.com/OmniDocX/unicell) |
| UniMail | Email, calendar and contacts | [unimail.omnidoc.top](https://unimail.omnidoc.top/) |
| UniPic | Image and vector editing | [pic.unidoc.top](https://pic.unidoc.top/) |
| vecmeta | SVG ↔ EMF conversion | [Source](https://github.com/OmniDocX/vecmeta) |
| Source collections | Pinned copies of the three published components | [omnidoc](https://github.com/OmniDocX/omnidoc) · [omnidocx](https://github.com/OmniDocX/omnidocx) |

Hosted products may offer features beyond the public local editions. Their availability and terms are defined by each product.

## License and commercial use

First-party code and documentation use the [OmniDoc Non-Commercial Source License 1.0](LICENSE). Qualifying non-commercial use is free. Commercial use, including internal business use by companies in China or elsewhere, requires prior written permission. This is a source-available license, not an OSI-approved open-source license. Third-party components retain their own terms; prior lawful grants for earlier releases remain unaffected.

Commercial contact: [cc@omnidoc.top](mailto:cc@omnidoc.top) · WeChat: **13184071590**. Complete applications receive a response within 48 hours; submission or silence does not grant permission.
