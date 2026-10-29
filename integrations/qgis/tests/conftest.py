import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from qgis.core import QgsApplication  # noqa: E402

_app = QgsApplication([], False)
_app.initQgis()

SHEET = Path(os.environ.get("GRIDPIN_TEST_SHEET", "/sheet/mc.bin"))


@pytest.fixture
def sheet_file():
    if not SHEET.is_file():
        pytest.skip(f"no test sheet at {SHEET}")
    return SHEET


@pytest.fixture
def processing():
    import processing as _processing  # noqa: F401
    from processing.core.Processing import Processing

    from gridpin_geocoder.processing_gridpin import Provider

    Processing.initialize()
    provider = Provider()
    QgsApplication.processingRegistry().addProvider(provider)
    yield _processing
    QgsApplication.processingRegistry().removeProvider(provider)
