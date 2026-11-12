"""Smoke of a packaged SQLite extension: it loads into Python's sqlite3, registers its functions, and the engine
inside it answers — opening a sheet that does not exist must fail with the engine's own message.

    python3 .github/sqlite_smoke.py <folder with gridpin_sqlite.so|.dylib|.dll>
"""
import sqlite3
import sys
from pathlib import Path

if not hasattr(sqlite3.Connection, "enable_load_extension"):
    # The Python of the macOS CI runner is built without extension loading; CI installs sqlean.py there
    # (a drop-in sqlite3 with loading). Anywhere else a missing module is a loud ImportError, not a skip.
    import sqlean as sqlite3  # type: ignore[no-redef]

folder = Path(sys.argv[1]).resolve()
c = sqlite3.connect(":memory:")
c.enable_load_extension(True)
c.load_extension(str(folder / "gridpin_sqlite"))
try:
    c.execute("SELECT gridpin_open('no-such-sheet.bin')").fetchone()
except sqlite3.OperationalError as e:
    assert "cannot open" in str(e), e
    print(f"loaded; the engine answered: {e}")
else:
    sys.exit("::error::gridpin_open accepted a sheet that does not exist")
