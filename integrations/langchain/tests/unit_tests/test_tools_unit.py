"""Tool behaviour that needs no sheet: names, descriptions, schemas and readable errors."""
import os

import pytest
from langchain_core.messages import ToolCall
from pydantic import ValidationError

from gridpin_mcp import sheets
from langchain_gridpin import GridPinGeocode, GridPinReverse, GridPinToolkit, download_sheet


def call(tool, args):
    return tool.invoke(ToolCall(name=tool.name, args=args, id="1", type="tool_call"))


def test_country_is_normalised_and_names_the_tool():
    tool = GridPinGeocode(country="United States")
    assert tool.country == "us"
    assert tool.name == "gridpin_geocode_us"
    assert "United States" in tool.description
    assert GridPinReverse(country="FR").name == "gridpin_reverse_fr"


def test_explicit_name_and_description_are_kept():
    tool = GridPinGeocode(country="us", name="find_address", description="Find an address.")
    assert (tool.name, tool.description) == ("find_address", "Find an address.")


def test_unknown_country_fails_at_construction_and_lists_the_available_ones():
    with pytest.raises(ValueError) as err:
        GridPinGeocode(country="Atlantis")
    assert "unknown country" in str(err.value) and "us (United States)" in str(err.value)


@pytest.mark.parametrize("tool, args", [
    (GridPinGeocode(country="us"), {"query": "x", "k": 0}),
    (GridPinGeocode(country="us"), {"query": "x", "k": 11}),
    (GridPinReverse(country="us"), {"lat": 90.5, "lon": 0}),
    (GridPinReverse(country="us"), {"lat": 0, "lon": -180.5}),
])
def test_input_schema_rejects_out_of_range_arguments(tool, args):
    with pytest.raises(ValidationError):
        tool.invoke(args)


def test_input_schema_accepts_the_limits():
    GridPinGeocode(country="us").get_input_schema()(query="x", k=10)
    GridPinReverse(country="us").get_input_schema()(lat=-90, lon=180, k=1)


def test_missing_downloaded_sheet_is_a_message_naming_the_download(tmp_path, monkeypatch):
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path))
    message = call(GridPinGeocode(country="us"), {"query": "1 Main St"})
    assert message.status == "error"
    assert "gridpin-fetch us" in message.content


def test_missing_sheet_file_is_a_message_naming_the_path(tmp_path):
    missing = tmp_path / "nowhere.bin"
    message = call(GridPinReverse(country="us", sheet=str(missing)), {"lat": 40.0, "lon": -75.0})
    assert message.status == "error"
    assert f"no sheet file at {missing}" in message.content


def test_toolkit_gives_both_tools_for_its_country_and_sheet():
    geocode, reverse = GridPinToolkit(country="United States", sheet="/s.bin").get_tools()
    assert (type(geocode), type(reverse)) == (GridPinGeocode, GridPinReverse)
    assert [t.name for t in (geocode, reverse)] == ["gridpin_geocode_us", "gridpin_reverse_us"]
    assert geocode.sheet == reverse.sheet == "/s.bin"


def test_download_sheet_returns_a_present_sheet_without_network(tmp_path, monkeypatch):
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path))
    sheet = sheets.resolve("rs")
    path = sheets.local_path(sheet)
    path.parent.mkdir(parents=True)
    with open(path, "wb") as f:  # sparse: the pinned size, no disk blocks
        f.truncate(sheet.size)

    def no_network(*args, **kwargs):
        raise AssertionError("network used for a sheet that is already downloaded")

    monkeypatch.setattr(sheets, "_open", no_network)
    assert download_sheet("Serbia") == path
    assert os.path.getsize(path) == sheet.size
