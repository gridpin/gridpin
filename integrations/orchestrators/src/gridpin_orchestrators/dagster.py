"""A Dagster asset factory: the geocoded CSV is the asset, the receipt is its materialization metadata."""
from __future__ import annotations

from typing import Any, Optional, Sequence, Union

import dagster as dg
from gridpin.files import geocode_file

__all__ = ["geocoded_csv_asset"]


def geocoded_csv_asset(name: str, *, input: str, output: str, sheet: str,
                       column: Union[str, Sequence[str]] = "address", poi: Optional[str] = None,
                       deps: Optional[Sequence[Any]] = None) -> dg.AssetsDefinition:
    """An asset named `name` that writes `output` from `input`; `deps` names the upstream assets that make `input`."""

    @dg.asset(name=name, deps=list(deps or []), description=f"{input} geocoded on {sheet}")
    def _geocoded() -> dg.MaterializeResult:
        receipt = geocode_file(input, output, sheet, column=column, poi=poi)
        return dg.MaterializeResult(metadata={**receipt, "output": dg.MetadataValue.path(output)})

    return _geocoded
