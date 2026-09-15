# UniCell — SUM optimization and performance benchmarks

**English** | [简体中文](README.zh-CN.md)

The bounded-range SUM optimization is included in the public local edition. On the same machine and the same 10,000-row CSV (100,000 cells, 20,000 formulas), median import plus calculation improved from **20.217 s to 0.661 s, approximately 30.6× faster**. Each case uses two warmups and seven measured iterations.

This is a before/after observation for one synthetic workload, not a speed comparison with Excel, Office 365 or WPS or an overall spreadsheet speedup. Background load and temperature on the shared workstation were not fully controlled. Unoptimized paths such as XLSX export may be slower; all results are retained. This repository update does not deploy the hosted service.

## Optimization

Previously, each `SUM(Arow:Hrow)` scanned the worksheet to determine its used range, repeating work for independent row totals. The new code queries the used range only for whole-row or whole-column references; bounded references iterate their stated range directly. Formulas are still evaluated rather than replaced with cached answers. The patch and regression tests in `server/vendor/ironcalc_base/` retain MIT OR Apache-2.0 licensing.

## Environment

| Field | Value |
| --- | --- |
| OS | Microsoft Windows 10 专业版 (10.0.19045, AMD64) |
| CPU | 11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz (4 cores / 8 threads) |
| RAM | 31.70 GiB |
| Rust | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Python | 3.13.11 |
| Optimized runtime source commit | `dc6f6c6a5c5de532e93b3adbf6a00336562a1a9f` |
| Baseline runtime source commit | `51e007388d702f23290bb44df7270d164daa0e2d` |
| Profile | `cargo build --release --locked --manifest-path server/Cargo.toml; default opt-level=3; ironcalc and ironcalc_base opt-level=1` |
| Binary SHA-256 | `eb228b490c20361927a2bea290de3a36752896181192b773241ef1710f2c884e` |
| Harness SHA-256 | `769a6efb2d6b90b95dd5d04dda8a9aba46fec4ae2e80226f1d4add9e185b78e8` |
| UTC | 2026-09-15T23:32:43.714362+00:00 |


## Workload and correctness

One worksheet with ten columns: eight numeric inputs, `SUM(A:H)`, and a dependent multiplication. CSV/XLSX imports include automatic calculation; a batch edit updates column A in every row and includes recalculation and history. Every new warmup and measured iteration validates XLSX ZIP integrity, all cell/formula counts, and every formula expression and cached numeric value: all **20,000 formulas** at 10,000 rows. Validation exports, parsing and assertions after import/edit occur outside timing. The historical baseline sampled values in first/middle/last rows; validation is now stronger, and inter-iteration exports can change cache and load conditions.

## Optimized public-edition results

| Operation | Size (rows) | Median ms | P95 ms |
| --- | ---: | ---: | ---: |
| CSV import + calculation | 100 | 10.25 | 21.82 |
| XLSX export | 100 | 10.93 | 36.19 |
| XLSX import + calculation | 100 | 14.21 | 17.15 |
| Batch edit + recalculation | 100 | 2.59 | 3.14 |
| CSV import + calculation | 1,000 | 68.16 | 79.31 |
| XLSX export | 1,000 | 48.55 | 57.71 |
| XLSX import + calculation | 1,000 | 80.59 | 96.26 |
| Batch edit + recalculation | 1,000 | 13.00 | 15.96 |
| CSV import + calculation | 10,000 | 660.92 | 707.79 |
| XLSX export | 10,000 | 562.30 | 618.65 |
| XLSX import + calculation | 10,000 | 1144.12 | 1248.97 |
| Batch edit + recalculation | 10,000 | 120.68 | 135.50 |

[JSON](results/2026-09-16-sum-optimized-windows-x64.json)

## Preserved baseline

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

[JSON](results/2026-09-16-windows-x64.json)

## Earlier approximately 55× experiment

The earlier full-application optimization experiment recorded **20.217 s → 0.368 s, approximately 55×**, with all 20,000 formulas and results checked on each run. Its optimized executable is a different build from this public local edition, with different service and harness paths. These figures are retained as the original experiment and do not replace the public-edition rerun above. Only the absolute executable path in the experiment JSON was replaced with a filename; timings, validation fields and binary hash are unchanged, and the original file hash is recorded.

[JSON](experiments/2026-09-16-csv-sum-fixed.json)

## Method and limitations

HTTP timing spans request transmission through full response read, including processing and calculation. Fixture generation, validation, application startup and browser rendering are excluded. Every measured sample is retained. The median is the fourth ordered observation; nearest-rank P95 equals the maximum for seven samples and is not a stable tail estimate. Complex real workbooks, concurrency, peak memory, UI frame rate, AI and cloud services are unmeasured. The old JSON and full baseline table are preserved; the historical harness is in the [baseline release](https://github.com/OmniDocX/unicell/tree/44bfc75e3c49eda4ac408f50c711ce70b0e34e7a/benchmarks).

## Reproduction and regression tests

Local release regression checks passed: 13 SUM tests and 4 three-dimensional reference tests, with no failures. These targeted suites are also required in CI; this statement does not imply a full engine-suite pass.

```sh
cargo build --release --locked --manifest-path server/Cargo.toml
python benchmarks/run.py --binary server/target/release/opencell-server --sizes 100,1000,10000 --warmups 2 --samples 7 --output benchmarks/results/local.json
cargo test --locked --manifest-path server/vendor/ironcalc_base/Cargo.toml --lib test_fn_sum::
cargo test --locked --manifest-path server/vendor/ironcalc_base/Cargo.toml --lib test_fn_3d_references::
python benchmarks/verify_results.py
```
Append `.exe` on Windows. The standard-library-only harness starts and stops its own loopback service.
