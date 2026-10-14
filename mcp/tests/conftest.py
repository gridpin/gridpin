import sys
from pathlib import Path

# Tests import the package from the source tree; no install step needed.
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
