//! `gridpin serve` with one shared key (ANSWERS §359, plan G0): every request but /health carries
//! `Authorization: Bearer <key>`, the Google-compatible path also `key=`; a refused request never reaches the engine.
//! The key comes from GRIDPIN_SERVE_KEY or --key-file, set on the child process only.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";
const KEY: &str = "k3y-0123456789abcdefghijKLMN";

fn sheet(dir: &Scratch) -> PathBuf {
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!("{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n"),
    )
    .unwrap();
    let man = dir.join("m.json");
    std::fs::write(&man, r#"{"country":"mc","layer":"addresses","license":"t","source_release":"t","attribution":"t"}"#)
        .unwrap();
    let bin = dir.join("s.bin");
    let st = Command::new(BIN)
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
        st.status.success(),
        "{}",
        String::from_utf8_lossy(&st.stderr)
    );
    bin
}

fn command(bin: &Path, env: &[(&str, &str)], extra: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args([
        "serve",
        bin.to_str().unwrap(),
        "--port",
        "0",
        "--threads",
        "2",
    ])
    .args(extra)
    .env_remove("GRIDPIN_SERVE_KEY")
    .env_remove("GRIDPIN_REQUIRE_KEY");
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

/// A running server, killed and reaped when dropped (PRINCIPLES 4.4).
struct Server {
    child: Child,
    addr: String,
}

impl Server {
    fn start(bin: &Path, env: &[(&str, &str)], extra: &[&str]) -> Server {
        let mut child = command(bin, env, extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(!line.contains(KEY), "the key on the first line");
        let addr = line
            .trim()
            .strip_prefix("listening on http://")
            .unwrap_or_else(|| panic!("first line: {line:?}"))
            .to_string();
        Server { child, addr }
    }

    fn raw(&self, request: &str) -> (u16, String, String) {
        let mut s = TcpStream::connect(&self.addr).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut text = String::new();
        s.read_to_string(&mut text).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").expect("no header end");
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, head.to_string(), body.to_string())
    }

    fn get(&self, target: &str, auth: Option<&str>) -> (u16, String, String) {
        let header = auth
            .map(|a| format!("Authorization: {a}\r\n"))
            .unwrap_or_default();
        self.raw(&format!("GET {target} HTTP/1.1\r\nHost: x\r\n{header}\r\n"))
    }

    fn post(&self, target: &str, body: &str, auth: Option<&str>) -> (u16, String, String) {
        let header = auth
            .map(|a| format!("Authorization: {a}\r\n"))
            .unwrap_or_default();
        self.raw(&format!(
            "POST {target} HTTP/1.1\r\nHost: x\r\n{header}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ))
    }

    fn requests_seen(&self) -> u64 {
        let (status, _, body) = self.get("/health", None);
        assert_eq!(status, 200, "{body}");
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["requests"]
            .as_u64()
            .unwrap()
    }

    /// What the server printed on stderr, after it is stopped.
    fn stop(mut self) -> String {
        let _ = self.child.kill();
        let mut err = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        let _ = self.child.wait();
        err
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn bearer(key: &str) -> String {
    format!("Bearer {key}")
}

#[test]
fn with_a_key_only_the_right_bearer_passes_and_health_stays_open() {
    let dir = Scratch::dir("serve-key-rules");
    let bin = sheet(&dir);
    let s = Server::start(&bin, &[("GRIDPIN_SERVE_KEY", KEY)], &[]);
    let geocode = "/v1/geocode?q=Rue%20A%201%2C%20Ville";
    let (ok, _, body) = s.get(geocode, Some(&bearer(KEY)));
    assert_eq!(ok, 200, "{body}");
    assert!(body.contains("Rue A"), "{body}");
    // the scheme in any case
    assert_eq!(s.get(geocode, Some(&format!("bearer {KEY}"))).0, 200);
    // near misses (6.6): none, wrong, a prefix, one more character, two spaces, another scheme, the key alone
    let prefix = &KEY[..KEY.len() - 1];
    let longer = format!("{KEY}x");
    for auth in [
        None,
        Some(bearer("x".repeat(KEY.len()).as_str())),
        Some(bearer(prefix)),
        Some(bearer(&longer)),
        Some(format!("Bearer  {KEY}")),
        Some(format!("Basic {KEY}")),
        Some(KEY.to_string()),
    ] {
        let (status, head, body) = s.get(geocode, auth.as_deref());
        assert_eq!(status, 401, "{auth:?}: {body}");
        assert!(head.starts_with("HTTP/1.1 401 Unauthorized"), "{head}");
        assert!(head.contains("WWW-Authenticate: Bearer"), "{head}");
        assert!(!body.contains(KEY), "{body}");
    }
    // /health needs no key: the host's probe reads it
    assert_eq!(s.get("/health", None).0, 200);
}

#[test]
fn a_refused_request_never_reaches_the_engine_and_learns_nothing_of_the_routes() {
    let dir = Scratch::dir("serve-key-engine");
    let bin = sheet(&dir);
    let s = Server::start(&bin, &[("GRIDPIN_SERVE_KEY", KEY)], &[]);
    let mut refused = Vec::new();
    for target in [
        "/v1/geocode?q=a",
        "/search?q=a",
        "/reverse?lat=1&lon=1",
        "/status",
        "/",
        "/ui",
        "/no/such/path",
    ] {
        refused.push(s.get(target, Some(&bearer("wrong-key-0123456789abcdef"))));
    }
    refused.push(s.post("/v1/geocode", r#"{"q":"Rue A 1, Ville"}"#, None));
    // the key in the body or in the address of a native path is not a key
    refused.push(s.post(
        "/v1/geocode",
        &format!(r#"{{"q":"a","key":"{KEY}"}}"#),
        None,
    ));
    refused.push(s.get(&format!("/v1/geocode?q=a&key={KEY}"), None));
    let first = refused[0].clone();
    for (status, head, body) in &refused {
        assert_eq!(*status, 401, "{head}");
        assert_eq!(body, &first.2, "one answer for every path");
    }
    // only /health itself was routed: requests counts the request that reads it
    assert_eq!(s.requests_seen(), 1);
}

#[test]
fn the_google_path_takes_the_key_from_its_address_and_denies_as_google_does() {
    let dir = Scratch::dir("serve-key-google");
    let bin = sheet(&dir);
    let s = Server::start(&bin, &[("GRIDPIN_SERVE_KEY", KEY)], &[]);
    let (status, _, body) = s.get(
        &format!("/maps/api/geocode/json?address=Rue%20A%201%2C%20Ville&key={KEY}"),
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"status\":\"OK\""), "{body}");
    let (status, _, body) = s.get("/maps/api/geocode/json?address=Rue%20A%201&key=wrong", None);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("REQUEST_DENIED"), "{body}");
    assert!(!body.contains(KEY));
    // the header works there too
    let (status, _, body) = s.get(
        "/maps/api/geocode/json?address=Rue%20A%201%2C%20Ville",
        Some(&bearer(KEY)),
    );
    assert!(
        status == 200 && body.contains("\"status\":\"OK\""),
        "{body}"
    );
    assert_eq!(s.requests_seen(), 3);
}

#[test]
fn the_key_file_gives_the_same_server_as_the_variable() {
    let dir = Scratch::dir("serve-key-file");
    let bin = sheet(&dir);
    let file = dir.join("key");
    std::fs::write(&file, format!("{KEY}\nsecond line is not read\n")).unwrap();
    let s = Server::start(
        &bin,
        &[],
        &["--key-file", file.to_str().unwrap(), "--require-key"],
    );
    assert_eq!(s.get("/v1/geocode?q=a", Some(&bearer(KEY))).0, 200);
    assert_eq!(s.get("/v1/geocode?q=a", None).0, 401);
}

#[test]
fn answers_with_the_key_are_byte_for_byte_those_of_the_open_server() {
    let dir = Scratch::dir("serve-key-same");
    let bin = sheet(&dir);
    let open = Server::start(&bin, &[], &[]);
    let keyed = Server::start(&bin, &[("GRIDPIN_SERVE_KEY", KEY)], &[]);
    for target in [
        "/v1/geocode?q=Rue%20A%201%2C%20Ville",
        "/v1/reverse?lat=43.73&lon=7.42",
        "/search?q=Rue%20A%201&format=json",
        "/maps/api/geocode/json?address=Rue%20A%201",
        "/no/such/path",
    ] {
        let (a_status, _, a) = open.get(target, None);
        let (b_status, _, b) = keyed.get(target, Some(&bearer(KEY)));
        assert_eq!((a_status, &a), (b_status, &b), "{target}");
    }
    // without a key the open server answers as before: no 401 anywhere
    assert_eq!(open.get("/v1/geocode?q=a", None).0, 200);
}

/// A start that must be refused: the environment, the arguments, and a word of the refusal.
type StartCase<'a> = (Vec<(&'a str, &'a str)>, Vec<&'a str>, &'a str);

/// Run a start that must be refused: its exit code and everything it printed. A server that starts anyway is killed
/// after 15 s and gives `None`, so a broken refusal fails the test instead of hanging it.
fn refused_start(bin: &Path, env: &[(&str, &str)], extra: &[&str]) -> (Option<i32>, String) {
    let mut child = command(bin, env, extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t0 = std::time::Instant::now();
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if t0.elapsed() > std::time::Duration::from_secs(15) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let mut text = String::new();
    let _ = child.stderr.take().unwrap().read_to_string(&mut text);
    let _ = child.stdout.take().unwrap().read_to_string(&mut text);
    (code, text)
}

#[test]
fn a_bad_or_missing_key_stops_the_start_and_never_prints_the_key() {
    let dir = Scratch::dir("serve-key-start");
    let bin = sheet(&dir);
    let file = dir.join("key");
    std::fs::write(&file, format!("{KEY}\n")).unwrap();
    let short = "short-key-0123456789";
    let cases: Vec<StartCase> = vec![
        (
            vec![("GRIDPIN_SERVE_KEY", short)],
            vec![],
            "shorter than 24",
        ),
        (
            vec![("GRIDPIN_SERVE_KEY", "has a space 0123456789abcdef")],
            vec![],
            "space",
        ),
        (
            vec![("GRIDPIN_SERVE_KEY", KEY)],
            vec!["--key-file", file.to_str().unwrap()],
            "both",
        ),
        (vec![], vec!["--require-key"], "no key"),
        (vec![("GRIDPIN_REQUIRE_KEY", "1")], vec![], "no key"),
        (
            vec![("GRIDPIN_SERVE_KEY", "")],
            vec!["--require-key"],
            "no key",
        ),
    ];
    for (env, extra, says) in cases {
        let (code, err) = refused_start(&bin, &env, &extra);
        assert!(
            code.is_some_and(|c| c != 0),
            "{env:?} {extra:?} started: {err}"
        );
        assert!(err.contains(says), "{env:?} {extra:?}: {err}");
        assert!(!err.contains(KEY) && !err.contains(short), "{err}");
        assert!(!err.contains("listening"), "{err}");
    }
    // a running server prints its key nowhere either
    let s = Server::start(&bin, &[("GRIDPIN_SERVE_KEY", KEY)], &[]);
    s.get("/v1/geocode?q=a", None);
    s.get("/v1/geocode?q=a", Some(&bearer(KEY)));
    let err = s.stop();
    assert!(!err.contains(KEY), "{err}");
}

#[test]
fn the_help_names_no_key() {
    let out = Command::new(BIN)
        .args(["serve", "--help"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains(KEY));
}
