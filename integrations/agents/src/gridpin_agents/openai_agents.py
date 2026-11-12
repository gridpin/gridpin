"""OpenAI Agents SDK: `FunctionTool`s over a local GridPin sheet (`pip install "gridpin-agents[openai-agents]"`)."""

from typing import List, Optional

from agents import FunctionTool, function_tool

from .core import Calls


def gridpin_tools(country: str, sheet: Optional[str] = None) -> List[FunctionTool]:
    calls = Calls(country, sheet)
    names, docs = calls.names(), calls.docs()

    def geocode(query: str, k: int = 1) -> dict:
        """Geocode an address.

        Args:
            query: the address as a person writes it.
            k: number of candidates, 1-10.
        """
        return calls.geocode(query, k)

    def reverse(lat: float, lon: float, k: int = 1) -> dict:
        """Reverse geocode a point.

        Args:
            lat: latitude, WGS84 degrees.
            lon: longitude, WGS84 degrees.
            k: number of nearest addresses, 1-10.
        """
        return calls.reverse(lat, lon, k)

    return [
        function_tool(geocode, name_override=names["geocode"], description_override=docs["geocode"]),
        function_tool(reverse, name_override=names["reverse"], description_override=docs["reverse"]),
    ]
