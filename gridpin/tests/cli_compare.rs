//! `gridpin parse` and `gridpin same` on a two-street fixture sheet: the reading of a line, the level two
//! lines share, and the pairs file whose output keeps the input order and reports a bad line in place.
use std::path::PathBuf;
use std::process::Command;

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

fn sheet(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("compare-{name}"));
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!(
            "{HDR}rue a,001,ville,10000,1,,7.420,43.730,Rue A,Ville\nrue a,001,ville,10000,3,,7.421,43.731,Rue A,Ville\n\
             rue b,001,ville,10000,1,,7.425,43.735,Rue B,Ville\n"
        ),
    )
    .unwrap();
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        r#"{"country":"mc","layer":"addresses","license":"t","source_release":"test"}"#,
    )
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

fn json(args: &[&str]) -> serde_json::Value {
    let out = Command::new(BIN).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn parse_prints_the_first_reading_and_the_answer() {
    let (_dir, bin) = sheet("parse");
    let r = json(&["parse", bin.to_str().unwrap(), "rue a 3 ville"]);
    assert_eq!(r["house_number"], 3);
    assert_eq!(r["words"], "rue a ville");
    assert_eq!(r["answer"]["housenumber"], "3");
}

#[test]
fn same_prints_the_shared_level() {
    let (_dir, bin) = sheet("same");
    let s = bin.to_str().unwrap();
    assert_eq!(
        json(&["same", s, "rue a 1 ville", "Rue A 1, Ville"])["level"],
        "house"
    );
    assert_eq!(
        json(&["same", s, "rue a 1 ville", "rue a 3 ville"])["level"],
        "street"
    );
    assert_eq!(
        json(&["same", s, "rue a 1 ville", "rue b 1 ville"])["level"],
        "commune"
    );
    let none = json(&["same", s, "rue a 1 ville", "zzzz qqqq"]);
    assert_eq!(
        (none["level"].as_str(), none["text"].as_str()),
        (Some("unknown"), Some("different_text"))
    );
}

#[test]
fn the_pairs_file_keeps_its_order_and_reports_a_bad_line_in_place() {
    let (dir, bin) = sheet("pairs");
    let input = dir.join("pairs.jsonl");
    let output = dir.join("out.jsonl");
    std::fs::write(
        &input,
        "{\"a\":\"rue a 1 ville\",\"b\":\"rue a 3 ville\"}\n{\"a\":\"only one\"}\n{\"a\":\"rue a 1 ville\",\"b\":\"rue a 1 ville\"}\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args([
            "same",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "--pairs",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(&output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["level"], "street");
    assert!(rows[1]["error"].as_str().unwrap().contains("\"a\""));
    assert_eq!(rows[2]["level"], "house");
}
