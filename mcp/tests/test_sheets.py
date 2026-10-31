"""Sheet resolution, cache location and verified downloads, with a fake network."""
import hashlib
import io
from collections import namedtuple

import pytest

from gridpin_mcp import sheets

PAYLOAD = b"gridpin-sheet-bytes" * 1000


class FakeResponse(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()


@pytest.fixture
def home(tmp_path, monkeypatch):
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path))
    return tmp_path


@pytest.fixture
def tiny(monkeypatch):
    """Replace the French sheet pin with a small payload of known size and hash."""
    sheet = sheets.Sheet("fr", "France", "france.bin", len(PAYLOAD), hashlib.sha256(PAYLOAD).hexdigest())
    monkeypatch.setitem(sheets.SHEETS, "fr", sheet)
    return sheet


def opener_for(payload, calls=None):
    def opener(url):
        if calls is not None:
            calls.append(url)
        return FakeResponse(payload)
    return opener


def test_resolve_accepts_code_and_english_name_in_any_case():
    assert sheets.resolve("fr").file == "france.bin"
    assert sheets.resolve(" FR ").code == "fr"
    assert sheets.resolve("Netherlands").code == "nl"


def test_resolve_unknown_country_lists_what_is_available():
    with pytest.raises(sheets.SheetError) as err:
        sheets.resolve("pl")
    assert "unknown country 'pl'" in str(err.value)
    assert all(code in str(err.value) for code in sheets.SHEETS)


def test_every_pin_is_a_full_sha256_and_url_points_to_the_release():
    assert len(sheets.SHEETS) == 9
    for s in sheets.SHEETS.values():
        assert len(s.sha256) == 64 and int(s.sha256, 16) >= 0
        assert s.url == f"https://dl.gridpin.dev/{sheets.RELEASE}/{s.file}"


def test_home_prefers_gridpin_home_then_xdg(tmp_path, monkeypatch):
    monkeypatch.delenv("GRIDPIN_HOME", raising=False)
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path / "xdg"))
    assert sheets.home() == tmp_path / "xdg" / "gridpin"
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path / "own"))
    assert sheets.home() == tmp_path / "own"


def test_missing_sheet_error_names_the_fetch_command(home):
    with pytest.raises(sheets.SheetError) as err:
        sheets.sheet_path("de")
    assert "`gridpin-fetch de`" in str(err.value) and "download_sheet" in str(err.value)


def test_download_verifies_and_moves_into_place(home, tiny):
    seen = []
    path = sheets.download("France", opener=opener_for(PAYLOAD), progress=lambda d, t: seen.append((d, t)))
    assert path == home / sheets.RELEASE / "france.bin"
    assert path.read_bytes() == PAYLOAD
    assert sheets.sheet_path("fr") == path
    assert seen[-1] == (len(PAYLOAD), len(PAYLOAD))
    assert [p.name for p in path.parent.iterdir()] == ["france.bin"]


def test_already_downloaded_sheet_is_not_fetched_again(home, tiny):
    sheets.download("fr", opener=opener_for(PAYLOAD))
    calls = []
    sheets.download("fr", opener=opener_for(PAYLOAD, calls))
    assert calls == []


@pytest.mark.parametrize("payload, message", [
    (PAYLOAD[:-1], "expected"),                       # truncated
    (PAYLOAD + b"x", "more than"),                     # oversized stream is cut off
    (PAYLOAD[:-1] + b"X", "SHA-256 does not match"),   # same size, one byte changed
])
def test_bad_download_is_discarded(home, tiny, payload, message):
    with pytest.raises(sheets.SheetError) as err:
        sheets.download("fr", opener=opener_for(payload))
    assert message in str(err.value)
    folder = home / sheets.RELEASE
    assert list(folder.iterdir()) == []
    assert not sheets.is_downloaded(tiny)


def test_not_enough_disk_space_stops_before_any_request(home, tiny, monkeypatch):
    Usage = namedtuple("Usage", "total used free")
    monkeypatch.setattr(sheets.shutil, "disk_usage", lambda p: Usage(0, 0, tiny.size + sheets.DISK_MARGIN - 1))
    calls = []
    with pytest.raises(sheets.SheetError) as err:
        sheets.download("fr", opener=opener_for(PAYLOAD, calls))
    assert "not enough disk space" in str(err.value) and "GRIDPIN_HOME" in str(err.value)
    assert calls == []


def test_a_wrong_size_file_in_place_is_not_treated_as_downloaded(home, tiny):
    target = home / sheets.RELEASE / "france.bin"
    target.parent.mkdir(parents=True)
    target.write_bytes(PAYLOAD[:10])
    assert not sheets.is_downloaded(tiny)
    with pytest.raises(sheets.SheetError):
        sheets.sheet_path("fr")
