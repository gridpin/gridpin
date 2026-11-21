"""GridPin in Tableau through TabPy: functions deployed to a TabPy server that geocode a column offline.

    python gridpin_tabpy.py --sheet /data/france.bin [--tabpy http://localhost:9004/]

deploys gridpin_lat, gridpin_lon, gridpin_label and gridpin_weak. In Tableau (Analysis > Manage Analytics Extension
Connection: TabPy) a calculated field asks one of them for a whole column at once:

    SCRIPT_REAL("return tabpy.query('gridpin_lat', _arg1)['response']", ATTR([Address]))
    SCRIPT_STR("return tabpy.query('gridpin_label', _arg1)['response']", ATTR([Address]))

The TabPy server needs the gridpin wheel (pip install gridpin) and the sheet on its own disk: nothing leaves it.
"""
from __future__ import annotations

import argparse
from typing import Any, Callable, Dict, List, Optional


def functions(sheet: str) -> Dict[str, Callable[[List[Any]], List[Any]]]:
    """The four functions, each a list of addresses in and one value per address out (None: no answer, or no address)."""

    def best(addresses: List[Any]) -> List[Optional[Dict[str, Any]]]:
        import gridpin  # imported on the TabPy server, where the functions run

        cache = globals().setdefault("_gridpin_open", {})
        g = cache.get(sheet) or cache.setdefault(sheet, gridpin.Geocoder(sheet))
        asked = [str(a).strip() if a is not None else "" for a in addresses]
        found = iter(g.geocode_many([a for a in asked if a], 1))
        return [(next(found) or [None])[0] if a else None for a in asked]

    def lat(addresses: List[Any]) -> List[Optional[float]]:
        return [h["lat"] if h else None for h in best(addresses)]

    def lon(addresses: List[Any]) -> List[Optional[float]]:
        return [h["lon"] if h else None for h in best(addresses)]

    def label(addresses: List[Any]) -> List[Optional[str]]:
        def one(h: Optional[Dict[str, Any]]) -> Optional[str]:
            if not h:
                return None
            street = " ".join(p for p in (h.get("housenumber") or "", h.get("street") or "") if p)
            return ", ".join(p for p in (street, h.get("commune") or "") if p)
        return [one(h) for h in best(addresses)]

    def weak(addresses: List[Any]) -> List[Optional[bool]]:
        return [bool(h.get("weak")) if h else None for h in best(addresses)]

    return {"gridpin_lat": lat, "gridpin_lon": lon, "gridpin_label": label, "gridpin_weak": weak}


DESCRIPTIONS = {
    "gridpin_lat": "Latitude of the best answer (GridPin, offline); None when there is no answer",
    "gridpin_lon": "Longitude of the best answer (GridPin, offline); None when there is no answer",
    "gridpin_label": "House number, street and town of the best answer",
    "gridpin_weak": "True when the answer needs a check before use (town level, low confidence)",
}


def deploy(sheet: str, tabpy: str = "http://localhost:9004/", client: Any = None) -> List[str]:
    """Deploy (or replace) the four functions on a TabPy server; returns their names."""
    if client is None:
        from tabpy.tabpy_tools.client import Client
        client = Client(tabpy)
    for name, fn in functions(sheet).items():
        client.deploy(name, fn, DESCRIPTIONS[name], override=True)
    return list(DESCRIPTIONS)


def main(argv: Optional[List[str]] = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--sheet", required=True, help="the .bin file, as the TabPy server sees its path")
    p.add_argument("--tabpy", default="http://localhost:9004/")
    a = p.parse_args(argv)
    print("deployed:", ", ".join(deploy(a.sheet, a.tabpy)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
