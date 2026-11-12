"""CrewAI: `BaseTool`s over a local GridPin sheet (`pip install "gridpin-agents[crewai]"`)."""

from typing import List, Optional, Type

from crewai.tools import BaseTool
from pydantic import BaseModel, Field, PrivateAttr

from .core import MAX_K, Calls


class GeocodeArgs(BaseModel):
    query: str = Field(description="the address as a person writes it")
    k: int = Field(default=1, ge=1, le=MAX_K, description="number of candidates")


class ReverseArgs(BaseModel):
    lat: float = Field(ge=-90, le=90, description="latitude, WGS84 degrees")
    lon: float = Field(ge=-180, le=180, description="longitude, WGS84 degrees")
    k: int = Field(default=1, ge=1, le=MAX_K, description="number of nearest addresses")


class GridPinGeocode(BaseTool):
    name: str = "gridpin_geocode"
    description: str = "Geocode an address."
    args_schema: Type[BaseModel] = GeocodeArgs
    _calls: Calls = PrivateAttr()

    def __init__(self, calls: Calls, **data):
        super().__init__(name=calls.names()["geocode"], description=calls.docs()["geocode"], **data)
        self._calls = calls

    def _run(self, query: str, k: int = 1) -> dict:
        return self._calls.geocode(query, k)


class GridPinReverse(BaseTool):
    name: str = "gridpin_reverse"
    description: str = "Reverse geocode a point."
    args_schema: Type[BaseModel] = ReverseArgs
    _calls: Calls = PrivateAttr()

    def __init__(self, calls: Calls, **data):
        super().__init__(name=calls.names()["reverse"], description=calls.docs()["reverse"], **data)
        self._calls = calls

    def _run(self, lat: float, lon: float, k: int = 1) -> dict:
        return self._calls.reverse(lat, lon, k)


def gridpin_tools(country: str, sheet: Optional[str] = None) -> List[BaseTool]:
    calls = Calls(country, sheet)
    return [GridPinGeocode(calls), GridPinReverse(calls)]
