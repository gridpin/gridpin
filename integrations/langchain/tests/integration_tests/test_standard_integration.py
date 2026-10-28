"""LangChain standard integration tests (langchain-tests) against the Monaco smoke sheet."""
from typing import Any, Dict, Type

from langchain_tests.integration_tests import ToolsIntegrationTests

from conftest import ADDRESS, SHEET, require_engine_and_sheet
from langchain_gridpin import GridPinGeocode, GridPinReverse

require_engine_and_sheet()


class TestGridPinGeocodeIntegration(ToolsIntegrationTests):
    @property
    def tool_constructor(self) -> Type[GridPinGeocode]:
        return GridPinGeocode

    @property
    def tool_constructor_params(self) -> Dict[str, Any]:
        return {"country": "fr", "sheet": str(SHEET)}

    @property
    def tool_invoke_params_example(self) -> Dict[str, Any]:
        return {"query": ADDRESS, "k": 2}


class TestGridPinReverseIntegration(ToolsIntegrationTests):
    @property
    def tool_constructor(self) -> Type[GridPinReverse]:
        return GridPinReverse

    @property
    def tool_constructor_params(self) -> Dict[str, Any]:
        return {"country": "fr", "sheet": str(SHEET)}

    @property
    def tool_invoke_params_example(self) -> Dict[str, Any]:
        return {"lat": 43.7397, "lon": 7.4265, "k": 2}
