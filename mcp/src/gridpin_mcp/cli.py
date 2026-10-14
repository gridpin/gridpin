"""Command line: `gridpin-fetch` downloads sheets, `gridpin-mcp` starts the MCP server."""

from __future__ import annotations

import argparse
import sys
from typing import List, Optional

from . import sheets


def _progress(done: int, total: int) -> None:
    sys.stderr.write(f"\r  {done / 1e6:7.1f} / {total / 1e6:.1f} MB")
    if done >= total:
        sys.stderr.write("\n")
    sys.stderr.flush()


def fetch(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        prog="gridpin-fetch",
        description=f"Download GridPin country sheets ({sheets.RELEASE}) and verify them "
        "against the signed release. Prints the path of each sheet.",
    )
    parser.add_argument("countries", nargs="*", help="ISO code or English name, e.g. fr de Spain")
    parser.add_argument("--list", action="store_true", help="list countries and exit")
    args = parser.parse_args(argv)
    if args.list or not args.countries:
        for s in sheets.SHEETS.values():
            state = "downloaded" if sheets.is_downloaded(s) else "not downloaded"
            print(f"{s.code}  {s.name:<12} {s.size / 1e6:6.0f} MB  {state}")
        print(f"sheets directory: {sheets.home() / sheets.RELEASE}")
        return 0 if args.list else 2
    status = 0
    for country in args.countries:
        try:
            print(sheets.download(country, progress=_progress))
        except sheets.SheetError as exc:
            print(f"gridpin-fetch: {exc}", file=sys.stderr)
            status = 1
    return status


def serve() -> None:
    try:
        from .server import main
    except ImportError as exc:
        sys.exit(f"gridpin-mcp: the MCP SDK is missing ({exc}); run `pip install gridpin-mcp`")
    main()


def fetch_main() -> None:
    sys.exit(fetch())
