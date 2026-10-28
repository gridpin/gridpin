"""The tools over the real engine and the Monaco smoke sheet (make mc)."""
import asyncio
import json

from langchain_core.messages import ToolCall

from conftest import ADDRESS, SHEET, require_engine_and_sheet
from gridpin_mcp import tools as engine_tools
from langchain_gridpin import GridPinGeocode, GridPinReverse, GridPinToolkit

require_engine_and_sheet()


def call(tool, args):
    return tool.invoke(ToolCall(name=tool.name, args=args, id="1", type="tool_call"))


def geocoder():
    return GridPinGeocode(country="fr", sheet=str(SHEET))


def test_geocode_message_carries_json_and_the_same_data_as_artifact():
    message = call(geocoder(), {"query": ADDRESS, "k": 3})
    assert message.status == "success"
    assert json.loads(message.content) == message.artifact
    out = message.artifact
    assert out["country"] == "fr" and out["query"] == ADDRESS
    top = out["results"][0]
    assert top["housenumber"] == "27" and top["precision"] == "house"
    assert isinstance(top["lat"], float) and isinstance(top["lon"], float)
    assert 1 <= len(out["results"]) <= 3


def test_k_reaches_the_engine():
    one = call(geocoder(), {"query": "avenue monaco"}).artifact["results"]
    many = call(geocoder(), {"query": "avenue monaco", "k": 4}).artifact["results"]
    assert len(one) == 1 and len(many) == 4


def test_no_match_is_an_empty_list_not_an_error():
    message = call(geocoder(), {"query": "xyzzy plugh nonexistent street 999"})
    assert message.status == "success" and message.artifact["results"] == []


def test_blank_query_comes_back_as_an_error_message():
    message = call(geocoder(), {"query": "   "})
    assert message.status == "error" and "non-empty" in message.content


def test_reverse_returns_nearest_addresses_with_distance():
    message = call(GridPinReverse(country="fr", sheet=str(SHEET)), {"lat": 43.7397, "lon": 7.4265, "k": 2})
    out = message.artifact
    assert (out["lat"], out["lon"]) == (43.7397, 7.4265)
    assert len(out["results"]) == 2
    assert all(r["distance_m"] < 100 for r in out["results"])


def test_async_invoke_gives_the_same_answer():
    tool = geocoder()
    args = ToolCall(name=tool.name, args={"query": ADDRESS, "k": 2}, id="1", type="tool_call")
    assert asyncio.run(tool.ainvoke(args)).artifact == tool.invoke(args).artifact


def test_tools_on_one_sheet_open_it_once(monkeypatch):
    opened = []
    real = engine_tools._engine()

    class Counting:
        @staticmethod
        def Geocoder(path):
            opened.append(path)
            return real.Geocoder(path)

    monkeypatch.setattr(engine_tools, "_engine", lambda: Counting)
    geocode, reverse = GridPinToolkit(country="fr", sheet=str(SHEET)).get_tools()
    call(geocode, {"query": ADDRESS})
    call(reverse, {"lat": 43.7397, "lon": 7.4265})
    call(geocoder(), {"query": ADDRESS})
    assert opened == [str(SHEET)]
