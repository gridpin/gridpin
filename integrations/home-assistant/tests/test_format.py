import importlib.util
from pathlib import Path

spec = importlib.util.spec_from_file_location("gp_ha_format", Path(__file__).resolve().parents[1] / "custom_components/gridpin/format.py")
fmt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fmt)


def test_the_url_and_the_state_of_an_answer():
    assert fmt.reverse_url("http://127.0.0.1:8080/", 43.73, 7.42) == "http://127.0.0.1:8080/v1/reverse?lat=43.730000&lon=7.420000&k=1"
    state, attrs = fmt.state_of({"results": [{"housenumber": "27", "street": "Boulevard des Moulins", "postcode": "98000",
                                              "commune": "Monaco", "distance_m": 4.2, "match_level": "PointAddress", "lat": 1, "lon": 2}]})
    assert state == "27 Boulevard des Moulins, 98000 Monaco"
    assert attrs["distance_m"] == 4.2
    assert fmt.state_of({"results": []}) == (None, {})
