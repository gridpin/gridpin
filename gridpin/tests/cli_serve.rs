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
    let dir = Scratch::dir(&format!("serve-{name}"));
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!("{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n"),
    )
    .unwrap();
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
