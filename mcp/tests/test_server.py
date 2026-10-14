"""The MCP wiring, exercised through the SDK's in-process client."""
import asyncio
import os
from pathlib import Path

import pytest

pytest.importorskip("mcp")
pytest.importorskip("gridpin")
from mcp import Client  # noqa: E402

from gridpin_mcp import server, tools  # noqa: E402

CODE = Path(__file__).resolve().parents[2]
SHEET = Path(os.environ.get("GRIDPIN_TEST_SHEET", CODE / "data" / "mc.bin"))


def run(coro):
    return asyncio.run(coro)


@pytest.fixture
def app():
    if not SHEET.exists():
        pytest.skip(f"no test sheet at {SHEET} (make mc)")
    return server.build_server(tools.Session(open_sheet=lambda code: SHEET))


def test_tools_are_listed_with_read_only_hints(app):
    async def go():
        async with Client(app) as client:
            return (await client.list_tools()).tools
    listed = {t.name: t for t in run(go())}
    assert set(listed) == {"list_countries", "download_sheet", "geocode", "reverse", "batch"}
    for name in ("list_countries", "geocode", "reverse", "batch"):
        assert listed[name].annotations.read_only_hint is True, name
    assert listed["download_sheet"].annotations.read_only_hint is False
    assert listed["download_sheet"].annotations.open_world_hint is True
    assert "query" in listed["geocode"].input_schema["properties"]
    # Agents see the result fields in the schema, not an opaque object.
    hit = listed["geocode"].output_schema["$defs"]["Hit"]
    assert {"lat", "lon", "precision", "confidence"} <= set(hit["required"])


def test_geocode_through_mcp_returns_structured_results(app):
    async def go():
        async with Client(app) as client:
            return await client.call_tool("geocode", {"query": "boulevard des moulins 27 monaco", "country": "fr"})
    result = run(go())
    assert not result.is_error
    assert result.structured_content["results"][0]["lat"] > 43


def test_anticipated_error_reaches_the_model_verbatim(app):
    async def go():
        async with Client(app) as client:
            return await client.call_tool("geocode", {"query": "x", "country": "Poland"})
    result = run(go())
    assert result.is_error
    text = result.content[0].text
    # The SDK prefixes the tool name; a crash would carry nothing after it.
    assert text.startswith("Error executing tool geocode: unknown country 'Poland'; available: br (Brazil)")


def test_instructions_name_every_country_and_the_offline_promise():
    text = server.INSTRUCTIONS
    assert "no query leaves the machine" in text
    for name in ("Brazil", "France", "Germany", "Italy", "Netherlands", "Serbia", "Spain"):
        assert name in text
