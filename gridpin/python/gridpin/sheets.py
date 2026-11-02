"""Country sheets of the release: list, cache folder, check, download.

The registry is compiled into the engine, so this is the same list, cache folder, size and SHA-256
check and download as `gridpin sheets list / where / verify / fetch` on the command line. A sheet
fetched here is found by the CLI, gridpin-mcp, the R and Node packages, and the other way round.

The folder is GRIDPIN_HOME, else $XDG_CACHE_HOME/gridpin, else ~/.cache/gridpin, then
`<release>/<file>`. Downloading runs the system `curl`; GRIDPIN_OFFLINE=1 (or offline=True)
forbids the network. Geocoding itself never touches the network.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from typing import List, Optional, Union

from . import _gridpin

RELEASE, BASE_URL = _gridpin._sheets_release()

__all__ = ["RELEASE", "BASE_URL", "Sheet", "list", "resolve", "home", "path", "is_downloaded",
           "verify", "fetch", "open_path"]


@dataclass(frozen=True)
class Sheet:
    code: str
    name: str
    file: str
    size: int
    sha256: str
    url: str

    @property
    def path(self) -> Path:
        """Where this sheet lives in the cache, downloaded or not."""
        return Path(_gridpin._sheets_path(self.code))

    @property
    def downloaded(self) -> bool:
        """The file is in the cache with the pinned size (the full check is verify())."""
        return _gridpin._sheets_downloaded(self.code)


_ALL = tuple(Sheet(**row) for row in _gridpin._sheets_registry())

# `list` is part of the public names below; keep the builtin for the annotations.
_list = list


def list() -> List[Sheet]:  # noqa: A001 — mirrors `gridpin sheets list`
    """Every sheet of the release, in the registry order."""
    return _list(_ALL)


def resolve(country: str) -> Sheet:
    """A country code or English name ("us", "US", "United States") to its sheet; ValueError otherwise."""
    _gridpin._sheets_path(country)  # raises ValueError naming the available countries
    key = country.strip().lower()
    return next(s for s in _ALL if key in (s.code, s.name.lower()))


def home() -> Path:
    """The cache folder (without the release part)."""
    return Path(_gridpin._sheets_home())


def path(country: str) -> Path:
    """Where the country's sheet lives in the cache, downloaded or not."""
    return Path(_gridpin._sheets_path(country))


def is_downloaded(country: str) -> bool:
    return _gridpin._sheets_downloaded(country)


def verify(country: str, file: Optional[Union[str, os.PathLike]] = None) -> str:
    """Check a sheet against the release: "ok", "missing", "wrong_size" or "wrong_sha256".

    `file` defaults to the cache; pass a path to check a sheet you downloaded yourself.
    """
    return _gridpin._sheets_verify(country, None if file is None else os.fspath(file))


def fetch(country: str, offline: bool = False) -> Path:
    """Download the country's sheet unless it is already in place and checked; returns its path.

    A download that does not match the release size and SHA-256 is discarded, never kept.
    """
    return Path(_gridpin._sheets_fetch(country, offline))


def open_path(sheet: Union[str, os.PathLike]) -> Path:
    """A path to a .bin file as is, or a country code/name to its downloaded sheet.

    A country whose sheet is not downloaded is a FileNotFoundError that names the fetch call.
    """
    text = os.fspath(sheet)
    candidate = Path(text).expanduser()
    if candidate.suffix == ".bin" or os.sep in text or (os.altsep and os.altsep in text):
        return candidate
    found = resolve(text)
    if not found.downloaded:
        raise FileNotFoundError(
            f"the {found.name} sheet is not downloaded ({found.size / 1e6:.0f} MB): "
            f"run gridpin.sheets.fetch({found.code!r}) once, or `gridpin sheets fetch {found.code}`")
    return found.path
