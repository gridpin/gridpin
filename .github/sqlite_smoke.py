"""Smoke of a packaged SQLite extension: it loads into Python's sqlite3, registers its functions, and the engine
inside it answers — opening a sheet that does not exist must fail with the engine's own message.

    python3 .github/sqlite_smoke.py <folder with gridpin_sqlite.so|.dylib|.dll>
"""
import sqlite3
import sys
from pathlib import Path

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
