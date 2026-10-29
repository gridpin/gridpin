"""One open geocoder per sheet file, shared by the locator and Processing."""

import threading

_lock = threading.Lock()
_engines = {}

MISSING = (
    "The gridpin Python package is not installed. Install the QPIP plugin and restart QGIS, "
    "or run `pip install \"gridpin>=0.5,<0.6\"` in the QGIS Python environment."
)


def available():
    try:
        import gridpin  # noqa: F401
    except ImportError:
        return False
    return True


def _engine(path):
    import gridpin

    key = str(path)
    with _lock:
        if key not in _engines:
            _engines[key] = (gridpin.Geocoder(key), threading.Lock())
        return _engines[key]


def geocode(path, query, k=1):
    engine, lock = _engine(path)
    with lock:
        return engine.geocode(query, k)


def reverse(path, lat, lon, k=1):
    engine, lock = _engine(path)
    with lock:
        return engine.reverse(lat, lon, k)


def address(hit):
    street = " ".join(p for p in (hit.get("housenumber"), hit.get("street")) if p)
    return ", ".join(p for p in (street, hit.get("commune"), hit.get("region"), hit.get("postcode")) if p)
