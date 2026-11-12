"""GridPin: offline forward and reverse geocoding over one country file ("sheet").

    import gridpin
    path = gridpin.sheets.fetch("us")            # once: downloaded and checked against the release
    g = gridpin.Geocoder(path)
    g.geocode("1600 Pennsylvania Ave NW, Washington, DC", 1)

Helpers that need other packages load only when used:

    gridpin.geocode_df(df, "address", "us")      # pandas: pip install "gridpin[pandas]"

A sheet of one's own places (a campus, sites, depots), asked before the country's sheet:

    gridpin.build_places("places.csv", "own.bin")  # name, lat, lon[, group][, code][, aliases]
    g = gridpin.Geocoder(path, also="own.bin")
    from gridpin.geopy import GridPin            # geopy/GeoPandas: pip install "gridpin[geopy]"
"""

from . import sheets
from ._gridpin import Geocoder

try:  # engine 0.9 and later
    from ._gridpin import build_places
except ImportError:  # pragma: no cover - an older engine wheel under this package
    pass

__all__ = ["Geocoder", "build_places", "sheets", "geocode_df", "reverse_df"]


def __getattr__(name):
    # The DataFrame helpers live in gridpin.pandas, imported on first use so `import gridpin`
    # neither needs pandas nor pays for importing it.
    if name in ("geocode_df", "reverse_df"):
        from . import pandas as _pandas

        return getattr(_pandas, name)
    raise AttributeError(f"module 'gridpin' has no attribute {name!r}")
