"""Offline geocoding steps for Airflow, Dagster and Prefect.

Each module fits one function, `gridpin.files.geocode_file`, to its orchestrator: a CSV in, the same CSV with the
`gp_*` answer columns out, and a receipt of the run (rows, answered, weak, without address, seconds) as the step's
result. Nothing leaves the machine: the sheet is a local file.

    from gridpin_orchestrators.airflow import GridPinGeocodeOperator
    from gridpin_orchestrators.dagster import geocoded_csv_asset
    from gridpin_orchestrators.prefect import geocode_csv
"""
from gridpin.files import GP_COLUMNS, geocode_file

__all__ = ["GP_COLUMNS", "geocode_file"]
