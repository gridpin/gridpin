"""Real addresses for Faker: `fake.add_provider(GridPinProvider("us", n=1000, seed=1))`.

Faker makes addresses up ("Changchester"); this provider takes them from a downloaded sheet, so a street, its
number, its town and its postcode belong together and the point is on the map. Nothing here needs Faker at import
time: Faker only asks a provider for its public methods. The addresses are public data under the terms of the
sheet's sources (`gridpin meta <sheet>`); they name places, not people.
"""

from __future__ import annotations

import random
from typing import Optional

from ._open import SheetLike, geocoder

__all__ = ["GridPinProvider"]


class GridPinProvider:
    """A Faker provider over `n` real addresses sampled from one sheet.

    country  a country code whose sheet is downloaded, a path to a .bin file, or an open gridpin.Geocoder.
    n        how many addresses to hold; the sample is drawn once, when the provider is made.
    seed     the same seed gives the same addresses in the same order.
    Other keyword arguments narrow the area, as in `gridpin sample`: city, postcode, near, bbox, region.
    """

    def __init__(self, country: SheetLike, n: int = 1000, seed: Optional[int] = None, **area):
        self._rows = geocoder(country).sample(n, seed=seed, **area)
        if not self._rows:
            raise ValueError("no address in this area of the sheet")
        self._random = random.Random(seed)

    def real_address_row(self) -> dict:
        """One address as a dict: lat, lon, street, housenumber, postcode, commune, and region if known."""
        return self._random.choice(self._rows)

    def street_address(self) -> str:
        row = self.real_address_row()
        return " ".join(p for p in (row["housenumber"], row["street"]) if p)

    def city(self) -> str:
        return self.real_address_row()["commune"]

    def postcode(self) -> str:
        return self.real_address_row()["postcode"]

    def address(self) -> str:
        row = self.real_address_row()
        street = " ".join(p for p in (row["housenumber"], row["street"]) if p)
        place = " ".join(p for p in (row["postcode"], row["commune"]) if p)
        return ", ".join(p for p in (street, place) if p)

    def messy(self, address: Optional[str] = None, profile: Optional[dict] = None) -> str:
        """One address as people write it (`gridpin.messy`): a real address of the sample unless one is given,
        distorted by one class drawn with the shares of `profile` (default: equal shares)."""
        from .messy import messy

        text, _ = messy(address if address is not None else self.address(), profile, seed=self._random.random())
        return text

    def latlng(self) -> tuple:
        row = self.real_address_row()
        return (row["lat"], row["lon"])
