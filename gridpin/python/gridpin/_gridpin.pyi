"""Types of the native module gridpin._gridpin (gridpin/src/py.rs): every name the module exports, and nothing
else."""

from os import PathLike
from pathlib import Path
from typing import Any, Optional, Sequence, Tuple, Union, final

__all__ = [
    "Geocoder",
    "_extras_columns",
    "build_places",
    "_sheets_release",
    "_sheets_registry",
    "_sheets_home",
    "_sheets_path",
    "_sheets_downloaded",
    "_sheets_verify",
    "_sheets_fetch",
]

_Path = Union[str, "PathLike[str]"]
# One answer: lat, lon, precision, score, confidence, street, commune, postcode, flags, weak, match_level,
# match_status, and housenumber, region, distance_m when known; `extras` adds the columns it names.
_Hit = dict[str, Any]
# (street, number, city, region, postcode): number, region and postcode may be None, city empty
_FieldRow = Tuple[str, Optional[str], str, Optional[str], Optional[str]]

@final
class Geocoder:
    def __new__(cls, index_path: _Path, poi: Optional[_Path] = None, *, also: Optional[_Path] = None) -> "Geocoder": ...
    def geocode(self, q: str, k: int = 10, *, extras: Optional[str] = None) -> list[_Hit]: ...
    def geocode_or_ask(self, q: str, k: int = 1) -> dict[str, Any]: ...
    def parse(self, q: str) -> dict[str, Any]: ...
    def extract(self, text: str, *, verify: bool = False) -> list[dict[str, Any]]: ...
    def exists(self, q: str) -> dict[str, Any]: ...
    def same_address(self, a: str, b: str) -> dict[str, Any]: ...
    def geocode_many(self, queries: Sequence[str], k: int = 1, *, extras: Optional[str] = None) -> list[list[_Hit]]: ...
    def houses(
        self,
        *,
        street: Optional[str] = None,
        city: Optional[str] = None,
        postcode: Optional[str] = None,
        near: Optional[Tuple[float, float, float]] = None,
        bbox: Optional[Tuple[float, float, float, float]] = None,
        polygon: Optional[str] = None,
        region: Optional[str] = None,
        limit: Optional[int] = None,
    ) -> list[dict[str, Any]]: ...
    def sample(
        self,
        n: int,
        *,
        seed: Optional[int] = None,
        street: Optional[str] = None,
        city: Optional[str] = None,
        postcode: Optional[str] = None,
        near: Optional[Tuple[float, float, float]] = None,
        bbox: Optional[Tuple[float, float, float, float]] = None,
        polygon: Optional[str] = None,
        region: Optional[str] = None,
    ) -> list[dict[str, Any]]: ...
    def geocode_fields_many(
        self, rows: Sequence[_FieldRow], k: int = 1, *, extras: Optional[str] = None
    ) -> list[list[_Hit]]: ...
    def reverse(self, lat: float, lon: float, k: int = 3, *, extras: Optional[str] = None) -> list[_Hit]: ...

def build_places(places: _Path, out: _Path, license: Optional[str] = None) -> int: ...
def _extras_columns(spec: str) -> list[str]: ...
def _sheets_release() -> Tuple[str, str]: ...
def _sheets_registry() -> list[dict[str, Any]]: ...
def _sheets_home() -> Path: ...
def _sheets_path(country: str) -> Path: ...
def _sheets_downloaded(country: str) -> bool: ...
def _sheets_verify(country: str, path: Optional[_Path] = None) -> str: ...
def _sheets_fetch(country: str, offline: bool = False) -> Path: ...
