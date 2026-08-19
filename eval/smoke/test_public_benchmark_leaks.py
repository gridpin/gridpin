#!/usr/bin/env python3
"""Recursively scan a fresh public surface for private evaluation material."""

from __future__ import annotations

import os
import pathlib
import re
import subprocess
import sys
import tempfile


ROOT = pathlib.Path(__file__).resolve().parents[2]
EXPECTED_PATHS = (
    "docs-public/BENCHMARK.md",
    "README.md",
    "examples/README.md",
    "examples/public_benchmark.py",
    "examples/gridpin_http.py",
    "eval/smoke/test_public_benchmark_contract.py",
    "eval/smoke/test_http_adapter_contract.py",
)
PRIVATE_MARKERS = (
    "full" + "_" + "cases",
    "homonym" + "_" + "province",
    "live" + "83",
    "real" + "_" + "cases",
    "eval/" + "scrape",
    "quality" + "_" + "stand",
    "58 of " + "83",
    "83" + "/83",
)
PRIVATE_PATTERNS = (
    re.compile(r"(?:make\s+|[/_.-])adver" r"sarial(?:\b|[_./-])", re.IGNORECASE),
    re.compile(r"\badver" r"sarial\s+(?:sets?|corpus|corpora|datasets?|suites?|cases?)\b", re.IGNORECASE),
)
CYRILLIC = re.compile("[\u0400-\u04ff]")
SKIP_DIRECTORIES = {
    ".git",
    "target",
    "node_modules",
    ".venv-py",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
}
# Every exception has a narrow public reason.  New paths fail until reviewed.
CYRILLIC_ALLOWED = {
    "ATTRIBUTIONS.md": "legal attribution",
    "docs-public/FORMAT.md": "documented product-format example",
    "gridpin/src/builder.rs": "product-language string literals",
    "gridpin/src/norm.rs": "product-language string literals",
    "gridpin/src/query.rs": "product-language string literals",
    "gridpin/src/rules.rs": "product-language string literals",
}


def _fresh_public_surface() -> tuple[pathlib.Path, tempfile.TemporaryDirectory | None]:
    exporter = ROOT / "tools" / "export_public.py"
    if not exporter.is_file():
        return ROOT, None
    temporary = tempfile.TemporaryDirectory(prefix="gridpin-public-leak-scan-")
    destination = pathlib.Path(temporary.name) / "public"
    env = os.environ.copy()
    env["GRIDPIN_PUBLIC_OUT"] = str(destination)
    process = subprocess.run(
        [sys.executable, str(exporter)],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if process.returncode != 0:
        temporary.cleanup()
        raise SystemExit(
            "public benchmark leak scan: fresh public export failed:\n"
            + process.stdout[-1000:]
            + process.stderr[-1000:]
        )
    return destination, temporary


def _text_files(root: pathlib.Path):
    for path in root.rglob("*"):
        relative = path.relative_to(root)
        if any(part in SKIP_DIRECTORIES or part.endswith(".egg-info") for part in relative.parts):
            continue
        if not path.is_file():
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        if "\x00" in text:
            continue
        yield relative.as_posix(), text


def main() -> None:
    surface, temporary = _fresh_public_surface()
    try:
        missing = [relative for relative in EXPECTED_PATHS if not (surface / relative).is_file()]
        if missing:
            raise SystemExit("public benchmark leak scan: missing public files: " + ", ".join(missing))

        failures: list[str] = []
        scanned = 0
        scan_roots = [(surface, "")]
        if temporary is not None and (ROOT / "site").is_dir():
            scan_roots.append((ROOT / "site", "site/"))
        for scan_root, prefix in scan_roots:
            for local_relative, text in _text_files(scan_root):
                relative = prefix + local_relative
                scanned += 1
                lowered = text.casefold()
                for marker in PRIVATE_MARKERS:
                    if marker.casefold() in lowered:
                        failures.append(f"{relative}: private marker {marker!r}")
                for pattern in PRIVATE_PATTERNS:
                    match = pattern.search(text)
                    if match:
                        failures.append(f"{relative}: private dataset reference {match.group(0)!r}")
                if CYRILLIC.search(text) and relative not in CYRILLIC_ALLOWED:
                    failures.append(
                        f"{relative}: Cyrillic text without an explicit legal/product allowlist entry"
                    )

        if scanned == 0:
            failures.append("fresh public surface contained no readable text files")
        if failures:
            raise SystemExit("public benchmark leak scan: FAIL\n  " + "\n  ".join(failures))
        print(f"public benchmark leak scan: PASS ({scanned} text files, recursive fresh-export scan)")
    finally:
        if temporary is not None:
            temporary.cleanup()


if __name__ == "__main__":
    main()
