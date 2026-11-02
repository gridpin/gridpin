//! `gridpin batch` with CSV and TSV files: the input comes back with its own columns plus `gp_*` answers.
//! Each observer drives the built binary on a one-address fixture sheet and compares the answer cells with
//! what `gridpin query` returns for the same text, not with a hand-written expectation.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";
const GP: [&str; 10] = [
    "gp_lat",
    "gp_lon",
    "gp_precision",
    "gp_confidence",
    "gp_weak",
    "gp_street",
    "gp_housenumber",
    "gp_postcode",
    "gp_commune",
    "gp_region",
];

fn sheet(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("tab-{name}"));
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!("{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n"),
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

fn batch(bin: &Path, input: &Path, output: &Path, extra: &[&str]) -> Output {
    let mut args = vec![
        "batch",
        bin.to_str().unwrap(),
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    Command::new(BIN).args(&args).output().unwrap()
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "batch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The first answer of `gridpin query` for `q`, as JSON.
fn query(bin: &Path, q: &str) -> serde_json::Value {
    let out = Command::new(BIN)
        .args(["query", bin.to_str().unwrap(), q, "-k", "1"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    serde_json::from_str(text.lines().next().expect("query gave no answer")).unwrap()
}

fn read_table(path: &Path, delimiter: u8) -> (Vec<String>, Vec<Vec<String>>) {
    let mut r = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(path)
        .unwrap();
    let head = r.headers().unwrap().iter().map(String::from).collect();
    let rows = r
        .records()
        .map(|x| x.unwrap().iter().map(String::from).collect())
        .collect();
    (head, rows)
}

fn cell<'a>(head: &[String], row: &'a [String], name: &str) -> &'a str {
    &row[head
        .iter()
        .position(|h| h == name)
        .unwrap_or_else(|| panic!("no column {name}"))]
}

/// The gp_* cells of a row agree with the `gridpin query` answer for `q`.
fn assert_answers(bin: &Path, head: &[String], row: &[String], q: &str) {
    let hit = query(bin, q);
    assert_eq!(cell(head, row, "gp_lat"), hit["lat"].to_string(), "{q}");
    assert_eq!(cell(head, row, "gp_lon"), hit["lon"].to_string(), "{q}");
    assert_eq!(
        cell(head, row, "gp_precision"),
        hit["precision"].as_str().unwrap(),
        "{q}"
    );
    assert_eq!(
        cell(head, row, "gp_street"),
        hit["street"].as_str().unwrap(),
        "{q}"
    );
    assert_eq!(
        cell(head, row, "gp_housenumber"),
        hit["housenumber"].as_str().unwrap_or(""),
        "{q}"
    );
}

#[test]
fn a_csv_comes_back_with_its_columns_and_the_same_answer_as_query() {
    let (dir, bin) = sheet("basic");
    let input = dir.join("a.csv");
    std::fs::write(&input, "id,Address,note\n7,\"rue a 1, ville\",keep me\n").unwrap();
    let output = dir.join("out.csv");
    ok(&batch(&bin, &input, &output, &[]));
    let (head, rows) = read_table(&output, b',');
    let mut want: Vec<String> = ["id", "Address", "note"].map(String::from).to_vec();
    want.extend(GP.map(String::from));
    assert_eq!(
        head, want,
        "input columns first, then gp_* (no gp_rank at -k 1)"
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(&rows[0][..3], ["7", "rue a 1, ville", "keep me"]);
    assert_answers(&bin, &head, &rows[0], "rue a 1, ville");
}

#[test]
fn every_row_stays_even_without_an_address_or_an_answer() {
    let (dir, bin) = sheet("rows");
    let input = dir.join("a.csv");
    std::fs::write(
        &input,
        "id,address\n1,rue a 1 ville\n2,\n3,   \n4,zzzz qqqq\n5,rue a 1 ville\n",
    )
    .unwrap();
    let output = dir.join("out.csv");
    let out = batch(&bin, &input, &output, &[]);
    ok(&out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("2 of 5 rows had no address"),
        "{out:?}"
    );
    let (head, rows) = read_table(&output, b',');
    let ids: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(
        ids,
        ["1", "2", "3", "4", "5"],
        "N rows in, N rows out, in input order"
    );
    for r in &rows[1..4] {
        assert!(
            GP.iter().all(|g| cell(&head, r, g).is_empty()),
            "unanswered row keeps empty gp_*: {r:?}"
        );
    }
    assert_answers(&bin, &head, &rows[4], "rue a 1 ville");
}

#[test]
fn quoted_cells_come_back_byte_for_byte() {
    let (dir, bin) = sheet("quotes");
    let input = dir.join("a.csv");
    let note = "comma, \"quote\" and\nnewline";
    let mut w = csv::Writer::from_path(&input).unwrap();
    w.write_record(["address", "note"]).unwrap();
    w.write_record(["rue a 1 ville", note]).unwrap();
    w.flush().unwrap();
    drop(w);
    let output = dir.join("out.csv");
    ok(&batch(&bin, &input, &output, &[]));
    let (_, rows) = read_table(&output, b',');
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], note);
}

#[test]
fn a_row_with_a_different_number_of_fields_stops_without_output() {
    let (dir, bin) = sheet("ragged");
    let input = dir.join("a.csv");
    std::fs::write(
        &input,
        "address,note\nrue a 1 ville,x\nrue a 1 ville,x,extra\n",
    )
    .unwrap();
    let output = dir.join("out.csv");
    let out = batch(&bin, &input, &output, &[]);
    assert!(
        !out.status.success(),
        "a ragged row must not shift columns silently"
    );
    assert!(!output.exists());
    assert_no_temp(&dir, "out.csv");
}

#[test]
fn semicolons_tsv_and_an_explicit_format_are_read() {
    let (dir, bin) = sheet("delims");
    let semi = dir.join("semi.csv");
    std::fs::write(&semi, "id;address\n1;rue a 1, ville\n").unwrap();
    let out_semi = dir.join("semi_out.csv");
    ok(&batch(&bin, &semi, &out_semi, &["--delimiter", ";"]));
    let (head, rows) = read_table(&out_semi, b',');
    assert_answers(&bin, &head, &rows[0], "rue a 1, ville");

    let tsv = dir.join("a.tsv");
    std::fs::write(&tsv, "id\taddress\n1\true a 1, ville\n").unwrap();
    let out_tsv = dir.join("a_out.tsv");
    ok(&batch(&bin, &tsv, &out_tsv, &[]));
    let (head, rows) = read_table(&out_tsv, b'\t');
    assert_answers(&bin, &head, &rows[0], "rue a 1, ville");

    let txt = dir.join("a.txt");
    std::fs::write(&txt, "address\nrue a 1 ville\n").unwrap();
    let out_txt = dir.join("a_out.txt");
    ok(&batch(&bin, &txt, &out_txt, &["--format", "csv"]));
    let (head, rows) = read_table(&out_txt, b',');
    assert_answers(&bin, &head, &rows[0], "rue a 1 ville");

    let out_conv = dir.join("conv.tsv");
    ok(&batch(
        &bin,
        &semi,
        &out_conv,
        &["--delimiter", ";", "--output-format", "tsv"],
    ));
    assert!(std::fs::read_to_string(&out_conv)
        .unwrap()
        .starts_with("id\taddress\tgp_lat"));
}

#[test]
fn several_columns_are_joined_with_comma_space() {
    let (dir, bin) = sheet("join");
    let input = dir.join("a.csv");
    std::fs::write(&input, "Street,City,id\nrue a 1,ville,9\n").unwrap();
    let output = dir.join("out.csv");
    ok(&batch(&bin, &input, &output, &["--column", "street, CITY"]));
    let (head, rows) = read_table(&output, b',');
    assert_answers(&bin, &head, &rows[0], "rue a 1, ville");
}

#[test]
fn a_missing_address_column_names_the_columns_and_writes_nothing() {
    let (dir, bin) = sheet("nocol");
    let input = dir.join("a.csv");
    // "Address " with a trailing space is another column: no guess, an error that lists the header.
    std::fs::write(&input, "id,Address \n1,rue a 1 ville\n").unwrap();
    let output = dir.join("out.csv");
    let out = batch(&bin, &input, &output, &[]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("\"id\"") && err.contains("\"Address \""),
        "{err}"
    );
    assert!(!output.exists());
    assert_no_temp(&dir, "out.csv");

    let out = batch(&bin, &input, &output, &["--column", "street"]);
    assert!(!out.status.success());
    assert!(!output.exists());
}

#[test]
fn a_byte_order_mark_and_crlf_are_read_and_other_encodings_stop_with_a_hint() {
    let (dir, bin) = sheet("encoding");
    let bom = dir.join("bom.csv");
    std::fs::write(&bom, b"\xEF\xBB\xBFaddress,note\r\nrue a 1 ville,x\r\n").unwrap();
    let out_bom = dir.join("bom_out.csv");
    ok(&batch(&bin, &bom, &out_bom, &[]));
    let (head, rows) = read_table(&out_bom, b',');
    assert_eq!(
        head[0], "address",
        "the byte-order mark is not part of the first column name"
    );
    assert_answers(&bin, &head, &rows[0], "rue a 1 ville");

    // windows-1252 "ü" (0xFC) is not UTF-8: a stop that says how to fix it, no output, no mojibake
    let latin = dir.join("latin.csv");
    std::fs::write(&latin, b"address,note\nrue a 1 ville,M\xFCller\n").unwrap();
    let output = dir.join("latin_out.csv");
    let out = batch(&bin, &latin, &output, &[]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("CSV UTF-8"),
        "{out:?}"
    );
    assert!(!output.exists());
    assert_no_temp(&dir, "latin_out.csv");
}

#[test]
fn more_candidates_give_a_long_table_with_a_rank_column() {
    let (dir, bin) = sheet("rank");
    let input = dir.join("a.csv");
    std::fs::write(&input, "id,address\n1,rue a 1 ville\n2,\n").unwrap();
    let output = dir.join("out.csv");
    ok(&batch(&bin, &input, &output, &["-k", "3"]));
    let (head, rows) = read_table(&output, b',');
    assert_eq!(head.last().unwrap(), "gp_rank");
    assert_eq!(cell(&head, &rows[0], "gp_rank"), "1");
    let last = rows.last().unwrap();
    assert_eq!(
        (last[0].as_str(), cell(&head, last, "gp_rank")),
        ("2", ""),
        "unanswered row keeps one line"
    );
}

#[test]
fn the_input_is_never_overwritten() {
    let (dir, bin) = sheet("same");
    let input = dir.join("a.csv");
    std::fs::write(&input, "address\nrue a 1 ville\n").unwrap();
    let out = batch(&bin, &input, &input, &[]);
    assert!(!out.status.success());
    assert_eq!(
        std::fs::read_to_string(&input).unwrap(),
        "address\nrue a 1 ville\n"
    );
    let link = dir.join("link.csv");
    std::fs::hard_link(&input, &link).unwrap();
    let out = batch(&bin, &input, &link, &[]);
    // The refusal compares file identity (dev + inode), which only unix exposes; elsewhere the
    // answers go to a fresh file that replaces the link, and the input keeps its bytes either way.
    if cfg!(unix) {
        assert!(!out.status.success());
    }
    assert_eq!(
        std::fs::read_to_string(&input).unwrap(),
        "address\nrue a 1 ville\n"
    );
}

#[test]
fn jsonl_only_options_and_mixed_formats_are_refused() {
    let (dir, bin) = sheet("refuse");
    let input = dir.join("a.csv");
    std::fs::write(&input, "address\nrue a 1 ville\n").unwrap();
    for extra in [
        &["--dump"][..],
        &["--diagnose"][..],
        &["--output-format", "jsonl"][..],
    ] {
        let out = batch(&bin, &input, &dir.join("o.csv"), extra);
        assert!(
            !out.status.success(),
            "{extra:?} must be refused for a CSV input"
        );
    }
    let jsonl = dir.join("q.jsonl");
    std::fs::write(&jsonl, "{\"q\":\"rue a 1 ville\"}\n").unwrap();
    for extra in [&["--output-format", "csv"][..], &["--column", "q"][..]] {
        let out = batch(&bin, &jsonl, &dir.join("o.jsonl"), extra);
        assert!(
            !out.status.success(),
            "{extra:?} must be refused for a JSONL input"
        );
    }
}

#[test]
fn jsonl_output_is_the_same_with_and_without_an_explicit_format() {
    let (dir, bin) = sheet("jsonl");
    let input = dir.join("q.jsonl");
    std::fs::write(&input, "{\"q\":\"rue a 1 ville\"}\n{\"q\":\"zzzz\"}\n").unwrap();
    let plain = dir.join("plain.jsonl");
    let named = dir.join("named.jsonl");
    ok(&batch(&bin, &input, &plain, &[]));
    ok(&batch(&bin, &input, &named, &["--format", "jsonl"]));
    let text = std::fs::read_to_string(&plain).unwrap();
    assert_eq!(text, std::fs::read_to_string(&named).unwrap());
    assert!(text.starts_with("{\"results\":[{"), "{text}");
    assert_eq!(text.lines().count(), 2);
}

#[test]
fn an_earlier_output_is_not_geocoded_twice() {
    let (dir, bin) = sheet("again");
    let input = dir.join("a.csv");
    std::fs::write(&input, "address,GP_LAT\nrue a 1 ville,1\n").unwrap();
    let out = batch(&bin, &input, &dir.join("o.csv"), &[]);
    assert!(
        !out.status.success(),
        "a gp_* column in the input would give two columns of one name"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("GP_LAT"));
}

fn assert_no_temp(dir: &Path, output_name: &str) {
    let prefix = format!(".{output_name}.tmp");
    let leftover = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with(&prefix));
    assert!(!leftover, "the partial temp must be cleaned up");
}
