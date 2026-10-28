"""LangChain tools over a local GridPin sheet: address to coordinates and back, no API key.

Queries never leave the machine. The tools read a country sheet that is already on disk; they
do not download anything. Fetch a sheet once with ``gridpin-fetch <country>`` or
``download_sheet``. The geocoding itself is the same code path as the GridPin MCP server
(``gridpin_mcp.tools``), so both return the same fields.
"""

from __future__ import annotations

import json
import threading
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple, Type

from gridpin_mcp import sheets
from gridpin_mcp import tools as engine_tools
from langchain_core.callbacks import CallbackManagerForToolRun
from langchain_core.tools import BaseTool, BaseToolkit, ToolException
from pydantic import BaseModel, Field

MAX_K = engine_tools.MAX_K

_sessions: Dict[Optional[str], engine_tools.Session] = {}
_sessions_lock = threading.Lock()


def _session(sheet: Optional[str]) -> engine_tools.Session:
    """One engine session per sheet source, shared by every tool that reads it."""
    with _sessions_lock:
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


def download_sheet(country: str) -> Path:
    """Fetch one country sheet into the GridPin cache (network, once) and return its path.

    The download is checked against the size and SHA-256 pinned from the signed GridPin
    release; a sheet that is already present is returned without any network access.
    """
    return sheets.download(country)


class GeocodeInput(BaseModel):
    query: str = Field(
        description="Address or place as a person would write it, "
        "e.g. '1600 Pennsylvania Ave NW, Washington, DC 20500'"
    )
    k: int = Field(default=1, ge=1, le=MAX_K, description="Number of ranked candidates to return")


class ReverseInput(BaseModel):
    lat: float = Field(ge=-90, le=90, description="Latitude in decimal degrees (WGS 84)")
    lon: float = Field(ge=-180, le=180, description="Longitude in decimal degrees (WGS 84)")
    k: int = Field(default=1, ge=1, le=MAX_K, description="Number of nearest addresses to return")


class _GridPinTool(BaseTool):
    """Shared settings: which country sheet the tool reads, and how its errors reach the model."""

    country: str
    """Country code or English name of the sheet, e.g. ``"us"`` or ``"United States"``."""
    sheet: Optional[str] = None
    """Path to a sheet file; default is the downloaded sheet of ``country``."""

    response_format: str = "content_and_artifact"
    # A missing sheet or a bad query comes back to the model as a message it can act on.
    handle_tool_error: bool = True

    _verb = ""
    _summary = ""

    def model_post_init(self, context: Any) -> None:
        super().model_post_init(context)
        try:
            found = sheets.resolve(self.country)
        except sheets.SheetError as exc:
            raise ValueError(str(exc)) from exc
        self.country = found.code
        if "name" not in self.model_fields_set:
            self.name = f"gridpin_{self._verb}_{found.code}"
        if "description" not in self.model_fields_set:
            self.description = self._summary.format(country=found.name)

    def _answer(self, call) -> Tuple[str, Dict[str, Any]]:
        try:
            out = call(_session(self.sheet))
        except engine_tools.ToolError as exc:
            raise ToolException(str(exc)) from exc
        return json.dumps(out, ensure_ascii=False), out


class GridPinGeocode(_GridPinTool):
    """Address or place text to coordinates, from a local GridPin sheet.

    Example:
        .. code-block:: python

            from langchain_gridpin import GridPinGeocode

            tool = GridPinGeocode(country="us")
            tool.invoke({"query": "1600 Pennsylvania Ave NW, Washington, DC 20500"})
    """

    name: str = "gridpin_geocode"
    description: str = "Geocode an address."
    args_schema: Type[BaseModel] = GeocodeInput

    _verb = "geocode"
    _summary = (
        "Offline geocoding of an address or place covered by the GridPin {country} sheet: "
        "returns ranked candidates with lat, lon, precision (house, interp, street, city, ...) "
        "and confidence. Treat street or city precision as approximate. An empty results list "
        "means no match."
    )

    def _run(
        self, query: str, k: int = 1, run_manager: Optional[CallbackManagerForToolRun] = None
    ) -> Tuple[str, Dict[str, Any]]:
        return self._answer(lambda s: s.geocode(query, self.country, k))


class GridPinReverse(_GridPinTool):
    """Coordinates to the nearest addresses, from a local GridPin sheet.

    Example:
        .. code-block:: python

            from langchain_gridpin import GridPinReverse

            tool = GridPinReverse(country="us")
            tool.invoke({"lat": 38.8977, "lon": -77.0365})
    """

    name: str = "gridpin_reverse"
    description: str = "Reverse geocode coordinates."
    args_schema: Type[BaseModel] = ReverseInput

    _verb = "reverse"
    _summary = (
        "Offline reverse geocoding of a point covered by the GridPin {country} sheet: returns "
        "the nearest indexed addresses, each with distance_m from the point."
    )

    def _run(
        self,
        lat: float,
        lon: float,
        k: int = 1,
        run_manager: Optional[CallbackManagerForToolRun] = None,
    ) -> Tuple[str, Dict[str, Any]]:
        return self._answer(lambda s: s.reverse(lat, lon, self.country, k))


class GridPinToolkit(BaseToolkit):
    """Both GridPin tools for one country sheet.

    Example:
        .. code-block:: python

            from langchain_gridpin import GridPinToolkit

            tools = GridPinToolkit(country="us").get_tools()
    """

    country: str
    sheet: Optional[str] = None

    def get_tools(self) -> List[BaseTool]:
        return [
            GridPinGeocode(country=self.country, sheet=self.sheet),
            GridPinReverse(country=self.country, sheet=self.sheet),
        ]
