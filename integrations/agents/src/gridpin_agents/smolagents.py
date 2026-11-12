"""smolagents (Hugging Face): `Tool`s over a local GridPin sheet (`pip install "gridpin-agents[smolagents]"`)."""

from typing import List, Optional

from smolagents import Tool

from .core import Calls


class _Geocode(Tool):
    inputs = {
        "query": {"type": "string", "description": "the address as a person writes it"},
        "k": {"type": "integer", "description": "number of candidates, 1-10", "nullable": True},
    }
    output_type = "object"

    def __init__(self, calls: Calls):
        self.name, self.description = calls.names()["geocode"], calls.docs()["geocode"]
        self._calls = calls
        super().__init__()

    def forward(self, query: str, k: Optional[int] = None) -> dict:
        return self._calls.geocode(query, k or 1)


class _Reverse(Tool):
    inputs = {
        "lat": {"type": "number", "description": "latitude, WGS84 degrees"},
        "lon": {"type": "number", "description": "longitude, WGS84 degrees"},
        "k": {"type": "integer", "description": "number of nearest addresses, 1-10", "nullable": True},
    }
    output_type = "object"

    def __init__(self, calls: Calls):
        self.name, self.description = calls.names()["reverse"], calls.docs()["reverse"]
        self._calls = calls
        super().__init__()

    def forward(self, lat: float, lon: float, k: Optional[int] = None) -> dict:
        return self._calls.reverse(lat, lon, k or 1)


def gridpin_tools(country: str, sheet: Optional[str] = None) -> List[Tool]:
    calls = Calls(country, sheet)
    return [_Geocode(calls), _Reverse(calls)]
