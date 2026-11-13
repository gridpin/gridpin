"""The MCP wiring, exercised through the SDK's in-process client."""
import asyncio
import os
from pathlib import Path

import pytest

# Not importorskip("mcp"): run from the repository root, the mcp/ folder of this package is an
# importable namespace package named "mcp", so that check passes without the SDK and the
# collection fails. A real SDK module is the only proof the SDK is installed.
pytest.importorskip("mcp.server.mcpserver")
if not hasattr(pytest.importorskip("gridpin"), "Geocoder"):  # same trap for the gridpin/ folder
    pytest.skip("gridpin engine wheel not installed", allow_module_level=True)
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
    assert set(listed) == {"list_countries", "download_sheet", "geocode", "reverse", "batch", "extract_addresses"}
    for name in ("list_countries", "geocode", "reverse", "batch", "extract_addresses"):
        assert listed[name].annotations.read_only_hint is True, name
    assert listed["download_sheet"].annotations.read_only_hint is False
    assert listed["download_sheet"].annotations.open_world_hint is True
    assert "query" in listed["geocode"].input_schema["properties"]
    # Agents see the result fields in the schema, not an opaque object.
    hit = listed["geocode"].output_schema["$defs"]["Hit"]
    assert {"lat", "lon", "precision", "confidence"} <= set(hit["required"])


def test_every_tool_states_title_and_destructive_hint(app):
    # Required of a directory listing: Anthropic Software Directory Policy, section 5.E.
    async def go():
        async with Client(app) as client:
            return (await client.list_tools()).tools
    tools_listed = run(go())
    assert len(tools_listed) == 6
    for tool in tools_listed:
        name = tool.name
        assert tool.annotations.title, name
        assert tool.annotations.destructive_hint is False, name


def test_geocode_through_mcp_returns_structured_results(app):
    async def go():
        async with Client(app) as client:
            return await client.call_tool("geocode", {"query": "boulevard des moulins 27 monaco", "country": "fr"})
    result = run(go())
    assert not result.is_error
    assert result.structured_content["results"][0]["lat"] > 43


def test_extract_addresses_through_mcp_finds_the_address_in_a_text(app):
    text = "Please send the parcel to 27 boulevard des Moulins, Monaco by Friday."
    async def go():
        async with Client(app) as client:
            return await client.call_tool("extract_addresses", {"text": text, "country": "fr"})
    result = run(go())
    assert not result.is_error
    found = result.structured_content["results"]
    assert found, "no address found in the text"
    first = found[0]
    assert text.encode()[first["start"]:first["end"]].decode() == first["text"]
    assert "moulins" in first["text"].lower() and first["hit"]["lat"] > 43


def test_anticipated_error_reaches_the_model_verbatim(app):
    async def go():
        async with Client(app) as client:
            return await client.call_tool("geocode", {"query": "x", "country": "Poland"})
    result = run(go())
    assert result.is_error
    text = result.content[0].text
    # The SDK prefixes the tool name; a crash would carry nothing after it.
    assert text.startswith("Error executing tool geocode: unknown country 'Poland'; available: ")
    assert "br (Brazil)" in text


def test_instructions_name_every_country_and_the_offline_promise():
    text = server.INSTRUCTIONS
    assert "no query leaves the machine" in text
    for name in ("Brazil", "France", "Germany", "Italy", "Netherlands", "Serbia", "Spain"):
        assert name in text
