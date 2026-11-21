# gridpin-orchestrators

Offline geocoding steps for Airflow, Dagster and Prefect. Nothing leaves the machine that runs the step: the country
file is local, there is no API key and no network call per address.

One step everywhere: a CSV in, the same CSV with the `gp_*` answer columns out — byte for byte what
`gridpin batch sheet in.csv out.csv --column address` writes — and a receipt of the run:

```python
{"rows": 4, "answered": 2, "weak": 0, "without_address": 1, "seconds": 0.008, "input": "orders.csv",
 "output": "orders_geo.csv", "sheet": "us"}
```

The step is `gridpin.files.geocode_file` from the engine wheel (gridpin 0.10 or later); this package fits it to each
orchestrator.

## Install

```bash
pip install "gridpin-orchestrators[airflow] @ git+https://github.com/gridpin/gridpin@v0.10.0#subdirectory=integrations/orchestrators"
python -c 'import gridpin; print(gridpin.sheets.fetch("us"))'   # the country file, once, verified against the release
```

Use `[dagster]` or `[prefect]` for the others.

## Airflow

```python
from gridpin_orchestrators.airflow import GridPinGeocodeOperator

GridPinGeocodeOperator(task_id="geocode", input_csv="orders.csv", output_csv="orders_geo.csv",
                       sheet="us", column="address")
```

`input_csv`, `output_csv` and `sheet` are templated. The receipt is the task's return value (XCom).

## Dagster

```python
from gridpin_orchestrators.dagster import geocoded_csv_asset

orders_geo = geocoded_csv_asset("orders_geo", input="orders.csv", output="orders_geo.csv", sheet="us")
```

The receipt is the materialization metadata; `deps=` names the upstream assets that make the input.

## Prefect

```python
from prefect import flow
from gridpin_orchestrators.prefect import geocode_csv

@flow
def nightly():
    geocode_csv("orders.csv", "orders_geo.csv", "us", column="address")
```

The receipt is the task result and a table artifact named `gridpin-receipt`.

## Arguments

| Argument | Meaning |
|---|---|
| input, output | CSV paths; the output is written next to itself and renamed at the end, so a failed run leaves no half file |
| sheet | a country code (`us`, downloaded once by `gridpin-fetch`) or the path of a `.bin` file |
| column | the address column, or a list of columns joined with `", "` (default `address`) |
| poi | optional places layer, as in the engine |

A row without an address, or without an answer, keeps its columns and gets empty `gp_*` cells with
`gp_match_status` `U`. A weak answer is marked `gp_weak` `true`: check it before use.

## Tests

`make orchestrators-venv` (one venv per orchestrator: their pins clash) and `make test-orchestrators` run the step for
real in each, on the Monaco smoke sheet, and compare the output with the CLI byte for byte. Checked with Airflow 3.4.0,
Dagster 1.13.30 and Prefect 3.9.3.
