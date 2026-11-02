"""A geopy-style geocoder over one local sheet, for geopy and GeoPandas: no API key, no network per query.

    from gridpin.geopy import GridPin
    GridPin("us").geocode("1600 Pennsylvania Ave NW, Washington, DC")     # geopy Location
    geopandas.tools.geocode(df.address, provider=GridPin, country="us")

Needs geopy (`pip install "gridpin[geopy]"`). The sheet is found as in gridpin.sheets: a country
code whose sheet is downloaded (gridpin.sheets.fetch("us")), or `sheet=` a path to a .bin file.
"""

from __future__ import annotations

from typing import Optional

try:
    from geopy.location import Location
    from geopy.point import Point
except ImportError as e:  # pragma: no cover - exercised without geopy installed
    raise ImportError('gridpin.geopy needs geopy: pip install "gridpin[geopy]"') from e

from ._open import geocoder

__all__ = ["GridPin"]


class GridPin:
    """A geopy-style geocoder over one local GridPin sheet: no API key, no network per query."""

    def __init__(self, country: Optional[str] = None, sheet=None, poi=None):
        if country is None and sheet is None:
            raise ValueError('name a country ("us") or pass sheet= a path to a .bin file')
        self.engine = geocoder(sheet if sheet is not None else country, poi)

    def geocode(self, query, exactly_one=True, limit=10):
        return self._places(self.engine.geocode(query, 1 if exactly_one else limit), exactly_one)

    def reverse(self, query, exactly_one=True, limit=10):
        point = Point(query)  # (lat, lon), "lat, lon" or a geopy Point
        hits = self.engine.reverse(point.latitude, point.longitude, 1 if exactly_one else limit)
        return self._places(hits, exactly_one)

    @staticmethod
    def _places(hits, exactly_one):
        places = [Location(_address(h), (h["lat"], h["lon"]), h) for h in hits]
        if not places:
            return None
        return places[0] if exactly_one else places


def _address(hit):
    street = " ".join(p for p in (hit.get("housenumber"), hit["street"]) if p)
    return ", ".join(p for p in (street, hit["commune"], hit.get("region"), hit["postcode"]) if p)
