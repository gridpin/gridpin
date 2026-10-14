"""Geocoding tools over local sheets, shaped for agents: plain JSON in, plain JSON out.

These functions do not depend on the MCP SDK, so they are usable (and tested) on their own.
The server module only wires them to MCP tools.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Callable, Dict, List, Optional

from . import sheets

MAX_BATCH = 1000
MAX_K = 10


class ToolError(Exception):
    """An error the agent can act on; the message names the next step."""


def _engine():
    try:
        import gridpin  # the Rust engine wheel
    except ImportError as exc:  # pragma: no cover - exercised only without the wheel
        raise ToolError("the gridpin engine is not installed; run `pip install gridpin`") from exc
    return gridpin


def _check_k(k: int) -> int:
    if not isinstance(k, int) or k < 1 or k > MAX_K:
        raise ToolError(f"k must be an integer from 1 to {MAX_K}, got {k!r}")
    return k


class Session:
    """Opened sheets, one engine handle per country, kept for the life of the server."""

    def __init__(self, open_sheet: Optional[Callable[[str], Path]] = None):
        self._open_sheet = open_sheet or sheets.sheet_path
        self._geocoders: Dict[str, Any] = {}

    def geocoder(self, country: str):
        try:
            code = sheets.resolve(country).code
            if code not in self._geocoders:
                path = self._open_sheet(code)
                self._geocoders[code] = _engine().Geocoder(str(path))
        except sheets.SheetError as exc:
            raise ToolError(str(exc)) from exc
        return code, self._geocoders[code]

    def geocode(self, query: str, country: str, k: int = 1) -> Dict[str, Any]:
        """Address or place text -> ranked candidates with coordinates."""
        if not isinstance(query, str) or not query.strip():
            raise ToolError("query must be a non-empty address string")
        code, geo = self.geocoder(country)
        return {"country": code, "query": query, "results": list(geo.geocode(query, _check_k(k)))}

    def reverse(self, lat: float, lon: float, country: str, k: int = 1) -> Dict[str, Any]:
        """Coordinates -> nearest indexed addresses; each result carries distance_m."""
        code, geo = self.geocoder(country)
        try:
            hits = geo.reverse(float(lat), float(lon), _check_k(k))
        except ValueError as exc:
            raise ToolError(str(exc)) from exc
        return {"country": code, "lat": lat, "lon": lon, "results": list(hits)}

    def batch(self, queries: List[str], country: str, k: int = 1) -> Dict[str, Any]:
        """Many addresses in one call (up to MAX_BATCH); results keep the input order."""
        if not isinstance(queries, list) or not queries:
            raise ToolError("queries must be a non-empty list of address strings")
        if len(queries) > MAX_BATCH:
            raise ToolError(
                f"at most {MAX_BATCH} queries per call, got {len(queries)}; split the list, "
                "or use the Python package or the CLI `gridpin batch` for files"
            )
        if not all(isinstance(q, str) for q in queries):
            raise ToolError("every query must be a string")
        code, geo = self.geocoder(country)
        rows = geo.geocode_many(queries, _check_k(k))
        return {
            "country": code,
            "results": [{"query": q, "results": list(r)} for q, r in zip(queries, rows)],
        }


def countries() -> Dict[str, Any]:
    """Every country this release covers, with download state and size."""
    return {
        "release": sheets.RELEASE,
        "home": str(sheets.home()),
        "countries": [
            {
                "code": s.code,
                "name": s.name,
                "size_mb": round(s.size / 1e6),
                "downloaded": sheets.is_downloaded(s),
            }
            for s in sheets.SHEETS.values()
        ],
    }


def download_sheet(country: str, opener=None) -> Dict[str, Any]:
    """Fetch one country sheet (network) and verify it against the signed release."""
    try:
        sheet = sheets.resolve(country)
        already = sheets.is_downloaded(sheet)
        path = sheets.download(country, **({"opener": opener} if opener else {}))
    except sheets.SheetError as exc:
        raise ToolError(str(exc)) from exc
    return {
        "country": sheet.code,
        "path": str(path),
        "size_mb": round(sheet.size / 1e6),
        "already_downloaded": already,
    }
