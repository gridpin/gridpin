"""The two calls every adapter exposes: address to coordinates and back, over a local GridPin sheet.

Each framework module of this package wraps these functions in its own tool type and nothing else, so
every framework answers the same JSON as the GridPin MCP server (`gridpin_mcp.tools`). Queries never leave
the machine; the sheet must already be on disk (`gridpin-fetch <country>`).
"""

from __future__ import annotations

import threading
from pathlib import Path
from typing import Any, Callable, Dict, Optional

from gridpin_mcp import sheets
from gridpin_mcp import tools as engine_tools

MAX_K = engine_tools.MAX_K

GEOCODE_DOC = (
    "Offline geocoding of an address or place in {country}: returns ranked candidates with lat, lon, "
    "precision (house, interp, street, city, ...), confidence and weak. weak = true means the engine "
    "itself says the answer needs a check. An empty results list means no match."
)
REVERSE_DOC = (
    "Offline reverse geocoding of a point in {country}: returns the nearest addresses, each with "
    "distance_m from the point."
)

_sessions: Dict[Optional[str], engine_tools.Session] = {}
_lock = threading.Lock()


def session(sheet: Optional[str] = None) -> engine_tools.Session:
    """One engine session per sheet source, shared by every tool that reads it."""
    with _lock:
        if sheet not in _sessions:
            if sheet is None:
                _sessions[sheet] = engine_tools.Session()
            else:
                path = Path(sheet).expanduser()

                def open_sheet(code: str) -> Path:
                    if not path.is_file():
                        raise sheets.SheetError(f"no sheet file at {path}")
                    return path

                _sessions[sheet] = engine_tools.Session(open_sheet=open_sheet)
        return _sessions[sheet]


class GridPinError(RuntimeError):
    """A missing sheet or a bad query, worded for the model to act on."""


class Calls:
    """`geocode` and `reverse` bound to one country (and optionally one sheet file)."""

    def __init__(self, country: str, sheet: Optional[str] = None):
        try:
            found = sheets.resolve(country)
        except sheets.SheetError as exc:
            raise ValueError(str(exc)) from exc
        self.country, self.country_name, self.sheet = found.code, found.name, sheet

    def _run(self, call: Callable[[engine_tools.Session], Dict[str, Any]]) -> Dict[str, Any]:
        try:
            return call(session(self.sheet))
        except engine_tools.ToolError as exc:
            raise GridPinError(str(exc)) from exc

    def geocode(self, query: str, k: int = 1) -> Dict[str, Any]:
        return self._run(lambda s: s.geocode(query, self.country, k))

    def reverse(self, lat: float, lon: float, k: int = 1) -> Dict[str, Any]:
        return self._run(lambda s: s.reverse(lat, lon, self.country, k))

    def names(self) -> Dict[str, str]:
        return {"geocode": f"gridpin_geocode_{self.country}", "reverse": f"gridpin_reverse_{self.country}"}

    def docs(self) -> Dict[str, str]:
        return {"geocode": GEOCODE_DOC.format(country=self.country_name),
                "reverse": REVERSE_DOC.format(country=self.country_name)}
