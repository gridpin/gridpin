"""One open Geocoder per sheet file for the helper modules (a sheet opens instantly, but not for free)."""

from __future__ import annotations

import os
import threading
from typing import Dict, Optional, Tuple, Union

from . import sheets
from ._gridpin import Geocoder

SheetLike = Union[str, "os.PathLike[str]", Geocoder]

_lock = threading.Lock()
_open: Dict[Tuple[str, Optional[str]], Geocoder] = {}


def geocoder(sheet: SheetLike, poi: Optional[Union[str, "os.PathLike[str]"]] = None) -> Geocoder:
    """A Geocoder as is, or the Geocoder of a sheet path / country code (opened once per process)."""
    if isinstance(sheet, Geocoder):
        if poi is not None:
            raise ValueError("poi applies when the sheet is opened: pass Geocoder(sheet, poi=...) instead")
        return sheet
    key = (str(sheets.open_path(sheet)), None if poi is None else str(os.fspath(poi)))
    with _lock:
        found = _open.get(key)
        if found is None:
            found = _open[key] = Geocoder(key[0], poi=key[1])
        return found
