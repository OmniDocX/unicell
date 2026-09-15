# UniCell

**English** | [简体中文](README.zh-CN.md)

[OmniDoc](https://omnidoc.top/) · [GitHub](https://github.com/OmniDocX)

**A local spreadsheet application with a Rust calculation engine and browser interface.**

UniCell is an independently developed Chinese application project from OmniDoc. It provides local spreadsheet editing, formulas and document conversion. Its implementation incorporates third-party open-source components, including IronCalc and KaTeX; their origins and licenses are documented in [third-party notices](THIRD_PARTY_NOTICES.md).

## Project positioning

**Microsoft 365 (Office 365) and WPS Office** are the reference office products. Our ambition is to build the most complete China-developed office platform with publicly available source and reproducible engineering evidence. This is a development objective; see the [product comparison](docs/COMPARISON.md) for current scope and evidence. First-party code uses a non-commercial source license, detailed below.

## Performance

<!-- BENCHMARK:START -->
The bounded-range SUM optimization is included in the public local edition. On the same machine and the same 10,000-row CSV (100,000 cells, 20,000 formulas), median import plus calculation improved from **20.217 s to 0.661 s, approximately 30.6× faster**. Each case uses two warmups and seven measured iterations.

| Operation | Size (rows) | Median ms | P95 ms |
| --- | ---: | ---: | ---: |
| CSV import + calculation | 10,000 | 660.92 | 707.79 |
| XLSX export | 10,000 | 562.30 | 618.65 |
| XLSX import + calculation | 10,000 | 1144.12 | 1248.97 |
| Batch edit + recalculation | 10,000 | 120.68 | 135.50 |

[Full report, baseline and earlier approximately 55× experiment](benchmarks/README.md) · [JSON](benchmarks/results/2026-09-16-sum-optimized-windows-x64.json)

All 20,000 formulas and results passed validation on every new iteration. This is a before/after observation for one synthetic workload, not a speed comparison with Excel, Office 365 or WPS or an overall spreadsheet speedup. Background load and temperature on the shared workstation were not fully controlled. Unoptimized paths such as XLSX export may be slower; all results are retained. This repository update does not deploy the hosted service.
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

First-party code and documentation use the unmodified [PolyForm Noncommercial 1.0.0](LICENSE). Uses permitted by that license are free. Commercial uses outside its permitted purposes require a separate paid commercial license: contact us to apply, agree on fees and obtain written authorization before use. The standard license's institutional permissions remain fully applicable. This is a source-available license, not an OSI-approved open-source license. Third-party terms and valid earlier grants remain unchanged.

[License scope and permitted uses](docs/LICENSING.md) · [Commercial licensing and application](docs/COMMERCIAL_LICENSE.md).

Commercial contact: [cc@omnidoc.top](mailto:cc@omnidoc.top) · WeChat: **13184071590**. Complete applications receive a response within 48 hours; submission or silence does not grant permission.
