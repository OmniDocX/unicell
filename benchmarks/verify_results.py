"""Check published benchmark statistics, provenance and documentation coverage."""
from pathlib import Path
import hashlib
import json
import math
import statistics

root = Path(__file__).resolve().parents[1]
reports = sorted((root / "benchmarks/results").glob("20*.json"))
assert reports, "No dated benchmark report"
for path in reports:
    report = json.loads(path.read_text(encoding="utf-8"))
    assert report["schema_version"] == 1
    assert report["concurrency"] == 1
    assert report["samples_per_case"] > 0
    assert report["warmups_per_case"] >= 0
    assert len(report["environment"]["source_commit"]) == 40
    if path.name == "2026-09-16-sum-optimized-windows-x64.json":
        assert report["environment"]["harness_sha256"] == hashlib.sha256((root / "benchmarks/run.py").read_bytes()).hexdigest()
    keys = set()
    for case in report["cases"]:
        key = (case["operation"], case["size"])
        assert key not in keys, key
        keys.add(key)
        samples = case["samples_ms"]
        assert len(samples) == report["samples_per_case"]
        assert all(isinstance(v, (float, int)) and math.isfinite(v) and v > 0 for v in samples)
        assert case["median_ms"] == statistics.median(samples)
        assert case["p95_ms"] == sorted(samples)[math.ceil(.95 * len(samples))-1]
        assert case["min_ms"] == min(samples) and case["max_ms"] == max(samples)
        assert len(case["validation"]) == len(samples) and all(case["validation"])
        if path.name == "2026-09-16-sum-optimized-windows-x64.json":
            assert all(v["all_formula_expressions_and_values_checked"] and v["correct"] for v in case["validation"])
            assert all(v["formulas"] == case["size"] * 2 and v["cells"] == case["size"] * 10 for v in case["validation"])
        row_values = f"| {case['size']:,} | {case['median_ms']:.2f} | {case['p95_ms']:.2f} |"
        for language in ["README.md", "README.zh-CN.md"]:
            assert row_values in (root / "benchmarks" / language).read_text(encoding="utf-8"), (key, language)
    print(f"{report['project']}: {len(keys)} cases, statistics and bilingual reports verified")
for name in ["README.md", "README.zh-CN.md"]:
    text = (root / name).read_text(encoding="utf-8")
    assert "Microsoft 365" in text and "WPS Office" in text
    assert "benchmarks/README" in text
    assert "<!-- BENCHMARK:START -->\n<!-- BENCHMARK:END -->" not in text

experiment = json.loads((root / "benchmarks/experiments/2026-09-16-csv-sum-fixed.json").read_text(encoding="utf-8"))
assert experiment["samples"] == 7 and experiment["warmups"] == 2
assert len(experiment["original_record_sha256"]) == 64
assert experiment["binary"] == "opencell-server.exe"
for case in experiment["cases"]:
    for op in ["import", "export"]:
        assert len(case[op + "_ms"]) == 7
        assert statistics.median(case[op + "_ms"]) == case[op + "_median_ms"]
print("Earlier experiment statistics verified; separate build scope retained")
