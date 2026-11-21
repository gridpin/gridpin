"""A Prefect task: geocode a CSV file into another one; the receipt is the task's result and a table artifact."""
from __future__ import annotations

from typing import Any, Dict, Optional, Sequence, Union

from gridpin.files import geocode_file
from prefect import task
from prefect.artifacts import create_table_artifact

__all__ = ["geocode_csv"]


@task(name="gridpin-geocode-csv")
def geocode_csv(input: str, output: str, sheet: str, column: Union[str, Sequence[str]] = "address",
                poi: Optional[str] = None) -> Dict[str, Any]:
    receipt = geocode_file(input, output, sheet, column=column, poi=poi)
    create_table_artifact(key="gridpin-receipt", table=[receipt], description=f"{input} geocoded on {sheet}")
    return receipt
