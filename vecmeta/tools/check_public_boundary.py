"""Scan the Git index that will be committed, without printing secret values.

This is a high-confidence publication check, not a complete secret detector.
Run from any directory in a Git checkout. Untracked/ignored local files are not
read; force-added private files are rejected even when their contents are empty.
"""

from __future__ import annotations

import argparse
import io
import re
import subprocess
import sys
import zipfile
import xml.etree.ElementTree as ET
from pathlib import Path, PurePosixPath


CONTENT_RULES = (
    ("private key", re.compile(rb"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY-----")),
    ("AWS access key", re.compile(rb"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b")),
    ("GitHub token", re.compile(rb"\bgh[pousr]_[A-Za-z0-9]{36,}\b")),
    ("GitHub fine-grained token", re.compile(rb"\bgithub_pat_[A-Za-z0-9_]{50,}\b")),
    ("OpenAI project key", re.compile(rb"\bsk-proj-[A-Za-z0-9_-]{20,}\b")),
    ("GitLab token", re.compile(rb"\bglpat-[A-Za-z0-9_-]{20,}\b")),
    ("private conversation URL", re.compile(rb"https?://(?:chatgpt\.com|chat\.openai\.com)/c/[a-zA-Z0-9-]+")),
    ("personal mailbox", re.compile(rb"[A-Za-z0-9._%+-]+@(?:qq|163|126)\.com\b", re.I)),
    ("workstation-specific path", re.compile(rb"[A-Z]:[\\/]+(?:Users[\\/]+(?:Administrator|[^\\/\s]+[\\/]+(?:Documents|Downloads|\.codex))|BaiduNetdiskDownload|ebook-translator|ProgramData[\\/]+miniconda3[\\/]+envs)[\\/]", re.I)),
    ("private compute host", re.compile(rb"\b[a-z0-9-]+(?:-ssh)?\.gpuhome\.cc\b", re.I)),
)


def git(root: Path, *args: str) -> bytes:
    return subprocess.run(
        ["git", "-C", str(root), *args], check=True, capture_output=True
    ).stdout


def private_path(name: str) -> bool:
    path = PurePosixPath(name.lower())
    leaf = path.name
    return (
        leaf == ".env"
        or (leaf.startswith(".env.") and leaf != ".env.example")
        or leaf.endswith((".env.local", ".dpapi.xml", ".pem", ".key", ".p12", ".pfx"))
        or leaf in {"id_rsa", "id_ed25519", "id_ecdsa", "id_dsa"}
        or ".unippt-mcp" in path.parts
        or path.parts[0] in {"ara", "deploy", ".tmp", ".codex-logs", "outputs", "artifacts"}
        or leaf.endswith(".bundle")
        or (leaf.startswith("unippt_production_handoff") and leaf.endswith(".md"))
    )


def scan_content(name: str, content: bytes, depth: int = 0) -> list[str]:
    """Inspect raw bytes and bounded ZIP members without printing their values."""
    hits = [f"{name}: {label}" for label, pattern in CONTENT_RULES if pattern.search(content)]
    if content.startswith((b"PK\x03\x04", b"PK\x05\x06")):
        if depth >= 4:
            return hits + [f"{name}: archive nesting exceeds review limit"]
        try:
            with zipfile.ZipFile(io.BytesIO(content)) as archive:
                entries = archive.infolist()
                if len(entries) > 10000 or sum(e.file_size for e in entries) > 128 * 1024 * 1024:
                    return hits + [f"{name}: archive exceeds review limit"]
                for entry in entries:
                    data = archive.read(entry)
                    member = f"{name}!{entry.filename}"
                    hits.extend(scan_content(member, data, depth + 1))
                    if entry.filename == "docProps/core.xml" and not name.startswith("vendor/"):
                        tree = ET.fromstring(data)
                        for node in tree.iter():
                            if node.tag.split("}")[-1] in {"creator", "lastModifiedBy"} and (node.text or "").strip() not in {"", "OmniDoc"}:
                                hits.append(f"{member}: personal document author metadata")
        except (zipfile.BadZipFile, RuntimeError, ET.ParseError, OSError, ValueError):
            hits.append(f"{name}: archive could not be completely inspected")
    return hits


def scan_history(root: Path) -> list[str]:
    """Review all reachable Git objects and author/committer metadata."""
    hits = scan_content("Git commit metadata", git(root, "log", "--all", "--format=%an <%ae>%n%cn <%ce>%n%B"))
    entries = git(root, "rev-list", "--objects", "--all").splitlines()
    with subprocess.Popen(["git", "-C", str(root), "cat-file", "--batch"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as reader:
        try:
            assert reader.stdin is not None and reader.stdout is not None
            for line in entries:
                parts = line.split(b" ", 1)
                if len(parts) != 2:
                    continue
                oid, raw_name = parts
                reader.stdin.write(oid + b"\n")
                reader.stdin.flush()
                header = reader.stdout.readline().split()
                if len(header) != 3:
                    raise RuntimeError("Cannot read a historical Git object")
                size = int(header[2])
                content = reader.stdout.read(size)
                if len(content) != size or reader.stdout.read(1) != b"\n":
                    raise RuntimeError("Incomplete historical Git object")
                if header[1] != b"blob":
                    continue
                name = raw_name.decode("utf-8", errors="replace")
                if private_path(name):
                    hits.append(f"history/{name}: private file is reachable")
                hits.extend(scan_content(name, content))
        finally:
            if reader.stdin is not None:
                reader.stdin.close()
        if reader.wait() != 0:
            raise RuntimeError("Git history reader failed")
    return sorted(set(hits))


def scan_index(root: Path) -> list[str]:
    hits: list[str] = []
    entries = git(root, "ls-files", "--stage", "-z").split(b"\0")
    # Index OIDs ensure unstaged replacements/deletions cannot mask published
    # bytes. Scan all extensions, including binary files, without exemptions.
    with subprocess.Popen(
        ["git", "-C", str(root), "cat-file", "--batch"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    ) as reader:
        try:
            assert reader.stdin is not None and reader.stdout is not None
            for entry in entries:
                if not entry:
                    continue
                metadata, raw_name = entry.split(b"\t", 1)
                mode, oid, stage = metadata.split()
                name = raw_name.decode("utf-8", errors="replace")
                if stage != b"0":
                    hits.append(f"{name}: unresolved index conflict")
                    continue
                if private_path(name):
                    hits.append(f"{name}: private file is staged/tracked")
                if mode == b"160000":
                    hits.append(f"{name}: submodule contents require a separate publication scan")
                    continue
                reader.stdin.write(oid + b"\n")
                reader.stdin.flush()
                header = reader.stdout.readline().split()
                if len(header) != 3 or header[1] != b"blob":
                    raise RuntimeError("Cannot read an indexed Git blob")
                size = int(header[2])
                content = reader.stdout.read(size)
                if len(content) != size or reader.stdout.read(1) != b"\n":
                    raise RuntimeError("Incomplete indexed Git blob")
                hits.extend(scan_content(name, content))
        finally:
            if reader.stdin is not None:
                reader.stdin.close()
        if reader.wait() != 0:
            raise RuntimeError("Git blob reader failed")
    return sorted(set(hits))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--history", action="store_true", help="Also inspect all reachable history and commit metadata")
    args = parser.parse_args()
    try:
        root = Path(git(args.repo, "rev-parse", "--show-toplevel").decode().strip())
        hits = scan_index(root)
        if args.history:
            hits.extend(scan_history(root))
    except (OSError, subprocess.SubprocessError, RuntimeError, ValueError) as error:
        # Git exception bodies may contain caller-controlled data.
        print(f"Public boundary check could not complete ({type(error).__name__})", file=sys.stderr)
        return 2
    if hits:
        print("Public boundary check failed (values redacted):")
        print("\n".join(hits))
        return 1
    print("Public boundary check passed (Git index; local ignored files excluded)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
