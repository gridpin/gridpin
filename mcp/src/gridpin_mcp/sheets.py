"""Country sheets: where they live, how to fetch one, how a download is verified.

A sheet is a single memory-mapped file per country. This module is the only place in
gridpin-mcp that touches the network, and only when you ask for a download. Geocoding
itself never does.

Every sheet of a release is pinned below by size and SHA-256. The values are copied from
the signed release attestation (``https://dl.gridpin.dev/<release>/attestation.json``,
SSH signature ``attestation.json.sig``, signer key ``gridpin-release-signers`` in the
GridPin repository). A download that does not match is discarded, never opened.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import tempfile
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

RELEASE = "v0.6.0"
BASE_URL = "https://dl.gridpin.dev"
# SHA-256 of attestation.json for RELEASE; the pins below are taken from that file.
ATTESTATION_SHA256 = "c42d27538e95b4153621fd0c64f412ea8702af9506b6699793369f76bf16504c"

CHUNK = 1 << 20
# Free space required beyond the sheet itself, so a download never fills the disk.
DISK_MARGIN = 512 << 20


@dataclass(frozen=True)
class Sheet:
    code: str
    name: str
    file: str
    size: int
    sha256: str

    @property
    def url(self) -> str:
        return f"{BASE_URL}/{RELEASE}/{self.file}"


SHEETS = {
    s.code: s
    for s in (
        Sheet("us", "United States", "usa.bin", 1451500169,
              "4f3422f4d95939aefbcf454ceebc9a8dd16f78f2a13c5e6660028ddfad712cc0"),
        Sheet("ca", "Canada", "canada.bin", 182551507,
              "234515843c54b6c4feb93ab83c7b023a3e14beec8357c6d570eeed0f66f35400"),
        Sheet("br", "Brazil", "brazil.bin", 676851417,
              "016e102debb05aafad0ae409c3a2acec478b60bb48b74d03210b80dc52d42a6d"),
        Sheet("de", "Germany", "germany.bin", 213315143,
              "c0ac3d0cbf3dbd480bff6aebd717e2df96e9f0a2eb4e83aeffde736fa42b18af"),
        Sheet("es", "Spain", "spain.bin", 185426996,
              "c671b9a46089e32580189ee3c5a65e024779691a50ab26e77f5639023994819b"),
        Sheet("fr", "France", "france.bin", 364964425,
              "f7278398c1a63308ea3dc69e5cda2f779250716fc231b2122d6ee890da237839"),
        Sheet("it", "Italy", "italy.bin", 246502930,
              "37346d884e551189e91f0453d4aad7aafe211430e62b1e9054141081488b021c"),
        Sheet("nl", "Netherlands", "netherlands.bin", 103234504,
              "d5205f8e323eeda309f3d411a3efcd45c259005d2da25fef33998bf9ca9d28e7"),
        Sheet("rs", "Serbia", "serbia.bin", 31998043,
              "9e358bd6a940b290e01cd52081e476e2799513f16dd934dc6a5bc89ba4fb5388"),
    )
}


class SheetError(Exception):
    """A problem the caller can act on; the message says what to do next."""


def resolve(country: str) -> Sheet:
    """Map a country code or English name ("fr", "FR", "France") to its sheet."""
    key = (country or "").strip().lower()
    for sheet in SHEETS.values():
        if key in (sheet.code, sheet.name.lower()):
            return sheet
    known = ", ".join(f"{s.code} ({s.name})" for s in SHEETS.values())
    raise SheetError(f"unknown country {country!r}; available: {known}")


def home() -> Path:
    """Directory that holds downloaded sheets.

    ``GRIDPIN_HOME`` wins; otherwise ``$XDG_CACHE_HOME/gridpin`` or ``~/.cache/gridpin``.
    Sheets of different releases live in separate subdirectories.
    """
    explicit = os.environ.get("GRIDPIN_HOME")
    if explicit:
        return Path(explicit).expanduser()
    cache = os.environ.get("XDG_CACHE_HOME")
    base = Path(cache).expanduser() if cache else Path.home() / ".cache"
    return base / "gridpin"


def local_path(sheet: Sheet) -> Path:
    return home() / RELEASE / sheet.file


def is_downloaded(sheet: Sheet) -> bool:
    path = local_path(sheet)
    return path.is_file() and path.stat().st_size == sheet.size


def sheet_path(country: str) -> Path:
    """Path of a downloaded sheet, or an error that names the command to fetch it."""
    sheet = resolve(country)
    if not is_downloaded(sheet):
        raise SheetError(
            f"the {sheet.name} sheet is not downloaded ({sheet.size / 1e6:.0f} MB); run "
            f"`gridpin-fetch {sheet.code}` or call the download_sheet tool"
        )
    return local_path(sheet)


Opener = Callable[[str], "urllib.response.addinfourl"]
Progress = Callable[[int, int], None]


def _open(url: str):
    request = urllib.request.Request(url, headers={"User-Agent": f"gridpin-mcp ({RELEASE})"})
    return urllib.request.urlopen(request, timeout=60)


def download(country: str, opener: Opener = _open, progress: Optional[Progress] = None) -> Path:
    """Fetch one sheet into ``home()``, verify size and SHA-256, then move it into place.

    Already present with the pinned size: returned as is, nothing is fetched. The partial
    file is removed on any failure, so a broken download is never mistaken for a sheet.
    """
    sheet = resolve(country)
    target = local_path(sheet)
    if is_downloaded(sheet):
        return target
    target.parent.mkdir(parents=True, exist_ok=True)
    free = shutil.disk_usage(target.parent).free
    if free < sheet.size + DISK_MARGIN:
        raise SheetError(
            f"not enough disk space in {target.parent}: the {sheet.name} sheet needs "
            f"{(sheet.size + DISK_MARGIN) / 1e9:.1f} GB, {free / 1e9:.1f} GB free; "
            f"set GRIDPIN_HOME to another directory"
        )
    digest = hashlib.sha256()
    received = 0
    fd, tmp = tempfile.mkstemp(prefix=sheet.file + ".", suffix=".part", dir=target.parent)
    try:
        with os.fdopen(fd, "wb") as out, opener(sheet.url) as response:
            while True:
                block = response.read(CHUNK)
                if not block:
                    break
                received += len(block)
                if received > sheet.size:
                    raise SheetError(f"{sheet.url}: server sent more than {sheet.size} bytes")
                digest.update(block)
                out.write(block)
                if progress:
                    progress(received, sheet.size)
        if received != sheet.size:
            raise SheetError(f"{sheet.url}: got {received} bytes, expected {sheet.size}; retry")
        if digest.hexdigest() != sheet.sha256:
            raise SheetError(f"{sheet.url}: SHA-256 does not match the signed release; not used")
        os.replace(tmp, target)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise
    return target
