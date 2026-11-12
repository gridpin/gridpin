"""MCP server: GridPin offline geocoding as tools for AI agents (stdio transport)."""

from __future__ import annotations

import functools
from typing import Any, Dict, List, Optional

from typing_extensions import NotRequired, TypedDict

from mcp.server.mcpserver import MCPServer
from mcp.server.mcpserver.exceptions import ToolError as McpToolError
from mcp.types import ToolAnnotations

from . import __version__, sheets, tools

INSTRUCTIONS = f"""\
GridPin geocodes addresses offline from one data file per country ("sheet").
Countries in release {sheets.RELEASE}: {", ".join(f"{s.code} ({s.name})" for s in sheets.SHEETS.values())}.
Workflow: call list_countries; if the country is not downloaded, call download_sheet once
(32-677 MB, verified against the signed release); then geocode, reverse or batch.
No API key, no rate limit, and no query leaves the machine. Results carry precision
(house, interp, street, city, ...), confidence and weak; weak is true when the engine itself says
the answer needs a check before use. Treat street- or city-level results as approximate.
geocode also says kind: "clarify" means the address exists in several far-apart places and the query named
none of them; ask the user which one (clarify.options) and call geocode again with that city or region added.
"none" comes with a reason (no_street, no_house, city_unknown, outside_sheet, unparsed); tell the user what to add.
"""


def _read_only(title: str) -> ToolAnnotations:
    return ToolAnnotations(title=title, read_only_hint=True, destructive_hint=False, open_world_hint=False)


class Hit(TypedDict):
    """One candidate, the same fields the CLI, Python and DuckDB return."""

    lat: float
    lon: float
    precision: str
    score: float
    confidence: float
    street: str
    commune: str
    postcode: str
    housenumber: NotRequired[str]
    region: NotRequired[str]
    flags: NotRequired[List[str]]
    distance_m: NotRequired[float]
    # true when the answer needs a check before use (engine 0.7 and later)
    weak: NotRequired[bool]
    # how far the answer reaches, on the Esri Addr_type scale, and M/T (T when weak); engine 0.9 and later
    match_level: NotRequired[str]
    match_status: NotRequired[str]
    # only when the extras argument asks for them (engine 0.9 and later): h3, geohash, placekey_where,
    # plus_code, mgrs, dms, street_point, street_point_m, links, provenance
    extras: NotRequired[Dict[str, Any]]


class Choice(TypedDict):
    label: str
    city: str
    region: NotRequired[str]
    lat: float
    lon: float


class Clarify(TypedDict):
    missing: str
    options: List[Choice]


class GeocodeOut(TypedDict):
    country: str
    query: str
    results: List[Hit]
    # engine 0.9 and later: "answer", "clarify" (ask the user which place) or "none" (see reason)
    kind: NotRequired[str]
    clarify: NotRequired[Clarify]
    reason: NotRequired[str]


class ReverseOut(TypedDict):
    country: str
    lat: float
    lon: float
    results: List[Hit]


class BatchRow(TypedDict):
    query: str
    results: List[Hit]


class BatchOut(TypedDict):
    country: str
    results: List[BatchRow]


class Country(TypedDict):
    code: str
    name: str
    size_mb: int
    downloaded: bool


class CountriesOut(TypedDict):
    release: str
    home: str
    countries: List[Country]


class DownloadOut(TypedDict):
    country: str
    path: str
    size_mb: int
    already_downloaded: bool


def _agent_errors(fn):
    """Turn an anticipated tools.ToolError into the SDK error the model can read."""

    @functools.wraps(fn)
    def wrapper(*args, **kwargs):
        try:
            return fn(*args, **kwargs)
        except tools.ToolError as exc:
            raise McpToolError(str(exc)) from exc

    return wrapper


def build_server(session: Optional[tools.Session] = None) -> MCPServer:
    session = session or tools.Session()
    server = MCPServer(
        name="gridpin",
        title="GridPin offline geocoder",
        instructions=INSTRUCTIONS,
        website_url="https://gridpin.dev",
        version=__version__,
    )

    @server.tool(annotations=_read_only("List countries"))
    @_agent_errors
    def list_countries() -> CountriesOut:
        """List covered countries, their sheet sizes and whether each sheet is downloaded."""
        return tools.countries()

    @server.tool(
        annotations=ToolAnnotations(
            title="Download a country sheet",
            read_only_hint=False,
            destructive_hint=False,
            idempotent_hint=True,
            open_world_hint=True,
        )
    )
    @_agent_errors
    def download_sheet(country: str) -> DownloadOut:
        """Download one country sheet from dl.gridpin.dev and verify its SHA-256 against the
        signed release. Needed once per country; the only tool that uses the network.

        country: ISO code or English name, e.g. "fr" or "France".
        """
        return tools.download_sheet(country)

    @server.tool(annotations=_read_only("Geocode an address"))
    @_agent_errors
    def geocode(query: str, country: str, k: int = 1, extras: Optional[str] = None) -> GeocodeOut:
        """Geocode one address or place: text -> coordinates, offline.

        query: the address as a person would write it, e.g. "1 rue de la Paix, Paris".
        country: ISO code or English name of the country the address is in.
        k: number of ranked candidates, 1-10.
        extras: optional comma-separated extra fields per result: h3 (or h3:8), geohash, placekey, codes
            (Plus Code, MGRS, DMS), street_point, links (five map links), provenance (the sheet's source,
            date and licence).
        """
        return session.geocode(query, country, k, extras)

    @server.tool(annotations=_read_only("Reverse geocode a point"))
    @_agent_errors
    def reverse(lat: float, lon: float, country: str, k: int = 1, extras: Optional[str] = None) -> ReverseOut:
        """Reverse geocode: coordinates -> nearest indexed addresses, offline.
        Approximate: each result carries distance_m from the given point.
        extras: optional extra fields per result, as in geocode.
        """
        return session.reverse(lat, lon, country, k, extras)

    @server.tool(annotations=_read_only("Geocode many addresses"))
    @_agent_errors
    def batch(queries: List[str], country: str, k: int = 1) -> BatchOut:
        """Geocode up to 1000 addresses of one country in a single call, in input order."""
        return session.batch(queries, country, k)

    return server


def main() -> None:
    build_server().run("stdio")
