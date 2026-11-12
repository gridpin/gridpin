"""Fetch the SQLite headers (sqlite3.h, sqlite3ext.h) the loadable extension is compiled against.

The amalgamation is pinned by its SHA-256; a different file stops the build. Only the two headers are kept: the
extension calls SQLite through the API table of the host process and links nothing of SQLite itself.

    python3 .github/sqlite_header.py <dir>
"""
import hashlib
import io
import sys
import urllib.request
import zipfile
from pathlib import Path

URL = "https://sqlite.org/2026/sqlite-amalgamation-3540100.zip"  # 3.54.1, 2026-10-16
SHA256 = "0a9c8e34352dad46006aa82836d5383bff9dc6bc9ce5a60553c05aa30e6ad1f9"
HEADERS = ("sqlite3.h", "sqlite3ext.h")


def main(dest: str) -> None:
    req = urllib.request.Request(URL, headers={"User-Agent": "gridpin-release (https://gridpin.dev)"})
    body = urllib.request.urlopen(req, timeout=120).read()
    got = hashlib.sha256(body).hexdigest()
    if got != SHA256:
        sys.exit(f"::error::{URL}: sha256 {got}, expected {SHA256}")
    out = Path(dest)
    out.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(io.BytesIO(body)) as z:
        for name in HEADERS:
            (out / name).write_bytes(z.read(f"sqlite-amalgamation-3540100/{name}"))
    print(f"SQLite 3.54.1 headers in {out}")


if __name__ == "__main__":
    main(sys.argv[1])
