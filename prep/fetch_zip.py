#!/usr/bin/env python3
"""Bounded download + single-member zip extraction.

A raw `curl | unzip` bounds neither the download size, nor the decompressed size, nor the member
names — a zip bomb or a swapped mirror could blow up the disk or the paths. Here:
  * the download streams under a hard byte cap;
  * EXACTLY ONE declared member is extracted (no zip-slip: the destination is a fixed path,
    archive member names are never used as filesystem paths);
  * the member's decompressed size is capped, counting ACTUAL bytes (headers are not trusted).

  python3 prep/fetch_zip.py <url> <member> <dest> [--max-download MB] [--max-member MB]
Example (geonames): python3 prep/fetch_zip.py https://download.geonames.org/export/dump/RS.zip \\
                        RS.txt data/geonames_rs.txt
"""
# PEP 604 hints need 3.10+; deferred for the system 3.9.
from __future__ import annotations

import argparse
import os
import pathlib
import tempfile
import urllib.request
import zipfile

MB = 1024 * 1024


def download_capped(url: str, dest: pathlib.Path, max_bytes: int) -> None:
    req = urllib.request.Request(url, headers={"User-Agent": "gridpin-prep/1.0"})
    got = 0
    with urllib.request.urlopen(req, timeout=120) as r, open(dest, "wb") as f:
        while True:
            chunk = r.read(1 * MB)
            if not chunk:
                break
            got += len(chunk)
            if got > max_bytes:
                raise SystemExit(f"STOP: {url} exceeded the {max_bytes // MB} MB download cap — "
                                 f"swapped mirror or bloated archive?")
            f.write(chunk)


def extract_one(zpath: pathlib.Path, member: str, dest: pathlib.Path, max_member: int) -> None:
    with zipfile.ZipFile(zpath) as z:
        try:
            info = z.getinfo(member)
        except KeyError:
            raise SystemExit(f"STOP: no member {member!r} in the archive (has: {z.namelist()[:5]}…)")
        if info.file_size > max_member:
            raise SystemExit(f"STOP: {member} declares {info.file_size // MB} MB decompressed — "
                             f"over the {max_member // MB} MB cap (zip bomb?)")
        # Extract to a SIBLING temp, then os.replace ONLY a fully-valid file into dest (
        # ). z.open verifies the member's CRC as it is read, raising BadZipFile at EOF on
        # mismatch/truncation — so a corrupt archive never leaves a 0-byte/partial dest that a later
        # `test -f` would accept. A member name is never used as a filesystem path (no zip-slip).
        tmp = dest.with_name(f".{dest.name}.part-{os.getpid()}")
        written = 0
        try:
            with z.open(info) as src, open(tmp, "wb") as out:
                while True:
                    chunk = src.read(1 * MB)  # raises BadZipFile at EOF if the CRC does not match
                    if not chunk:
                        break
                    written += len(chunk)
                    if written > max_member:  # count actual bytes — headers are not trusted
                        raise SystemExit(f"STOP: {member} decompressed past its declared size — zip bomb")
                    out.write(chunk)
                out.flush()
                os.fsync(out.fileno())
            if written != info.file_size:  # declared vs actual — a truncated member
                raise SystemExit(f"STOP: {member} extracted {written} B ≠ declared {info.file_size} B — corrupt archive")
            os.replace(tmp, dest)  # atomic; the old dest (if any) is only ever replaced by a valid file
            dfd = os.open(str(dest.parent), os.O_RDONLY)  # durable rename
            try:
                os.fsync(dfd)
            finally:
                os.close(dfd)
        finally:
            pathlib.Path(tmp).unlink(missing_ok=True)  # no-op after a successful replace; cleanup on any failure


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("url")
    ap.add_argument("member", help="file name inside the archive (exactly one, e.g. RS.txt)")
    ap.add_argument("dest", type=pathlib.Path)
    ap.add_argument("--max-download", type=int, default=500, help="download cap, MB (500)")
    ap.add_argument("--max-member", type=int, default=6144, help="decompressed member cap, MB (6144)")
    args = ap.parse_args()
    args.dest.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(suffix=".zip", delete=False) as tf:
        tmp = pathlib.Path(tf.name)
    try:
        download_capped(args.url, tmp, args.max_download * MB)
        extract_one(tmp, args.member, args.dest, args.max_member * MB)
    finally:
        tmp.unlink(missing_ok=True)
    print(f"{args.dest}: extracted {args.member} (within the caps)")


if __name__ == "__main__":
    main()
