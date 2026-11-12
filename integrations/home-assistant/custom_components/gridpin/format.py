"""Pure helpers of the GridPin integration: the request URL and the state text of an answer of /v1/reverse."""
from urllib.parse import urlencode


def reverse_url(server: str, lat: float, lon: float) -> str:
    return f"{server.rstrip('/')}/v1/reverse?{urlencode({'lat': f'{lat:.6f}', 'lon': f'{lon:.6f}', 'k': 1})}"


def state_of(body: dict) -> tuple[str | None, dict]:
    """The sensor's state (the address) and attributes from the server's answer; (None, {}) when there is none."""
    hits = (body or {}).get("results") or []
    if not hits:
        return None, {}
    h = hits[0]
    street = " ".join(x for x in (h.get("housenumber") or "", h.get("street") or "") if x)
    place = " ".join(x for x in (h.get("postcode") or "", h.get("commune") or "") if x)
    state = ", ".join(x for x in (street, place) if x)[:255]
    return state, {"distance_m": h.get("distance_m"), "match_level": h.get("match_level"), "commune": h.get("commune"),
                   "latitude": h.get("lat"), "longitude": h.get("lon")}
