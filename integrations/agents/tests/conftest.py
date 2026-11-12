import sys
from pathlib import Path

# Tests import the package and the MCP session from the source tree; no install step needed.
CODE = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
sys.path.insert(0, str(CODE / "mcp" / "src"))
