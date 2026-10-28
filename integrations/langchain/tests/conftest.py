"""Shared setup: which packages the tests import, and the smoke sheet they geocode against.

Installed packages win (the release job tests the built wheel); otherwise the tests run against
the source trees of this repository, so no install step is needed.
"""
import os
import sys
from pathlib import Path

import pytest

PACKAGE = Path(__file__).resolve().parents[1]
CODE = PACKAGE.parents[1]
for module, source in (("gridpin_mcp", CODE / "mcp" / "src"), ("langchain_gridpin", PACKAGE / "src")):
    try:
        __import__(module)
    except ImportError:
        sys.path.insert(0, str(source))

# The Monaco smoke sheet (make mc). It is labelled "fr": the tools need a country, the sheet
# path overrides which file is read.
SHEET = Path(os.environ.get("GRIDPIN_TEST_SHEET", CODE / "data" / "mc.bin"))
ADDRESS = "boulevard des moulins 27 monaco"


def require_engine_and_sheet():
    # From the repository root the gridpin/ crate folder is a namespace package named "gridpin";
    # only the engine wheel defines Geocoder.
    if not hasattr(pytest.importorskip("gridpin"), "Geocoder"):
        pytest.skip("gridpin engine wheel not installed", allow_module_level=True)
    if not SHEET.exists():
        pytest.skip(f"no test sheet at {SHEET} (build the Monaco smoke sheet: make mc)",
                    allow_module_level=True)


@pytest.fixture(autouse=True)
def fresh_sessions():
    """Each test opens sheets anew: sessions are cached per sheet for the life of the process."""
    from langchain_gridpin import tools

    tools._sessions.clear()
    yield
    tools._sessions.clear()
