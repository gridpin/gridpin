"""LlamaIndex: `FunctionTool`s over a local GridPin sheet (`pip install "gridpin-agents[llamaindex]"`)."""

from typing import List, Optional

from llama_index.core.tools import FunctionTool

from .core import Calls


def gridpin_tools(country: str, sheet: Optional[str] = None) -> List[FunctionTool]:
    calls = Calls(country, sheet)
    names, docs = calls.names(), calls.docs()

    def geocode(query: str, k: int = 1) -> dict:
        """query: the address as a person writes it; k: number of candidates, 1-10."""
        return calls.geocode(query, k)

    def reverse(lat: float, lon: float, k: int = 1) -> dict:
        """lat, lon: WGS84 degrees; k: number of nearest addresses, 1-10."""
        return calls.reverse(lat, lon, k)

    return [
        FunctionTool.from_defaults(fn=geocode, name=names["geocode"], description=docs["geocode"]),
        FunctionTool.from_defaults(fn=reverse, name=names["reverse"], description=docs["reverse"]),
    ]
