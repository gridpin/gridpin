"""GridPin as a geocoding provider of Odoo's base_geolocalize.

Odoo calls `_call_<tech_name>` with the address line and expects (latitude, longitude) or None. The server is
`gridpin serve <sheet>` on your own machine; its address is the system parameter base_geolocalize_gridpin.url.
"""
import logging

import requests

from odoo import _, api, models
from odoo.exceptions import UserError

_logger = logging.getLogger(__name__)

# countries whose addresses end "city, STATE postcode": the state as its code, the postcode after it
CITY_STATE_ZIP = {"US", "CA", "AU"}


class GeoCoder(models.AbstractModel):
    _inherit = "base.geocoder"

    @api.model
    def _geo_query_address_gridpin(self, street=None, zip=None, city=None, state=None, country=None):
        # A GridPin sheet is one country: the country's name at the end only repeats the city of a city-state
        # ("27 boulevard des Moulins, Monaco, Monaco") and costs the house, so it is left out.
        code = self._gridpin_country_code(country)
        if code in CITY_STATE_ZIP:
            # 09.11.2026: "1600 Pennsylvania Ave NW, 20500 Washington, District of Columbia" got only the city and
            # "350 Fifth Avenue, 10118 New York, New York" nothing; written the way the country writes it,
            # "…, Washington, DC 20500", both find the house.
            tail = " ".join(p for p in (self._gridpin_state_code(state, code) or state, zip) if p)
            return ", ".join(p for p in (street, city, tail) if p)
        # Elsewhere the region after the town costs answers (revision 0.9.0, the same partners with and without it):
        # Serbia "…, 11000 Beograd, Beograd" 133 -> 93 houses within 150 m and weak answers 2 -> 26; Brazil
        # "…, Belo Horizonte, Minas Gerais" 3 -> 9 answers more than 1 km away that would become the partner's
        # point. The region is written only when there is no town to name the place.
        town = " ".join(p for p in (zip, city) if p)
        return ", ".join(p for p in (street, town, None if city else state) if p)

    @api.model
    def _gridpin_country_code(self, country):
        if not country:
            return None
        found = self.env["res.country"].search([("name", "=", country)], limit=1)
        return found.code if found else None

    @api.model
    def _gridpin_state_code(self, state, country_code):
        if not state:
            return None
        found = self.env["res.country.state"].search(
            [("name", "=", state), ("country_id.code", "=", country_code)], limit=1)
        return found.code if found else None

    @api.model
    def _call_gridpin(self, addr, **kw):
        params = self.env["ir.config_parameter"].sudo()
        url = params.get_param("base_geolocalize_gridpin.url", "http://127.0.0.1:8080")
        try:
            timeout = float(params.get_param("base_geolocalize_gridpin.timeout", "30"))
        except (TypeError, ValueError):
            timeout = 30.0
        try:
            r = requests.get(url.rstrip("/") + "/v1/geocode", params={"q": addr, "k": 1}, timeout=timeout)
            r.raise_for_status()
            results = r.json().get("results") or []
        except requests.Timeout as e:
            # a slow answer is not a server that cannot be reached (a busy machine, a large sheet on its first query)
            raise UserError(_("The GridPin server at %s did not answer within %s s; raise the system parameter "
                              "base_geolocalize_gridpin.timeout or try again", url, timeout)) from e
        except (requests.RequestException, ValueError) as e:
            raise UserError(_("Cannot reach the GridPin server at %s: %s", url, e)) from e
        if not results:
            _logger.info("GridPin: no answer for %r", addr)
            return None
        top = results[0]
        if top.get("weak"):
            _logger.info("GridPin: weak answer for %r (%s), not used", addr, top.get("precision"))
            return None
        return float(top["lat"]), float(top["lon"])
