//! `gridpin batch` with CSV and TSV files: the input comes back with its own columns plus `gp_*` answers.
//! Each observer drives the built binary on a one-address fixture sheet and compares the answer cells with
//! what `gridpin query` returns for the same text, not with a hand-written expectation.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";
const GP: [&str; 12] = [
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
    "gp_match_level",
    "gp_match_status",
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
    assert_eq!(
        cell(head, row, "gp_match_level"),
        hit["match_level"].as_str().unwrap(),
        "{q}"
    );
    assert_eq!(
        cell(head, row, "gp_match_status"),
        hit["match_status"].as_str().unwrap(),
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
            GP[..GP.len() - 1]
                .iter()
                .all(|g| cell(&head, r, g).is_empty()),
            "unanswered row keeps empty gp_*: {r:?}"
        );
        assert_eq!(cell(&head, r, "gp_match_status"), "U", "{r:?}");
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

/// A row under `--columns`, with what the matching line answers for it.
fn columns_row(
    bin: &Path,
    dir: &Path,
    header: &str,
    row: &str,
    spec: &str,
) -> Vec<(String, String)> {
    let input = dir.join("fields.csv");
    std::fs::write(&input, format!("{header}\n{row}\n")).unwrap();
    let output = dir.join("fields_out.csv");
    ok(&batch(bin, &input, &output, &["--columns", spec]));
    let (head, rows) = read_table(&output, b',');
    head.into_iter().zip(rows[0].iter().cloned()).collect()
}

fn field<'a>(row: &'a [(String, String)], name: &str) -> &'a str {
    &row.iter().find(|(k, _)| k == name).unwrap().1
}

#[test]
fn split_columns_find_the_door_and_keep_the_input_columns() {
    let (dir, bin) = sheet("fields");
    let row = columns_row(
        &bin,
        &dir,
        "id,Street,No,Town,ZIP",
        "7,Rue A,1,Ville,10000",
        "street=Street,number=No,city=Town,postcode=ZIP",
    );
    assert_eq!(field(&row, "id"), "7");
    assert_eq!(field(&row, "gp_precision"), "house");
    assert_eq!(field(&row, "gp_housenumber"), "1");
    assert_eq!(field(&row, "gp_match_level"), "PointAddress");
    assert_eq!(field(&row, "gp_match_status"), "M");
}

#[test]
fn a_street_cell_the_field_reading_cannot_use_is_answered_by_the_joined_line() {
    // "1 rue a ville" names no street of the sheet as a field (JSONL: empty), the same text as a
    // line finds the house. Without the second step of query_fields_cascade this row stays empty.
    let (dir, bin) = sheet("fields-line");
    let row = columns_row(&bin, &dir, "Street", "1 rue a ville", "street=Street");
    assert_eq!(field(&row, "gp_precision"), "house");
    assert_eq!(field(&row, "gp_housenumber"), "1");
}

#[test]
fn an_exact_house_read_from_the_fields_outranks_what_the_joined_line_answers() {
    // A street named "10000": as a line ("10000, 5, 10000 Ville") the number reads as the postcode and
    // the answer is a neighbour on another street. The cell says it is a street. Without the first step
    // of query_fields_cascade the row gets the line's wrong answer.
    let dir = Scratch::dir("tab-fields-first");
    let csv_in = dir.join("in.csv");
    std::fs::write(
        &csv_in,
        format!(
            "{HDR}10000,001,ville,10000,5,,7.40,43.70,10000,Ville\nville,001,ville,10000,4,,7.44,43.74,Ville,Ville\n"
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
    assert!(Command::new(BIN)
        .args([
            "build",
            csv_in.to_str().unwrap(),
            bin.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap()
        ])
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(
        query(&bin, "10000, 5, 10000 Ville")["street"],
        "Ville",
        "the premise: the line lands on another street"
    );
    let row = columns_row(
        &bin,
        &dir,
        "s,n,c,z",
        "10000,5,Ville,10000",
        "street=s,number=n,city=c,postcode=z",
    );
    assert_eq!(field(&row, "gp_precision"), "house");
    assert_eq!(field(&row, "gp_street"), "10000");
    assert_eq!(field(&row, "gp_housenumber"), "5");
}

#[test]
fn a_blank_street_still_asks_what_the_other_cells_say_and_a_blank_row_stays_unmatched() {
    let (dir, bin) = sheet("fields-blank");
    let input = dir.join("a.csv");
    std::fs::write(&input, "Street,Town\n,Ville\n,\n").unwrap();
    let output = dir.join("out.csv");
    let out = batch(
        &bin,
        &input,
        &output,
        &["--columns", "street=Street,city=Town"],
    );
    ok(&out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("1 of 2 rows had no address"),
        "{out:?}"
    );
    let (head, rows) = read_table(&output, b',');
    assert_answers(&bin, &head, &rows[0], "Ville");
    assert_eq!(cell(&head, &rows[1], "gp_match_status"), "U");
    assert_eq!(cell(&head, &rows[1], "gp_lat"), "");
}

#[test]
fn columns_and_column_exclude_each_other_and_columns_is_for_tables_only() {
    let (dir, bin) = sheet("fields-cli");
    let input = dir.join("a.csv");
    std::fs::write(&input, "Street\nrue a 1\n").unwrap();
    let out = batch(
        &bin,
        &input,
        &dir.join("o.csv"),
        &["--column", "Street", "--columns", "street=Street"],
    );
    assert!(!out.status.success());
    let jsonl = dir.join("a.jsonl");
    std::fs::write(&jsonl, "{\"q\":\"rue a 1\"}\n").unwrap();
    let out = batch(
        &bin,
        &jsonl,
        &dir.join("o.jsonl"),
        &["--columns", "street=q"],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--columns"));
}

#[test]
fn a_rank_column_in_the_input_is_refused_only_when_a_rank_would_be_added() {
    let (dir, bin) = sheet("rank-input");
    let input = dir.join("a.csv");
    std::fs::write(&input, "address,gp_rank\nrue a 1 ville,9\n").unwrap();
    let output = dir.join("out.csv");
    let out = batch(&bin, &input, &output, &["-k", "2"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("gp_rank"),
        "{out:?}"
    );
    ok(&batch(&bin, &input, &output, &[]));
    let (head, rows) = read_table(&output, b',');
    assert_eq!(
        cell(&head, &rows[0], "gp_rank"),
        "9",
        "their own column, at -k 1"
    );
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

#[test]
fn geojson_in_keeps_the_properties_and_adds_the_answer_and_resume_finishes_a_jsonl_run() {
    let dir = Scratch::dir("geojson-in");
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n\
         rue a,001,ville,10000,1,,7.420,43.730,Rue A,Ville\nrue a,001,ville,10000,3,,7.421,43.731,Rue A,Ville\n",
    )
    .unwrap();
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        r#"{"country":"mc","layer":"addresses","license":"t","source_release":"test"}"#,
    )
    .unwrap();
    let bin = dir.join("s.bin");
    let run = |args: &[&str]| {
        let o = Command::new(BIN).args(args).output().unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    run(&[
        "build",
        csv.to_str().unwrap(),
        bin.to_str().unwrap(),
        "--meta",
        man.to_str().unwrap(),
    ]);
    let b = bin.to_str().unwrap();
    let gj = dir.join("in.geojson");
    std::fs::write(
        &gj,
        r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","properties":{"id":7,"address":"rue a 1 ville"},"geometry":null},
          {"type":"Feature","properties":{"id":8,"address":"rue a 3 ville"},"geometry":{"type":"Point","coordinates":[0,0]}}]}"#,
    )
    .unwrap();
    let out = dir.join("out.geojson");
    run(&[
        "batch",
        b,
        gj.to_str().unwrap(),
        out.to_str().unwrap(),
        "--replace-geometry",
    ]);
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let f = v["features"].as_array().unwrap();
    assert_eq!(f.len(), 2);
    assert_eq!(f[1]["properties"]["id"], 8, "the properties stay");
    assert_eq!(f[1]["properties"]["gp_housenumber"], "3");
    assert_eq!(
        f[1]["geometry"]["coordinates"][0], 7.421,
        "the answer's point replaces the geometry"
    );
    // reverse of a polygon: the mean of its vertices, said so
    let gl = dir.join("in.geojsonl");
    std::fs::write(&gl, r#"{"type":"Feature","properties":{"id":1},"geometry":{"type":"Polygon","coordinates":[[[7.419,43.729],[7.421,43.729],[7.421,43.731],[7.419,43.731]]]}}
"#).unwrap();
    let outl = dir.join("out.geojsonl");
    run(&[
        "batch",
        b,
        gl.to_str().unwrap(),
        outl.to_str().unwrap(),
        "--reverse",
    ]);
    let line: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&outl).unwrap().trim()).unwrap();
    assert_eq!(line["properties"]["gp_centroid"], "vertex-mean");
    assert_eq!(line["properties"]["gp_street"], "Rue A");
    // --resume: an output cut after two rows is finished without duplicates, the same as a whole run
    let q = dir.join("q.jsonl");
    std::fs::write(&q, "{\"q\":\"rue a 1 ville\"}\n{\"q\":\"rue a 3 ville\"}\n{\"q\":\"xyzzy\"}\n{\"q\":\"rue a 3 ville\"}\n").unwrap();
    let whole = dir.join("whole.jsonl");
    run(&[
        "batch",
        b,
        q.to_str().unwrap(),
        whole.to_str().unwrap(),
        "-k",
        "1",
    ]);
    let full = std::fs::read_to_string(&whole).unwrap();
    let part = dir.join("part.jsonl");
    std::fs::write(
        &part,
        full.lines()
            .take(2)
            .map(|l| format!("{l}\n"))
            .collect::<String>(),
    )
    .unwrap();
    run(&[
        "batch",
        b,
        q.to_str().unwrap(),
        part.to_str().unwrap(),
        "-k",
        "1",
        "--resume",
    ]);
    assert_eq!(std::fs::read_to_string(&part).unwrap(), full);
    // an interrupted run leaves "<output>.partial", cut in the middle of a line (09.11.2026: the unique temp of an
    // interrupted run vanished and --resume started from zero): a plain run refuses to overwrite it, --resume cuts the
    // broken line and finishes the file the same as a whole run, and the partial file is gone
    let cut = dir.join("cut.jsonl");
    let partial = dir.join("cut.jsonl.partial");
    let two: String = full.lines().take(2).map(|l| format!("{l}\n")).collect();
    std::fs::write(&partial, format!("{two}{{\"results\":[{{\"lat\"")).unwrap();
    let plain = Command::new(BIN)
        .args([
            "batch",
            b,
            q.to_str().unwrap(),
            cut.to_str().unwrap(),
            "-k",
            "1",
        ])
        .output()
        .unwrap();
    assert!(!plain.status.success(), "a plain run over a partial file");
    assert!(
        String::from_utf8_lossy(&plain.stderr).contains("--resume"),
        "{plain:?}"
    );
    run(&[
        "batch",
        b,
        q.to_str().unwrap(),
        cut.to_str().unwrap(),
        "-k",
        "1",
        "--resume",
    ]);
    assert_eq!(std::fs::read_to_string(&cut).unwrap(), full);
    assert!(
        !partial.exists(),
        "the partial file is renamed into the output"
    );
}

#[test]
fn cluster_writes_a_zone_per_row_and_the_hulls() {
    let dir = Scratch::dir("cluster");
    let input = dir.join("p.csv");
    let mut rows = String::from("id,lat,lon\n");
    for i in 0..10 {
        rows += &format!("a{i},43.70{i},7.40{i}\nb{i},45.25{i},19.84{i}\n");
    }
    std::fs::write(&input, rows).unwrap();
    let (out, hull) = (dir.join("z.csv"), dir.join("z.geojson"));
    let o = Command::new(BIN)
        .args([
            "cluster",
            input.to_str().unwrap(),
            "--k",
            "2",
            "-o",
            out.to_str().unwrap(),
            "--geojson",
            hull.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = std::fs::read_to_string(&out).unwrap();
    let zone = |id: &str| {
        text.lines()
            .find(|l| l.starts_with(id))
            .unwrap()
            .split(',')
            .nth(3)
            .unwrap()
            .to_string()
    };
    assert_eq!(zone("a0"), zone("a9"));
    assert_ne!(zone("a0"), zone("b0"));
    let h: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&hull).unwrap()).unwrap();
    assert_eq!(h["features"].as_array().unwrap().len(), 2);
}

/// Revision 0.9.0: the documented `batch -> cluster` failed on batch's own output ("no lat column"), and one row
/// without a point stopped the whole file.
#[test]
fn cluster_reads_the_output_of_batch_and_keeps_rows_without_a_point() {
    let dir = Scratch::dir("cluster-batch");
    let input = dir.join("g.csv");
    let mut rows = String::from("id,address,gp_lat,gp_lon,gp_precision\n");
    for i in 0..10 {
        rows += &format!("a{i},x,43.70{i},7.40{i},house\nb{i},y,45.25{i},19.84{i},house\n");
    }
    rows += "none,nowhere,,,\n";
    std::fs::write(&input, rows).unwrap();
    let out = dir.join("z.csv");
    let o = Command::new(BIN)
        .args([
            "cluster",
            input.to_str().unwrap(),
            "--k",
            "2",
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("1 rows without a point"),
        "{o:?}"
    );
    let mut r = csv::Reader::from_path(&out).unwrap();
    let head = r.headers().unwrap().clone();
    let at = head.iter().position(|h| h == "gp_cluster").unwrap();
    let recs: Vec<csv::StringRecord> = r.records().map(Result::unwrap).collect();
    assert_eq!(recs.len(), 21, "every row is kept");
    let zone = |id: &str| recs.iter().find(|x| &x[0] == id).unwrap()[at].to_string();
    assert_eq!(zone("a0"), zone("a9"));
    assert_ne!(zone("a0"), zone("b0"));
    assert!(!zone("a0").is_empty());
    assert_eq!(zone("none"), "", "a row without a point has no zone");
    // the row after the one without a point keeps its own zone (no shift)
    let mut tail = std::fs::read_to_string(&input).unwrap();
    tail += "c0,z,43.700,7.400,house\n";
    std::fs::write(&input, tail).unwrap();
    let o = Command::new(BIN)
        .args([
            "cluster",
            input.to_str().unwrap(),
            "--k",
            "2",
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success());
    let mut r = csv::Reader::from_path(&out).unwrap();
    let recs: Vec<csv::StringRecord> = r.records().map(Result::unwrap).collect();
    let zone = |id: &str| recs.iter().find(|x| &x[0] == id).unwrap()[at].to_string();
    assert_eq!(zone("c0"), zone("a0"));
    // the user's own lat, lon win over the answer columns
    std::fs::write(
        &input,
        "id,lat,lon,gp_lat,gp_lon\nx,1,1,,\ny,1.0001,1.0001,,\n",
    )
    .unwrap();
    let o = Command::new(BIN)
        .args([
            "cluster",
            input.to_str().unwrap(),
            "--k",
            "1",
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // no point at all is still an error
    std::fs::write(&input, "id,gp_lat,gp_lon\nx,,\n").unwrap();
    let o = Command::new(BIN)
        .args([
            "cluster",
            input.to_str().unwrap(),
            "--k",
            "1",
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!o.status.success());
}

/// Revision 0.9.0: a blank or bad line after a resumed run deleted the partial file with the rows already answered;
/// a partial file longer than the input made an output with extra rows and rc 0.
#[test]
fn resume_keeps_the_answered_rows_when_a_line_is_bad_and_refuses_a_longer_partial() {
    let (dir, bin) = sheet("resume-bad");
    let b = bin.to_str().unwrap();
    let run = |args: &[&str]| {
        let o = Command::new(BIN).args(args).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    };
    let good = "{\"q\":\"rue a 1 ville\"}\n{\"q\":\"rue a 3 ville\"}\n{\"q\":\"rue a 1 ville\"}\n";
    let q = dir.join("q.jsonl");
    std::fs::write(&q, good).unwrap();
    let whole = dir.join("whole.jsonl");
    run(&[
        "batch",
        b,
        q.to_str().unwrap(),
        whole.to_str().unwrap(),
        "-k",
        "1",
    ]);
    let full = std::fs::read_to_string(&whole).unwrap();
    let first: String = full.lines().take(1).map(|l| format!("{l}\n")).collect();
    // a blank line at the end, resumed after one row: rc != 0, the partial file keeps the three good rows
    let bad = dir.join("bad.jsonl");
    std::fs::write(&bad, format!("{good}\n")).unwrap();
    let out = dir.join("o.jsonl");
    let partial = dir.join("o.jsonl.partial");
    std::fs::write(&partial, &first).unwrap();
    let o = Command::new(BIN)
        .args([
            "batch",
            b,
            bad.to_str().unwrap(),
            out.to_str().unwrap(),
            "-k",
            "1",
            "--resume",
        ])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(!out.exists(), "no output from a bad input");
    assert_eq!(
        std::fs::read_to_string(&partial).unwrap(),
        full,
        "the rows before the bad line are kept"
    );
    // the input fixed, --resume finishes the file as a whole run
    run(&[
        "batch",
        b,
        q.to_str().unwrap(),
        out.to_str().unwrap(),
        "-k",
        "1",
        "--resume",
    ]);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), full);
    // a bad line in the middle, plain run: the partial file is cut at that line
    let mid = dir.join("mid.jsonl");
    std::fs::write(
        &mid,
        "{\"q\":\"rue a 1 ville\"}\nnot json\n{\"q\":\"rue a 1 ville\"}\n",
    )
    .unwrap();
    let out2 = dir.join("o2.jsonl");
    let o = Command::new(BIN)
        .args([
            "batch",
            b,
            mid.to_str().unwrap(),
            out2.to_str().unwrap(),
            "-k",
            "1",
        ])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert_eq!(
        std::fs::read_to_string(dir.join("o2.jsonl.partial")).unwrap(),
        first
    );
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("before line 2"),
        "{o:?}"
    );
    // a partial file longer than the input: refused, the partial file stays
    let short = dir.join("short.jsonl");
    std::fs::write(&short, "{\"q\":\"rue a 1 ville\"}\n").unwrap();
    let out3 = dir.join("o3.jsonl");
    let p3 = dir.join("o3.jsonl.partial");
    std::fs::write(&p3, &full).unwrap();
    let o = Command::new(BIN)
        .args([
            "batch",
            b,
            short.to_str().unwrap(),
            out3.to_str().unwrap(),
            "-k",
            "1",
            "--resume",
        ])
        .output()
        .unwrap();
    assert!(!o.status.success(), "{o:?}");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("has only 1 lines"),
        "{o:?}"
    );
    assert_eq!(std::fs::read_to_string(&p3).unwrap(), full);
    assert!(!out3.exists());
}
