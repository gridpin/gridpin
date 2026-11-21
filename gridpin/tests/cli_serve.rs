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
    assert_eq!(s.get("/search?q=x&format=html").0, 400);
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

#[test]
fn the_nominatim_prefix_answers_as_the_root_and_nothing_else_moves_under_it() {
    let (_dir, bin) = sheet("prefix");
    let s = Server::start(&bin, &[]);
    for target in [
        "/search?q=1+rue+a+ville&format=jsonv2&addressdetails=1&namedetails=1",
        "/search.php?q=rue+a+ville&format=json",
        "/search?q=ville&format=geojson",
        "/reverse?lat=43.73&lon=7.42&format=jsonv2",
        "/reverse.php?lat=43.73&lon=7.42",
        "/status?format=json",
        "/status.php",
    ] {
        let root = s.get(target);
        let prefixed = s.get(&format!("/nominatim{target}"));
        assert_eq!(root.0, 200, "{target}: {}", root.1);
        assert_eq!(prefixed, root, "{target}");
    }
    // only the Nominatim paths take the prefix: the native and Google paths keep their own address
    for target in [
        "/nominatim/v1/geocode?q=ville",
        "/nominatim/maps/api/geocode/json?address=ville",
        "/nominatim/",
        "/nominatim",
    ] {
        assert_eq!(s.get(target).0, 404, "{target}");
    }
}

#[test]
fn a_nominatim_answer_carries_name_addresstype_country_and_namedetails() {
    let (_dir, bin) = sheet("nominatim-fields");
    let s = Server::start(&bin, &[]);
    let house = &s.json("/search?q=1+rue+a+ville&format=jsonv2&addressdetails=1&namedetails=1")[0];
    assert_eq!(house["name"], "");
    assert_eq!(house["addresstype"], "place");
    assert_eq!(house["namedetails"], serde_json::json!({}));
    assert_eq!(house["address"]["country"], "Monaco");
    assert_eq!(house["address"]["country_code"], "mc");
    let street = &s.json("/search?q=rue+a+ville&format=json&namedetails=1")[0];
    assert_eq!(
        (street["type"].as_str(), street["name"].as_str()),
        (Some("street"), Some("Rue A"))
    );
    assert_eq!(
        street["place_rank"], 26,
        "json writes place_rank as Nominatim does"
    );
    assert_eq!(street["namedetails"]["name"], "Rue A");
    let city = &s.json("/search?q=ville&format=geojson")["features"][0]["properties"];
    assert_eq!(
        (city["name"].as_str(), city["addresstype"].as_str()),
        (Some("Ville"), Some("city"))
    );
    let back = s.json("/reverse?lat=43.73&lon=7.42&format=json");
    assert_eq!(
        back["address"]["country"], "Monaco",
        "reverse has address details by default"
    );
    assert!(
        back.get("namedetails").is_none(),
        "namedetails only when asked"
    );
    let named = s.json("/reverse?lat=43.73&lon=7.42&format=jsonv2&namedetails=1");
    assert_eq!(
        named["namedetails"],
        serde_json::json!({}),
        "reverse reads namedetails too"
    );
}

#[test]
fn a_preflight_gets_back_the_headers_it_asked_for() {
    let (_dir, bin) = sheet("preflight");
    let s = Server::start(&bin, &[]);
    let (code, head, _) = s.raw(
        "OPTIONS /search HTTP/1.1\r\nOrigin: http://example.org\r\nAccess-Control-Request-Method: GET\r\n\
         Access-Control-Request-Headers: user-agent, x-foo\r\n\r\n",
    );
    assert_eq!(code, 204);
    assert!(
        head.contains("Access-Control-Allow-Headers: user-agent, x-foo\r\n"),
        "{head}"
    );
    let (_, head, _) =
        s.raw("OPTIONS /search HTTP/1.1\r\nAccess-Control-Request-Headers: a;b\r\n\r\n");
    assert!(
        head.contains("Access-Control-Allow-Headers: Content-Type\r\n"),
        "{head}"
    );
    let (_, head, _) = s.raw("GET /health HTTP/1.1\r\n\r\n");
    assert!(
        head.contains("Access-Control-Allow-Headers: Content-Type\r\n"),
        "{head}"
    );
}

/// The coordinates of a Photon answer, feature by feature, beside those of `/v1/geocode` asked for as many.
fn photon_points(v: &serde_json::Value) -> Vec<(f64, f64)> {
    v["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let c = &f["geometry"]["coordinates"];
            (c[1].as_f64().unwrap(), c[0].as_f64().unwrap())
        })
        .collect()
}

fn native_points(v: &serde_json::Value) -> Vec<(f64, f64)> {
    v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| (h["lat"].as_f64().unwrap(), h["lon"].as_f64().unwrap()))
        .collect()
}

#[test]
fn the_photon_paths_answer_what_the_native_api_answers_in_photon_shape() {
    let (_dir, bin) = sheet_of(
        "photon-shape",
        "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\nrue a,001,ville,10000,3,,7.4205,43.7302,Rue A,Ville\n",
    );
    let s = Server::start(&bin, &[]);
    let v = s.json("/photon/api?q=1+rue+a+ville");
    assert_eq!(v["type"], "FeatureCollection");
    // Photon's default limit is 15: the same list as the native API asked for 15
    assert_eq!(
        photon_points(&v),
        native_points(&s.json("/v1/geocode?q=1+rue+a+ville&k=15"))
    );
    let p = &v["features"][0]["properties"];
    assert_eq!(v["features"][0]["type"], "Feature");
    assert_eq!(
        (
            p["type"].as_str(),
            p["housenumber"].as_str(),
            p["street"].as_str()
        ),
        (Some("house"), Some("1"), Some("Rue A"))
    );
    assert_eq!(
        (p["countrycode"].as_str(), p["country"].as_str()),
        (Some("MC"), Some("Monaco"))
    );
    assert_eq!(p["extra"]["match_level"], "PointAddress");
    assert!(p.get("osm_id").is_none(), "no OSM object is invented: {p}");
    // the path with a slash at the end answers the same (Mobilizon asks /api/)
    assert_eq!(
        s.get("/photon/api/?q=1+rue+a+ville"),
        s.get("/photon/api?q=1+rue+a+ville")
    );
    let one = s.json("/photon/api?q=1+rue+a+ville&limit=1");
    assert_eq!(
        photon_points(&one),
        native_points(&s.json("/v1/geocode?q=1+rue+a+ville&k=1"))
    );
    // limit is held within its bounds, as Photon does, not refused
    assert_eq!(
        s.json("/photon/api?q=1+rue+a+ville&limit=0")["features"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // an unknown parameter is skipped: Traccar adds `key`, Atlas `dedupe`; an excluded tag is one no answer carries
    assert_eq!(
        s.get("/photon/api?q=1+rue+a+ville&key=k&lang=fr&zoom=12&dedupe=1&osm_tag=!place"),
        s.get("/photon/api?q=1+rue+a+ville")
    );
    // nothing found is an empty array, never null or an absent key (geopy reads `features` by key)
    assert_eq!(
        s.json("/photon/api?q=zzzz+qqqq")["features"],
        serde_json::json!([])
    );
}

#[test]
fn photon_filters_by_layer_box_country_and_point_without_new_answers() {
    let (_dir, bin) = sheet_of(
        "photon-filters",
        "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\nrue a,001,ville,10000,3,,7.4205,43.7302,Rue A,Ville\n\
         rue a,002,autre,10001,1,,7.50,43.80,Rue A,Autre\n",
    );
    let s = Server::start(&bin, &[]);
    let n = |path: &str| s.json(path)["features"].as_array().unwrap().len();
    // the nearest answer of a point is a house: a filter on streets leaves nothing
    assert_eq!(n("/photon/reverse?lat=43.73&lon=7.42&layer=street"), 0);
    assert_eq!(
        n("/photon/reverse?lat=43.73&lon=7.42&layer=house"),
        n("/photon/reverse?lat=43.73&lon=7.42")
    );
    assert_eq!(
        n("/photon/api?q=1+rue+a+ville&layer=house"),
        n("/photon/api?q=1+rue+a+ville")
    );
    assert_eq!(
        n("/photon/api?q=1+rue+a+ville&layer=city"),
        0,
        "a house is not a city"
    );
    let streets = s.json("/photon/api?q=rue+a+ville&layer=street");
    assert!(streets["features"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["properties"]["type"] == "street"));
    assert!(n("/photon/api?q=rue+a+ville&layer=street") >= 1);
    // the box holds the point or does not
    assert_eq!(
        n("/photon/api?q=1+rue+a+ville&bbox=7.4,43.7,7.5,43.8"),
        n("/photon/api?q=1+rue+a+ville")
    );
    assert_eq!(n("/photon/api?q=1+rue+a+ville&bbox=8,44,9,45"), 0);
    // a filter reads the engine's longest list, not its first answer: the street of the other town is found
    let boxed = s.json("/photon/api?q=rue+a&bbox=7.45,43.75,7.55,43.85&limit=1");
    assert_eq!(
        boxed["features"][0]["properties"]["city"], "Autre",
        "{boxed}"
    );
    // countrycode names the sheet: this one is Monaco
    assert_eq!(
        n("/photon/api?q=1+rue+a+ville&countrycode=MC"),
        n("/photon/api?q=1+rue+a+ville")
    );
    assert_eq!(n("/photon/api?q=1+rue+a+ville&countrycode=de"), 0);
    // lat and lon pull toward the point as `near` of the native API does: the same street in another town wins
    let toward = s.json("/photon/api?q=1+rue+a&lat=43.80&lon=7.50&limit=2");
    assert_eq!(
        toward["features"][0]["properties"]["city"], "Autre",
        "{toward}"
    );
    assert_eq!(
        photon_points(&toward),
        native_points(&s.json("/v1/geocode?q=1+rue+a&near=43.80,7.50&k=2"))
    );
    let away = s.json("/photon/api?q=1+rue+a&lat=43.73&lon=7.42&limit=2");
    assert_eq!(away["features"][0]["properties"]["city"], "Ville", "{away}");
}

#[test]
fn photon_answers_nothing_to_a_tag_it_cannot_hold_and_ignores_an_excluded_one() {
    let (_dir, bin) = sheet_of(
        "photon-osm-tag",
        "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n",
    );
    let s = Server::start(&bin, &[]);
    let n = |path: &str| s.json(path)["features"].as_array().unwrap().len();
    let plain = n("/photon/api?q=1+rue+a+ville");
    assert!(plain >= 1);
    // Dawarich Atlas searches a category as a name plus the tags of the category: a sheet has no tags, so nothing
    for tag in ["shop", "shop:supermarket", ":cafe", "amenity:cafe"] {
        assert_eq!(
            n(&format!("/photon/api?q=1+rue+a+ville&osm_tag={tag}")),
            0,
            "{tag}"
        );
    }
    assert_eq!(
        n("/photon/api?q=1+rue+a+ville&osm_tag=!shop&osm_tag=shop:supermarket"),
        0,
        "one asked tag is enough"
    );
    // an exclusion of a tag no answer carries leaves every answer in place
    for tag in ["!shop", "!amenity:cafe", "shop:!bakery"] {
        assert_eq!(
            n(&format!("/photon/api?q=1+rue+a+ville&osm_tag={tag}")),
            plain,
            "{tag}"
        );
    }
    let empty = s.json("/photon/api?q=1+rue+a+ville&osm_tag=shop");
    assert_eq!(empty["type"], "FeatureCollection");
}

#[test]
fn photon_refuses_in_its_own_form_and_reverse_keeps_to_its_radius() {
    let (_dir, bin) = sheet("photon-refusals");
    let s = Server::start(&bin, &[]);
    for (target, code) in [
        ("/photon/api", 400),
        ("/photon/api?q=+", 400),
        ("/photon/api?q=x&limit=abc", 400),
        ("/photon/api?q=x&bbox=1,2,3", 400),
        ("/photon/api?q=x&bbox=9,2,3,4", 400),
        ("/photon/api?q=x&lat=43.7", 400),
        ("/photon/api?q=x&lat=95&lon=7", 400),
        ("/photon/api?q=x&layer=planet", 400),
        ("/photon/reverse?lat=43.73", 400),
        ("/photon/reverse?lat=43.73&lon=7.42&radius=-1", 400),
    ] {
        let (status, body) = s.get(target);
        assert_eq!(status, code, "{target}: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["message"].is_string(), "{target}: {body}");
    }
    let (status, body) = s.get(&format!("/photon/api?q={}", "a".repeat(2000)));
    assert_eq!(status, 414, "{body}");
    let back = s.json("/photon/reverse?lat=43.73&lon=7.42");
    assert_eq!(
        photon_points(&back),
        native_points(&s.json("/v1/reverse?lat=43.73&lon=7.42&k=15"))
    );
    assert_eq!(back["features"][0]["properties"]["housenumber"], "1");
    // Leaflet Control Geocoder asks the path with a slash at the end
    assert_eq!(
        s.get("/photon/reverse/?lat=43.73&lon=7.42"),
        s.get("/photon/reverse?lat=43.73&lon=7.42")
    );
    // about 1.7 km north: outside Photon's default radius of 1 km, inside 2 km
    assert_eq!(
        s.json("/photon/reverse?lat=43.745&lon=7.42")["features"],
        serde_json::json!([])
    );
    let far = s.json("/photon/reverse?lat=43.745&lon=7.42&radius=2");
    assert_eq!(
        far["features"][0]["properties"]["type"], "house",
        "the nearest house far away is a house"
    );
    assert_eq!(
        s.json("/photon/reverse?lat=43.745&lon=7.42&radius=2")["features"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let st = s.json("/photon/status");
    assert_eq!(
        (st["status"].as_str(), st["import_date"].as_str()),
        (Some("Ok"), Some("test-release"))
    );
}

#[test]
fn photon_on_a_server_of_several_sheets_asks_the_sheet_countrycode_names() {
    let dir = Scratch::dir("serve-photon-sheets");
    let mc = country_sheet(
        &dir,
        "mc",
        "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n",
        None,
    );
    let rs = country_sheet(&dir, "rs", RS_ROWS, None);
    let s = Server::start(&mc, &["--sheets", rs.to_str().unwrap()]);
    let v = s.json("/photon/api?q=ulica+b+1+grad&countrycode=rs");
    let p = &v["features"][0]["properties"];
    assert_eq!(
        (p["countrycode"].as_str(), p["street"].as_str()),
        (Some("RS"), Some("Ulica B"))
    );
    assert_eq!(
        photon_points(&v),
        native_points(&s.json("/v1/geocode?q=ulica+b+1+grad&k=15&country=rs"))
    );
    assert_eq!(
        s.json("/photon/api?q=ulica+b+1+grad&countrycode=de")["features"],
        serde_json::json!([])
    );
    let back = s.json("/photon/reverse?lat=45.25&lon=19.84&limit=1");
    assert_eq!(
        back["features"][0]["properties"]["countrycode"], "RS",
        "the nearest sheet answers"
    );
}

#[test]
fn the_server_writes_the_valhalla_location_the_command_line_writes() {
    let (_dir, bin) = sheet("valhalla");
    let s = Server::start(&bin, &[]);
    let (status, body) = s.get("/v1/geocode?q=1+rue+a+ville&format=valhalla");
    assert_eq!(status, 200, "{body}");
    let cli = Command::new(BIN)
        .args([
            "query",
            bin.to_str().unwrap(),
            "1 rue a ville",
            "-k",
            "1",
            "--format",
            "valhalla",
        ])
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert_eq!(
        body,
        String::from_utf8_lossy(&cli.stdout).trim_end(),
        "one line, byte for byte"
    );
    let (status, body) = s.get("/v1/geocode?q=ville&format=valhalla");
    assert_eq!(status, 422, "a weak answer is no location: {body}");
    assert!(body.contains("weak"), "{body}");
    assert_eq!(s.get("/v1/geocode?q=zzzz+qqqq&format=valhalla").0, 422);
    // another format is left unread, as before format=valhalla existed
    assert_eq!(
        s.get("/v1/geocode?q=1+rue+a+ville&format=jsonv2"),
        s.get("/v1/geocode?q=1+rue+a+ville")
    );
}

#[test]
fn xml_is_answered_as_xml_with_the_coordinates_of_jsonv2() {
    let (_dir, bin) = sheet("nominatim-xml");
    let s = Server::start(&bin, &[]);
    let (status, head, body) =
        s.raw("GET /search?q=1+rue+a+ville&format=xml&addressdetails=1 HTTP/1.1\r\n\r\n");
    assert_eq!(status, 200, "{body}");
    assert!(
        head.contains("Content-Type: text/xml; charset=utf-8"),
        "{head}"
    );
    let v2 = &s.json("/search?q=1+rue+a+ville&format=jsonv2")[0];
    let lat = v2["lat"].as_str().unwrap();
    assert!(body.contains(&format!(" lat=\"{lat}\"")), "{body}");
    assert!(body.contains("<road>Rue A</road>"), "{body}");
    // the same under the prefix, and json_callback leaves an XML answer as it is
    assert_eq!(
        s.get("/nominatim/search?q=1+rue+a+ville&format=xml&addressdetails=1")
            .1
            .split("querystring")
            .nth(1),
        body.split("querystring").nth(1)
    );
    let (_, wrapped) = s.get("/search?q=1+rue+a+ville&format=xml&json_callback=cb");
    assert!(wrapped.starts_with("<?xml"), "{wrapped}");
    let (status, back) = s.get("/reverse?lat=43.73&lon=7.42&format=xml");
    assert_eq!(status, 200);
    assert!(
        back.contains("<addressparts>") && back.contains("<house_number>1</house_number>"),
        "{back}"
    );
    assert!(s
        .get("/reverse?lat=0&lon=0&format=xml")
        .1
        .contains("<error>Unable to geocode</error>"));
    // reverse without format keeps answering jsonv2, as since 0.7.0
    assert!(s
        .json("/reverse?lat=43.73&lon=7.42")
        .get("category")
        .is_some());
}

#[test]
fn geocodejson_carries_the_coordinates_of_jsonv2_in_every_feature() {
    let (_dir, bin) = sheet("nominatim-geocodejson");
    let s = Server::start(&bin, &[]);
    let v = s.json("/search?q=1+rue+a+ville&format=geocodejson&addressdetails=1&namedetails=1");
    assert_eq!(v["geocoding"]["query"], "1 rue a ville");
    assert_eq!(
        v["geocoding"]["licence"], "t",
        "the licence of the sheet's passport"
    );
    let f = &v["features"][0];
    let v2 = &s.json("/search?q=1+rue+a+ville&format=jsonv2")[0];
    let c = &f["geometry"]["coordinates"];
    assert_eq!(
        format!("{:.7}", c[1].as_f64().unwrap()),
        v2["lat"].as_str().unwrap()
    );
    assert_eq!(
        format!("{:.7}", c[0].as_f64().unwrap()),
        v2["lon"].as_str().unwrap()
    );
    assert_eq!(f["properties"]["geocoding"]["housenumber"], "1");
    assert_eq!(
        s.json("/search?q=zzzz+qqqq&format=geocodejson")["features"],
        serde_json::json!([])
    );
    let back = s.json("/reverse?lat=43.7301&lon=7.42&format=geocodejson");
    assert_eq!(
        back["features"][0]["properties"]["geocoding"]["accuracy"],
        11
    );
    assert_eq!(
        s.json("/reverse?lat=0&lon=0&format=geocodejson"),
        serde_json::json!({"error": "Unable to geocode"})
    );
}

#[test]
fn reverse_zoom_answers_house_street_or_town_and_holds_19_at_18() {
    let (_dir, bin) = sheet("nominatim-zoom");
    let s = Server::start(&bin, &[]);
    let at = |z: &str| {
        s.json(&format!(
            "/reverse?lat=43.73&lon=7.42&format=jsonv2&zoom={z}"
        ))
    };
    assert_eq!(at("19"), at("18"), "TeslaMate asks 19");
    assert_eq!(at("18")["type"], "house");
    assert_eq!(at("16")["type"], "street");
    assert!(at("16")["address"].get("house_number").is_none());
    assert_eq!(at("13")["type"], "city");
    assert_eq!(at("13")["name"], "Ville");
    assert_eq!(s.get("/reverse?lat=43.73&lon=7.42&zoom=x").0, 400);
    assert_eq!(
        s.json("/reverse?lat=43.73&lon=7.42&format=jsonv2"),
        at("18"),
        "18 is the default"
    );
}

#[test]
fn without_ids_no_answer_carries_an_object_number_and_with_ids_every_format_does() {
    let (_dir, bin) = sheet("nominatim-ids");
    let plain = Server::start(&bin, &[]);
    let ids = Server::start(&bin, &["--ids"]);
    let paths = [
        "/search?q=1+rue+a+ville&format=jsonv2",
        "/search?q=1+rue+a+ville&format=json",
        "/search?q=1+rue+a+ville&format=geojson",
        "/search?q=1+rue+a+ville&format=geocodejson",
        "/search?q=1+rue+a+ville&format=xml",
        "/reverse?lat=43.73&lon=7.42&format=jsonv2",
        "/photon/api?q=1+rue+a+ville",
    ];
    for path in paths {
        let (_, body) = plain.get(path);
        assert!(!body.contains("osm_id"), "{path}: {body}");
        let (_, body) = ids.get(path);
        assert!(body.contains("osm_id"), "{path}: {body}");
    }
    let v = &ids.json("/search?q=1+rue+a+ville&format=jsonv2")[0];
    assert_eq!(v["osm_type"], "node");
    assert!(
        v["osm_id"].as_i64().unwrap() < 0,
        "negative: never an OpenStreetMap object"
    );
    let p = &ids.json("/photon/api?q=1+rue+a+ville")["features"][0]["properties"];
    assert_eq!(
        (p["osm_type"].as_str(), &p["osm_id"]),
        (Some("N"), &v["osm_id"])
    );
    let g = &ids.json("/search?q=1+rue+a+ville&format=geocodejson")["features"][0]["properties"]
        ["geocoding"];
    assert_eq!(g["osm_id"], v["osm_id"]);
    let (_, x) = ids.get("/search?q=1+rue+a+ville&format=xml");
    assert!(
        x.contains(&format!(" osm_type=\"node\" osm_id=\"{}\"", v["osm_id"])),
        "{x}"
    );
}

#[test]
fn lookup_answers_the_house_of_its_own_number_and_nothing_else() {
    let (_dir, bin) = sheet_of(
        "nominatim-lookup",
        "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\nrue a,001,ville,10000,3,,7.4205,43.7302,Rue A,Ville\n",
    );
    let s = Server::start(&bin, &["--ids"]);
    let one = s.json("/search?q=1+rue+a+ville&format=jsonv2")[0].clone();
    let three = s.json("/search?q=3+rue+a+ville&format=jsonv2")[0].clone();
    let (a, b) = (
        one["osm_id"].as_i64().unwrap(),
        three["osm_id"].as_i64().unwrap(),
    );
    assert_ne!(a, b);
    let found = s.json(&format!("/lookup?osm_ids=N{a},N{b}&format=jsonv2"));
    assert_eq!(found.as_array().unwrap().len(), 2, "{found}");
    assert_eq!(
        (&found[0]["lat"], &found[0]["lon"], &found[0]["osm_id"]),
        (&one["lat"], &one["lon"], &one["osm_id"])
    );
    assert_eq!(
        found[1]["address"]["house_number"], "3",
        "address details are on by default"
    );
    // someone else's number (an OpenStreetMap id), a way, garbage and a street's number give nothing
    let street = s.json("/search?q=rue+a+ville&format=jsonv2")[0]["osm_id"]
        .as_i64()
        .unwrap();
    for bad in [
        "N123",
        "W-5",
        "garbage",
        &format!("N{street}"),
        &format!("N{}", a - 1),
    ] {
        assert_eq!(
            s.json(&format!("/lookup?osm_ids={bad}")),
            serde_json::json!([]),
            "{bad}"
        );
    }
    let many = vec![format!("N{a}"); 51].join(",");
    assert_eq!(s.get(&format!("/lookup?osm_ids={many}")).0, 400);
    assert_eq!(s.get("/lookup").0, 400);
    let (_, x) = s.get(&format!("/lookup?osm_ids=N{a}&format=xml"));
    assert!(
        x.contains("<lookupresults ") && x.contains("<house_number>1</house_number>"),
        "{x}"
    );
    assert_eq!(
        s.json(&format!("/nominatim/lookup?osm_ids=N{a}")),
        s.json(&format!("/lookup?osm_ids=N{a}"))
    );
}

#[test]
fn two_addresses_at_one_point_never_answer_for_each_other() {
    // a house with two numbers: the same point, two addresses
    let rows: String = (1..=12)
        .map(|n| format!("rue b,001,ville,10000,{n},,7.43,43.74,Rue B,Ville\n"))
        .collect();
    let (_dir, bin) = sheet_of("nominatim-lookup-shared", &rows);
    let s = Server::start(&bin, &["--ids"]);
    let mut parted = 0;
    for n in 1..=12 {
        let v = s.json(&format!("/search?q={n}+rue+b+ville&format=jsonv2"))[0].clone();
        assert_eq!(
            v["address"].get("house_number"),
            None,
            "no address details asked"
        );
        let id = v["osm_id"].as_i64().unwrap();
        let found = s.json(&format!("/lookup?osm_ids=N{id}"));
        match found.as_array().unwrap().as_slice() {
            [] => {}
            [one] => {
                assert_eq!(
                    one["address"]["house_number"],
                    n.to_string(),
                    "another house under the number of {n}"
                );
                parted += 1;
            }
            more => panic!("{} answers for one number", more.len()),
        }
    }
    // three bits part most of twelve numbers at one point; those that share them give no answer
    assert!(parted >= 3, "{parted} of 12 found back");
}

#[test]
fn more_addresses_at_one_point_than_the_lookup_sees_answer_for_none_of_them() {
    // 41 numbers at one point: more than the 40 nearest answers /lookup reads. The numbers are chosen so that among
    // the 40 it sees (the smallest) exactly one, 1, shares the three address bits of the hidden 900: read as all of
    // the cell, the list would answer 1 for 900
    let numbers = [
        1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 24, 25, 26, 27, 28,
        29, 31, 32, 33, 34, 35, 36, 37, 39, 40, 42, 43, 44, 45, 46, 900,
    ];
    let rows: String = numbers
        .iter()
        .map(|n| format!("rue c,001,ville,10000,{n},,7.44,43.75,Rue C,Ville\n"))
        .collect();
    let (_dir, bin) = sheet_of("nominatim-lookup-crowded", &rows);
    let s = Server::start(&bin, &["--ids"]);
    for n in numbers {
        let id = s.json(&format!("/search?q={n}+rue+c+ville&format=jsonv2"))[0]["osm_id"]
            .as_i64()
            .unwrap();
        let found = s.json(&format!("/lookup?osm_ids=N{id}"));
        for f in found.as_array().unwrap() {
            assert_eq!(
                f["address"]["house_number"],
                n.to_string(),
                "another house under the number of {n}"
            );
        }
    }
}

#[test]
fn the_darktable_search_word_for_word_gets_places_with_what_darktable_reads() {
    // darktable's own default, plugins/map/geotagging_search_url, with %s and %d filled as it fills them
    // (src/libs/location.c: g_strdup_printf(search_url, text, 5)); it reads display_name, lat, lon, boundingbox, type
    let (_dir, bin) = sheet("darktable");
    let s = Server::start(&bin, &[]);
    let (status, body) = s.get("/search?q=1+rue+a+ville&format=xml&limit=5&polygon_text=1");
    assert_eq!(status, 200, "{body}");
    let place = body
        .split("<place ")
        .nth(1)
        .unwrap_or_else(|| panic!("no <place>: {body}"));
    for attribute in [
        "display_name=\"1 Rue A, Ville, 10000\"",
        "lat=\"43.7300000\"",
        "lon=\"7.4200000\"",
        "boundingbox=\"",
        "type=\"house\"",
    ] {
        assert!(place.contains(attribute), "{attribute}: {place}");
    }
    let bbox = place
        .split("boundingbox=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    assert_eq!(
        bbox.split(',').count(),
        4,
        "four numbers, latitudes then longitudes: {bbox}"
    );
}

/// Step A.3 of plan K1/K9: the BAN (Addok) dialect answers as the French service does, Lambert-93 on France only.
#[test]
fn ban_search_and_reverse_answer_in_the_shape_of_the_french_service() {
    let dir = Scratch::dir("serve-ban-fr");
    let rows = "rue des lilas,80021,amiens,80000,8,,2.290084,49.897442,Rue des Lilas,Amiens\n\
                rue des lilas,80021,amiens,80000,10,,2.2902,49.8975,Rue des Lilas,Amiens\n";
    let bin = country_sheet(&dir, "fr", rows, None);
    let s = Server::start(&bin, &[]);
    let v = s.json("/ban/search?q=8+rue+des+lilas+amiens&limit=1");
    assert_eq!(v["type"], "FeatureCollection");
    assert_eq!(v["query"], "8 rue des lilas amiens");
    let f = &v["features"][0];
    let p = &f["properties"];
    assert_eq!(p["type"], "housenumber", "{v}");
    assert_eq!(p["housenumber"], "8");
    assert_eq!(p["street"], "Rue des Lilas");
    assert_eq!(p["name"], "8 Rue des Lilas");
    assert_eq!(p["label"], "8 Rue des Lilas 80000 Amiens");
    assert_eq!(p["postcode"], "80000");
    assert_eq!(p["city"], "Amiens");
    for absent in ["citycode", "context", "importance", "banId", "id"] {
        assert!(
            p.get(absent).is_none(),
            "{absent} is not kept in a sheet: {p}"
        );
    }
    let native = s.json("/v1/geocode?q=8+rue+des+lilas+amiens&k=1&with=crs:EPSG:2154");
    let top = &native["results"][0];
    assert_eq!(
        f["geometry"]["coordinates"],
        serde_json::json!([top["lon"], top["lat"]])
    );
    assert_eq!(
        p["score"].as_f64().unwrap(),
        top["confidence"].as_f64().unwrap()
    );
    assert_eq!(p["x"], top["extras"]["crs"]["x"]);
    assert_eq!(p["y"], top["extras"]["crs"]["y"]);
    // the trailing slash of Mobilizon's base URL is the same path
    assert_eq!(
        s.json("/ban/search/?q=8+rue+des+lilas+amiens&limit=1")["features"],
        v["features"]
    );
    let r = s.json("/ban/reverse?lat=49.89745&lon=2.29009&limit=1");
    assert_eq!(r["features"][0]["properties"]["housenumber"], "8");
    let d = r["features"][0]["properties"]["distance"].as_i64().unwrap();
    assert!((0..=10).contains(&d), "{r}");
    assert!(r.get("query").is_none());
    // a street is a street: number and street never come apart
    // a type filter keeps only that type: the houses of an address query are not a street or a town
    for t in ["street", "municipality"] {
        let only = s.json(&format!("/ban/search?q=8+rue+des+lilas+amiens&type={t}"));
        for f in only["features"].as_array().unwrap() {
            assert_eq!(f["properties"]["type"], t, "{only}");
        }
    }
    let st = s.json("/ban/search?q=rue+des+lilas+amiens&type=street");
    assert!(!st["features"].as_array().unwrap().is_empty(), "{st}");
    for f in st["features"].as_array().unwrap() {
        assert_eq!(f["properties"]["type"], "street");
        assert!(f["properties"].get("housenumber").is_none(), "{f}");
    }
    assert!(
        !s.json("/ban/search?q=8+rue+des+lilas+amiens&postcode=80000")["features"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s.json("/ban/search?q=8+rue+des+lilas+amiens&postcode=80001")["features"],
        serde_json::json!([])
    );
}

#[test]
fn ban_refuses_what_it_cannot_check_in_its_own_form_and_scores_a_weak_answer_low() {
    let (_d, bin) = sheet_of(
        "ban-mc",
        "rue des lilas,001,ville,10000,1,,7.42,43.73,Rue des Lilas,Ville\n\
         rue des lilas,001,ville,10000,3,,7.4205,43.7302,Rue des Lilas,Ville\n",
    );
    let s = Server::start(&bin, &["--ids"]);
    let ok = s.json("/ban/search?q=1+rue+des+lilas+ville&limit=1");
    let p = &ok["features"][0]["properties"];
    assert!(
        p.get("x").is_none() && p.get("y").is_none(),
        "Lambert-93 is for France only: {p}"
    );
    assert!(
        p["id"].as_str().unwrap().starts_with('-'),
        "with --ids the answer carries its own number: {p}"
    );
    for (path, word) in [
        ("/ban/search?q=", "q: required param"),
        ("/ban/search?q=1+rue+des+lilas&citycode=75056", "citycode"),
        ("/ban/search?q=1+rue+des+lilas&type=house", "type"),
        ("/ban/search?q=1+rue+des+lilas&index=poi", "index"),
        ("/ban/reverse?lat=43.73", "lat and lon"),
        ("/ban/search?q=1+rue+des+lilas&limit=x", "limit"),
    ] {
        let (status, body) = s.get(path);
        assert_eq!(status, 400, "{path}: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["code"], 400, "{path}");
        assert_eq!(v["message"], "Failed parsing query", "{path}");
        assert!(
            v["detail"][0].as_str().unwrap().contains(word),
            "{path}: {body}"
        );
    }
    let native = s.json("/v1/geocode?q=1+rue+des+lila&k=1");
    assert_eq!(native["results"][0]["weak"], true, "{native}");
    let weak = s.json("/ban/search?q=1+rue+des+lila&limit=1");
    let score = weak["features"][0]["properties"]["score"].as_f64().unwrap();
    assert!(score <= 0.4, "a weak answer never scores above 0.4: {weak}");
}

#[test]
fn census_lists_only_firm_houses_as_address_matches_and_refuses_as_the_census_service() {
    let (_d, bin) = sheet_of(
        "census",
        "rue des lilas,001,ville,10000,1,,7.42,43.73,Rue des Lilas,Ville\n\
         rue des lilas,001,ville,10000,3,,7.4205,43.7302,Rue des Lilas,Ville\n",
    );
    let s = Server::start(&bin, &[]);
    let base = "/census/geocoder/locations";
    let v = s.json(&format!("{base}/onelineaddress?address=1+rue+des+lilas+ville&benchmark=Public_AR_Current&format=json"));
    assert_eq!(
        v["result"]["input"]["address"]["address"],
        "1 rue des lilas ville"
    );
    assert_eq!(
        v["result"]["input"]["benchmark"]["benchmarkName"],
        "Public_AR_Current"
    );
    let m = &v["result"]["addressMatches"][0];
    assert_eq!(m["matchedAddress"], "1 RUE DES LILAS, VILLE, 10000");
    assert_eq!(m["addressComponents"]["fromAddress"], "1");
    assert_eq!(m["addressComponents"]["zip"], "10000");
    assert!(m.get("tigerLine").is_none());
    let native = s.json("/v1/geocode?q=1+rue+des+lilas+ville&k=1");
    assert_eq!(m["coordinates"]["x"], native["results"][0]["lon"]);
    assert_eq!(m["coordinates"]["y"], native["results"][0]["lat"]);
    // a street, the neighbour's door and a weak answer are not address matches
    for q in [
        "rue+des+lilas+ville",
        "9+rue+des+lilas+ville",
        "1+rue+des+lila",
    ] {
        let n = s.json(&format!("{base}/onelineaddress?address={q}&benchmark=4"));
        assert_eq!(
            n["result"]["addressMatches"],
            serde_json::json!([]),
            "{q}: {n}"
        );
    }
    let parts = s.json(&format!(
        "{base}/address?street=1+rue+des+lilas&city=ville&zip=10000&benchmark=4&format=json"
    ));
    assert_eq!(
        parts["result"]["addressMatches"][0]["addressComponents"]["fromAddress"],
        "1"
    );
    assert_eq!(
        parts["result"]["input"]["address"]["street"],
        "1 rue des lilas"
    );
    for (path, status, word) in [
        (
            format!("{base}/address?city=ville&state=DC&benchmark=4"),
            400,
            "Street address cannot be empty",
        ),
        (
            format!("{base}/onelineaddress?address=1+rue+des+lilas&benchmark=garbage"),
            400,
            "Invalid benchmark",
        ),
        (
            format!("{base}/onelineaddress?benchmark=4"),
            400,
            "Address cannot be empty",
        ),
        (
            "/census/geocoder/geographies/onelineaddress?address=x&benchmark=4&vintage=4"
                .to_string(),
            501,
            "gridpin-pro",
        ),
    ] {
        let (got, body) = s.get(&path);
        assert_eq!(got, status, "{path}: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["status"], status.to_string(), "{path}");
        assert!(
            v["errors"][0].as_str().unwrap().contains(word),
            "{path}: {body}"
        );
    }
}

fn multipart(fields: &[(&str, &str)]) -> (String, String) {
    let boundary = "gpXboundary";
    let mut body = String::new();
    for (name, value) in fields {
        let file = if *name == "addressFile" {
            "; filename=\"a.csv\"\r\nContent-Type: text/csv"
        } else {
            ""
        };
        body.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"{file}\r\n\r\n{value}\r\n"));
    }
    body.push_str(&format!("--{boundary}--\r\n"));
    (format!("multipart/form-data; boundary={boundary}"), body)
}

#[test]
fn census_batch_answers_a_csv_line_per_row_on_the_batch_path_and_on_the_one_line_path() {
    let (_d, bin) = sheet_of(
        "census-batch",
        "rue des lilas,001,ville,10000,1,,7.42,43.73,Rue des Lilas,Ville\n\
         rue des lilas,001,ville,10000,3,,7.4205,43.7302,Rue des Lilas,Ville\n",
    );
    let s = Server::start(&bin, &[]);
    let file = "7,1 rue des lilas,ville,,10000\n8,qqqq zzzz,,,\n9,1 rue des lila,,,\n";
    for path in [
        "/census/geocoder/locations/addressbatch",
        "/census/geocoder/locations/onelineaddress",
    ] {
        let (ct, body) = multipart(&[
            ("addressFile", file),
            ("benchmark", "Public_AR_Current"),
            ("vintage", "Current_Current"),
        ]);
        let (status, _, out) = s.raw(&format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ));
        assert_eq!(status, 200, "{path}: {out}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert!(lines[0].starts_with("\"7\",\"1 rue des lilas, ville, , 10000\",\"Match\",\"Exact\",\"1 RUE DES LILAS, VILLE, 10000\",\"7.42,43.73\""), "{out}");
        assert_eq!(lines[1], "\"8\",\"qqqq zzzz, , , \",\"No_Match\"", "{out}");
        assert!(
            lines[2].contains("\"No_Match\""),
            "a weak answer is no match: {out}"
        );
    }
    let (ct, body) = multipart(&[("addressFile", file), ("benchmark", "garbage")]);
    let (status, _, out) = s.raw(&format!(
        "POST /census/geocoder/locations/addressbatch HTTP/1.1\r\nHost: x\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    assert_eq!(status, 400);
    assert!(out.contains("Invalid benchmark"), "{out}");
    let (ct, body) = multipart(&[("benchmark", "4")]);
    let (status, _, out) = s.raw(&format!(
        "POST /census/geocoder/locations/addressbatch HTTP/1.1\r\nHost: x\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    assert_eq!(status, 400);
    assert!(out.contains("addressFile"), "{out}");
    // other paths still take GET only
    let (status, _, _) =
        s.raw("POST /ban/search?q=x HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n");
    assert_eq!(status, 405);
}

/// Step B.5 of plan K1/K9: the Pelias dialect answers in Pelias' GeoJSON, its gid opens the place again, and without
/// --ids there is no gid at all.
#[test]
fn pelias_paths_answer_in_its_shape_and_a_gid_opens_its_place() {
    let (_d, bin) = sheet_of(
        "pelias",
        "rue des lilas,001,ville,10000,1,,7.42,43.73,Rue des Lilas,Ville\n\
         rue des lilas,001,ville,10000,3,,7.4205,43.7302,Rue des Lilas,Ville\n",
    );
    let s = Server::start(&bin, &["--ids"]);
    let v = s.json("/pelias/v1/search?text=1+rue+des+lilas+ville&size=1");
    assert_eq!(v["type"], "FeatureCollection");
    assert_eq!(v["geocoding"]["query"]["text"], "1 rue des lilas ville");
    let f = &v["features"][0];
    let p = &f["properties"];
    assert_eq!(p["layer"], "address");
    assert_eq!(p["housenumber"], "1");
    assert_eq!(p["street"], "Rue des Lilas");
    assert_eq!(p["locality"], "Ville");
    assert_eq!(p["postalcode"], "10000");
    assert_eq!(p["match_type"], "exact");
    assert_eq!(p["source"], "gridpin");
    let native = s.json("/v1/geocode?q=1+rue+des+lilas+ville&k=1");
    assert_eq!(
        f["geometry"]["coordinates"],
        serde_json::json!([native["results"][0]["lon"], native["results"][0]["lat"]])
    );
    let gid = p["gid"].as_str().unwrap().to_string();
    assert!(gid.starts_with("gridpin:address:-"), "{gid}");
    let place = s.json(&format!("/pelias/v1/place?ids={gid}"));
    assert_eq!(place["features"][0]["geometry"], f["geometry"]);
    // only our own source's numbers are read: the same number under another source opens nothing
    let number = gid.trim_start_matches("gridpin:address:");
    assert_eq!(
        s.json(&format!(
            "/pelias/v1/place?ids=openstreetmap:address:{number}"
        ))["features"],
        serde_json::json!([])
    );
    // autocomplete answers each suggestion of the typed start with its address, which carries a gid too: the
    // street of "rue des l" comes back as the firm answer to "Rue des Lilas, Ville", not the weak guess of the start
    let ac = s.json("/pelias/v1/autocomplete?text=rue+des+l&size=3");
    let first = &ac["features"][0]["properties"];
    assert_eq!(first["name"], "Rue des Lilas", "{ac}");
    assert!(first["gid"].is_string());
    assert!(first["confidence"].as_f64().unwrap() > 0.3, "{ac}");
    let r = s.json("/pelias/v1/reverse?point.lat=43.73&point.lon=7.42&size=1");
    assert_eq!(r["features"][0]["properties"]["housenumber"], "1");
    // a weak answer is never an exact match
    let weak = s.json("/pelias/v1/search?text=1+rue+des+lila&size=1");
    assert_ne!(
        weak["features"][0]["properties"]["match_type"], "exact",
        "{weak}"
    );
    // layers keep only that layer; another country answers nothing; the sheet's own, by two or three letters, answers
    for f in s.json("/pelias/v1/search?text=1+rue+des+lilas+ville&layers=street")["features"]
        .as_array()
        .unwrap()
    {
        assert_eq!(f["properties"]["layer"], "street");
    }
    assert_eq!(
        s.json("/pelias/v1/search?text=1+rue+des+lilas&boundary.country=DEU")["features"],
        serde_json::json!([])
    );
    assert!(
        !s.json("/pelias/v1/search?text=1+rue+des+lilas&boundary.country=MCO")["features"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for (path, word) in [
        ("/pelias/v1/search?text=", "text"),
        ("/pelias/v1/autocomplete", "text"),
        ("/pelias/v1/reverse?point.lat=43.73", "point.lat"),
        ("/pelias/v1/place", "ids"),
        ("/pelias/v1/search?text=x&size=y", "size"),
    ] {
        let (status, body) = s.get(path);
        assert_eq!(status, 400, "{path}: {body}");
        let e: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            e["geocoding"]["errors"][0].as_str().unwrap().contains(word),
            "{path}: {body}"
        );
    }
    drop(s);
    // without --ids there is no number to open, and no gid at all
    let plain = Server::start(&bin, &[]);
    let p = &plain.json("/pelias/v1/search?text=1+rue+des+lilas+ville&size=1")["features"][0]
        ["properties"];
    assert!(p.get("gid").is_none() && p.get("id").is_none(), "{p}");
}

/// GET /v1/addresses (address finder): the houses of a postcode, narrowed by the number and by street words.
#[test]
fn addresses_of_a_postcode_narrowed_by_number_and_street() {
    let (_dir, bin) = sheet_of(
        "addresses",
        "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n\
         rue a,001,ville,10000,1,b,7.4201,43.7301,Rue A,Ville\n\
         rue a,001,ville,10000,12,,7.4202,43.7302,Rue A,Ville\n\
         rue b,001,ville,10000,1,,7.43,43.74,Rue B,Ville\n\
         rue c,002,autre,20000,1,,7.90,43.90,Rue C,Autre\n",
    );
    let srv = Server::start(&bin, &[]);
    let labels = |target: &str| -> (Vec<String>, bool) {
        let v = srv.json(target);
        let list = v["addresses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["label"].as_str().unwrap().to_string())
            .collect();
        (list, v["more"].as_bool().unwrap())
    };
    let (all, more) = labels("/v1/addresses?postcode=10000");
    assert_eq!(all.len(), 4, "{all:?}");
    assert!(!more && all.iter().all(|l| l.contains("10000")), "{all:?}");
    let (ones, _) = labels("/v1/addresses?postcode=10000&number=1");
    assert_eq!(ones.len(), 3, "1, 1b and the 1 of Rue B; not 12: {ones:?}");
    assert!(ones.iter().all(|l| !l.contains(" 12")), "{ones:?}");
    let (b, _) = labels("/v1/addresses?postcode=10000&number=rue+b+1");
    assert_eq!(b.len(), 1, "{b:?}");
    assert!(b[0].contains("Rue B"), "{b:?}");
    let (cut, more) = labels("/v1/addresses?postcode=10000&limit=1");
    assert_eq!(cut.len(), 1);
    assert!(more, "a cut list says so");
    let (none, more) = labels("/v1/addresses?postcode=99999");
    assert!(none.is_empty() && !more);
    let (other, _) = labels("/v1/addresses?postcode=20000");
    assert_eq!(other.len(), 1);
    assert!(other[0].contains("Autre"));
    let (status, body) = srv.get("/v1/addresses");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("missing postcode"));
    let (status, _) = srv.get("/v1/addresses?postcode=10000&limit=0");
    assert_eq!(status, 400);
}

/// --postcode-layer (Brazil's eight-digit CEP, plan K6.3): a query that is only a code answers its street from the
/// layer, on the CLI and in /v1/geocode alike; /ws/<cep>/json/ answers in the shape of ViaCEP; every other query and
/// a server without the layer answer as before.
#[test]
fn a_postcode_layer_answers_a_code_alone_and_the_viacep_path() {
    let (dir, bin) = sheet("postcode-layer");
    let layer = dir.join("cep.tsv");
    std::fs::write(
        &layer,
        "#gridpin postcode layer 1\tcountry=BR\trelease=test\n\
         01001000\t\t\tSao Paulo\tSP\t-23.5496830\t-46.6336900\t4715\n\
         01310100\tAvenida Paulista\t\tSao Paulo\tSP\t-23.5645260\t-46.6525040\t89\n\
         01310100\tRua Stray\t\tSao Paulo\tSP\t-23.9000000\t-46.7000000\t1\n",
    )
    .unwrap();
    let (b, l) = (bin.to_str().unwrap(), layer.to_str().unwrap());

    let hits = cli(&["query", b, "01310-100", "-k", "5", "--postcode-layer", l]);
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert_eq!(hits[0]["street"], "Avenida Paulista");
    assert_eq!(hits[0]["postcode"], "01310-100");
    assert_eq!(hits[0]["precision"], "street");
    assert_eq!(hits[0]["weak"], false);
    assert_eq!(hits[1]["weak"], true, "one house of ninety: a stray street");
    // an address, not a code, answers from the sheet as without the layer
    let plain = cli(&["query", b, "1 rue a ville"]);
    assert_eq!(
        cli(&["query", b, "1 rue a ville", "--postcode-layer", l]),
        plain
    );

    let srv = Server::start(&bin, &["--postcode-layer", l]);
    let v = srv.json("/v1/geocode?q=01310100&k=1");
    assert_eq!(v["results"][0]["street"], "Avenida Paulista");
    assert_eq!(v["results"][0]["flags"][0], "postcode_layer");
    let post = srv.post(
        "/v1/geocode",
        r#"{"queries":["01310-100","1 rue a ville"]}"#,
    );
    assert_eq!(post.0, 200);
    let post: serde_json::Value = serde_json::from_str(&post.1).unwrap();
    assert_eq!(post["results"][0][0]["street"], "Avenida Paulista");
    assert_eq!(post["results"][1][0]["street"], plain[0]["street"]);

    for path in ["/ws/01310100/json/", "/ws/01310-100/json"] {
        let v = srv.json(path);
        assert_eq!(v["cep"], "01310-100", "{path}");
        assert_eq!(
            v["logradouro"], "Avenida Paulista",
            "the street with most houses"
        );
        assert_eq!(
            (v["localidade"].as_str(), v["uf"].as_str()),
            (Some("Sao Paulo"), Some("SP"))
        );
        assert!(v["lat"].as_f64().unwrap() < -23.5 && v["lon"].as_f64().unwrap() < -46.6);
        assert_eq!(v["bairro"], "", "the layer has no district");
    }
    let town = srv.json("/ws/01001000/json/");
    assert_eq!(
        (town["logradouro"].as_str(), town["localidade"].as_str()),
        (Some(""), Some("Sao Paulo"))
    );
    assert_eq!(
        srv.json("/ws/99999999/json/"),
        serde_json::json!({"erro": "true"})
    );
    for bad in ["/ws/0131010/json/", "/ws/abc/json/", "/ws/013101000/json/"] {
        assert_eq!(srv.get(bad).0, 400, "{bad}");
    }
    assert_eq!(srv.get("/ws/01310100/xml/").0, 404);
    assert_eq!(srv.json("/health")["postcode_layer"]["rows"], 3);

    let without = Server::start(&bin, &[]);
    let (status, body) = without.get("/ws/01310100/json/");
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("--postcode-layer"), "{body}");
    assert!(without.json("/health").get("postcode_layer").is_none());
    assert!(without.json("/v1/geocode?q=01310-100")["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|h| h["flags"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f != "postcode_layer")));
}

#[test]
fn suggest_near_puts_the_streets_near_the_point_first_and_changes_nothing_without_it() {
    // Rue A of Ville is the biggest (three houses); Rue A of Loin, 1 house, lies 2 km off; Rue A of Ailleurs, 1 house,
    // more than 50 km away
    let (_d, bin) = sheet_of(
        "suggest-near",
        "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n\
         rue a,001,ville,10000,2,,7.421,43.731,Rue A,Ville\n\
         rue a,001,ville,10000,3,,7.422,43.732,Rue A,Ville\n\
         rue a,002,loin,20000,1,,7.44,43.745,Rue A,Loin\n\
         rue a,003,ailleurs,30000,1,,8.5,44.5,Rue A,Ailleurs\n",
    );
    let server = Server::start(&bin, &[]);
    let communes = |target: &str| -> Vec<String> {
        let (status, body) = server.get(target);
        assert_eq!(status, 200, "{target}: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        v["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["commune"].as_str().unwrap().to_string())
            .collect()
    };
    let plain = server.get("/v1/suggest?q=rue+a").1;
    assert_eq!(
        communes("/v1/suggest?q=rue+a")[0],
        "Ville",
        "the biggest first"
    );
    // near Loin: Loin first, Ville next (within 50 km, farther), Ailleurs last (outside the radius, in the usual order)
    assert_eq!(
        communes("/v1/suggest?q=rue+a&near=43.745,7.44"),
        ["Loin", "Ville", "Ailleurs"]
    );
    // near Ailleurs: the only street within the radius comes first, the rest keep the usual order
    assert_eq!(
        communes("/v1/suggest?q=rue+a&near=44.5,8.5"),
        ["Ailleurs", "Ville", "Loin"]
    );
    // a point with nothing within the radius (the ocean): the usual order, the very same answer
    assert_eq!(server.get("/v1/suggest?q=rue+a&near=0,-30").1, plain);
    for bad in ["near=abc", "near=91,7", "near=43.7"] {
        assert_eq!(
            server.get(&format!("/v1/suggest?q=rue+a&{bad}")).0,
            400,
            "{bad}"
        );
    }
}

/// The coordinates of a Mapbox v5 answer, feature by feature.
fn carmen_points(v: &serde_json::Value) -> Vec<(f64, f64)> {
    v["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let c = &f["center"];
            (c[1].as_f64().unwrap(), c[0].as_f64().unwrap())
        })
        .collect()
}

#[test]
fn the_mapbox_v5_path_answers_what_the_native_api_answers_in_carmen_shape() {
    let (_dir, bin) = sheet_of(
        "mapbox-shape",
        "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\nrue a,001,ville,10000,3,,7.4205,43.7302,Rue A,Ville\n",
    );
    let s = Server::start(&bin, &[]);
    let base = "/geocoding/v5/mapbox.places";
    // autocomplete=false: the geocoder, the same points as the native API asked for as many
    let v = s.json(&format!(
        "{base}/1%20rue%20a%20ville.json?autocomplete=false&access_token=pk.x"
    ));
    assert_eq!(v["type"], "FeatureCollection");
    assert_eq!(
        carmen_points(&v),
        native_points(&s.json("/v1/geocode?q=1+rue+a+ville&k=5"))
    );
    let f = &v["features"][0];
    assert_eq!(f["geometry"]["coordinates"], f["center"]);
    assert_eq!(
        (f["address"].as_str(), f["text"].as_str()),
        (Some("1"), Some("Rue A"))
    );
    assert_eq!(f["place_type"][0], "address");
    assert_eq!(f["properties"]["accuracy"], "rooftop");
    assert_eq!(f["properties"]["weak"], false);
    assert!(f["id"].as_str().unwrap().starts_with("address."));
    assert_eq!(v["query"], serde_json::json!(["1", "rue", "a", "ville"]));
    // a token is never checked: any value answers the same
    assert_eq!(
        s.get(&format!(
            "{base}/1%20rue%20a%20ville.json?autocomplete=false&access_token=other"
        )),
        s.get(&format!(
            "{base}/1%20rue%20a%20ville.json?autocomplete=false"
        ))
    );
    // autocomplete (Mapbox's default): the suggestions of an address field
    let sug = s.json(&format!("{base}/rue%20a.json"));
    assert_eq!(sug["features"][0]["properties"]["suggestion"], true);
    assert_eq!(sug["features"][0]["text"], "Rue A");
    // a whole address has no suggestion: the geocoder answers it
    let whole = s.json(&format!("{base}/1%20rue%20a%20ville.json"));
    assert_eq!(
        carmen_points(&whole)[0],
        native_points(&s.json("/v1/geocode?q=1+rue+a+ville&k=5"))[0]
    );
    // two numbers are a reverse query, lon first, one feature by default
    let r = s.json(&format!("{base}/7.42,43.73.json"));
    assert_eq!(r["query"], serde_json::json!([7.42, 43.73]));
    assert_eq!(r["features"].as_array().unwrap().len(), 1);
    assert_eq!(carmen_points(&r)[0], (43.73, 7.42));
    // limit, types and country
    assert_eq!(
        s.json(&format!(
            "{base}/1%20rue%20a%20ville.json?autocomplete=false&limit=1"
        ))["features"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        s.json(&format!("{base}/1%20rue%20a%20ville.json?country=fr"))["features"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        carmen_points(&s.json(&format!(
            "{base}/1%20rue%20a%20ville.json?autocomplete=false&country=mc"
        ))),
        carmen_points(&v)
    );
    // refusals in Mapbox's shape
    for (path, code) in [
        (format!("{base}/%20.json"), 422),
        (format!("{base}/x.json?types=poi"), 422),
        (format!("{base}/x.json?limit=11"), 422),
        (format!("{base}/x.json?proximity=a,b"), 422),
    ] {
        let (status, _, body) = s.raw(&format!("GET {path} HTTP/1.1\r\n\r\n"));
        assert_eq!(status, code, "{path}: {body}");
        assert!(body.contains("\"message\""), "{body}");
    }
    // only the v5 places paths are ours
    let (status, _, _) = s.raw("GET /geocoding/v6/mapbox.places/x.json HTTP/1.1\r\n\r\n");
    assert_eq!(status, 404);
}
