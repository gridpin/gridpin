//! `gridpin serve`: the built binary on a one-address fixture sheet, spoken to over raw HTTP on loopback.
//! Answers are compared with `gridpin query` / `gridpin reverse`, not with hand-written expectations.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

fn sheet(name: &str) -> (Scratch, PathBuf) {
    sheet_of(name, "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n")
}

fn sheet_of(name: &str, rows: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("serve-{name}"));
    let csv = dir.join("in.csv");
    std::fs::write(&csv, format!("{HDR}{rows}")).unwrap();
    let man = dir.join("m.json");
    std::fs::write(&man, r#"{"country":"mc","layer":"addresses","license":"t","source_release":"test-release","attribution":"fixture data"}"#)
        .unwrap();
    let bin = dir.join("s.bin");
    let out = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            bin.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (dir, bin)
}

/// A running server, stopped when dropped.
struct Server {
    child: Child,
    addr: String,
}

impl Server {
    fn start(bin: &Path, extra: &[&str]) -> Server {
        let mut args = vec![
            "serve",
            bin.to_str().unwrap(),
            "--port",
            "0",
            "--threads",
            "2",
        ];
        args.extend_from_slice(extra);
        let mut child = Command::new(BIN)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let addr = line
            .trim()
            .strip_prefix("listening on http://")
            .unwrap_or_else(|| panic!("first line: {line:?}"))
            .to_string();
        Server { child, addr }
    }

    /// One raw request; returns (status, headers, body).
    fn raw(&self, request: &str) -> (u16, String, String) {
        let mut s = TcpStream::connect(&self.addr).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut text = String::new();
        s.read_to_string(&mut text).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").expect("no header end");
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, head.to_string(), body.to_string())
    }

    fn get(&self, target: &str) -> (u16, String) {
        let (status, _, body) = self.raw(&format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n"));
        (status, body)
    }

    fn json(&self, target: &str) -> serde_json::Value {
        let (status, body) = self.get(target);
        assert_eq!(status, 200, "{target}: {body}");
        serde_json::from_str(&body).unwrap()
    }

    fn post(&self, target: &str, body: &str) -> (u16, String) {
        let (status, _, out) = self.raw(&format!(
            "POST {target} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ));
        (status, out)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn cli(args: &[&str]) -> Vec<serde_json::Value> {
    let out = Command::new(BIN).args(args).output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// What `gridpin batch` writes for one query: the native API promises the same JSON, byte for byte.
fn batch_results(bin: &Path, dir: &Path, q: &str, k: &str) -> serde_json::Value {
    let input = dir.join("one.jsonl");
    let output = dir.join("one_out.jsonl");
    std::fs::write(&input, serde_json::json!({ "q": q }).to_string() + "\n").unwrap();
    let out = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "-k",
            k,
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let line = std::fs::read_to_string(&output).unwrap();
    serde_json::from_str::<serde_json::Value>(line.trim()).unwrap()["results"].clone()
}

#[test]
fn the_native_api_answers_what_batch_and_reverse_answer() {
    let (dir, bin) = sheet("native");
    let s = Server::start(&bin, &[]);
    let b = bin.to_str().unwrap();
    let got = s.json("/v1/geocode?q=rue+a+1+ville&k=3");
    assert_eq!(
        got["results"],
        batch_results(&bin, &dir, "rue a 1 ville", "3")
    );
    assert_eq!(
        got["results"].as_array().unwrap().len(),
        1,
        "the fixture holds one address"
    );
    // `gridpin reverse` prints confidence as f32; compare the place, not the float formatting
    let got = s.json("/v1/reverse?lat=43.73&lon=7.42&k=1");
    let want = cli(&["reverse", b, "43.73", "7.42", "-k", "1"]);
    for key in [
        "lat",
        "lon",
        "street",
        "housenumber",
        "commune",
        "postcode",
        "precision",
        "distance_m",
    ] {
        assert_eq!(got["results"][0][key], want[0][key], "{key}");
    }
    // POST keeps the order and gives each query its own list
    let (status, body) = s.post(
        "/v1/geocode",
        r#"{"queries": ["rue a 1 ville", "zzzz", ""], "k": 1}"#,
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let r = v["results"].as_array().unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[0], batch_results(&bin, &dir, "rue a 1 ville", "1"));
    assert_eq!(
        (&r[1], &r[2]),
        (&serde_json::json!([]), &serde_json::json!([]))
    );
}

#[test]
fn json_callback_wraps_the_nominatim_answer_for_a_script_and_refuses_anything_but_a_name() {
    // ANSWERS §300: the search box of ipyleaflet / leafmap asks with json_callback (JSONP), as Nominatim allows
    let (_dir, bin) = sheet("jsonp");
    let s = Server::start(&bin, &[]);
    let plain = s.get("/search?q=rue+a+1+ville&format=json").1;
    let (status, head, body) =
        s.raw("GET /search?q=rue+a+1+ville&format=json&json_callback=L.Control.Search.callJsonp HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(status, 200, "{body}");
    assert!(head.contains("application/javascript"), "{head}");
    assert_eq!(body, format!("L.Control.Search.callJsonp({plain})"));
    let (status, _, rev) = s.raw(
        "GET /reverse?lat=43.73&lon=7.42&format=json&json_callback=cb HTTP/1.1\r\nHost: x\r\n\r\n",
    );
    assert_eq!(status, 200);
    assert!(rev.starts_with("cb({") && rev.ends_with("})"), "{rev}");
    for bad in ["alert(1)", "a;b", "1cb", "cb.", "x%3Cscript"] {
        let (status, body) = s.get(&format!("/search?q=rue+a+1+ville&json_callback={bad}"));
        assert_eq!(status, 400, "{bad}: {body}");
        assert!(!body.contains('('), "{bad}: {body}");
    }
    // without the parameter the answer is plain JSON, as before
    assert!(plain.starts_with('['), "{plain}");
}

#[test]
fn the_nominatim_paths_answer_in_the_documented_shape() {
    let (_dir, bin) = sheet("nominatim");
    let s = Server::start(&bin, &[]);
    let native = &s.json("/v1/geocode?q=rue+a+1+ville")["results"][0];
    let list = s.json("/search?q=rue+a+1+ville&format=json&addressdetails=1&limit=5");
    let place = &list.as_array().expect("search answers an array")[0];
    assert_eq!(
        place["lat"],
        format!("{:.7}", native["lat"].as_f64().unwrap()),
        "lat is a string, as in Nominatim"
    );
    assert_eq!(
        place["lon"],
        format!("{:.7}", native["lon"].as_f64().unwrap())
    );
    assert_eq!(place["boundingbox"].as_array().unwrap().len(), 4);
    assert_eq!(place["licence"], "fixture data");
    assert_eq!(place["address"]["country_code"], "mc");
    assert!(place.get("osm_id").is_none());
    assert!(place.get("class").is_some());
    let v2 = s.json("/search?q=rue+a+1+ville");
    assert!(
        v2[0].get("category").is_some(),
        "jsonv2 is the default: {v2}"
    );
    // structured search fields are joined like a free-form query
    let structured = s.json("/search?street=rue+a+1&city=ville&format=json");
    assert_eq!(structured[0]["lat"], place["lat"]);
    // a country the sheet does not hold gets nothing, its own country is answered
    assert_eq!(
        s.json("/search?q=rue+a+1+ville&countrycodes=fr,de"),
        serde_json::json!([])
    );
    assert_eq!(
        s.json("/search?q=rue+a+1+ville&countrycodes=MC")[0]["lat"],
        place["lat"]
    );
    assert_eq!(
        s.json("/search?q=zzzz+qqqq&format=json"),
        serde_json::json!([])
    );
    let geo = s.json("/search?q=rue+a+1+ville&format=geojson");
    assert_eq!(geo["type"], "FeatureCollection");
    assert_eq!(geo["features"][0]["geometry"]["type"], "Point");
    let rev = s.json("/reverse?lat=43.73&lon=7.42&format=json");
    assert_eq!(rev["lat"], place["lat"]);
    assert_eq!(
        s.json("/reverse?lat=-40&lon=-100&format=json"),
        serde_json::json!({ "error": "Unable to geocode" }),
        "Nominatim's own answer when nothing is near"
    );
    let status = s.json("/status?format=json");
    assert_eq!(
        (status["status"].as_i64(), status["data_updated"].as_str()),
        (Some(0), Some("test-release"))
    );
    let (code, text) = s.get("/status");
    assert_eq!((code, text.as_str()), (200, "OK"));
}

#[test]
fn extratags_are_the_match_level_when_asked_and_absent_otherwise() {
    let (_dir, bin) = sheet("extra");
    let server = Server::start(&bin, &[]);
    let plain = server.json("/search?q=rue+a+1+ville&format=jsonv2");
    assert!(plain[0].get("extratags").is_none(), "{plain}");
    let asked = server.json("/search?q=rue+a+1+ville&format=jsonv2&extratags=1");
    assert_eq!(asked[0]["extratags"]["match_level"], "PointAddress");
    assert_eq!(asked[0]["extratags"]["match_status"], "M");
    let reverse = server.json("/reverse?lat=43.73&lon=7.42&format=jsonv2&extratags=1");
    assert_eq!(reverse["extratags"]["match_level"], "PointAddress");
}

#[test]
fn with_puts_the_same_extras_on_every_path_and_nothing_without_it() {
    let (_dir, bin) = sheet("with");
    let server = Server::start(&bin, &[]);
    let cli = cli(&[
        "query",
        bin.to_str().unwrap(),
        "rue a 1 ville",
        "-k",
        "1",
        "--with",
        "h3,codes",
    ]);
    let want = &cli[0]["extras"];
    assert!(want["h3"].is_string(), "{want}");
    let native = server.json("/v1/geocode?q=rue+a+1+ville&with=h3,codes");
    assert_eq!(&native["results"][0]["extras"], want);
    assert!(server.json("/v1/geocode?q=rue+a+1+ville")["results"][0]
        .get("extras")
        .is_none());
    let back = server.json("/v1/reverse?lat=43.73&lon=7.42&with=h3,codes");
    assert_eq!(&back["results"][0]["extras"], want);
    let nominatim = server.json("/search?q=rue+a+1+ville&format=jsonv2&with=h3,codes");
    assert_eq!(&nominatim[0]["extras"], want);
    let geojson = server.json("/search?q=rue+a+1+ville&format=geojson&with=h3,codes");
    assert_eq!(&geojson["features"][0]["properties"]["extras"], want);
    let reverse = server.json("/reverse?lat=43.73&lon=7.42&format=jsonv2&with=h3,codes");
    assert_eq!(&reverse["extras"], want);
    let google = server.json("/maps/api/geocode/json?address=rue+a+1+ville&with=h3,codes");
    assert_eq!(&google["results"][0]["extras"], want);
    assert_eq!(
        google["results"][0]["plus_code"]["global_code"]
            .as_str()
            .unwrap()
            .len(),
        11,
        "Google's own field: ten digits and the plus"
    );
    let (status, body) = server.post(
        "/v1/geocode",
        r#"{"queries": ["rue a 1 ville"], "with": "h3,codes"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(&v["results"][0][0]["extras"], want);
    let (status, body) = server.get("/v1/geocode?q=rue+a+1+ville&with=zip");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("unknown field"), "{body}");
}

#[test]
fn a_post_takes_lines_and_fields_side_by_side() {
    let (_dir, bin) = sheet("fields");
    let server = Server::start(&bin, &[]);
    let (status, body) = server.post(
        "/v1/geocode",
        r#"{"queries": ["rue a 1 ville", {"street": "Rue A", "housenumber": "1", "city": "Ville", "postcode": "10000"}, {"street": "Rue A", "number": 1, "city": "Ville", "region": "X"}]}"#,
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    for row in v["results"].as_array().unwrap() {
        assert_eq!(row[0]["precision"], "house", "{v}");
        assert_eq!(row[0]["housenumber"], "1", "{v}");
        assert_eq!(row[0]["match_level"], "PointAddress", "{v}");
    }
    let (status, _) = server.post("/v1/geocode", r#"{"queries": [{"city": "Ville"}]}"#);
    assert_eq!(status, 400);
}

#[test]
fn a_server_with_aliases_answers_a_name_as_its_address_and_nothing_else_changes() {
    let (dir, bin) = sheet("aliases");
    let table = dir.join("aliases.csv");
    std::fs::write(
        &table,
        "name,address\nHQ,\"rue a 1, ville\"\nrue a 1 ville,rue a 1 ville\n",
    )
    .unwrap();
    let server = Server::start(&bin, &["--aliases", table.to_str().unwrap()]);
    // a name that is its own address: on the near path the flag follows the table, not the text
    let same = server.json("/v1/geocode?q=rue+a+1+ville&near=43.7,7.4");
    assert_eq!(same["results"][0]["alias_name"], "rue a 1 ville");
    assert!(same["results"][0]["flags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f == "alias"));
    let named = server.json("/v1/geocode?q=hq");
    assert_eq!(named["results"][0]["street"], "Rue A");
    assert_eq!(named["results"][0]["alias_name"], "HQ");
    assert!(named["results"][0]["flags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f == "alias"));
    let near = server.json("/v1/geocode?q=hq&near=43.7,7.4");
    assert_eq!(near["results"][0]["alias_name"], "HQ");
    let other = server.json("/v1/geocode?q=1+rue+a+ville");
    assert!(other["results"][0].get("alias_name").is_none());
    let (status, body) = server.post("/v1/geocode", r#"{"queries": ["HQ", "1 rue a ville"]}"#);
    assert_eq!(status, 200, "{body}");
    let many: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(many["results"][0][0]["alias_name"], "HQ");
    assert!(many["results"][1][0].get("alias_name").is_none());
    let found = server.json("/search?q=HQ&format=jsonv2");
    assert_eq!(found[0]["type"], "house", "{found}");
}

#[test]
fn the_google_path_answers_in_the_documented_shape_with_a_key_or_without() {
    let (_dir, bin) = sheet("google");
    let server = Server::start(&bin, &[]);
    let v = server.json("/maps/api/geocode/json?address=rue+a+1+ville&key=ANY");
    assert_eq!(v["status"], "OK", "{v}");
    let top = &v["results"][0];
    let plain = &cli(&["query", bin.to_str().unwrap(), "rue a 1 ville", "-k", "1"])[0];
    assert_eq!(top["geometry"]["location"]["lat"], plain["lat"]);
    assert_eq!(top["geometry"]["location"]["lng"], plain["lon"]);
    assert_eq!(top["geometry"]["location_type"], "ROOFTOP");
    assert!(top["formatted_address"].as_str().unwrap().contains("Rue A"));
    assert!(top["place_id"].is_string());
    assert!(top["geometry"]["viewport"]["northeast"]["lat"].is_number());
    assert_eq!(top["types"][0], "street_address");
    let kinds: Vec<&str> = top["address_components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["types"][0].as_str().unwrap())
        .collect();
    for kind in [
        "street_number",
        "route",
        "locality",
        "country",
        "postal_code",
    ] {
        assert!(kinds.contains(&kind), "{kind}: {kinds:?}");
    }
}

#[test]
fn the_google_path_says_zero_results_and_invalid_request_as_documented() {
    let (_dir, bin) = sheet("google-status");
    let server = Server::start(&bin, &[]);
    let none = server.json("/maps/api/geocode/json?address=zzzz+qqqq");
    assert_eq!(none["status"], "ZERO_RESULTS");
    assert_eq!(none["results"].as_array().unwrap().len(), 0);
    // Google itself answers HTTP 400 with this body (checked against the live service, 2026-11-07)
    let (status, body) = server.get("/maps/api/geocode/json");
    assert_eq!(status, 400);
    assert!(body.contains("INVALID_REQUEST"), "{body}");
    let (status, body) = server.get("/maps/api/geocode/json?latlng=abc");
    assert_eq!(status, 400);
    assert!(body.contains("INVALID_REQUEST"), "{body}");
    let (status, _) = server.get("/maps/api/geocode/json?latlng=91,0");
    assert_eq!(status, 400, "not a coordinate");
}

#[test]
fn the_google_path_reads_latlng_and_components() {
    let (_dir, bin) = sheet("google-more");
    let server = Server::start(&bin, &[]);
    let back = server.json("/maps/api/geocode/json?latlng=43.73,7.42");
    assert_eq!(back["status"], "OK");
    assert_eq!(back["results"][0]["geometry"]["location_type"], "ROOFTOP");
    let narrowed = server
        .json("/maps/api/geocode/json?address=rue+a+1&components=locality:ville|postal_code:10000");
    assert_eq!(
        narrowed["results"][0]["geometry"]["location_type"],
        "ROOFTOP"
    );
    let same = server.json("/maps/api/geocode/json?address=rue+a+1+ville&components=country:MC");
    assert_eq!(same["status"], "OK", "the sheet's own country");
    let other = server.json("/maps/api/geocode/json?address=rue+a+1+ville&components=country:US");
    assert_eq!(
        other["status"], "ZERO_RESULTS",
        "another country is not on this sheet"
    );
    let only = server.json("/maps/api/geocode/json?components=locality:ville");
    assert_eq!(only["status"], "OK", "components alone are a query");
}

#[test]
fn traccar_finds_every_field_it_reads_in_the_reverse_answer() {
    // Traccar's Nominatim geocoder: GET <geocoder.url>?format=json&lat=..&lon=..&zoom=18&addressdetails=1,
    // then display_name and address.{house_number, road, city, postcode, country_code}.
    let (_dir, bin) = sheet("traccar");
    let server = Server::start(&bin, &[]);
    let v = server.json("/reverse?format=json&lat=43.73&lon=7.42&zoom=18&addressdetails=1");
    assert!(v["display_name"].as_str().unwrap().contains("Rue A"), "{v}");
    let a = &v["address"];
    assert_eq!(a["road"], "Rue A");
    assert_eq!(a["house_number"], "1");
    assert_eq!(a["city"], "Ville");
    assert_eq!(a["postcode"], "10000");
    assert_eq!(a["country_code"], "mc");
}

#[test]
fn health_names_the_sheet_and_turns_ready() {
    let (_dir, bin) = sheet("health");
    let s = Server::start(&bin, &[]);
    let mut h = s.json("/health");
    for _ in 0..100 {
        if h["status"] == "ok" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        h = s.json("/health");
    }
    assert_eq!(h["status"], "ok");
    assert_eq!(h["sheet"]["country"], "mc");
    assert_eq!(h["sheet"]["file"], "s.bin");
    assert_eq!(h["sheet"]["source_release"], "test-release");
    assert_eq!(h["panics"], 0);
}

#[test]
fn bad_requests_get_their_own_status_and_cors_is_open() {
    let (_dir, bin) = sheet("errors");
    let s = Server::start(&bin, &[]);
    assert_eq!(s.get("/nope").0, 404);
    assert_eq!(s.get("/search?q=x&format=xml").0, 400);
    assert_eq!(s.get("/search?format=json").0, 400, "no q");
    assert_eq!(s.get("/search?q=x&limit=0").0, 400);
    assert_eq!(s.get("/v1/reverse?lat=abc&lon=1").0, 400);
    assert_eq!(
        s.get("/v1/reverse?lat=95&lon=1").0,
        400,
        "latitude out of range"
    );
    assert_eq!(s.get(&format!("/search?q={}", "a".repeat(2000))).0, 414);
    assert_eq!(s.get("/search?q=%E0%A4").0, 400, "not UTF-8");
    let (code, _, _) = s.raw("DELETE /search HTTP/1.1\r\n\r\n");
    assert_eq!(code, 405);
    let (code, head, _) = s.raw("OPTIONS /search HTTP/1.1\r\nOrigin: http://example.org\r\n\r\n");
    assert_eq!(code, 204);
    assert!(head.contains("Access-Control-Allow-Origin: *"), "{head}");
    let (_, head, _) = s.raw("GET /health HTTP/1.1\r\n\r\n");
    assert!(head.contains("Access-Control-Allow-Origin: *") && head.contains("application/json"));
    assert_eq!(s.post("/v1/geocode", "not json").0, 400);
    assert_eq!(s.post("/v1/geocode", r#"{"queries": [1]}"#).0, 400);
    let many = format!("{{\"queries\": [{}]}}", vec!["\"a\""; 10_001].join(","));
    assert_eq!(s.post("/v1/geocode", &many).0, 413);
    let (code, _, _) = s.raw("POST /v1/geocode HTTP/1.1\r\nContent-Length: 2000000\r\n\r\n");
    assert_eq!(code, 413);
    assert_eq!(s.post("/search", "{}").0, 405, "POST only for /v1/geocode");
}

#[test]
fn it_listens_on_loopback_unless_told_otherwise() {
    let (_dir, bin) = sheet("loopback");
    let s = Server::start(&bin, &[]);
    assert!(s.addr.starts_with("127.0.0.1:"), "{}", s.addr);
}

#[test]
fn probe_succeeds_only_on_status_200() {
    let (_dir, bin) = sheet("probe");
    let s = Server::start(&bin, &[]);
    let ok = Command::new(BIN)
        .args(["serve", "--probe", &format!("http://{}/health", s.addr)])
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let bad = Command::new(BIN)
        .args(["serve", "--probe", &format!("http://{}/nope", s.addr)])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    let closed = Command::new(BIN)
        .args(["serve", "--probe", "http://127.0.0.1:1/health"])
        .output()
        .unwrap();
    assert!(!closed.status.success());
}

#[test]
fn suggest_names_the_streets_that_start_as_typed_and_keeps_nothing() {
    let (_d, bin) = sheet("suggest");
    let server = Server::start(&bin, &[]);
    let json = |target: &str| -> serde_json::Value {
        let (status, body) = server.get(target);
        assert_eq!(status, 200, "{target}: {body}");
        serde_json::from_str(&body).unwrap()
    };
    let found = json("/v1/suggest?q=rue");
    let s = &found["suggestions"][0];
    assert_eq!(
        (
            s["street"].as_str(),
            s["commune"].as_str(),
            s["label"].as_str()
        ),
        (Some("Rue A"), Some("Ville"), Some("Rue A, Ville"))
    );
    assert_eq!(
        (s["lat"].as_f64(), s["lon"].as_f64()),
        (Some(43.73), Some(7.42))
    );
    assert_eq!(
        json("/v1/suggest?q=12+rue+a,+vil")["suggestions"][0]["label"],
        "12 Rue A, Ville",
        "the number stays in the label"
    );
    // most countries write the number after the street: it is taken off the end and stays after the street
    assert_eq!(
        json("/v1/suggest?q=rue+a+12")["suggestions"][0]["label"],
        "Rue A 12, Ville",
        "a number after the street"
    );
    assert_eq!(
        json("/v1/suggest?q=rue+a+12b,+vil")["suggestions"][0]["label"],
        "Rue A 12b, Ville",
        "a number with a letter, and a town after the comma"
    );
    for empty in [
        "/v1/suggest?q=ru",
        "/v1/suggest?q=rue+a,+zzz",
        "/v1/suggest?q=boulevard",
        "/v1/suggest?q=boulevard+12",
        "/v1/suggest?q=rue+a+12,+zzz",
        "/v1/suggest?q=rue+a+douze",
    ] {
        assert_eq!(json(empty)["suggestions"], serde_json::json!([]), "{empty}");
    }
    assert_eq!(server.get("/v1/suggest").0, 400);
    assert!(
        server.get("/").1.contains("/v1/suggest"),
        "the home page lists the path"
    );
}

#[test]
fn suggest_points_at_the_typed_house_and_says_when_it_could_not() {
    // Rue A of Ville: the biggest street (Autre has a range point too, but it is no house), no house 12, but a second Rue A of the same commune (another village,
    // another code) has house 40. Rue A of Autre has house 12.
    let (_d, bin) = sheet_of(
        "suggest-house",
        "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n\
         rue a,001,ville,10000,2,,7.421,43.731,Rue A,Ville\n\
         rue a,001,ville,10000,3,,7.422,43.732,Rue A,Ville\n\
         rue a,002,autre,20000,5,,7.51,43.81,Rue A,Autre\n\
         rue a,002,autre,20000,12,,7.5,43.8,Rue A,Autre\n\
         rue a,002,autre,20000,30,~,7.6,43.9,Rue A,Autre\n\
         rue a,003,ville,10000,40,,7.43,43.74,Rue A,Ville\n",
    );
    let server = Server::start(&bin, &[]);
    let first = |q: &str| -> Vec<(String, String, f64, f64)> {
        let (status, body) = server.get(&format!("/v1/suggest?q={q}"));
        assert_eq!(status, 200, "{q}: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        v["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["commune"].as_str().unwrap().to_string(),
                    s["precision"].as_str().unwrap().to_string(),
                    s["lat"].as_f64().unwrap(),
                    s["lon"].as_f64().unwrap(),
                )
            })
            .collect()
    };
    // the street that has the house comes first, at the house, though Ville's Rue A has more addresses
    let twelve = first("rue+a+12");
    assert_eq!(
        (twelve[0].0.as_str(), twelve[0].1.as_str()),
        ("Autre", "house")
    );
    assert_eq!(
        (twelve[0].2, twelve[0].3),
        (43.8, 7.5),
        "the point is house 12, not the street's middle"
    );
    assert_eq!(
        (twelve[1].0.as_str(), twelve[1].1.as_str()),
        ("Ville", "street")
    );
    // the number before the street is looked up the same way
    assert_eq!(first("12+rue+a")[0].1, "house");
    // a number no street has: every suggestion says it is the street, and the bigger street stays first
    let none = first("rue+a+99");
    assert!(none.iter().all(|s| s.1 == "street"), "{none:?}");
    assert_eq!(none[0].0, "Ville");
    // a near number is not the house: 1 is a house, 10 and 4 are not
    assert!(first("rue+a+10").iter().all(|s| s.1 == "street"));
    assert!(first("rue+a+4").iter().all(|s| s.1 == "street"));
    // one suggestion per street name and commune: the Rue A of Ville that has house 40 speaks for Ville
    let forty = first("rue+a+40");
    assert_eq!(
        forty.iter().filter(|s| s.0 == "Ville").count(),
        1,
        "{forty:?}"
    );
    assert_eq!(
        forty[0],
        ("Ville".to_string(), "house".to_string(), 43.74, 7.43)
    );
    // a point sampled from an address range (suffix "~", US TIGER) is not a house, though it carries number 30
    assert!(
        first("rue+a+30").iter().all(|s| s.1 == "street"),
        "{:?}",
        first("rue+a+30")
    );
    // no number: the street
    assert!(first("rue+a").iter().all(|s| s.1 == "street"));
}

#[test]
fn batch_lines_answers_one_line_per_line_on_stdin() {
    let (_d, bin) = sheet("lines");
    let mut child = Command::new(BIN)
        .args(["batch", bin.to_str().unwrap(), "-", "-", "--lines"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"1 rue a, ville\nnowhere 9, atlantis\n1 rue a,\\tville\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(
        (first["street"].as_str(), first["housenumber"].as_str()),
        (Some("Rue A"), Some("1"))
    );
    assert_eq!(
        lines[1], "",
        "no answer is an empty line, so rows stay in step"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(lines[2]).unwrap()["housenumber"],
        "1",
        "an escaped tab is read as a tab"
    );
    let refused = Command::new(BIN)
        .args(["batch", bin.to_str().unwrap(), "in.jsonl", "-", "--lines"])
        .output()
        .unwrap();
    assert!(!refused.status.success() && String::from_utf8_lossy(&refused.stderr).contains("- -"));
}

#[test]
fn extract_finds_the_address_in_a_posted_text_as_the_cli_does() {
    let (_dir, bin) = sheet("extract");
    let s = Server::start(&bin, &[]);
    let (status, body) = s.post(
        "/v1/extract",
        r#"{"text": "Please deliver to rue a 1 ville tomorrow."}"#,
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let r = v["results"].as_array().unwrap();
    assert_eq!(r.len(), 1, "{body}");
    assert_eq!(r[0]["level"], "house");
    assert_eq!(r[0]["hit"]["housenumber"], "1");
    // the same answers, field for field, as `gridpin extract` gives on the same text
    let text = bin.with_file_name("letter.txt");
    std::fs::write(&text, "Please deliver to rue a 1 ville tomorrow.").unwrap();
    let cli = Command::new(BIN)
        .args([
            "extract",
            bin.to_str().unwrap(),
            "--text",
            text.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(cli.status.success());
    let cli: Vec<serde_json::Value> = String::from_utf8(cli.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(&cli, r, "the server and the command answer alike");
    assert_eq!(
        s.post("/v1/extract", r#"{"q": 1}"#).0,
        400,
        "the body names a text"
    );
}

#[test]
fn a_server_of_two_sheets_asks_the_named_sheet_and_else_its_main_one() {
    let (dir, mc) = sheet("two-sheets");
    let csv = dir.join("rs.csv");
    std::fs::write(&csv, format!("{HDR}rue c,004,grad,21000,9,,19.83,45.25,Rue C,Grad\nulica b,002,grad,21000,1,,19.84,45.25,Ulica B,Grad\nulica b,002,grad,21000,3,,19.841,45.25,Ulica B,Grad\n")).unwrap();
    let man = dir.join("rs.json");
    std::fs::write(&man, r#"{"country":"rs","layer":"addresses","license":"t","source_release":"test","attribution":"fixture"}"#).unwrap();
    let rs = dir.join("rs.bin");
    assert!(Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            rs.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    let s = Server::start(&mc, &["--sheets", rs.to_str().unwrap()]);
    let top = |path: &str| s.json(path)["results"][0].clone();
    let has = |h: &serde_json::Value, f: &str| {
        h["flags"]
            .as_array()
            .is_some_and(|a| a.iter().any(|x| x == f))
    };
    // country= names the sheet
    let b = top("/v1/geocode?q=ulica+b+3+grad&country=rs");
    assert_eq!(b["housenumber"], "3", "{b}");
    assert!(has(&b, "country_rs"), "{b}");
    let upper = top("/v1/geocode?q=ulica+b+3+grad&country=RS");
    assert_eq!(
        upper["housenumber"], "3",
        "country= is read without case: {upper}"
    );
    // no country named: the main sheet alone answers, the second sheet's house is not offered (ANSWERS §279)
    let main = s.json("/v1/geocode?q=ulica+b+3+grad")["results"].clone();
    let main = main.as_array().unwrap();
    assert!(
        main.iter()
            .all(|h| !has(h, "country_rs") && h["housenumber"] != "3"),
        "{main:?}"
    );
    let a = top("/v1/geocode?q=rue+a+1+ville");
    assert!(has(&a, "country_mc"), "{a}");
    // the country named at the end picks its sheet in any form of the shared table, though the Serbian rule pack has
    // no country words (09.11.2026, ANSWERS §294)
    for q in [
        "ulica+b+3+grad,+Serbia",
        "ulica+b+3+grad,+Srbija",
        "ulica+b+3+grad,+%D0%A1%D1%80%D0%B1%D0%B8%D1%98%D0%B0",
    ] {
        let named = top(&format!("/v1/geocode?q={q}"));
        assert!(
            has(&named, "country_rs") && named["housenumber"] == "3",
            "{q}: {named}"
        );
    }
    let other = top("/v1/geocode?q=ulica+b+3+grad,+Spain");
    assert!(
        !has(&other, "country_rs"),
        "a country with no sheet in the server leaves the main sheet: {other}"
    );
    let forced = top("/v1/geocode?q=ulica+b+3+grad&country=mc");
    assert_ne!(forced["housenumber"], "3", "country= asks that sheet only");
    assert_eq!(s.get("/v1/geocode?q=x&country=xx").0, 400);
    // one sheet: the answer has no country flag and country= is ignored
    let one = Server::start(&mc, &[]);
    let plain = one.json("/v1/geocode?q=rue+a+1+ville&country=xx");
    assert!(!plain["results"][0]["flags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f.as_str().unwrap().starts_with("country_")));
}

#[test]
fn the_ui_page_is_one_file_without_outside_resources() {
    let (_dir, bin) = sheet("ui");
    let s = Server::start(&bin, &[]);
    let (status, body) = s.get("/ui");
    assert_eq!(status, 200);
    assert!(
        body.contains(r#"type="file""#)
            && body.contains("/v1/geocode")
            && body.contains("function parseCsv")
    );
    for attr in ["src=\"http", "href=\"http", "@import"] {
        assert!(!body.contains(attr), "no outside resource: {attr}");
    }
    assert!(!body.contains("__CSV_JS__"), "the CSV code is inlined");
    // the page holds a cell back by the server's own limit, so one long note cannot turn a chunk into a 414
    assert!(
        body.contains(&format!(
            "const MAX_QUERY_BYTES = {};",
            gridpin::query::MAX_QUERY_BYTES
        )),
        "the page's limit is the engine's"
    );
    assert!(body.contains("sniffDelimiter(text)") && body.contains("toCsv(out, delimiter)"));
    assert!(
        body.contains(r#"get("country")"#),
        "the page passes country= on"
    );
    // one pass at a time, a file that is not UTF-8 refused, the table scrolls in its own box
    assert!(body.contains(r#"$("go").disabled = true;"#) && body.contains("if (running) return;"));
    assert!(body.contains("utf8Text(await file.arrayBuffer())"));
    assert!(body.contains(r#"<div class="scroll"><table id="preview">"#));
}

/// A fixture sheet of one country, with that country's rule pack when `rules` names one (`../rules/<cc>`).
fn country_sheet(dir: &Path, cc: &str, rows: &str, rules: Option<&str>) -> PathBuf {
    let csv = dir.join(format!("{cc}.csv"));
    std::fs::write(&csv, format!("{HDR}{rows}")).unwrap();
    let man = dir.join(format!("{cc}.json"));
    std::fs::write(&man, format!(r#"{{"country":"{cc}","layer":"addresses","license":"t","source_release":"rel-{cc}","attribution":"fixture data of {cc}"}}"#)).unwrap();
    let bin = dir.join(format!("{cc}.bin"));
    let mut args = vec![
        "build".to_string(),
        csv.to_str().unwrap().into(),
        bin.to_str().unwrap().into(),
        "--meta".into(),
        man.to_str().unwrap().into(),
    ];
    if let Some(r) = rules {
        args.push("--rules".into());
        args.push(format!("{}/../rules/{r}", env!("CARGO_MANIFEST_DIR")));
    }
    let out = Command::new(BIN).args(&args).output().unwrap();
    assert!(
        out.status.success(),
        "build {cc}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

const RS_ROWS: &str = "ulica b,002,grad,21000,1,,19.84,45.25,Ulica B,Grad\nulica b,002,grad,21000,3,,19.841,45.25,Ulica B,Grad\n";

/// Revision 0.9.0 (11.11.2026): every path of a server of several sheets asks the sheet it chose and signs the answer
/// with that sheet's country and licence — the POST batch, the Nominatim and Google parameters of a country, exists,
/// extract, explain, reason, reverse and /health; and a US state code at the end is a region, not a country.
#[test]
fn every_path_of_a_server_of_several_sheets_asks_and_signs_the_sheet_it_chose() {
    // The US and Canadian rule packs (`rules/us`, `rules/ca`) live in the private tree only: the public export has no
    // rules/ at all, so there the test says so and stops instead of failing on a sheet built without its rules.
    let rules = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rules");
    if !rules.join("us").is_dir() || !rules.join("ca").is_dir() {
        eprintln!("skipped: no rules/us and rules/ca in this tree (they are not part of the public export)");
        return;
    }
    let dir = Scratch::dir("serve-sheets-every-path");
    let us = country_sheet(
        &dir,
        "us",
        "hobart blvd,001,los angeles,90005,808,,-118.3058,34.0573,Hobart Blvd,Los Angeles\n",
        Some("us"),
    );
    let ca = country_sheet(
        &dir,
        "ca",
        "hobart blvd,002,los angeles,90005,808,,-79.40,43.70,Hobart Blvd,Los Angeles\n",
        Some("ca"),
    );
    let rs = country_sheet(&dir, "rs", RS_ROWS, None);
    let s = Server::start(
        &us,
        &[
            "--sheets",
            &format!("{},{}", ca.to_str().unwrap(), rs.to_str().unwrap()),
        ],
    );
    let has = |h: &serde_json::Value, f: &str| {
        h["flags"]
            .as_array()
            .is_some_and(|a| a.iter().any(|x| x == f))
    };
    let top = |path: &str| s.json(path)["results"][0].clone();

    // a state code at the end is a region of the US sheet: "CA" is California, not Canada
    for q in [
        "808+Hobart+Blvd,+Los+Angeles,+CA",
        "808+Hobart+Blvd,+Los+Angeles,+CA+90005",
    ] {
        let h = top(&format!("/v1/geocode?q={q}"));
        assert!(has(&h, "country_us"), "{q}: {h}");
        assert!(
            (h["lat"].as_f64().unwrap() - 34.0573).abs() < 1e-6,
            "{q}: {h}"
        );
    }
    let named = top("/v1/geocode?q=808+Hobart+Blvd,+Los+Angeles,+Canada");
    assert!(
        has(&named, "country_ca"),
        "the country's name still picks its sheet: {named}"
    );

    // POST: country= holds for every query, a line and fields alike, on the rayon pool too
    let (status, body) = s.post(
        "/v1/geocode?country=rs",
        r#"{"queries":["ulica b 3 grad",{"street":"ulica b","housenumber":"3","city":"grad"}],"k":1}"#,
    );
    assert_eq!(status, 200, "{body}");
    let many: serde_json::Value = serde_json::from_str(&body).unwrap();
    let alone = batch_results(&rs, &dir, "ulica b 3 grad", "1");
    for row in many["results"].as_array().unwrap() {
        let h = &row[0];
        assert!(has(h, "country_rs") && h["housenumber"] == "3", "{row}");
        // the coordinates are those `gridpin batch` gives on the Serbian sheet alone, byte for byte
        assert_eq!(h["lat"].to_string(), alone[0]["lat"].to_string());
        assert_eq!(h["lon"].to_string(), alone[0]["lon"].to_string());
    }
    // with=provenance describes the sheet that answered
    for path in [
        "/v1/geocode?q=ulica+b+3+grad&country=rs&with=provenance",
        "/search?q=ulica+b+3+grad&countrycodes=rs&with=provenance",
    ] {
        let v = s.json(path);
        let text = v.to_string();
        assert!(
            text.contains("rel-rs") && !text.contains("rel-us"),
            "{path}: {v}"
        );
    }
    // fields name their region at the end too
    let (_, body) = s.post(
        "/v1/geocode",
        r#"{"queries":[{"street":"Hobart Blvd","housenumber":"808","city":"Los Angeles","region":"CA"}]}"#,
    );
    let fields: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(has(&fields["results"][0][0], "country_us"), "{fields}");

    // Nominatim: countrycodes= and a structured country name pick the sheet; the answer is signed by it
    for path in [
        "/search?q=ulica+b+3+grad&countrycodes=rs&addressdetails=1",
        "/search?street=ulica+b+3&city=grad&country=Serbia&addressdetails=1",
        "/search?street=ulica+b+3&city=grad&country=RS&addressdetails=1",
    ] {
        let a = s.json(path);
        assert_eq!(a[0]["address"]["house_number"], "3", "{path}: {a}");
        assert_eq!(a[0]["address"]["country_code"], "rs", "{path}: {a}");
        assert_eq!(a[0]["licence"], "fixture data of rs", "{path}: {a}");
    }
    assert_eq!(
        s.json("/search?q=ulica+b+3+grad&countrycodes=fr"),
        serde_json::json!([]),
        "a country this server has no sheet of answers nothing"
    );
    let geo = s.json("/search?q=ulica+b+3+grad,+Serbia&format=geojson");
    assert_eq!(geo["licence"], "fixture data of rs", "{geo}");

    // Google: components=country: picks the sheet, the country component is that sheet's
    let g = s.json("/maps/api/geocode/json?address=ulica+b+3+grad&components=country:RS");
    assert_eq!(g["status"], "OK", "{g}");
    let country = g["results"][0]["address_components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| {
            c["types"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t == "country")
        })
        .cloned()
        .unwrap();
    assert_eq!(
        country["short_name"].as_str().unwrap().to_ascii_lowercase(),
        "rs",
        "{g}"
    );
    assert_eq!(
        s.json("/maps/api/geocode/json?address=ulica+b+3+grad&components=country:FR")["status"],
        "ZERO_RESULTS"
    );

    // exists, explain, extract, reason: the sheet country= names, as a server of that sheet alone answers
    let one = Server::start(&rs, &[]);
    let same = |path: &str| {
        let both = (s.json(&format!("{path}&country=rs")), one.json(path));
        assert_eq!(both.0, both.1, "{path}");
    };
    same("/v1/exists?q=ulica+b+3+grad");
    same("/v1/explain?q=ulica+b+3+grad");
    // a street of the main US sheet asked of the Serbian one: the reason is the Serbian sheet's, not the main sheet's
    let silent = s.json("/v1/geocode?q=hobart+blvd+808&country=rs");
    let silent_one = one.json("/v1/geocode?q=hobart+blvd+808");
    assert_eq!(silent["results"], serde_json::json!([]), "{silent}");
    assert_eq!(silent["reason"], silent_one["reason"], "{silent}");
    assert_eq!(silent["reason"], "no_street", "{silent}");
    let text = r#"{"text":"Please deliver to ulica b 3 grad tomorrow."}"#;
    let (_, many) = s.post("/v1/extract?country=rs", text);
    let (_, alone) = one.post("/v1/extract", text);
    let (many, alone): (serde_json::Value, serde_json::Value) = (
        serde_json::from_str(&many).unwrap(),
        serde_json::from_str(&alone).unwrap(),
    );
    assert!(!alone["results"].as_array().unwrap().is_empty(), "{alone}");
    assert_eq!(
        many["results"][0]["hit"]["lat"], alone["results"][0]["hit"]["lat"],
        "{many}"
    );
    assert!(has(&many["results"][0]["hit"], "country_rs"), "{many}");
    drop(one);

    // reverse: the sheet with the nearest address, on every path; country= asks that sheet
    let r = top("/v1/reverse?lat=45.25&lon=19.841");
    assert!(has(&r, "country_rs") && r["housenumber"] == "3", "{r}");
    // country= asks that sheet alone: the US sheet has no address near Novi Sad, and says so
    let forced = s.json("/v1/reverse?lat=45.25&lon=19.841&country=us");
    assert_eq!(forced["results"], serde_json::json!([]), "{forced}");
    let la = top("/v1/reverse?lat=34.0573&lon=-118.3058");
    assert!(has(&la, "country_us"), "{la}");
    let n = s.json("/reverse?lat=45.25&lon=19.841&format=jsonv2");
    assert_eq!(n["address"]["country_code"], "rs", "{n}");
    assert_eq!(n["licence"], "fixture data of rs", "{n}");
    let gr = s.json("/maps/api/geocode/json?latlng=45.25,19.841");
    assert_eq!(gr["status"], "OK", "{gr}");

    // /health and / name every sheet
    let health = s.json("/health");
    let countries: Vec<&str> = health["sheets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["country"].as_str().unwrap())
        .collect();
    assert_eq!(countries, ["us", "ca", "rs"], "{health}");
    assert_eq!(health["sheet"]["country"], "us");
    let home = s.get("/").1;
    assert!(
        home.contains("rs.bin (rs)") && home.contains("ca.bin (ca)"),
        "{home}"
    );
}
