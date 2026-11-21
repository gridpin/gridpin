"""`show_on_map` of MCP Apps: the tool, its ui:// page and what a client without MCP Apps gets.

The server built here adds the extension the way the release-day patch adds it to `build_server`
(`MCPServer(extensions=[map_app.extension(session)])`).
"""
import asyncio
import base64
import json
import zlib
import os
import re
from pathlib import Path

import pytest

pytest.importorskip("mcp.server.mcpserver")
pytest.importorskip("mcp.server.apps")
if not hasattr(pytest.importorskip("gridpin"), "Geocoder"):
    pytest.skip("gridpin engine wheel not installed", allow_module_level=True)
from mcp import Client  # noqa: E402
from mcp.client.extension import ClientExtension  # noqa: E402
from mcp.server.mcpserver import MCPServer  # noqa: E402

from gridpin_mcp import map_app, tools  # noqa: E402

CODE = Path(__file__).resolve().parents[2]
SHEET = Path(os.environ.get("GRIDPIN_TEST_SHEET", CODE / "data" / "mc.bin"))
APP_MIME = "text/html;profile=mcp-app"


class AppsClient(ClientExtension):
    identifier = "io.modelcontextprotocol/ui"

    def settings(self):
        return {"mimeTypes": [APP_MIME]}


def run(coro):
    return asyncio.run(coro)


@pytest.fixture
def session():
    if not SHEET.exists():
        pytest.skip(f"no test sheet at {SHEET} (make mc)")
    return tools.Session(open_sheet=lambda code: SHEET)


@pytest.fixture
def server(session):
    return MCPServer(name="gridpin", extensions=[map_app.extension(session)])


def call(server, args, apps=True):
    async def go():
        async with Client(server, extensions=[AppsClient()] if apps else None) as client:
            listed = {t.name: t for t in (await client.list_tools()).tools}
            res = await client.call_tool("show_on_map", args)
            page = await client.read_resource(map_app.URI) if apps else None
            return listed, res, page
    return run(go())


ADDRESSES = ["boulevard des moulins 27, monaco", "avenue de la costa 1 monaco", "xyzzy plugh"]


def test_a_client_of_mcp_apps_gets_the_tool_bound_to_the_page_and_the_base_points(server, session):
    listed, res, page = call(server, {"queries": ADDRESSES, "country": "fr"})
    tool = listed["show_on_map"]
    assert tool.meta["ui"]["resourceUri"] == map_app.URI
    assert tool.annotations.read_only_hint is True
    out = res.structured_content
    assert [p["query"] for p in out["pins"]] == ADDRESSES[:2]
    assert out["missing"] == ["xyzzy plugh"]
    # the pins are the top answers of geocode on the same sheet, field for field
    for pin in out["pins"]:
        top = session.geocode(pin["query"], "fr", 1)["results"][0]
        assert (pin["lat"], pin["lon"], pin["precision"], pin["weak"]) == (top["lat"], top["lon"], top["precision"], top["weak"])
    base = out["base"]
    assert base["complete"] is True
    assert len(base["near"]["origins"]) == len(out["pins"])
    for grid in (base["overview"], base["near"]):
        cells = unpack(grid)
        assert len(cells) == grid["size"][0] * grid["size"][1] * len(grid["origins"])
        assert any(cells)
    content = page.contents[0]
    assert content.mime_type == APP_MIME
    assert "ui/notifications/tool-result" in content.text and "ui/initialize" in content.text


def unpack(grid):
    return zlib.decompress(base64.b64decode(grid["cells"]))


def cell(grid, window, lon, lat):
    """The byte of the cell of `window` that holds (lon, lat), or None outside it."""
    (nx, ny), (sx, sy) = grid["size"], grid["step"]
    west, south = grid["origins"][window]
    i, j = int((lon - west) / sx), int((lat - south) / sy)
    if not (0 <= i < nx and 0 <= j < ny):
        return None
    return unpack(grid)[window * nx * ny + j * nx + i]


def test_the_grids_are_dense_under_each_house_and_empty_where_the_sheet_has_no_address(session):
    # a house on the sheet lies in a built cell of both grids: this sees swapped axes, a shifted window or a lost scale
    out = map_app.map_view(session, ["boulevard des moulins 27, monaco", "avenue de la costa 1 monaco",
                                     "rue grimaldi 10, monaco"], "fr")
    base = out["base"]
    houses = [(w, p) for w, p in enumerate(out["pins"]) if p["precision"] == "house"]
    assert houses, out["pins"]
    for w, p in houses:
        assert cell(base["overview"], 0, p["lon"], p["lat"]), ("overview", p)
        assert cell(base["near"], w, p["lon"], p["lat"]), ("fine", p)
        # the window is centred on its pin
        nx, _ = base["near"]["size"]
        west, south = base["near"]["origins"][w]
        sx, sy = base["near"]["step"]
        assert abs((p["lon"] - west) / sx - nx / 2) <= 1 and abs((p["lat"] - south) / sy - nx / 2) <= 1
    # an empty cell of the overview holds no address of the sheet, a shaded one holds some (the sea and the land)
    grid = base["overview"]
    (nx, ny), (sx, sy), (west, south) = grid["size"], grid["step"], grid["origins"][0]
    cells = unpack(grid)
    geo = session.geocoder("fr")[1]
    empty = [k for k, v in enumerate(cells) if v == 0]
    full = [k for k, v in enumerate(cells) if v]
    assert empty and full
    for k in empty[:: max(1, len(empty) // 40)] + full[:: max(1, len(full) // 40)]:
        i, j = k % nx, k // nx
        inner = (west + (i + 0.1) * sx, south + (j + 0.1) * sy, west + (i + 0.9) * sx, south + (j + 0.9) * sy)
        has = bool(geo.sample(1, seed=1, bbox=inner))
        if not cells[k]:
            assert not has, ("an address in an empty cell", i, j)
    # one fine cell with one address is drawn clearly, a full one at most
    assert map_app._fine_shade(0) == 0 and map_app._fine_shade(1) >= 150 and map_app._fine_shade(99) == 255


def test_a_frame_too_big_to_read_whole_reads_the_sheet_again_around_the_first_pins(session, monkeypatch):
    # a frame over FRAME_POINTS addresses is a sample: the fine grid of a pin then comes from its own read, and only
    # the first NEAR_READS pins get one
    monkeypatch.setattr(map_app, "FRAME_POINTS", 50)
    monkeypatch.setattr(map_app, "NEAR_READS", 1)
    out = map_app.map_view(session, ["boulevard des moulins 27, monaco", "avenue de la costa 1 monaco"], "fr")
    base = out["base"]
    assert base["complete"] is False
    assert len(base["near"]["origins"]) == 1
    pin = out["pins"][0]
    assert pin["precision"] == "house" and cell(base["near"], 0, pin["lon"], pin["lat"])


def test_a_client_without_mcp_apps_gets_the_pins_and_no_base_points(server):
    _, res, _ = call(server, {"queries": ADDRESSES, "country": "fr"}, apps=False)
    out = res.structured_content
    assert len(out["pins"]) == 2 and "base" not in out
    assert json.loads(res.content[0].text)["pins"] == out["pins"]


def test_a_weak_answer_is_marked_and_more_than_200_addresses_say_what_was_shown(session):
    out = map_app.map_view(session, ["monaco", "boulevard des moulins 27, monaco"] + ["rue grimaldi"] * 250, "fr")
    assert out["pins"][0]["weak"] is True, out["pins"][0]
    assert out["pins"][1]["weak"] is False
    assert len(out["pins"]) + len(out["missing"]) == map_app.MAX_PINS
    assert out["note"] == "shown the first 200 of 252 addresses"
    # the result stays well under the 150 000 characters a client keeps
    assert len(json.dumps(out)) < 150_000


def test_bad_calls_are_tool_errors(session):
    for queries in ([], ["  "], [3], "rue grimaldi"):
        with pytest.raises(tools.ToolError):
            map_app.map_view(session, queries, "fr")


def test_the_page_loads_nothing_from_outside_and_draws_weak_pins_hollow():
    html = map_app.MAP_HTML
    assert not re.search(r"https?://", html), "the page names no outside host"
    assert not re.search(r"<(script|link|img)[^>]+(src|href)=", html), "nothing is fetched by the page"
    assert "weak: check before use" in html
    # the weak branch of the drawing outlines the pin and does not fill it; the firm branch fills
    weak = re.search(r"if \(p\.weak\) \{([^}]*)\}\s*else \{([^}]*)\}", html)
    assert weak, "the pins are drawn in two branches, weak and firm"
    assert "stroke()" in weak.group(1) and "fill()" not in weak.group(1)
    assert "fill()" in weak.group(2)
    # the grids arrive as zlib bytes and are unpacked in the page, with no image address
    assert 'DecompressionStream("deflate")' in html and "data:" not in html
