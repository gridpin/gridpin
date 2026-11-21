"""The macro in a real LibreOffice (make test-libreoffice): a throwaway profile, gridpin serve on a free port, soffice
--headless running test/driver.py as a user script. Prints what the driver saw and fails on any difference."""
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
CODE = HERE.parents[2]
SOFFICE = os.environ.get("SOFFICE", "/Applications/LibreOffice.app/Contents/MacOS/soffice")
BIN = CODE / "gridpin" / "target" / "release" / "gridpin"
SHEET = CODE / "data" / "mc.bin"

s = socket.socket()
s.bind(("127.0.0.1", 0))
port = s.getsockname()[1]
s.close()
with tempfile.TemporaryDirectory() as tmp:
    profile = Path(tmp) / "profile"
    env = {**os.environ, "GRIDPIN_SERVER": f"http://127.0.0.1:{port}", "GRIDPIN_TEST_OUT": str(Path(tmp) / "out.json")}
    user = [f"-env:UserInstallation=file://{profile}", "--headless", "--invisible", "--norestore"]
    subprocess.run([SOFFICE, *user, "--terminate_after_init"], check=True, timeout=180)
    scripts = profile / "user" / "Scripts" / "python"
    (scripts / "pythonpath").mkdir(parents=True)
    shutil.copy(HERE.parent / "gridpin_calc.py", scripts / "pythonpath" / "gridpin_calc.py")
    shutil.copy(HERE / "driver.py", scripts / "driver.py")
    serve = subprocess.Popen([str(BIN), "serve", str(SHEET), "--port", str(port)], stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL)
    try:
        for _ in range(100):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=2)
                break
            except OSError:
                time.sleep(0.1)
        subprocess.run([SOFFICE, *user, "vnd.sun.star.script:driver.py$run_check?language=Python&location=user"],
                       env=env, timeout=300)
    finally:
        serve.terminate()
        serve.wait(timeout=30)
    got = json.loads(Path(env["GRIDPIN_TEST_OUT"]).read_text())
print(json.dumps(got, ensure_ascii=False))
assert "error" not in got, got["error"]
assert got["sent_first"] == 3 and got["sent_second"] == 0, "each new address once, then nothing: the answers are kept"
assert got["rows"][0][2] == "27 Boulevard des Moulins, Monaco" and got["rows"][0][3] == "" and abs(got["lat_value"] - 43.7436618) < 1e-6
assert got["rows"][1] == ["", "", "", ""] and got["rows"][2] == ["", "", "", ""], "no address and no answer: empty"
assert got["rows"][3][3] == "weak" and got["rows"][4] == got["rows"][0]
assert got["server_down"] == "#GRIDPIN: server not running at http://127.0.0.1:1"
assert got["cache_hidden"] is True
print("libreoffice macro: as expected")
sys.exit(0)
