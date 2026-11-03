"""Prepare the build context of docker/Dockerfile: binary, legal files and, for a country, the sheet.

    python3 docker/context.py --binary target/release/gridpin --legal-dir . --out ctx [--country rs]
    python3 docker/context.py --binary target/release/gridpin --legal-dir . --out ctx --sheet data/mc.bin --sheet-country mc

The sheet is downloaded from dl.gridpin.dev and checked against the SHA-256 and size pinned in
`gridpin_mcp.sheets` (the same registry the MCP server and the R package use); a mismatch stops here,
before any image exists. Prints the build arguments for `docker build` as KEY=VALUE lines.

`--sheet` takes a sheet built locally instead (CI checks the image on the Monaco smoke sheet): from
0.7.1 the registry names the release being prepared, whose sheets are uploaded only after the tag, so a
commit-time check cannot download them.
"""

from __future__ import annotations

import argparse
import hashlib
import shutil
import sys
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "mcp" / "src"))
from gridpin_mcp import sheets  # noqa: E402


# Name in the build context -> names looked up in --legal-dir, in order. The attribution file is
# ATTRIBUTIONS.en.md in the source tree and ATTRIBUTIONS.md in the public repository, which renames it.
LEGAL = {
    "LICENSE": ("LICENSE",),
    "THIRDPARTY.yml": ("THIRDPARTY.yml",),
    "ATTRIBUTIONS.md": ("ATTRIBUTIONS.en.md", "ATTRIBUTIONS.md"),
}


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def fetch(url: str, dest: Path) -> None:
    request = urllib.request.Request(url, headers={"User-Agent": "gridpin-docker-context"})
    with urllib.request.urlopen(request, timeout=120) as response, dest.open("wb") as out:
        shutil.copyfileobj(response, out, 1 << 20)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--binary", type=Path, required=True)
    ap.add_argument("--legal-dir", type=Path, required=True, help="holds LICENSE, THIRDPARTY.yml, ATTRIBUTIONS.md (or ATTRIBUTIONS.en.md)")
    ap.add_argument("--out", type=Path, required=True)
    source = ap.add_mutually_exclusive_group()
    source.add_argument("--country", choices=sorted(sheets.SHEETS))
    source.add_argument("--sheet", type=Path, help="a locally built sheet instead of a registry download")
    ap.add_argument("--sheet-country", help="country code of --sheet (image label)")
    args = ap.parse_args(argv)
    if (args.sheet is None) != (args.sheet_country is None):
        ap.error("--sheet and --sheet-country go together")
    args.out.mkdir(parents=True, exist_ok=True)
    shutil.copy2(args.binary, args.out / "gridpin")
    for name, candidates in LEGAL.items():
        source = next((args.legal_dir / c for c in candidates if (args.legal_dir / c).is_file()), None)
        if source is None:
            print(f"missing {args.legal_dir / candidates[-1]}", file=sys.stderr)
            return 1
        shutil.copy2(source, args.out / name)
    if args.sheet is not None:
        dest = args.out / args.sheet.name
        shutil.copy2(args.sheet, dest)
        print(f"SHEET={dest.name}")
        print(f"COUNTRY={args.sheet_country}")
        print(f"SHEET_SHA256={sha256(dest)}")
        print("SHEET_RELEASE=local")
        return 0
    if args.country is None:
        return 0
    sheet = sheets.SHEETS[args.country]
    dest = args.out / sheet.file
    fetch(sheet.url, dest)
    got_size, got_sha = dest.stat().st_size, sha256(dest)
    if (got_size, got_sha) != (sheet.size, sheet.sha256):
        print(f"{sheet.url}: {got_size} bytes, sha256 {got_sha}; expected {sheet.size}, {sheet.sha256}", file=sys.stderr)
        return 1
    print(f"SHEET={sheet.file}")
    print(f"COUNTRY={args.country}")
    print(f"SHEET_SHA256={sheet.sha256}")
    print(f"SHEET_RELEASE={sheets.RELEASE}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
