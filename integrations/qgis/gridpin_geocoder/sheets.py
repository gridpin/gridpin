"""Country sheets: pins, local paths and verification, with no QGIS import.

The pins are a copy of ``gridpin_mcp.sheets`` for the same release (a test in the
GridPin repository keeps the two equal). The plugin does not depend on gridpin-mcp:
that package brings an MCP stack QGIS does not need.
"""

import hashlib
import os
from dataclasses import dataclass
from pathlib import Path

RELEASE = "v0.5.0"
BASE_URL = "https://dl.gridpin.dev"
CHUNK = 1 << 20
DISK_MARGIN = 512 << 20


@dataclass(frozen=True)
class Sheet:
    code: str
    name: str
    file: str
    size: int
    sha256: str

    @property
    def url(self):
        return f"{BASE_URL}/{RELEASE}/{self.file}"


SHEETS = {
    s.code: s
    for s in (
        Sheet("us", "United States", "usa.bin", 1451500169,
              "4f3422f4d95939aefbcf454ceebc9a8dd16f78f2a13c5e6660028ddfad712cc0"),
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
CODES = list(SHEETS)


class SheetError(Exception):
    """A problem the user can act on; the message says what to do next."""


def resolve(country):
    key = (country or "").strip().lower()
    for sheet in SHEETS.values():
        if key in (sheet.code, sheet.name.lower()):
            return sheet
    known = ", ".join(f"{s.code} ({s.name})" for s in SHEETS.values())
    raise SheetError(f"unknown country {country!r}; available: {known}")


def home():
    """Same directory as gridpin-mcp uses, so a sheet is downloaded once per machine."""
    explicit = os.environ.get("GRIDPIN_HOME")
    if explicit:
        return Path(explicit).expanduser()
    cache = os.environ.get("XDG_CACHE_HOME")
    base = Path(cache).expanduser() if cache else Path.home() / ".cache"
    return base / "gridpin"


def local_path(sheet):
    return home() / RELEASE / sheet.file


def is_downloaded(sheet):
    path = local_path(sheet)
    return path.is_file() and path.stat().st_size == sheet.size


def sheet_path(country):
    sheet = resolve(country)
    if not is_downloaded(sheet):
        raise SheetError(
            f"the {sheet.name} sheet is not downloaded ({sheet.size / 1e6:.0f} MB); run "
            f"Processing > GridPin > Download country sheet"
        )
    return local_path(sheet)


def verify(sheet, path):
    """Raise unless ``path`` has the pinned size and SHA-256."""
    size = Path(path).stat().st_size
    if size != sheet.size:
        raise SheetError(f"{sheet.url}: got {size} bytes, expected {sheet.size}; retry")
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(CHUNK), b""):
            digest.update(block)
    if digest.hexdigest() != sheet.sha256:
        raise SheetError(f"{sheet.url}: SHA-256 does not match the signed release; not used")


def download(country, fetch, progress=None):
    """Fetch one sheet with ``fetch(url, part_path, progress)``, verify, move into place.

    ``fetch`` writes the body to ``part_path``; the partial file is removed on any failure.
    """
    import shutil
    import tempfile

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
    fd, tmp = tempfile.mkstemp(prefix=sheet.file + ".", suffix=".part", dir=target.parent)
    os.close(fd)
    try:
        fetch(sheet.url, tmp, progress)
        verify(sheet, tmp)
        os.replace(tmp, target)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise
    return target
