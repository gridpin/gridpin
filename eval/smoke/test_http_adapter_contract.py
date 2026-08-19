#!/usr/bin/env python3
"""Stdlib contract tests for examples/gridpin_http.py.

Uses a temporary fake GridPin executable and an empty index, so this always
verifies the HTTP boundary without downloads.  When this source checkout also
contains a built GridPin binary and the tiny Monaco sheet, a separate optional
test verifies the mapping from a real CLI hit.
"""

from __future__ import annotations

import http.client
import importlib.util
import json
import pathlib
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer

ROOT = pathlib.Path(__file__).resolve().parents[2]
ADAPTER = ROOT / "examples" / "gridpin_http.py"
spec = importlib.util.spec_from_file_location("gridpin_http", ADAPTER)
assert spec and spec.loader
gridpin_http = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gridpin_http)


class ContractTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.tmp.name)
        self.index = root / "sheet.bin"
        self.index.write_bytes(b"test")
        self.seen = root / "argv.json"
        self.binary = root / "fake-gridpin.py"
        self.binary.write_text(
            "#!/usr/bin/env python3\n"
            "import json, pathlib, sys, time\n"
            f"pathlib.Path({str(self.seen)!r}).write_text(json.dumps(sys.argv[1:]))\n"
            "if sys.argv[1] == 'reverse':\n"
            " print(json.dumps({'lat':48.8567,'lon':2.3523,'street':'Rue Reverse',"
            "'housenumber':'9','postcode':'75002','commune':'Paris','precision':'street',"
            "'distance_m':12.5,'region':'Île-de-France'}))\n"
            "elif sys.argv[1] == 'query':\n"
            " q = sys.argv[3]\n"
            " if q == 'timeout': time.sleep(5)\n"
            " elif q == 'bad-json': print('{')\n"
            " else: print(json.dumps({'lat':48.8566,'lon':2.3522,'street':'Rue Test',"
            "'housenumber':'7bis','postcode':'75001','commune':'Paris','precision':'house'}))\n"
            "else:\n"
            " sys.exit(64)\n",
            encoding="utf-8",
        )
        self.binary.chmod(0o755)
        self.state = gridpin_http.AdapterState(self.binary, self.index, "FR", 2.0, 1)
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), gridpin_http.make_handler(self.state))
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        self.tmp.cleanup()

    def request(self, method: str, path: str):
        conn = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=5)
        conn.request(method, path)
        response = conn.getresponse()
        body = json.loads(response.read())
        headers = dict(response.getheaders())
        conn.close()
        return response.status, headers, body

    def test_health_and_photon_success(self):
        status, headers, body = self.request("GET", "/health")
        self.assertEqual(status, 200)
        self.assertEqual(body, {"ok": True})
        self.assertIn("application/json", headers["Content-Type"])

        status, _, body = self.request(
            "GET", "/api/?q=7%20Rue%20Test&limit=1&lang=fr&lat=48.8&lon=2.3"
        )
        self.assertEqual(status, 200)
        self.assertEqual(body["type"], "FeatureCollection")
        self.assertEqual(len(body["features"]), 1)
        feature = body["features"][0]
        self.assertEqual(feature["geometry"], {"type": "Point", "coordinates": [2.3522, 48.8566]})
        self.assertEqual(feature["properties"]["country"], "France")
        self.assertEqual(feature["properties"]["countrycode"], "FR")
        self.assertEqual(feature["properties"]["housenumber"], "7bis")

        status, _, body = self.request("GET", "/api?q=7%20Rue%20Test&limit=1")
        self.assertEqual(status, 200)
        self.assertEqual(len(body["features"]), 1)

    def test_malformed_method_path_timeout_and_bad_upstream(self):
        for path in ("/api/", "/api/?q=", "/api/?q=%00x", "/api/?q=x&limit=0", "/api/?q=x&limit=101",
                     "/api/?q=x&limit=abc", "/api/?q=x&extra=1", "/api/?q=x&q=y",
                     "/api/?q=x&lat=48", "/api/?q=x&lon=2",
                     "/api/?q=x&lat=nan&lon=2", "/api/?q=x&lat=91&lon=2",
                     "/api/?q=x&lat=48&lon=181", "/api/?q=x&lat=48&lat=49&lon=2"):
            status, _, body = self.request("GET", path)
            self.assertEqual(status, 400, path)
            self.assertIn("error", body)
        status, _, _ = self.request("GET", "/nope")
        self.assertEqual(status, 404)
        status, _, _ = self.request("POST", "/api/?q=x")
        self.assertEqual(status, 405)
        status, _, body = self.request("GET", "/api/?q=timeout")
        self.assertEqual(status, 503)
        self.assertEqual(body, {"error": "geocoder unavailable"})
        status, _, _ = self.request("GET", "/api/?q=bad-json")
        self.assertEqual(status, 502)

    def test_concurrency_is_bounded(self):
        # Occupy the only permit directly, then the HTTP request must reject promptly rather
        # than queue an unbounded number of ThreadingHTTPServer workers.
        self.assertTrue(self.state.permits.acquire(blocking=False))
        try:
            status, _, body = self.request("GET", "/api/?q=x")
            self.assertEqual(status, 503)
            self.assertEqual(body, {"error": "geocoder unavailable"})
        finally:
            self.state.permits.release()

    def test_reverse_uses_the_same_concurrency_bound(self):
        self.assertTrue(self.state.permits.acquire(blocking=False))
        try:
            status, _, body = self.request("GET", "/reverse?lat=48&lon=2")
            self.assertEqual(status, 503)
            self.assertEqual(body, {"error": "geocoder unavailable"})
        finally:
            self.state.permits.release()

    def test_reverse_photon_success_and_cli_contract(self):
        status, _, body = self.request(
            "GET",
            "/reverse?lat=48.8566&lon=2.3522&limit=2&lang=fr"
            "&query_string_filter=osm_key%3Ahighway",
        )
        self.assertEqual(status, 200)
        self.assertEqual(body["type"], "FeatureCollection")
        self.assertEqual(len(body["features"]), 1)
        feature = body["features"][0]
        self.assertEqual(feature["geometry"],
                         {"type": "Point", "coordinates": [2.3523, 48.8567]})
        self.assertEqual(feature["properties"]["city"], "Paris")
        self.assertEqual(feature["properties"]["street"], "Rue Reverse")
        self.assertEqual(feature["properties"]["housenumber"], "9")
        self.assertEqual(feature["properties"]["type"], "street")
        self.assertNotIn("distance_m", feature["properties"])
        self.assertNotIn("region", feature["properties"])
        self.assertEqual(
            json.loads(self.seen.read_text()),
            ["reverse", str(self.index), "48.8566", "2.3522", "-k", "2"],
        )

    def test_reverse_uses_forward_default_limit_when_omitted(self):
        status, _, body = self.request("GET", "/reverse?lat=48&lon=2")
        self.assertEqual(status, 200)
        self.assertEqual(body["type"], "FeatureCollection")
        self.assertEqual(
            json.loads(self.seen.read_text()),
            ["reverse", str(self.index), "48.0", "2.0", "-k", "10"],
        )

    def test_reverse_is_fail_closed_and_exact_path_only(self):
        bad_paths = (
            "/reverse",
            "/reverse?lat=48",
            "/reverse?lon=2",
            "/reverse?lat=nan&lon=2",
            "/reverse?lat=inf&lon=2",
            "/reverse?lat=91&lon=2",
            "/reverse?lat=48&lon=181",
            "/reverse?lat=48&lat=49&lon=2",
            "/reverse?lat=48&lon=2&limit=0",
            "/reverse?lat=48&lon=2&limit=101",
            "/reverse?lat=48&lon=2&lang=bad_tag",
            "/reverse?lat=48&lon=2&q=x",
            "/reverse?lat=48&lon=2&extra=1",
            "/reverse?lat=48&lon=2&query_string_filter=",
            "/reverse?lat=48&lon=2&query_string_filter=osm_key%3Ahighway"
            "&query_string_filter=osm_key%3Ahighway",
            "/reverse?lat=48&lon=2&query_string_filter=osm_key%3Aplace",
        )
        for path in bad_paths:
            status, _, body = self.request("GET", path)
            self.assertEqual(status, 400, path)
            self.assertIn("error", body)
        status, _, _ = self.request("GET", "/reverse/?lat=48&lon=2")
        self.assertEqual(status, 404)
        for method in ("POST", "PUT", "DELETE"):
            status, _, body = self.request(method, "/reverse?lat=48&lon=2")
            self.assertEqual(status, 405, method)
            self.assertEqual(body, {"error": "method not allowed"})


REAL_BINARY = ROOT / "gridpin" / "target" / "release" / "gridpin"
REAL_SHEET = ROOT / "data" / "mc.bin"


@unittest.skipUnless(
    REAL_BINARY.is_file() and REAL_SHEET.is_file(),
    "optional real GridPin binary/tiny-sheet artifacts are not present",
)
class RealGridPinIntegrationTest(unittest.TestCase):
    def test_real_cli_hit_maps_the_matched_house_number_when_present(self):
        state = gridpin_http.AdapterState(REAL_BINARY, REAL_SHEET, "MC", 5.0, 1)
        hits = state.geocode("Avenue des Citronniers, Monaco", 1)
        self.assertTrue(hits)
        feature = gridpin_http.photon_feature(hits[0], state.country_code, state.country_name)
        lon, lat = feature["geometry"]["coordinates"]
        self.assertTrue(7.0 < lon < 8.0)
        self.assertTrue(43.0 < lat < 44.5)
        self.assertEqual(feature["properties"]["country"], "Monaco")
        self.assertEqual(feature["properties"]["countrycode"], "MC")
        if "housenumber" in hits[0]:
            self.assertEqual(feature["properties"]["housenumber"], hits[0]["housenumber"])
        else:
            self.assertNotIn("housenumber", feature["properties"])


class FocusRankingTest(unittest.TestCase):
    """Focus widens/injects in the engine, then the facade orders by distance."""

    STRASBOURG = (48.5734, 7.7521)

    def test_parse_returns_the_focus_pair_and_none_without_it(self):
        _, _, focus = gridpin_http.parse_api_query("q=x&lat=48.5734&lon=7.7521")
        self.assertEqual(focus, self.STRASBOURG)
        _, _, none_focus = gridpin_http.parse_api_query("q=x")
        self.assertIsNone(none_focus)

    def test_nearest_candidate_wins_regardless_of_engine_order(self):
        hits = [
            {"lat": 50.6292, "lon": 3.0573, "commune": "Lille"},
            {"lat": 43.2965, "lon": 5.3698, "commune": "Marseille"},
            {"lat": 48.5839, "lon": 7.7455, "commune": "Strasbourg"},
        ]
        ranked = gridpin_http.rank_by_focus(hits, self.STRASBOURG)
        self.assertEqual(
            [hit["commune"] for hit in ranked],
            ["Strasbourg", "Lille", "Marseille"],
        )

    def test_equal_distance_keeps_engine_order_and_unusable_sinks(self):
        same = {"lat": 48.5734, "lon": 7.7521, "commune": "first"}
        twin = {"lat": 48.5734, "lon": 7.7521, "commune": "second"}
        broken = {"commune": "no-coordinate"}
        nan_hit = {"lat": float("nan"), "lon": 1.0, "commune": "not-finite"}
        ranked = gridpin_http.rank_by_focus(
            [broken, same, nan_hit, twin], self.STRASBOURG
        )
        self.assertEqual(
            [hit["commune"] for hit in ranked],
            ["first", "second", "no-coordinate", "not-finite"],
        )
        self.assertEqual(len(ranked), 4)

    def test_focus_candidate_pool_never_exceeds_the_api_cap(self):
        self.assertLessEqual(gridpin_http.FOCUS_CANDIDATES, gridpin_http.MAX_LIMIT)

    def focus_fixture(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = pathlib.Path(tmp.name)
        index = root / "sheet.bin"
        index.write_bytes(b"test")
        seen = root / "argv.json"
        binary = root / "fake.py"
        # Deliberately emit the far result first. The HTTP facade must reorder it,
        # while the direct state call exposes the unchanged engine order.
        binary.write_text(
            "#!/usr/bin/env python3\n"
            "import json, sys, pathlib\n"
            f"pathlib.Path({str(seen)!r}).write_text(json.dumps(sys.argv[1:]))\n"
            "print(json.dumps({'lat':50.6292,'lon':3.0573,'commune':'Lille'}))\n"
            "print(json.dumps({'lat':48.5839,'lon':7.7455,'commune':'Strasbourg'}))\n",
            encoding="utf-8",
        )
        binary.chmod(0o755)
        state = gridpin_http.AdapterState(binary, index, "FR", 5.0, 2)
        return state, index, seen

    def test_state_passes_exact_near_argument_to_the_cli(self):
        state, index, seen = self.focus_fixture()
        hits = state.geocode("2 Rue Bellevue", 2, self.STRASBOURG)
        self.assertEqual([hit["commune"] for hit in hits], ["Lille", "Strasbourg"])
        self.assertEqual(
            json.loads(seen.read_text()),
            [
                "query",
                str(index),
                "2 Rue Bellevue",
                "-k",
                "2",
                "--near",
                "48.5734,7.7521",
            ],
        )

    def test_state_without_focus_does_not_add_near(self):
        state, index, seen = self.focus_fixture()
        state.geocode("2 Rue Bellevue", 2)
        self.assertEqual(
            json.loads(seen.read_text()),
            ["query", str(index), "2 Rue Bellevue", "-k", "2"],
        )

    def test_http_widens_passes_near_and_reranks_the_engine_pool(self):
        state, index, seen = self.focus_fixture()
        server = ThreadingHTTPServer(("127.0.0.1", 0), gridpin_http.make_handler(state))
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            conn = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=5)
            conn.request("GET", "/api/?q=2%20Rue%20Bellevue&limit=1&lat=48.5734&lon=7.7521")
            body = json.loads(conn.getresponse().read())
            conn.close()
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)
        self.assertEqual(
            json.loads(seen.read_text()),
            [
                "query",
                str(index),
                "2 Rue Bellevue",
                "-k",
                str(gridpin_http.FOCUS_CANDIDATES),
                "--near",
                "48.5734,7.7521",
            ],
        )
        self.assertEqual(len(body["features"]), 1)
        self.assertEqual(body["features"][0]["properties"]["city"], "Strasbourg")


if __name__ == "__main__":
    unittest.main(verbosity=2)
