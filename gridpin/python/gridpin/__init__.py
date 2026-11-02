"""GridPin: offline forward and reverse geocoding over one country file ("sheet").

    import gridpin
    path = gridpin.sheets.fetch("us")            # once: downloaded and checked against the release
    g = gridpin.Geocoder(path)
    g.geocode("1600 Pennsylvania Ave NW, Washington, DC", 1)

Helpers that need other packages load only when used:

    gridpin.geocode_df(df, "address", "us")      # pandas: pip install "gridpin[pandas]"
    from gridpin.geopy import GridPin            # geopy/GeoPandas: pip install "gridpin[geopy]"
"""

from . import sheets
from ._gridpin import Geocoder

__all__ = ["Geocoder", "sheets", "geocode_df", "reverse_df"]


def __getattr__(name):
    # The DataFrame helpers live in gridpin.pandas, imported on first use so `import gridpin`
    # neither needs pandas nor pays for importing it.
    if name in ("geocode_df", "reverse_df"):
        from . import pandas as _pandas

        return getattr(_pandas, name)
    raise AttributeError(f"module 'gridpin' has no attribute {name!r}")
