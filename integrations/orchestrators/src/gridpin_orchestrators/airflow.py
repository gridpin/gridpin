"""An Airflow operator: geocode a CSV file into another one. The receipt is the task's return value (XCom)."""
from __future__ import annotations

from typing import Any, Dict, Optional, Sequence, Union

from gridpin.files import geocode_file

try:  # Airflow 3
    from airflow.sdk import BaseOperator
except ImportError:  # pragma: no cover - Airflow 2
    from airflow.models import BaseOperator

__all__ = ["GridPinGeocodeOperator"]


class GridPinGeocodeOperator(BaseOperator):  # type: ignore[misc]
    """`input_csv`, `output_csv`: CSV paths (templated; `output` is a name Airflow keeps for itself). `sheet`: a
    country code ("us") or the path of a .bin file. `column`: the address column, or several joined with ", "."""

    template_fields: Sequence[str] = ("input_csv", "output_csv", "sheet")

    def __init__(self, *, input_csv: str, output_csv: str, sheet: str, column: Union[str, Sequence[str]] = "address",
                 poi: Optional[str] = None, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.input_csv, self.output_csv, self.sheet, self.column, self.poi = input_csv, output_csv, sheet, column, poi

    def execute(self, context: Any) -> Dict[str, Any]:
        receipt = geocode_file(self.input_csv, self.output_csv, self.sheet, column=self.column, poi=self.poi)
        self.log.info("gridpin: %(rows)s rows, %(answered)s answered, %(weak)s weak, "
                      "%(without_address)s without an address", receipt)
        return receipt
