#!/usr/bin/env python3
"""Check that a crate's THIRDPARTY.yml names every Rust library compiled into what we ship.

The wheel, the CLI archives and the DuckDB extension link their Rust dependencies statically, and
most of those licenses (MIT, Apache-2.0, BSD-3-Clause, ...) require their text to travel with every
copy. `make third-party-licenses` writes the texts to gridpin/THIRDPARTY.yml and
duckdb-ext/THIRDPARTY.yml (cargo-bundle-licenses); the files are committed and packed into the
artifacts. This check keeps them in step with the lockfiles:

    python3 .github/thirdparty_licenses.py gridpin duckdb-ext

For each crate directory, the libraries listed in THIRDPARTY.yml must be exactly the packages in
Cargo.lock, minus the crate itself and minus BUILD_ONLY. A library without a license entry, or one
whose text the generator could not find, fails the check too.

The same script checks what was actually built, because build tools skip a missing license file
without an error (maturin, setuptools) and a name check misses a file in the wrong place:

    python3 .github/thirdparty_licenses.py --artifacts gridpin gridpin/target/wheels/*.whl

A wheel must carry LICENSE and THIRDPARTY.yml in .dist-info/licenses/, byte for byte the files in
the crate directory, and nothing at the top of site-packages; an sdist must carry both at its
root. Needs Python 3.11+ (tomllib); no third-party module is required.
"""
from __future__ import annotations

import pathlib
import re
import sys
import tarfile
import tomllib
import zipfile

# Packages that run only inside build scripts: they are in Cargo.lock but never linked into a
# shipped artifact, so the generator leaves them out. Each name here is a reviewed decision.
BUILD_ONLY = {
    "pyo3-build-config": "build script of pyo3 (finds the Python interpreter)",
    "target-lexicon": "dependency of pyo3-build-config",
    "version_check": "build script of generic-array (probes the rustc version)",
}

ROOT = re.compile(r"^root_name: (\S+)$")
NAME = re.compile(r"^- package_name: (\S+)$")
VERSION = re.compile(r"^  package_version: (\S+)$")
LICENSE = re.compile(r"^  - license: \S")
NOT_FOUND = re.compile(r"""^    text: (["']?)NOT FOUND\1$""")

# License files every built artifact carries, compared with the crate directory's copies.
LICENSE_FILES = ("LICENSE", "THIRDPARTY.yml")


def read_bundle(path: pathlib.Path) -> tuple[str | None, list[dict]]:
    """root_name and the library entries of a cargo-bundle-licenses YAML file."""
    root, libs = None, []
    for line in path.read_text(encoding="utf-8").splitlines():
        if m := ROOT.match(line):
            root = m[1]
        elif m := NAME.match(line):
            libs.append({"name": m[1], "version": None, "licenses": 0, "not_found": 0})
        elif libs and (m := VERSION.match(line)):
            libs[-1]["version"] = m[1]
        elif libs and LICENSE.match(line):
            libs[-1]["licenses"] += 1
        elif libs and NOT_FOUND.match(line):
            libs[-1]["not_found"] += 1
    return root, libs


def check(crate_dir: pathlib.Path) -> list[str]:
    """Problems with crate_dir/THIRDPARTY.yml against crate_dir/Cargo.lock (empty list: in step)."""
    bundle = crate_dir / "THIRDPARTY.yml"
    if not bundle.is_file():
        return [f"{bundle}: missing; run `make third-party-licenses`"]
    crate = tomllib.loads((crate_dir / "Cargo.toml").read_text(encoding="utf-8"))["package"]["name"]
    lock = tomllib.loads((crate_dir / "Cargo.lock").read_text(encoding="utf-8"))
    locked = {(p["name"], p["version"]) for p in lock["package"]}
    root, libs = read_bundle(bundle)
    problems = []
    if root != crate:
        problems.append(f"{bundle}: root_name is {root!r}, the crate is {crate!r}")
    listed = {(lib["name"], lib["version"]) for lib in libs}
    if len(listed) != len(libs):
        problems.append(f"{bundle}: a library is listed twice")
    expected = {(n, v) for n, v in locked if n != crate and n not in BUILD_ONLY}
    for n, v in sorted(expected - listed):
        problems.append(f"{bundle}: {n} {v} is in Cargo.lock but has no license text here")
    for n, v in sorted(listed - expected):
        problems.append(f"{bundle}: {n} {v} is listed here but not expected from Cargo.lock")
    for lib in libs:
        if lib["licenses"] == 0:
            problems.append(f"{bundle}: {lib['name']} {lib['version']} has no license entry")
        if lib["not_found"]:
            problems.append(f"{bundle}: {lib['name']} {lib['version']}: license text NOT FOUND")
    if not problems:
        skipped = sorted(n for n, _ in locked if n in BUILD_ONLY)
        print(f"{bundle}: {len(libs)} libraries = {len(locked)} in Cargo.lock"
              f" - {crate} - build-only {skipped}")
    return problems


def _one_equal(artifact: pathlib.Path, where: str, copies: list[bytes], committed: pathlib.Path) -> list[str]:
    if len(copies) != 1:
        return [f"{artifact.name}: expected one {where}, found {len(copies)}"]
    if copies[0] != committed.read_bytes():
        return [f"{artifact.name}: {where} differs from {committed}"]
    return []


def check_artifact(artifact: pathlib.Path, crate_dir: pathlib.Path) -> list[str]:
    """Problems with the license files inside a built wheel or sdist (empty list: as committed)."""
    problems = []
    if artifact.name.endswith(".whl"):
        with zipfile.ZipFile(artifact) as z:
            names = z.namelist()
            top = [n for n in names if "/" not in n]
            if top:
                problems.append(f"{artifact.name}: installs files at the top of site-packages: {top}")
            for name in LICENSE_FILES:
                found = [n for n in names if re.fullmatch(r"[^/]+\.dist-info/licenses/" + re.escape(name), n)]
                problems += _one_equal(artifact, f".dist-info/licenses/{name}", [z.read(n) for n in found],
                                       crate_dir / name)
    elif artifact.name.endswith(".tar.gz"):
        with tarfile.open(artifact) as t:
            files = {m.name: m for m in t.getmembers() if m.isfile()}
            for name in LICENSE_FILES:
                found = [n for n in files if re.fullmatch(r"[^/]+/" + re.escape(name), n)]
                problems += _one_equal(artifact, f"{name} at the sdist root",
                                       [t.extractfile(files[n]).read() for n in found], crate_dir / name)
    else:
        problems.append(f"{artifact.name}: neither a wheel (.whl) nor an sdist (.tar.gz)")
    if not problems:
        print(f"{artifact.name}: {', '.join(LICENSE_FILES)} as in {crate_dir}")
    return problems


def main(argv: list[str]) -> int:
    artifacts = argv[:1] == ["--artifacts"]
    if (artifacts and len(argv) < 3) or not argv:
        print(__doc__, file=sys.stderr)
        return 2
    if artifacts:
        problems = [p for a in argv[2:] for p in check_artifact(pathlib.Path(a), pathlib.Path(argv[1]))]
    else:
        problems = [p for d in argv for p in check(pathlib.Path(d))]
    for p in problems:
        print(f"::error::{p}", file=sys.stderr)
    if problems and not artifacts:
        print("Regenerate with `make third-party-licenses` and commit the result.", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
