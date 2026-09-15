# UniCell · Local Basic Edition

UniCell is an independently developed Chinese spreadsheet application in the [OmniDoc](https://omnidoc.top) family. [中文说明](README.md) · [Project collection](https://github.com/OmniDocX/omnidocx)

Rust runs the spreadsheet engine and a local browser renders the editor. The edition includes cell/formula editing, styles, worksheets, undo/redo, rich text, filtering, sorting, validation, protection, XLSX/XLSM/CSV/UDOC/HTML import/export and optional locally configured AI and MCP tools. R2, cloud storage, coauthoring, sharing links, hosted accounts and service quotas have been removed.

## Run

Install stable Rust with edition 2024 support and a C/C++ toolchain. On Linux install pkg-config and OpenSSL development packages too. Then:

```sh
git clone https://github.com/OmniDocX/unicell.git
cd unicell
cargo run --locked --manifest-path server/Cargo.toml
```

Open http://127.0.0.1:8143. Append `-- --port=8145` to change ports. Run from the repository root or `server/`. The reviewed vecmeta source is included. Initial dependency installation requires internet access; core editing then works offline.

Workbooks live in local process memory and browser sessions are isolated. Restarting the process or leaving a session idle for 8 hours discards server state: save/export important files. Browser recovery is a convenience, not a durable backup.

CSV preserves display values only. VBA is not executed. Complex OOXML objects may be preserved without fully executable or editable semantics. This is not complete Excel compatibility; see [features and limitations](docs/FEATURES.md).

Copy `.env.example` to `.env.local` to configure an optional Chat Completions provider. Using AI sends the selected context to that provider. No other product's credentials are read. Optional math export uses `python -m pip install -r tools/requirements-math.txt`; HTML capture requires local Chromium. See [AI](docs/LOCAL_AI.md), [MCP](docs/MCP.md), [fonts](FONT_LOADING.md).

## Verification

```sh
cargo test --locked --manifest-path server/Cargo.toml
python -m unittest discover -s tools -p "test_*.py"
python tools/check_public_boundary.py
python tools/local_smoke.py --url http://127.0.0.1:8143
```

First-party code uses the [OmniDoc Non-Commercial Source License 1.0](LICENSE). Qualifying non-commercial use is free; commercial use requires written permission. It is source-available, not OSI-approved open source. [Third-party components](THIRD_PARTY_NOTICES.md) keep their independent licenses. Licensing contact: cc@omnidoc.top; WeChat 13184071590, normally within 48 hours.
