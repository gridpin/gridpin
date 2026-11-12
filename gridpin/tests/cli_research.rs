//! For research: `gridpin cite` and `batch --receipt`, through the built binary on a fixture sheet.
use std::path::PathBuf;
use std::process::Command;

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

fn sheet(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("research-{name}"));
    std::fs::write(
        dir.join("in.csv"),
        format!("{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("m.json"),
        r#"{"country":"mc","layer":"addresses","license":"Test Licence","source_release":"2025-04-01.0","attribution":"fixture"}"#,
    )
    .unwrap();
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

fn sha256(path: &std::path::Path) -> String {
    gridpin::sheets::sha256_file(path).unwrap()
}

#[test]
fn cite_names_the_sheet_by_its_hash_in_each_format() {
    let (_d, bin) = sheet("cite");
    let hash = sha256(&bin);
    for format in ["bibtex", "apa", "cff"] {
        let out = Command::new(BIN)
            .args(["cite", bin.to_str().unwrap(), "--format", format])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.contains(&hash), "{format}: {text}");
        assert!(
            text.contains("2025-04-01.0") && text.contains("Test Licence"),
            "{format}: {text}"
        );
        assert!(
            text.contains(env!("CARGO_PKG_VERSION")),
            "{format}: the engine version"
        );
        assert!(
            text.contains("unreleased build"),
            "{format}: a sheet outside the registry is not given a release"
        );
        assert!(text.contains("2025"), "{format}: the year of the data");
        assert!(
            !text.to_lowercase().contains("doi"),
            "{format}: no DOI is recorded for this sheet, none is printed"
        );
    }
    let bad = Command::new(BIN)
        .args(["cite", bin.to_str().unwrap(), "--format", "ris"])
        .output()
        .unwrap();
    assert!(!bad.status.success() && String::from_utf8_lossy(&bad.stderr).contains("bibtex"));
}

#[test]
fn the_receipt_holds_the_hashes_a_rerun_compares() {
    let (dir, bin) = sheet("receipt");
    let input = dir.join("a.csv");
    std::fs::write(&input, "address\n\"1 rue a, ville\"\n\"2 nowhere\"\n").unwrap();
    let out = dir.join("out.csv");
    let run = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            out.to_str().unwrap(),
            "--receipt",
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let r: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("out.csv.receipt.json")).unwrap())
            .unwrap();
    assert_eq!(r["sheet"]["sha256"], sha256(&bin));
    assert_eq!(r["input"]["sha256"], sha256(&input));
    assert_eq!(r["output"]["sha256"], sha256(&out));
    assert_eq!(
        (r["rows"].as_u64(), r["engine"].as_str()),
        (Some(2), Some(env!("CARGO_PKG_VERSION")))
    );
    assert!(r["command"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "--receipt"));
    assert!(
        r["started"].as_str().unwrap().ends_with('Z')
            && r["finished"].as_str().unwrap() >= r["started"].as_str().unwrap()
    );
    // the same command again: the same output hash
    let again = dir.join("again.csv");
    let rerun = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            again.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(rerun.status.success());
    assert_eq!(sha256(&again), sha256(&out));
    assert!(
        !dir.join("again.csv.receipt.json").exists(),
        "no receipt unless asked"
    );
}
