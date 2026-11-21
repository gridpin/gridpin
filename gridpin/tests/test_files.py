"""gridpin.files.geocode_file: the CSV it writes is byte for byte what `gridpin batch --column` writes for the same file,
and a failed run leaves no output behind."""
import os
import pathlib
import subprocess

import pytest

gridpin = pytest.importorskip("gridpin")
files = pytest.importorskip("gridpin.files")

CODE = pathlib.Path(__file__).resolve().parents[2]
SHEET = CODE / "data" / "mc.bin"
BIN = CODE / "gridpin" / "target" / "release" / "gridpin"
ROWS = "id,address\n1,boulevard des moulins 27 monaco\n2,\n3,boulevard des moulins 27 monaco\n4,zzzz qqqq\n5,\"x, y\"\n"


@pytest.fixture
def sheet():
    if not SHEET.exists():
        pytest.skip(f"no test sheet at {SHEET} (make mc)")
    return str(SHEET)


def test_the_file_is_what_the_cli_writes(tmp_path, sheet):
    if not BIN.exists():
        pytest.skip("no CLI built (make engine)")
    src = tmp_path / "in.csv"
    src.write_text(ROWS)
    receipt = files.geocode_file(src, tmp_path / "py.csv", sheet)
    subprocess.run([str(BIN), "batch", sheet, str(src), str(tmp_path / "cli.csv"), "--column", "address"],
                   check=True, capture_output=True)
    assert (tmp_path / "py.csv").read_bytes() == (tmp_path / "cli.csv").read_bytes()
    assert receipt["rows"] == 5 and receipt["without_address"] == 1 and receipt["answered"] == 2


def test_several_columns_are_joined_like_the_cli(tmp_path, sheet):
    src = tmp_path / "in.csv"
    src.write_text("street,town\nboulevard des moulins 27,monaco\n")
    files.geocode_file(src, tmp_path / "out.csv", sheet, column=["street", "town"])
    one = tmp_path / "one.csv"
    one.write_text("address\n\"boulevard des moulins 27, monaco\"\n")
    files.geocode_file(one, tmp_path / "one_out.csv", sheet)
    tail = lambda p: p.read_text().splitlines()[1].split(",")[-12:]  # noqa: E731
    assert tail(tmp_path / "out.csv") == tail(tmp_path / "one_out.csv")


def test_numbers_are_written_in_the_shortest_form_of_the_engine():
    assert files._number(7.0) == "7" and files._number(43.7436618) == "43.7436618" and files._number(1e-05) == "0.00001"
    # the confidence is a 32-bit float in the engine: 0.84 arrives as 0.8399999737739563
    assert files._number32(0.8399999737739563) == "0.84" and files._number32(0.4000000059604645) == "0.4"
    assert files._number32(1.0) == "1"


def test_a_failed_run_leaves_no_file(tmp_path, sheet):
    src = tmp_path / "in.csv"
    src.write_text("id,gp_lat\n1,2\n")
    out = tmp_path / "out.csv"
    with pytest.raises(ValueError, match="already has a 'gp_lat' column"):
        files.geocode_file(src, out, sheet, column="id")
    src.write_text("id,address\n1,x\n")
    with pytest.raises(KeyError, match="no column 'street'"):
        files.geocode_file(src, out, sheet, column="street")
    with pytest.raises(ValueError, match="same file"):
        files.geocode_file(src, src, sheet)
    (tmp_path / "empty.csv").write_text("")
    with pytest.raises(ValueError, match="is empty"):
        files.geocode_file(tmp_path / "empty.csv", out, sheet)
    assert sorted(os.listdir(tmp_path)) == ["empty.csv", "in.csv"]


def test_a_row_is_asked_as_its_cells_joined_with_a_comma():
    assert files._line(["10 Main St", " Springfield ", "", "62704"], [0, 1, 2, 3]) == "10 Main St, Springfield, 62704"
    assert files._line(["a"], [0, 3]) == "a"


def test_a_failure_while_writing_leaves_neither_the_output_nor_a_part_file(tmp_path, monkeypatch):
    class Broken:
        def geocode_many(self, *a, **k):
            raise RuntimeError("engine stopped")

    monkeypatch.setattr(files, "geocoder", lambda *a, **k: Broken())
    src = tmp_path / "in.csv"
    src.write_text("address\nx\n")
    with pytest.raises(RuntimeError, match="engine stopped"):
        files.geocode_file(src, tmp_path / "out.csv", "unused")
    assert sorted(os.listdir(tmp_path)) == ["in.csv"]
