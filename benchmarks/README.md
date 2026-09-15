# UniCell — Performance benchmark

**English** | [简体中文](README.zh-CN.md)

Measured on 2026-09-16: Windows 10, Intel Core i7-1165G7, 31.70 GiB RAM. Each case uses two warmups and seven measured iterations, executed sequentially.

## Environment

| Field | Value |
| --- | --- |
| OS | Microsoft Windows 10 专业版 (10.0.19045, AMD64) |
| CPU | 11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz (4 cores / 8 threads) |
| RAM | 31.70 GiB |
| Rust | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Python | 3.13.11 |
| Runtime source commit | `51e007388d702f23290bb44df7270d164daa0e2d` |
| Build profile | `cargo build --release --locked --manifest-path server/Cargo.toml; default opt-level=3; ironcalc and ironcalc_base opt-level=1` |
| Executable size | 22,524,416 bytes |
| Measured at UTC | 2026-09-15T23:02:50.938714+00:00 |

## Workload and timing boundary

One sheet, ten columns: eight numeric inputs, SUM(A:H), then a dependent multiplication. Import includes automatic calculation. A batch edit changes column A in every row and includes automatic recalculation and history bookkeeping. XLSX output is checked for all cell/formula counts; first/middle/last-row dependent values and formula retention are checked through the API. These are sampled value checks, not an exhaustive workbook compatibility test.

HTTP timing includes request transfer, server processing and full response read. Fixture generation and validation are outside the timed region. Servers start before measurement; browser rendering is excluded.

## Complete results

| Operation | Size (rows) | Median ms | P95 ms |
| --- | ---: | ---: | ---: |
| CSV import + calculation | 100 | 4.53 | 19.19 |
| XLSX export | 100 | 27.01 | 29.34 |
| XLSX import + calculation | 100 | 11.85 | 21.56 |
| Batch edit + recalculation | 100 | 16.42 | 16.76 |
| CSV import + calculation | 1,000 | 71.07 | 79.10 |
| XLSX export | 1,000 | 41.61 | 48.33 |
| XLSX import + calculation | 1,000 | 82.79 | 86.61 |
| Batch edit + recalculation | 1,000 | 33.82 | 48.10 |
| CSV import + calculation | 10,000 | 20217.27 | 28130.84 |
| XLSX export | 10,000 | 452.73 | 557.95 |
| XLSX import + calculation | 10,000 | 17865.22 | 24569.10 |
| Batch edit + recalculation | 10,000 | 19081.38 | 23292.38 |

[Raw observations](results/2026-09-16-windows-x64.json) · [Harness](run.py)

## Method and limitations

Warmup observations are excluded. The median is the fourth ordered sample. P95 uses nearest rank `ceil(0.95 × n)`; with n=7 it equals the observed maximum and is not a stable tail-latency estimate. Raw JSON retains every measured duration, input/output sizes or hashes, correctness results, binary SHA-256 and harness SHA-256. The source commit identifies the runtime code used for the build; this documentation/benchmark update does not modify that runtime code.

This was a shared workstation run without full control of background load, power management or file-cache state. Slow observations were not discarded. No concurrent load test was performed. Results apply to these synthetic workloads only; peak memory, full application startup, browser frame rate, complex real documents, external AI and cloud services were not measured. Microsoft 365 (Office 365) and WPS Office were not timed, so these results do not establish a relative speed ranking.

## Reproduce

```sh
cargo build --release --locked --manifest-path server/Cargo.toml
python benchmarks/run.py --binary server/target/release/opencell-server --sizes 100,1000,10000 --warmups 2 --samples 7 --output benchmarks/results/local.json
python benchmarks/verify_results.py
```

On Windows, append `.exe` to the executable path. Adjust `--binary` when building with `--target-dir`. Only the Python standard library is required; all inputs are generated. Server benchmarks start and stop their own temporary loopback process. Reproduce with the recorded runtime source, lockfile, compiler and build profile; timings vary with hardware and load.

[Office 365 / WPS comparison and controlled-test protocol](../docs/COMPARISON.md)
