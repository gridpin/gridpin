//! The outcome layer (`outcome`): a question back when the same street lies in two far-apart towns and the query
//! names neither, the reason of a silence; the answers themselves stay what they were. Through the built binary.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

/// Mill Street in Lexington 40505 (lat 38.0) and in Lexington 27292 (lat 35.8, 245 km south), houses 1-5 each.
fn towns(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("outcome-{name}"));
    let mut rows = String::new();
    for (insee, pc, lat) in [("001", "40505", 38.0), ("002", "27292", 35.8)] {
        for n in 1..=5 {
            rows += &format!(
                "mill street,{insee},lexington,{pc},{n},,{:.6},{lat:.6},Mill Street,Lexington\n",
                -84.5 + n as f64 * 0.0001
            );
        }
    }
    std::fs::write(dir.join("in.csv"), format!("{HDR}{rows}")).unwrap();
    std::fs::write(dir.join("m.json"), r#"{"country":"mc","layer":"addresses","license":"t","source_release":"t","attribution":"t"}"#).unwrap();
    let bin = dir.join("s.bin");
    let out = Command::new(BIN)
        .args([
            "build",
            dir.join("in.csv").to_str().unwrap(),
            bin.to_str().unwrap(),
            "--meta",
            dir.join("m.json").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (dir, bin)
}

fn query(bin: &Path, q: &str) -> (String, String) {
    let out = Command::new(BIN)
        .args(["query", bin.to_str().unwrap(), q, "-k", "1"])
        .output()
        .unwrap();
    assert!(out.status.success());
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn two_far_towns_with_the_street_get_a_question_and_the_answer_stays() {
    let (_d, bin) = towns("ask");
    let (stdout, stderr) = query(&bin, "Mill Street 3");
    assert!(
        stderr.contains("2 places match")
            && stderr.contains("Lexington 40505")
            && stderr.contains("Lexington 27292"),
        "{stderr}"
    );
    let hit: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        hit["housenumber"], "3",
        "the answer is still printed: {stdout}"
    );
    assert!(
        stderr.contains("add the postcode"),
        "two towns of one name differ by postcode here: {stderr}"
    );
    let (_, named) = query(&bin, "Mill Street 3, 40505");
    assert!(
        !named.contains("places match"),
        "a query that names the postcode gets no question: {named}"
    );
}

#[test]
fn a_silence_says_why() {
    let (_d, bin) = towns("why");
    let (stdout, stderr) = query(&bin, "Nowhere Road 1");
    assert!(stdout.is_empty());
    assert!(stderr.contains("no answer (no_street)"), "{stderr}");
    let (_, unparsed) = query(&bin, "12345 678");
    assert!(unparsed.contains("no answer (unparsed)"), "{unparsed}");
}

/// `gridpin serve` on the fixture, spoken to over raw HTTP (as in cli_serve.rs).
struct Server {
    child: Child,
    addr: String,
}

impl Server {
    fn start(bin: &Path) -> Server {
        let mut child = Command::new(BIN)
            .args([
                "serve",
                bin.to_str().unwrap(),
                "--port",
                "0",
                "--threads",
                "2",
            ])
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
            .unwrap()
            .to_string();
        Server { child, addr }
    }

    fn json(&self, target: &str) -> serde_json::Value {
        let mut s = TcpStream::connect(&self.addr).unwrap();
        s.write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .unwrap();
        let mut text = String::new();
        s.read_to_string(&mut text).unwrap();
        serde_json::from_str(text.split_once("\r\n\r\n").unwrap().1).unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_server_puts_the_question_and_the_reason_beside_unchanged_results() {
    let (_d, bin) = towns("serve");
    let server = Server::start(&bin);
    let asked = server.json("/v1/geocode?q=Mill+Street+3");
    assert_eq!(asked["results"][0]["housenumber"], "3");
    assert_eq!(asked["clarify"]["missing"], "postcode", "{asked}");
    let labels: Vec<&str> = asked["clarify"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels.len(), 2, "{labels:?}");
    let plain = server.json("/v1/geocode?q=Mill+Street+3,+40505");
    assert!(
        plain.get("clarify").is_none() && plain.get("reason").is_none(),
        "{plain}"
    );
    let silent = server.json("/v1/geocode?q=Nowhere+Road+1");
    assert_eq!(silent["results"], serde_json::json!([]));
    assert_eq!(silent["reason"], "no_street");
}
