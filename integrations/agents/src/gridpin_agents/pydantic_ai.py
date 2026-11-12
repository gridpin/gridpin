"""PydanticAI: `Tool`s over a local GridPin sheet (`pip install "gridpin-agents[pydantic-ai]"`)."""

from typing import List, Optional

from pydantic_ai import ModelRetry, Tool

from .core import Calls, GridPinError


def gridpin_tools(country: str, sheet: Optional[str] = None) -> List[Tool]:
    calls = Calls(country, sheet)
    names, docs = calls.names(), calls.docs()

    def geocode(query: str, k: int = 1) -> dict:
        """query: the address as a person writes it; k: number of candidates, 1-10."""
        try:
            return calls.geocode(query, k)
        except GridPinError as exc:  # the model gets the message and may try again
            raise ModelRetry(str(exc)) from exc

    def reverse(lat: float, lon: float, k: int = 1) -> dict:
        """lat, lon: WGS84 degrees; k: number of nearest addresses, 1-10."""
        try:
            return calls.reverse(lat, lon, k)
        except GridPinError as exc:
            raise ModelRetry(str(exc)) from exc

    return [
        Tool(geocode, takes_ctx=False, name=names["geocode"], description=docs["geocode"]),
        Tool(reverse, takes_ctx=False, name=names["reverse"], description=docs["reverse"]),
    ]
