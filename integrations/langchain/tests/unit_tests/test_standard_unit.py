"""LangChain standard unit tests (langchain-tests): construction and input schemas, no sheet."""
from typing import Any, Dict, Type

from langchain_tests.unit_tests import ToolsUnitTests

from langchain_gridpin import GridPinGeocode, GridPinReverse


class TestGridPinGeocodeUnit(ToolsUnitTests):
    @property
    def tool_constructor(self) -> Type[GridPinGeocode]:
        return GridPinGeocode

    @property
    def tool_constructor_params(self) -> Dict[str, Any]:
        return {"country": "us"}

    @property
    def tool_invoke_params_example(self) -> Dict[str, Any]:
        return {"query": "1600 Pennsylvania Ave NW, Washington, DC 20500", "k": 2}


class TestGridPinReverseUnit(ToolsUnitTests):
    @property
    def tool_constructor(self) -> Type[GridPinReverse]:
        return GridPinReverse

    @property
    def tool_constructor_params(self) -> Dict[str, Any]:
        return {"country": "us"}

    @property
    def tool_invoke_params_example(self) -> Dict[str, Any]:
        return {"lat": 38.8977, "lon": -77.0365, "k": 2}
