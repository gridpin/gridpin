#!/usr/bin/env python3
"""Small, local Photon-compatible HTTP facade for a GridPin sheet.

This is deliberately a loopback development adapter, not an Internet-facing
service.  It has no dependencies beyond Python's standard library and invokes
the GridPin CLI for each request:

  python3 examples/gridpin_http.py --bin ./gridpin --index ./france.bin --country FR

It implements the useful Photon forward/reverse-geocoding subset:
  GET /health
  GET /api/?q=<address>&limit=<1..100>[&lang=<BCP47-ish tag>]
      [&lat=<focus latitude>&lon=<focus longitude>]
  GET /reverse?lat=<latitude>&lon=<longitude>[&limit=<1..100>]
      [&lang=<BCP47-ish tag>][&query_string_filter=osm_key:highway]

``lat``/``lon`` are a focus point handled by two complementary layers.  The
facade asks for a bounded wider pool and re-ranks it by great-circle distance,
while GridPin's ``query --near`` adds same-name candidates from the local
spatial grid when the global prefix cap omitted them.  The facade only reorders
candidates returned by the engine; it never invents a result.
When GridPin resolves a house, the CLI returns the matched ``housenumber``
(including its suffix); the facade forwards that real component.
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import re
import subprocess
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MAX_QUERY_BYTES = 4096
MAX_LIMIT = 100
DEFAULT_LIMIT = 10
# Candidates pulled from the CLI when a focus point is given. A cityless query
# ("25 Rue du Presbytère") matches the same street name in many communes, and
# the wanted one is rarely first; this is the bounded pool the focus reorders.
FOCUS_CANDIDATES = MAX_LIMIT
DEFAULT_TIMEOUT_SECONDS = 10.0
DEFAULT_MAX_INFLIGHT = 8
LANG_RE = re.compile(r"[A-Za-z]{2,8}(?:-[A-Za-z0-9]{1,8})*")
COUNTRY_RE = re.compile(r"[A-Za-z]{2}")
COUNTRY_NAMES = {
    "FR": "France",
    "IT": "Italy",
    "MC": "Monaco",
    "NL": "Netherlands",
    "RS": "Serbia",
}


class AdapterError(ValueError):
    """A client-visible validation failure."""


class AdapterState:
    def __init__(self, binary: pathlib.Path, index: pathlib.Path, country: str,
                 timeout_seconds: float, max_inflight: int):
        if not binary.is_file():
            raise ValueError(f"gridpin binary is not a file: {binary}")
        if not index.is_file():
            raise ValueError(f"index is not a file: {index}")
        if not COUNTRY_RE.fullmatch(country):
            raise ValueError("country must be a two-letter code")
        country_code = country.upper()
        if country_code not in COUNTRY_NAMES:
            supported = ", ".join(sorted(COUNTRY_NAMES))
            raise ValueError(f"country must be one of: {supported}")
        if not math.isfinite(timeout_seconds) or timeout_seconds <= 0:
            raise ValueError("timeout must be positive")
        if not 1 <= max_inflight <= 128:
            raise ValueError("max-inflight must be between 1 and 128")
        self.binary = binary
        self.index = index
        self.country_code = country_code
        self.country_name = COUNTRY_NAMES[country_code]
        self.timeout_seconds = timeout_seconds
        self.permits = threading.BoundedSemaphore(max_inflight)

    def _run_cli(self, arguments: list[str]) -> list[dict]:
        if not self.permits.acquire(blocking=False):
            raise RuntimeError("busy")
        try:
            try:
                proc = subprocess.run(
                    [str(self.binary), *arguments],
                    capture_output=True,
                    text=True,
                    timeout=self.timeout_seconds,
                    check=False,
                )
            except subprocess.TimeoutExpired as exc:
                raise RuntimeError("timeout") from exc
            except (OSError, ValueError) as exc:
                raise RuntimeError("unavailable") from exc
            if proc.returncode != 0:
                raise RuntimeError("failed")
            hits: list[dict] = []
            for line in proc.stdout.splitlines():
                if not line.strip():
                    continue
                try:
                    hit = json.loads(line)
                except json.JSONDecodeError as exc:
                    raise RuntimeError("invalid response") from exc
                if not isinstance(hit, dict):
                    raise RuntimeError("invalid response")
                hits.append(hit)
            return hits
        finally:
            self.permits.release()

    def geocode(self, query: str, limit: int,
                focus: tuple[float, float] | None = None) -> list[dict]:
        arguments = ["query", str(self.index), query, "-k", str(limit)]
        if focus is not None:
            arguments.extend(["--near", f"{focus[0]},{focus[1]}"])
        return self._run_cli(arguments)

    def reverse(self, lat: float, lon: float, limit: int) -> list[dict]:
        return self._run_cli(
            ["reverse", str(self.index), str(lat), str(lon), "-k", str(limit)]
        )


def _one(params: dict[str, list[str]], key: str, *, required: bool = False) -> str | None:
    values = params.get(key, [])
    if len(values) > 1:
        raise AdapterError(f"parameter {key!r} must occur once")
    if not values:
        if required:
            raise AdapterError(f"missing parameter {key!r}")
        return None
    return values[0]


def _coordinate(raw: str, key: str, minimum: float, maximum: float) -> float:
    try:
        value = float(raw)
    except (TypeError, ValueError) as exc:
        raise AdapterError(f"parameter {key!r} must be a number") from exc
    if not math.isfinite(value) or not minimum <= value <= maximum:
        raise AdapterError(f"parameter {key!r} must be between {minimum:g} and {maximum:g}")
    return value


def _parameters(raw_query: str, allowed: set[str]) -> dict[str, list[str]]:
    try:
        params = urllib.parse.parse_qs(raw_query, keep_blank_values=True, strict_parsing=True)
    except ValueError as exc:
        raise AdapterError("malformed query string") from exc
    unknown = set(params) - allowed
    if unknown:
        raise AdapterError(f"unsupported parameter {sorted(unknown)[0]!r}")
    return params


def _limit_and_lang(params: dict[str, list[str]]) -> int:
    limit_raw = _one(params, "limit")
    if limit_raw is None:
        limit = DEFAULT_LIMIT
    elif not limit_raw.isascii() or not limit_raw.isdecimal():
        raise AdapterError("parameter 'limit' must be an integer")
    else:
        limit = int(limit_raw)
        if not 1 <= limit <= MAX_LIMIT:
            raise AdapterError(f"parameter 'limit' must be between 1 and {MAX_LIMIT}")
    lang = _one(params, "lang")
    if lang is not None and not LANG_RE.fullmatch(lang):
        raise AdapterError("parameter 'lang' is invalid")
    return limit


def parse_api_query(raw_query: str) -> tuple[str, int, tuple[float, float] | None]:
    params = _parameters(raw_query, {"q", "limit", "lang", "lat", "lon"})
    query = _one(params, "q", required=True)
    assert query is not None
    if not query.strip():
        raise AdapterError("parameter 'q' must not be empty")
    if any(ord(character) < 32 or ord(character) == 127 for character in query):
        raise AdapterError("parameter 'q' contains a control character")
    if len(query.encode("utf-8")) > MAX_QUERY_BYTES:
        raise AdapterError("parameter 'q' is too long")
    limit = _limit_and_lang(params)
    lat_raw = _one(params, "lat")
    lon_raw = _one(params, "lon")
    if (lat_raw is None) != (lon_raw is None):
        raise AdapterError("parameters 'lat' and 'lon' must occur together")
    focus: tuple[float, float] | None = None
    if lat_raw is not None and lon_raw is not None:
        # Photon treats these as a location bias. The same validated pair is
        # passed to the engine for candidate injection and used by the facade
        # to restore the shipped distance ordering of the returned pool.
        focus = (
            _coordinate(lat_raw, "lat", -90.0, 90.0),
            _coordinate(lon_raw, "lon", -180.0, 180.0),
        )
    return query, limit, focus


def parse_reverse_query(raw_query: str) -> tuple[float, float, int]:
    params = _parameters(
        raw_query, {"lat", "lon", "limit", "lang", "query_string_filter"}
    )
    lat_raw = _one(params, "lat", required=True)
    lon_raw = _one(params, "lon", required=True)
    assert lat_raw is not None and lon_raw is not None
    lat = _coordinate(lat_raw, "lat", -90.0, 90.0)
    lon = _coordinate(lon_raw, "lon", -180.0, 180.0)
    limit = _limit_and_lang(params)
    query_filter = _one(params, "query_string_filter")
    if query_filter is not None and query_filter != "osm_key:highway":
        raise AdapterError("parameter 'query_string_filter' is unsupported")
    return lat, lon, limit


def _distance_m(lat1: float, lon1: float, lat2: float, lon2: float) -> float:
    """Great-circle distance in metres (haversine)."""
    radius = 6371008.8
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp = p2 - p1
    dl = math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * radius * math.asin(math.sqrt(min(1.0, a)))


def rank_by_focus(hits: list[dict], focus: tuple[float, float]) -> list[dict]:
    """Order candidates by distance to the focus point, stably.

    Only reorders what the engine returned. A candidate whose coordinate is
    missing or unusable sinks to the end rather than raising here.
    """
    focus_lat, focus_lon = focus

    def key(item: tuple[int, dict]) -> tuple[float, int]:
        index, hit = item
        try:
            lat = float(hit["lat"])
            lon = float(hit["lon"])
        except (KeyError, TypeError, ValueError):
            return (math.inf, index)
        if not math.isfinite(lat) or not math.isfinite(lon):
            return (math.inf, index)
        return (_distance_m(focus_lat, focus_lon, lat, lon), index)

    return [hit for _, hit in sorted(enumerate(hits), key=key)]


def photon_feature(hit: dict, country_code: str, country_name: str) -> dict:
    try:
        lat = float(hit["lat"])
        lon = float(hit["lon"])
    except (KeyError, TypeError, ValueError) as exc:
        raise RuntimeError("invalid response") from exc
    if not math.isfinite(lat) or not math.isfinite(lon) or not -90 <= lat <= 90 or not -180 <= lon <= 180:
        raise RuntimeError("invalid response")
    street = hit.get("street")
    commune = hit.get("commune")
    props = {
        "name": street or commune or "",
        "country": country_name,
        "countrycode": country_code,
    }
    # Only expose address components present in the real GridPin CLI result.
    for photon_key, gridpin_key in (
        ("city", "commune"),
        ("street", "street"),
        ("housenumber", "housenumber"),
        ("postcode", "postcode"),
        ("type", "precision"),
    ):
        value = hit.get(gridpin_key)
        if value not in (None, ""):
            props[photon_key] = value
    return {"type": "Feature", "properties": props,
            "geometry": {"type": "Point", "coordinates": [lon, lat]}}


def make_handler(state: AdapterState):
    class Handler(BaseHTTPRequestHandler):
        server_version = "GridPinPhoton/0.1"

        def log_message(self, _format: str, *_args: object) -> None:
            pass

        def send_json(self, status: int, payload: object) -> None:
            body = json.dumps(payload, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self) -> None:
            parsed = urllib.parse.urlsplit(self.path)
            if parsed.path == "/health":
                if parsed.query:
                    self.send_json(400, {"error": "health takes no parameters"})
                else:
                    self.send_json(200, {"ok": True})
                return
            if parsed.path not in {"/api", "/api/", "/reverse"}:
                self.send_json(404, {"error": "not found"})
                return
            try:
                if parsed.path == "/reverse":
                    lat, lon, limit = parse_reverse_query(parsed.query)
                    hits = state.reverse(lat, lon, limit)
                else:
                    query, limit, focus = parse_api_query(parsed.query)
                    # Keep the shipped wide-pool distance ordering, but pass the
                    # point through so the engine can add local same-name streets
                    # that the global 300-row prefix cap omitted.
                    fetch = max(limit, FOCUS_CANDIDATES) if focus else limit
                    hits = state.geocode(query, fetch, focus)
                    if focus:
                        hits = rank_by_focus(hits, focus)
                features = [
                    photon_feature(hit, state.country_code, state.country_name)
                    for hit in hits[:limit]
                ]
            except AdapterError as exc:
                self.send_json(400, {"error": str(exc)})
                return
            except RuntimeError as exc:
                status = 503 if str(exc) in {"busy", "timeout"} else 502
                self.send_json(status, {"error": "geocoder unavailable"})
                return
            self.send_json(200, {"type": "FeatureCollection", "features": features})

        def do_POST(self) -> None:
            self.send_json(405, {"error": "method not allowed"})

        def do_PUT(self) -> None:
            self.do_POST()

        def do_DELETE(self) -> None:
            self.do_POST()

    return Handler


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="loopback Photon facade for a GridPin sheet")
    parser.add_argument("--bin", required=True, type=pathlib.Path, dest="binary")
    parser.add_argument("--index", required=True, type=pathlib.Path)
    parser.add_argument("--country", required=True, help="two-letter sheet country code")
    parser.add_argument("--host", default="127.0.0.1", help="default is loopback only")
    parser.add_argument("--port", default=2322, type=int)
    parser.add_argument("--timeout", default=DEFAULT_TIMEOUT_SECONDS, type=float)
    parser.add_argument("--max-inflight", default=DEFAULT_MAX_INFLIGHT, type=int)
    return parser


def main() -> None:
    args = build_parser().parse_args()
    if not 1 <= args.port <= 65535:
        raise SystemExit("port must be between 1 and 65535")
    try:
        state = AdapterState(args.binary, args.index, args.country, args.timeout, args.max_inflight)
    except ValueError as exc:
        raise SystemExit(str(exc)) from exc
    httpd = ThreadingHTTPServer((args.host, args.port), make_handler(state))
    print(f"GridPin Photon adapter: http://{args.host}:{args.port}/api/", flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        httpd.server_close()


if __name__ == "__main__":
    main()
