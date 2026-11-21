"""Each orchestrator runs the step for real on the Monaco smoke sheet, and the CSV it leaves is byte for byte the one
`gridpin batch --column address` writes. Each test needs its orchestrator installed (one venv per orchestrator:
`make test-orchestrators`); `GRIDPIN_TEST_BIN` names the CLI to compare with."""
import os
import pathlib
import subprocess
import sys

import pytest

CODE = pathlib.Path(__file__).resolve().parents[3]
SHEET = pathlib.Path(os.environ.get("GRIDPIN_TEST_SHEET", CODE / "data" / "mc.bin"))
BIN = pathlib.Path(os.environ.get("GRIDPIN_TEST_BIN", CODE / "gridpin" / "target" / "release" / "gridpin"))
ROWS = "id,address\n1,boulevard des moulins 27 monaco\n2,\n3,boulevard des moulins 27 monaco\n4,zzzz qqqq\n"
RECEIPT = {"rows": 4, "answered": 2, "weak": 0, "without_address": 1}


@pytest.fixture
def table(tmp_path):
    if not SHEET.exists():
        pytest.skip(f"no test sheet at {SHEET} (make mc)")
    src = tmp_path / "in.csv"
    src.write_text(ROWS)
    cli = tmp_path / "cli.csv"
    subprocess.run([str(BIN), "batch", str(SHEET), str(src), str(cli), "--column", "address"], check=True,
                   capture_output=True)
    return src, cli


def counts(receipt):
    return {k: receipt[k] for k in RECEIPT}


def test_prefect_task(table, tmp_path, monkeypatch):
    pytest.importorskip("prefect")
    monkeypatch.setenv("PREFECT_HOME", str(tmp_path / "prefect"))
    from prefect import flow

    from gridpin_orchestrators.prefect import geocode_csv
    src, cli = table

    @flow
    def run():
        return geocode_csv(str(src), str(tmp_path / "out.csv"), str(SHEET))

    assert counts(run()) == RECEIPT
    assert (tmp_path / "out.csv").read_bytes() == cli.read_bytes()


def test_dagster_asset(table, tmp_path, monkeypatch):
    dg = pytest.importorskip("dagster")
    monkeypatch.setenv("DAGSTER_HOME", str(tmp_path))
    from gridpin_orchestrators.dagster import geocoded_csv_asset
    src, cli = table
    asset = geocoded_csv_asset("geocoded", input=str(src), output=str(tmp_path / "out.csv"), sheet=str(SHEET))
    result = dg.materialize([asset])
    assert result.success
    meta = result.asset_materializations_for_node("geocoded")[0].metadata
    assert {k: meta[k].value for k in RECEIPT} == RECEIPT
    assert (tmp_path / "out.csv").read_bytes() == cli.read_bytes()


def test_airflow_operator(table, tmp_path):
    pytest.importorskip("airflow")
    src, cli = table
    dags = tmp_path / "dags"
    dags.mkdir()
    out = tmp_path / "out.csv"
    (dags / "gridpin_test.py").write_text(
        "import datetime\nfrom airflow.sdk import DAG\nfrom gridpin_orchestrators.airflow import GridPinGeocodeOperator\n"
        "with DAG('gridpin_test', start_date=datetime.datetime(2026, 1, 1), schedule=None):\n"
        f"    GridPinGeocodeOperator(task_id='geocode', input_csv={str(src)!r}, output_csv={str(out)!r}, "
        f"sheet={str(SHEET)!r})\n")
    env = {**os.environ, "AIRFLOW_HOME": str(tmp_path / "home"), "AIRFLOW__CORE__DAGS_FOLDER": str(dags),
           "AIRFLOW__CORE__LOAD_EXAMPLES": "False",
           "PYTHONPATH": os.pathsep.join([str(CODE / "integrations" / "orchestrators" / "src"),
                                          os.environ.get("PYTHONPATH", "")])}
    airflow = [sys.executable, "-m", "airflow"]
    subprocess.run([*airflow, "db", "migrate"], env=env, check=True, capture_output=True)
    run = subprocess.run([*airflow, "dags", "test", "gridpin_test"], env=env, capture_output=True, text=True)
    assert run.returncode == 0, run.stderr[-2000:]
    assert "gridpin: 4 rows, 2 answered, 0 weak, 1 without an address" in run.stdout + run.stderr
    assert out.read_bytes() == cli.read_bytes()
