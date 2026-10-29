import hashlib
import http.server
import threading
from pathlib import Path

import pytest
from qgis.core import QgsFeature, QgsField, QgsLocatorContext, QgsFeedback, QgsVectorLayer

from gridpin_geocoder import engine, locator, processing_gridpin, sheets


def _table(rows):
    layer = QgsVectorLayer("None?field=id:integer&field=addr:string", "t", "memory")
    provider = layer.dataProvider()
    for i, text in enumerate(rows):
        f = QgsFeature(layer.fields())
        f.setAttributes([i, text])
        provider.addFeature(f)
    return layer


def test_metadata_declares_qt6_and_qpip():
    text = (Path(__file__).parents[1] / "gridpin_geocoder" / "metadata.txt").read_text()
    assert "supportsQt6=True" in text
    assert "plugin_dependencies=qpip" in text
    req = (Path(__file__).parents[1] / "gridpin_geocoder" / "requirements.txt").read_text().split()
    assert req == ["gridpin>=0.5,<0.6"]


def test_geocode_field_keeps_every_row(processing, sheet_file):
    layer = _table(["avenue monaco", "zzzz qqqq xxxx", None])
    out = processing.run("gridpin:geocode", {
        "INPUT": layer, "FIELD": "addr", "SHEET": str(sheet_file), "OUTPUT": "memory:"})
    result = out["OUTPUT"]
    feats = list(result.getFeatures())
    assert [f["id"] for f in feats] == [0, 1, 2]
    assert out["FOUND"] == 1
    hit = feats[0]
    assert abs(hit.geometry().asPoint().y() - 43.738) < 0.01
    assert abs(hit.geometry().asPoint().x() - 7.42) < 0.01
    assert hit["gp_address"].endswith("Monaco")
    assert hit["gp_precision"] == "street"
    assert feats[1].geometry().isNull() and feats[2].geometry().isNull()
    assert result.crs().authid() == "EPSG:4326"


def test_geocode_without_sheet_names_the_download_step(processing, tmp_path, monkeypatch):
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path))
    from qgis.core import QgsProcessingException
    with pytest.raises(QgsProcessingException, match="Download country sheet"):
        processing.run("gridpin:geocode", {
            "INPUT": _table(["x"]), "FIELD": "addr", "COUNTRY": 0, "OUTPUT": "memory:"})


def test_locator_searches_downloaded_sheets(sheet_file, tmp_path, monkeypatch):
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path))
    fake = sheets.Sheet("mc", "Monaco", "mc.bin", sheet_file.stat().st_size, "")
    (tmp_path / sheets.RELEASE).mkdir()
    (tmp_path / sheets.RELEASE / "mc.bin").symlink_to(sheet_file)
    flt = locator.GridPinLocator(available=lambda: [fake])
    got = []
    flt.resultFetched.connect(got.append)
    flt.fetchResults("avenue monaco", QgsLocatorContext(), QgsFeedback())
    assert got and got[0].description.startswith("Monaco")
    assert got[0].displayString.endswith("Monaco")
    assert len(got) <= locator.PER_SHEET
    got.clear()
    flt.fetchResults("av", QgsLocatorContext(), QgsFeedback())
    assert got == []


def test_downloaded_list_checks_size(tmp_path, monkeypatch):
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path))
    sheet = sheets.SHEETS["rs"]
    (tmp_path / sheets.RELEASE).mkdir()
    (tmp_path / sheets.RELEASE / sheet.file).write_bytes(b"short")
    assert locator.downloaded() == []


class _Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a):
        pass


@pytest.fixture
def server(tmp_path):
    root = tmp_path / "srv"
    (root / sheets.RELEASE).mkdir(parents=True)
    handler = lambda *a, **k: _Quiet(*a, directory=str(root), **k)  # noqa: E731
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    yield root, f"http://127.0.0.1:{httpd.server_port}"
    httpd.shutdown()


def _serve(monkeypatch, server, body, pinned):
    root, base = server
    (root / sheets.RELEASE / "xx.bin").write_bytes(body)
    monkeypatch.setattr(sheets, "BASE_URL", base)
    fake = sheets.Sheet("xx", "Testland", "xx.bin", len(pinned), hashlib.sha256(pinned).hexdigest())
    monkeypatch.setitem(sheets.SHEETS, "xx", fake)
    return fake


def test_download_through_qgis_network(server, tmp_path, monkeypatch):
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path / "home"))
    body = b"sheet-bytes" * 1000
    _serve(monkeypatch, server, body, body)
    path = sheets.download("xx", lambda u, p, pr: processing_gridpin.fetch(u, p, pr))
    assert path.read_bytes() == body
    assert sheets.is_downloaded(sheets.SHEETS["xx"])


def test_download_with_wrong_hash_leaves_nothing(server, tmp_path, monkeypatch):
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path / "home"))
    body = b"a" * 4096
    _serve(monkeypatch, server, body, b"b" * 4096)
    with pytest.raises(sheets.SheetError, match="SHA-256"):
        sheets.download("xx", lambda u, p, pr: processing_gridpin.fetch(u, p, pr))
    assert list((tmp_path / "home" / sheets.RELEASE).iterdir()) == []


def test_download_http_error_is_reported(server, tmp_path, monkeypatch):
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path / "home"))
    _serve(monkeypatch, server, b"x", b"x")
    (server[0] / sheets.RELEASE / "xx.bin").unlink()
    with pytest.raises(sheets.SheetError):
        sheets.download("xx", lambda u, p, pr: processing_gridpin.fetch(u, p, pr))
    assert list((tmp_path / "home" / sheets.RELEASE).iterdir()) == []


def test_plugin_registers_and_unloads():
    from qgis.core import QgsApplication

    from gridpin_geocoder.plugin import GridPinPlugin

    class Iface:
        filters = []

        def registerLocatorFilter(self, f):
            self.filters.append(f)

        def deregisterLocatorFilter(self, f):
            self.filters.remove(f)

    iface = Iface()
    plugin = GridPinPlugin(iface)
    plugin.initGui()
    assert QgsApplication.processingRegistry().providerById("gridpin") is not None
    assert len(iface.filters) == 1
    plugin.unload()
    assert QgsApplication.processingRegistry().providerById("gridpin") is None
    assert iface.filters == []


def test_address_format():
    hit = {"housenumber": "5", "street": "Main St", "commune": "Springfield", "region": "IL", "postcode": "62701"}
    assert engine.address(hit) == "5 Main St, Springfield, IL, 62701"


def test_address_field_offers_only_text_fields():
    alg = processing_gridpin.GeocodeField()
    alg.initAlgorithm()
    param = alg.parameterDefinition("FIELD")
    assert int(param.dataType()) == int(processing_gridpin._STRING_FIELD)
    assert int(param.dataType()) != 0  # 0 = any field type
