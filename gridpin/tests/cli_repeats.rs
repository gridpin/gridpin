//! `gridpin batch` answers a row that repeats an earlier row once (gridpin::repeats). The promise is that nothing
//! else changes: the output is byte for byte the output of the run with the cache off (`GRIDPIN_REPEAT_CACHE=0`), at
//! one thread and at several, for one-line CSV rows, `--columns` fields, `--reverse` points and JSONL lines.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

/// Two communes with a "Rue A" each (another postcode, 40 km apart) and a "Rue B": rows that differ only in the
/// postcode, or only in the point, get different answers.
fn sheet(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("repeats-{name}"));
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!(
            "{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n\
             rue a,002,autre,20000,1,,7.90,43.90,Rue A,Autre\n\
             rue b,001,ville,10000,5,,7.43,43.74,Rue B,Ville\n"
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
        "build: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (dir, bin)
}

fn batch(
    bin: &Path,
    input: &Path,
    output: &Path,
    extra: &[&str],
    cap: &str,
    threads: &str,
) -> Output {
    let mut args = vec![
        "batch",
        bin.to_str().unwrap(),
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    Command::new(BIN)
        .args(&args)
        .env("GRIDPIN_REPEAT_CACHE", cap)
        .env("GRIDPIN_THREADS", threads)
        .output()
        .unwrap()
}

/// The output and the stderr note of every (cache, threads) pair; the outputs must all be the cache-off output.
fn same_as_without_cache(bin: &Path, input: &Path, extra: &[&str]) -> (Vec<u8>, String) {
    let dir = input.parent().unwrap();
    let mut first: Option<Vec<u8>> = None;
    let mut notes: Vec<String> = Vec::new();
    for (cap, threads) in [
        ("0", "1"),
        ("100000", "1"),
        ("100000", "4"),
        ("1", "4"),
        ("2", "1"),
    ] {
        let out_path = dir.join(format!(
            "out-{cap}-{threads}.{}",
            input.extension().unwrap().to_str().unwrap()
        ));
        let out = batch(bin, input, &out_path, extra, cap, threads);
        assert!(
            out.status.success(),
            "batch {cap}/{threads}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = std::fs::read(&out_path).unwrap();
        match &first {
            None => first = Some(bytes),
            Some(want) => assert_eq!(
                String::from_utf8_lossy(&bytes),
                String::from_utf8_lossy(want),
                "cache {cap}, {threads} threads: output differs from the cache-off run"
            ),
        }
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        notes.push(
            stderr
                .lines()
                .filter(|l| l.contains("repeat an earlier row"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    assert_eq!(notes[0], "", "the cache-off run reports no repeats");
    assert_eq!(
        notes[1], notes[2],
        "the count does not depend on the number of threads"
    );
    (first.unwrap(), notes[1].clone())
}

#[test]
fn csv_rows_that_repeat_get_the_cache_off_bytes_and_are_counted() {
    let (dir, bin) = sheet("csv");
    let input = dir.join("q.csv");
    std::fs::write(
        &input,
        "id,address\n1,1 Rue A Ville\n2,5 Rue B Ville\n3,1 Rue A Ville\n4,1 rue a ville\n5,\n6,1 Rue A Ville\n7,\n8,1 Rue A Autre\n",
    )
    .unwrap();
    let (out, note) = same_as_without_cache(&bin, &input, &["--column", "address"]);
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.lines().count(), 9, "one row per input row: {text}");
    // rows 3 and 6 repeat row 1; "1 rue a ville" is written another way and is asked; blank rows are not repeats
    assert_eq!(
        note,
        "note: 2 of 8 rows repeat an earlier row and were answered once"
    );
}

#[test]
fn fields_rows_differing_in_one_field_are_different_questions() {
    let (dir, bin) = sheet("fields");
    let input = dir.join("f.csv");
    std::fs::write(
        &input,
        "street,no,pc\nRue A,1,10000\nRue A,1,20000\nRue A,1,10000\nRue A,1,20000\n",
    )
    .unwrap();
    let (out, note) = same_as_without_cache(
        &bin,
        &input,
        &["--columns", "street=street,number=no,postcode=pc"],
    );
    let text = String::from_utf8(out).unwrap();
    let lats: Vec<&str> = text
        .lines()
        .skip(1)
        .map(|l| l.split(',').nth(3).unwrap_or(""))
        .collect();
    assert_ne!(lats[0], lats[1], "the postcode picks the commune: {text}");
    assert_eq!(lats[0], lats[2]);
    assert_eq!(lats[1], lats[3]);
    assert_eq!(
        note,
        "note: 2 of 4 rows repeat an earlier row and were answered once"
    );
}

#[test]
fn reverse_points_repeat_only_when_both_coordinates_match() {
    let (dir, bin) = sheet("points");
    let input = dir.join("p.csv");
    std::fs::write(
        &input,
        "lat,lon\n43.73,7.42\n43.90,7.90\n43.73,7.42\n43.73,7.90\n",
    )
    .unwrap();
    let (out, note) = same_as_without_cache(&bin, &input, &["--reverse"]);
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.lines().count(), 5);
    assert_eq!(
        note,
        "note: 1 of 4 rows repeat an earlier row and were answered once"
    );
}

#[test]
fn jsonl_lines_that_repeat_get_the_cache_off_bytes() {
    let (dir, bin) = sheet("jsonl");
    let input = dir.join("q.jsonl");
    std::fs::write(
        &input,
        "{\"q\":\"1 Rue A Ville\"}\n{\"q\":\"5 Rue B Ville\"}\n{\"q\":\"1 Rue A Ville\"}\n\
         {\"street\":\"Rue A\",\"housenumber\":\"1\",\"postcode\":\"20000\"}\n\
         {\"street\":\"Rue A\",\"housenumber\":\"1\",\"postcode\":\"10000\"}\n\
         {\"street\":\"Rue A\",\"housenumber\":\"1\",\"postcode\":\"20000\"}\n",
    )
    .unwrap();
    let (_, note) = same_as_without_cache(&bin, &input, &[]);
    assert_eq!(
        note,
        "note: 2 of 6 rows repeat an earlier row and were answered once"
    );
    let (_, note) = same_as_without_cache(&bin, &input, &["--with", "verdict"]);
    assert_eq!(
        note,
        "note: 2 of 6 rows repeat an earlier row and were answered once"
    );
}

#[test]
fn every_repeated_blank_jsonl_line_still_stops_the_run() {
    // a blank line has no "q": the run must count each of them, repeated or not, and write nothing
    let (dir, bin) = sheet("blank");
    let input = dir.join("bad.jsonl");
    std::fs::write(
        &input,
        "{\"q\":\"1 Rue A Ville\"}\n{}\n{}\n{\"q\":\"1 Rue A Ville\"}\n{}\n",
    )
    .unwrap();
    let out_path = dir.join("bad.out.jsonl");
    let out = batch(&bin, &input, &out_path, &[], "100000", "4");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("warning: 3 of 5 input lines"), "{stderr}");
    assert!(
        stderr.contains("before line 2"),
        "the first bad line is the second: {stderr}"
    );
    assert!(!out_path.exists());
}
