# Vendored IronCalc base

Upstream: https://github.com/ironcalc/IronCalc

Crate: ironcalc_base 0.8.2, upstream VCS b7ef4a4766a5cb3d66b09c098ee4aa463e14389c, directory base/.

This copy contains UniCell compatibility changes, including application history, formatting, validation, calculation and OOXML transport support. It is not an unmodified upstream distribution. Upstream copyright headers are retained. The crate remains available under its MIT OR Apache-2.0 license, with both original license texts included. The root application license does not restrict this component's upstream rights.
## 2026-09-16: bounded SUM range calculation

`src/functions/math_and_trigonometry/mathematical.rs` computes the worksheet
used range only for whole-row or whole-column SUM references. Bounded ranges
already supply their limits, so independent row totals no longer rescan the
entire worksheet. Whole-axis clipping and sheet/error handling are preserved.
Regression tests in `src/test/test_fn_sum.rs` cover reversed and empty ranges,
text/booleans, whole axes after edits, formula errors and the final sheet cell.
This modification and its tests are supplied under the same MIT OR Apache-2.0
terms as this vendored crate.
