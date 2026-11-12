"""Each framework's own tool call answers what the shared core answers, on the Monaco smoke sheet (make mc).

A framework that is not installed is skipped by name; the engine wheel and the sheet are required.
"""
import asyncio
import json
import os
from pathlib import Path

import pytest

if not hasattr(pytest.importorskip("gridpin"), "Geocoder"):
    pytest.skip("gridpin engine wheel not installed", allow_module_level=True)

from gridpin_agents import Calls, GridPinError  # noqa: E402

CODE = Path(__file__).resolve().parents[3]
SHEET = str(Path(os.environ.get("GRIDPIN_TEST_SHEET", CODE / "data" / "mc.bin")))
ADDRESS = "boulevard des moulins 27 monaco"


@pytest.fixture(scope="module")
def truth():
    if not Path(SHEET).exists():
        pytest.skip("no Monaco smoke sheet (make mc)")
    calls = Calls("fr", SHEET)
    return calls.geocode(ADDRESS, 1), calls


def test_the_core_names_the_country_and_reports_a_missing_sheet(truth, tmp_path):
    out, calls = truth
    assert out["results"][0]["housenumber"] == "27" and out["country"] == "fr"
    assert calls.names() == {"geocode": "gridpin_geocode_fr", "reverse": "gridpin_reverse_fr"}
    assert "France" in calls.docs()["geocode"]
    with pytest.raises(GridPinError, match="no sheet file"):
        Calls("fr", str(tmp_path / "none.bin")).geocode(ADDRESS)
    with pytest.raises(ValueError, match="unknown country"):
        Calls("Atlantis")


def test_llamaindex(truth):
    pytest.importorskip("llama_index.core")
    from gridpin_agents.llamaindex import gridpin_tools

    geocode, reverse = gridpin_tools("fr", SHEET)
    assert geocode.metadata.name == "gridpin_geocode_fr"
    assert geocode.call(query=ADDRESS, k=1).raw_output == truth[0]
    top = truth[0]["results"][0]
    assert reverse.call(lat=top["lat"], lon=top["lon"]).raw_output["results"][0]["housenumber"] == "27"


def test_pydantic_ai(truth):
    pytest.importorskip("pydantic_ai")
    from pydantic_ai import Agent, ModelRetry
    from pydantic_ai.models.test import TestModel

    from gridpin_agents.pydantic_ai import gridpin_tools

    geocode, reverse = gridpin_tools("fr", SHEET)
    assert geocode.name == "gridpin_geocode_fr"
    assert geocode.function(query=ADDRESS, k=1) == truth[0]
    # through an agent: the test model calls every tool once with generated arguments
    agent = Agent(TestModel(call_tools=[geocode.name]), tools=[geocode, reverse])
    result = agent.run_sync("find it")
    from pydantic_ai.messages import ToolReturnPart

    returned = [part for m in result.all_messages() for part in getattr(m, "parts", [])
                if isinstance(part, ToolReturnPart) and part.tool_name == geocode.name]
    assert returned and returned[0].content["country"] == "fr", "the agent called the tool and got its answer"
    broken = gridpin_tools("fr", "/nowhere.bin")[0]
    with pytest.raises(ModelRetry, match="no sheet file"):
        broken.function(query=ADDRESS)


def test_smolagents(truth):
    pytest.importorskip("smolagents")
    from gridpin_agents.smolagents import gridpin_tools

    geocode, reverse = gridpin_tools("fr", SHEET)
    assert geocode.name == "gridpin_geocode_fr"
    assert geocode(query=ADDRESS, k=1) == truth[0]
    top = truth[0]["results"][0]
    assert reverse(lat=top["lat"], lon=top["lon"])["results"][0]["housenumber"] == "27"


def test_openai_agents(truth):
    pytest.importorskip("agents")
    from agents.tool_context import ToolContext

    from gridpin_agents.openai_agents import gridpin_tools

    geocode, reverse = gridpin_tools("fr", SHEET)
    assert geocode.name == "gridpin_geocode_fr"
    assert set(geocode.params_json_schema["properties"]) == {"query", "k"}
    args = json.dumps({"query": ADDRESS, "k": 1})
    ctx = ToolContext(context=None, tool_name=geocode.name, tool_call_id="1", tool_arguments=args)
    out = asyncio.run(geocode.on_invoke_tool(ctx, args))
    assert (json.loads(out) if isinstance(out, str) else out) == truth[0]


def test_crewai(truth):
    pytest.importorskip("crewai.tools")
    from gridpin_agents.crewai import gridpin_tools

    geocode, reverse = gridpin_tools("fr", SHEET)
    assert geocode.name == "gridpin_geocode_fr"
    assert geocode.run(query=ADDRESS, k=1) == truth[0]
