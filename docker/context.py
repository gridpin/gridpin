"""Prepare the build context of docker/Dockerfile: binary, legal files and, for a country, the sheet.

    python3 docker/context.py --binary target/release/gridpin --legal-dir . --out ctx [--country rs]

The sheet is downloaded from dl.gridpin.dev and checked against the SHA-256 and size pinned in
`gridpin_mcp.sheets` (the same registry the MCP server and the R package use); a mismatch stops here,
before any image exists. Prints the build arguments for `docker build` as KEY=VALUE lines.
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
    ap.add_argument("--legal-dir", type=Path, required=True, help="holds LICENSE, THIRDPARTY.yml, ATTRIBUTIONS.en.md")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--country", choices=sorted(sheets.SHEETS))
    args = ap.parse_args(argv)
    args.out.mkdir(parents=True, exist_ok=True)
    shutil.copy2(args.binary, args.out / "gridpin")
    for name in ("LICENSE", "THIRDPARTY.yml", "ATTRIBUTIONS.en.md"):
        source = args.legal_dir / name
        if not source.is_file():
            print(f"missing {source}", file=sys.stderr)
            return 1
        shutil.copy2(source, args.out / name)
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
