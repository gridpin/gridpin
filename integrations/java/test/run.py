"""The Java wrapper against the Monaco smoke sheet: the failures a caller can meet, then the answers of the CLI for the
same queries, field for field.   python3 integrations/java/test/run.py   (from code/, after make mc and make engine)"""
import json
import pathlib
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parents[1]
CODE = HERE.parents[1]
SHEET = CODE / "data" / "mc.bin"
QUERIES = ["boulevard des moulins 27 monaco", "avenue de la costa 1 monaco", "zzzz qqqq"]

with tempfile.TemporaryDirectory() as tmp:
    out = pathlib.Path(tmp)
    subprocess.run(["sh", str(HERE / "build.sh"), str(out)], check=True)
    lib = next(out.glob("libgridpin.*"))

    def java(*args):
        return subprocess.run(["java", "--enable-native-access=ALL-UNNAMED", "-cp", str(out / "classes"), "Check",
                               str(SHEET), *args], check=True, capture_output=True, text=True,
                              env={"GRIDPIN_LIBRARY": str(lib), "PATH": "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin"})

    errors = java("errors").stdout.splitlines()
    for want in ["empty query: []", "null query: null", "k = 0: IllegalArgumentException: gridpin: k must be at least 1",
                 "after close: IllegalStateException: gridpin: the sheet is closed"]:
        assert want in errors, (want, errors)
    assert any(e.startswith("missing sheet: IllegalStateException: gridpin: gridpin_load: cannot open") for e in errors)
    (out / "q.txt").write_text("".join(q + "\n" for q in QUERIES))
    (out / "q.jsonl").write_text("".join(json.dumps({"q": q}) + "\n" for q in QUERIES))
    java("queries", str(out / "q.txt"), str(out / "java.jsonl"))
    subprocess.run([str(CODE / "gridpin" / "target" / "release" / "gridpin"), "batch", str(SHEET), str(out / "q.jsonl"),
                    str(out / "cli.jsonl"), "-k", "1"], check=True, capture_output=True)
    got = [json.loads(line) for line in (out / "java.jsonl").read_text().splitlines()]
    want = [json.loads(line)["results"] for line in (out / "cli.jsonl").read_text().splitlines()]
    assert got == want and got[0], (got, want)
print("java wrapper: errors as expected; answers equal the CLI")
sys.exit(0)
