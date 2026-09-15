#!/usr/bin/env python3
"""Reproducible, dependency-free local benchmark. See README.md in this folder."""
import argparse
import contextlib
import datetime
import hashlib
import http.cookiejar
import io
import json
import math
import os
from pathlib import Path
import platform
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def digest(data):
    return hashlib.sha256(data).hexdigest()


def command(args, **kwargs):
    return subprocess.check_output(args, text=True, encoding="utf-8", **kwargs).strip()


def environment(binary):
    info = {"os": platform.system(), "os_release": platform.release(),
            "os_version": platform.version(), "architecture": platform.machine(),
            "logical_cpus": os.cpu_count(), "python": platform.python_version(),
            "rustc": command(["rustc", "--version"]),
            "source_commit": command(["git", "rev-parse", "HEAD"], cwd=ROOT),
            "binary": binary.name, "binary_bytes": binary.stat().st_size,
            "binary_sha256": digest(binary.read_bytes()),
            "harness_sha256": digest(Path(__file__).read_bytes())}
    if os.name == "nt":
        script = "$c=Get-CimInstance Win32_Processor; $o=Get-CimInstance Win32_OperatingSystem; @{cpu=$c.Name; physical_cores=$c.NumberOfCores; ram_gib=[math]::Round($o.TotalVisibleMemorySize/1MB,2); os_caption=$o.Caption} | ConvertTo-Json -Compress"
        info.update(json.loads(command(["powershell", "-NoProfile", "-Command", script])))
    else:
        info["cpu"] = platform.processor() or "not reported"
    return info


def summary(samples):
    ordered = sorted(samples)
    return {"samples_ms": samples, "median_ms": statistics.median(samples),
            "p95_ms": ordered[math.ceil(len(ordered) * .95) - 1],
            "min_ms": min(samples), "max_ms": max(samples)}


def measure(name, size, args, operation, validate, prepare=None):
    samples, metadata = [], []
    for i in range(args.warmups + args.samples):
        if prepare:
            prepare(i)
        start = time.perf_counter_ns()
        result = operation(i)
        elapsed = (time.perf_counter_ns() - start) / 1e6
        checked = validate(result, i)
        if i >= args.warmups:
            samples.append(elapsed)
            metadata.append(checked)
    record = {"operation": name, "size": size, **summary(samples), "validation": metadata}
    print(f"{name} size={size}: median={record['median_ms']:.3f} ms p95={record['p95_ms']:.3f} ms", flush=True)
    return record


class Client:
    def __init__(self, base):
        self.base = base
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def request(self, path, data=None, content_type="application/json"):
        req = urllib.request.Request(self.base + path, data=data,
            headers={"Content-Type": content_type, "X-UniPPT-Filename": "benchmark.pptx"})
        with self.opener.open(req, timeout=180) as response:
            return response.read(), dict(response.headers)

    def json(self, path, obj=None):
        data = None if obj is None else json.dumps(obj, separators=(",", ":")).encode()
        return json.loads(self.request(path, data)[0])


@contextlib.contextmanager
def server(binary, variable, ready):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = dict(os.environ, **{variable: str(port)})
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    with tempfile.TemporaryFile() as log:
        child = subprocess.Popen([str(binary)], cwd=ROOT, env=env,
            stdout=log, stderr=log, creationflags=flags)
        client = Client(f"http://127.0.0.1:{port}")
        try:
            deadline = time.monotonic() + 90
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    raise RuntimeError("Benchmark server exited during startup")
                try:
                    client.request(ready)
                    break
                except OSError:
                    time.sleep(.1)
            else:
                raise RuntimeError("Benchmark server did not become ready")
            yield client
        finally:
            child.terminate()
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


def finish(args, binary, cases, **extra):
    report = {"schema_version": 1, "project": PROJECT,
              "measured_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "environment": environment(binary), "build_profile": PROFILE,
              "warmups_per_case": args.warmups, "samples_per_case": args.samples,
              "p95_method": "nearest rank: sorted[ceil(0.95*n)-1]",
              "concurrency": 1, "cases": cases, **extra}
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8", newline="\n")
    print(f"Saved {output.name}; {len(cases)} cases passed", flush=True)


def arguments(default_sizes):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, help="Path to a release executable")
    parser.add_argument("--output", default="benchmarks/results/local.json")
    parser.add_argument("--samples", type=int, default=7)
    parser.add_argument("--warmups", type=int, default=2)
    parser.add_argument("--sizes", default=default_sizes)
    args = parser.parse_args()
    if args.samples < 1 or args.warmups < 0:
        parser.error("samples must be positive and warmups nonnegative")
    args.sizes = [int(n) for n in args.sizes.split(",")]
    if not args.sizes or any(n < 1 for n in args.sizes):
        parser.error("sizes must be positive integers")
    binary = Path(args.binary).resolve(strict=True)
    return args, binary

PROJECT = "UniCell"
PROFILE = "cargo build --release --locked --manifest-path server/Cargo.toml; default opt-level=3; ironcalc and ironcalc_base opt-level=1"


def csv_fixture(rows):
    return ("\n".join(",".join([str(r), "2", "3", "4", "5", "6", "7", "8", f"=SUM(A{r}:H{r})", f"=I{r}*2"])
                     for r in range(1, rows + 1)) + "\n").encode()


def check_workbook(data, rows, delta=0):
    """Verify every formula expression and cached result outside the timed region."""
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        assert z.testzip() is None
        ns = {"s": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
        sheet = ET.fromstring(z.read("xl/worksheets/sheet1.xml"))
        cells = {c.attrib["r"]: c for c in sheet.findall(".//s:c", ns)}
        assert len(cells) == rows * 10
        assert len(sheet.findall(".//s:f", ns)) == rows * 2
        for row in range(1, rows + 1):
            for column, expected, formula in [
                ("I", row + 35 + delta, f"SUM(A{row}:H{row})"),
                ("J", (row + 35 + delta) * 2, f"I{row}*2"),
            ]:
                cell = cells[f"{column}{row}"]
                assert cell.findtext("s:f", namespaces=ns) == formula, (row, column)
                assert float(cell.findtext("s:v", namespaces=ns)) == expected, (row, column)
    return {"rows": rows, "cells": rows * 10, "formulas": rows * 2,
            "all_formula_expressions_and_values_checked": True, "correct": True}


def check_cells(client, rows, delta=0):
    return check_workbook(client.request("/api/export")[0], rows, delta)


def main():
    args, binary = arguments("100,1000,10000")
    cases = []
    with server(binary, "UNICELL_PORT", "/api/session") as client:
        for rows in args.sizes:
            csv = csv_fixture(rows)
            def import_csv(i):
                return client.request("/api/import-csv", csv, "text/csv")
            def check_csv(result, i):
                info = json.loads(result[0])
                assert info["rows"] == rows and info["columns"] == 10
                return {**check_cells(client, rows), "input_bytes": len(csv), "input_sha256": digest(csv)}
            cases.append(measure("import_csv_and_calculate", rows, args, import_csv, check_csv))
            artifacts = []
            def check_xlsx(result, i):
                data = result[0]
                checked = check_workbook(data, rows)
                artifacts.append(data)
                return {**checked, "bytes": len(data), "sha256": digest(data)}
            cases.append(measure("export_xlsx", rows, args,
                lambda i: client.request("/api/export"), check_xlsx))
            cases.append(measure("import_xlsx_and_calculate", rows, args,
                lambda i: client.request("/api/import", artifacts[i], "application/octet-stream"),
                lambda r, i: {**check_cells(client, rows), "input_bytes": len(artifacts[i]), "input_sha256": digest(artifacts[i])}))
            # Update all row inputs and recompute dependent formulas in one API operation.
            batches = [json.dumps({"sheet": 0, "cells": [{"r": r, "c": 1, "v": str(r+i+1)}
                for r in range(1, rows+1)]}, separators=(",", ":")).encode()
                for i in range(args.warmups + args.samples)]
            cases.append(measure("batch_edit_and_recalculate", rows, args,
                lambda i: client.request("/api/batch", batches[i]),
                lambda r, i: check_cells(client, rows, i+1)))
    finish(args, binary, cases, workload="10 columns: 8 numeric inputs + SUM(A:H) + dependent multiplication; one sheet, no styles/images/charts",
           timing_boundary="Local HTTP transfer, operation, calculation and full response read; input generation and correctness checks excluded; no browser rendering",
           calculation_mode="automatic; batch edits change every row and include history bookkeeping")


if __name__ == "__main__":
    main()
