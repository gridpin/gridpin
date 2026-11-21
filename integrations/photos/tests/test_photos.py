"""gridpin-photos on photos built here byte by byte (a JPEG with an EXIF GPS block), against the Monaco smoke sheet.
The photos must come out of a run byte for byte as they went in."""
import csv
import hashlib
import struct
from pathlib import Path

import pytest

gridpin = pytest.importorskip("gridpin")
pytest.importorskip("exifread")
import gridpin_photos as gp  # noqa: E402

SHEET = Path(__file__).resolve().parents[3] / "data" / "mc.bin"


def _rational(x: float) -> bytes:
    return struct.pack(">II", int(round(x * 1_000_000)), 1_000_000)


def _dms(value: float) -> bytes:
    v = abs(value)
    d = int(v)
    m = int((v - d) * 60)
    s = (v - d - m / 60) * 3600
    return _rational(d) + _rational(m) + _rational(s)


def jpeg(lat=None, lon=None) -> bytes:
    """A minimal JPEG: SOI, an APP1 Exif segment (with a GPS IFD when lat/lon are given), EOI."""
    if lat is None:
        tiff = b"MM\x00\x2a" + struct.pack(">I", 8) + struct.pack(">H", 0) + struct.pack(">I", 0)
    else:
        ifd0 = 8
        gps = ifd0 + 2 + 12 + 4
        data = gps + 2 + 4 * 12 + 4
        entries = [(1, 2, 2, (b"N" if lat >= 0 else b"S") + b"\x00\x00\x00"),
                   (2, 5, 3, struct.pack(">I", data)),
                   (3, 2, 2, (b"E" if lon >= 0 else b"W") + b"\x00\x00\x00"),
                   (4, 5, 3, struct.pack(">I", data + 24))]
        tiff = b"MM\x00\x2a" + struct.pack(">I", ifd0)
        tiff += struct.pack(">H", 1) + struct.pack(">HHI", 0x8825, 4, 1) + struct.pack(">I", gps) + struct.pack(">I", 0)
        tiff += struct.pack(">H", len(entries))
        for tag, kind, count, value in entries:
            tiff += struct.pack(">HHI", tag, kind, count) + value
        tiff += struct.pack(">I", 0) + _dms(lat) + _dms(lon)
    app1 = b"Exif\x00\x00" + tiff
    return b"\xff\xd8" + b"\xff\xe1" + struct.pack(">H", len(app1) + 2) + app1 + b"\xff\xd9"


@pytest.fixture
def folder(tmp_path):
    if not SHEET.exists():
        pytest.skip(f"no test sheet at {SHEET} (make mc)")
    (tmp_path / "house.jpg").write_bytes(jpeg(43.74366, 7.42848))       # boulevard des Moulins 27
    (tmp_path / "sea.jpg").write_bytes(jpeg(43.70, 7.50))                 # 6 km out at sea
    (tmp_path / "none.jpg").write_bytes(jpeg())
    (tmp_path / "zero.jpg").write_bytes(jpeg(0.0, 0.0))
    (tmp_path / "broken.jpg").write_bytes(b"\xff\xd8\xff\xe1\x00\x10Exif\x00\x00MM\x00*garbage")
    (tmp_path / "notes.txt").write_text("not a photo")
    return tmp_path


def digests(folder):
    return {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in folder.iterdir() if p.suffix == ".jpg"}


def test_the_point_is_read_with_its_hemisphere():
    import io
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        for lat, lon in ((43.74366, 7.42848), (-22.9068, -43.1729), (40.7484, -73.9857)):
            p = Path(d) / "p.jpg"
            p.write_bytes(jpeg(lat, lon))
            status, point = gp.photo_point(p)
            assert status == "ok" and abs(point[0] - lat) < 1e-5 and abs(point[1] - lon) < 1e-5, (lat, lon, point)


def test_a_run_captions_each_photo_and_never_writes_to_one(folder):
    before = digests(folder)
    counts = gp.run(folder, str(SHEET), xmp=True)
    assert digests(folder) == before
    rows = {r["file"]: r for r in csv.DictReader((folder / "gridpin_places.csv").open())}
    assert set(rows) == {"house.jpg", "sea.jpg", "none.jpg", "zero.jpg", "broken.jpg"}, "only photos, every photo"
    assert rows["house.jpg"]["status"] == "ok" and rows["house.jpg"]["street"] == "Boulevard des Moulins"
    assert rows["house.jpg"]["label"].startswith("27 Boulevard des Moulins, Monaco")
    assert rows["sea.jpg"]["status"] == "no_address" and rows["sea.jpg"]["street"] == ""
    assert rows["none.jpg"]["status"] == "no_geotag" and rows["zero.jpg"]["status"] == "null_island"
    assert rows["broken.jpg"]["status"] == "no_geotag" and rows["broken.jpg"]["label"] == ""
    assert counts["ok"] == 1 and sum(counts.values()) == 5
    assert (folder / "house.jpg.xmp").read_text().count('photoshop:City="Monaco"') == 1
    assert not (folder / "none.jpg.xmp").exists()


def test_a_sidecar_already_there_is_kept(folder):
    (folder / "house.jpg.xmp").write_text("mine")
    gp.run(folder, str(SHEET), xmp=True)
    assert (folder / "house.jpg.xmp").read_text() == "mine"


def test_the_caption_is_the_answer_of_reverse(folder):
    status, (lat, lon) = gp.photo_point(folder / "house.jpg")
    hit = gridpin.Geocoder(str(SHEET)).reverse(lat, lon, 1)[0]
    c = gp.caption(hit)
    assert (c["street"], c["housenumber"], c["place"]) == (hit["street"], hit["housenumber"], hit["commune"])


def test_a_far_or_weak_answer_names_the_place_and_not_the_street():
    hit = {"street": "Rue A", "housenumber": "1", "commune": "Ville", "postcode": "1", "distance_m": 151.0, "weak": False}
    assert gp.caption(hit)["street"] == "" and gp.caption(hit)["status"] == "place_only"
    assert gp.caption(hit)["label"] == "Ville"
    assert gp.caption({**hit, "distance_m": 150.0})["street"] == "Rue A"
    assert gp.caption({**hit, "distance_m": 10.0, "weak": True})["street"] == ""
    assert gp.caption({**hit, "distance_m": 10.0}, max_street_m=5)["status"] == "place_only"
    assert gp.caption(None) == {"status": "no_address"}
