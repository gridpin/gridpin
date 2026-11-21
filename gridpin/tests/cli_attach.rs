//! `gridpin attach`: a table joined by a code, through the built binary. One observer per refusal (the year of the
//! boundaries, the share of absent codes, repeated keys, dates, columns) and per status and key basis; near misses
//! of each condition next to it (a year one edition off, a code one digit short with and without a letter, a day on
//! the last day of validity and the day after it).
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn refused(out: &Output) -> String {
    assert!(
        !out.status.success(),
        "expected a refusal: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8(out.stderr.clone()).unwrap()
}

/// A tract table of 2020 boundaries: four tracts, one suppressed.
fn tract_table(dir: &Path, extra: &str) -> PathBuf {
    write(
        dir,
        "t.csv",
        "GEOID,income,income_moe,pop\n01001020100,62000,5000,1781\n01001020200,-666666666,-222222222,-666666666\n\
         01001020300,70000,4000,2000\n01001020400,1,1,1\n",
    );
    write(
        dir,
        "t.table.json",
        &format!(
            r#"{{"table":"acs_t","file":"t.csv","prefix":"acs",
            "key":{{"column":"GEOID","kind":"us_tract","vintage":"2020","width":11}},
            "take":{{"income":{{"as":"income","unit":"USD","moe":"income_moe","missing":["-666666666","-222222222"]}},
                     "pop":{{"as":"population","missing":["-666666666"]}}}},
            "edition":"ACS test","publisher":"test","licence":"public domain","coverage":"complete",
            "caveat":"A value of the area."{extra}}}"#
        ),
    )
}

/// One cell of a CSV output, read as CSV (an edition holds a quoted comma).
fn cell(text: &str, row: usize, column: &str) -> String {
    let mut r = csv::Reader::from_reader(text.as_bytes());
    let head = r.headers().unwrap().clone();
    let at = head
        .iter()
        .position(|h| h == column)
        .unwrap_or_else(|| panic!("{column} in {head:?}"));
    r.records().nth(row).unwrap().unwrap()[at].to_string()
}

#[test]
fn the_year_of_the_boundaries_must_be_named_and_equal() {
    let s = Scratch::dir("attach_year");
    let t = tract_table(&s, "");
    let rows = write(&s, "r.csv", "id,gp_tract\n1,01001020100\n");
    let (r, t) = (rows.to_str().unwrap(), t.to_str().unwrap());
    let e = refused(&run(&["attach", r, "--table", t, "--on", "gp_tract"]));
    assert!(e.contains("--key-vintage 2020"), "{e}");
    // one edition off
    let e = refused(&run(&[
        "attach",
        r,
        "--table",
        t,
        "--on",
        "gp_tract",
        "--key-vintage",
        "2010",
    ]));
    assert!(
        e.contains("2010") && e.contains("2020") && e.contains("t.csv"),
        "{e}"
    );
    let out = ok(&run(&[
        "attach",
        r,
        "--table",
        t,
        "--on",
        "gp_tract",
        "--key-vintage",
        "2020",
    ]));
    assert_eq!(cell(&out, 0, "gp_acs_income"), "62000");
    assert_eq!(cell(&out, 0, "gp_acs_income_moe"), "5000");
    assert_eq!(cell(&out, 0, "gp_acs_status"), "matched");
    assert_eq!(
        cell(&out, 0, "gp_acs_edition"),
        "ACS test; us_tract boundaries 2020"
    );
}

#[test]
fn every_status_and_every_key_basis_of_a_row() {
    let s = Scratch::dir("attach_status");
    let t = tract_table(&s, r#","max_absent_share":0.5"#);
    let rows = write(
        &s,
        "r.csv",
        "id,gp_tract,gp_precision,gp_weak\n\
         1,1001020100,house,false\n\
         2,01001020200,street,false\n\
         3,01001020100,city,false\n\
         4,99999999999,interp,false\n\
         5,01001020100,house,true\n\
         6,,house,false\n\
         7,100102030A,house,false\n\
         8,01001020300|Tract 3,near,false\n\
         9,01001020100;01001020300,interp,false\n",
    );
    let out = ok(&run(&[
        "attach",
        rows.to_str().unwrap(),
        "--table",
        t.to_str().unwrap(),
        "--on",
        "gp_tract",
        "--key-vintage",
        "2020",
    ]));
    let status: Vec<String> = (0..9).map(|i| cell(&out, i, "gp_acs_status")).collect();
    // the last row: a point on the border of two tracts (an interpolated point on the street between them)
    assert_eq!(
        status,
        [
            "matched",
            "suppressed",
            "no_key",
            "key_absent",
            "no_key",
            "no_key",
            "key_absent",
            "matched",
            "no_key"
        ]
    );
    let basis: Vec<String> = (0..9).map(|i| cell(&out, i, "gp_key_basis")).collect();
    assert_eq!(
        basis,
        [
            "point",
            "approximate",
            "point_too_coarse",
            "point",
            "weak_answer",
            "point",
            "point",
            "approximate",
            "point"
        ]
    );
    // a code of ten digits is padded; ten characters with a letter is not
    assert_eq!(cell(&out, 0, "gp_acs_income"), "62000");
    // a suppressed value is empty, not the publisher's mark
    assert_eq!(cell(&out, 1, "gp_acs_income"), "");
}

#[test]
fn the_point_method_decides_before_the_precision_and_a_town_centre_gets_no_tract() {
    let s = Scratch::dir("attach_method");
    let t = tract_table(&s, "");
    let rows = write(
        &s,
        "r.csv",
        "gp_tract,gp_precision,gp_point_method\n\
         01001020100,house,range_interpolated\n\
         01001020100,city,commune_centre\n\
         01001020100,city,place_table\n\
         01001020100,street,crossing_of_streets\n",
    );
    let out = ok(&run(&[
        "attach",
        rows.to_str().unwrap(),
        "--table",
        t.to_str().unwrap(),
        "--on",
        "gp_tract",
        "--key-vintage",
        "2020",
    ]));
    let basis: Vec<String> = (0..4).map(|i| cell(&out, i, "gp_key_basis")).collect();
    assert_eq!(
        basis,
        [
            "point",
            "point_too_coarse",
            "point_too_coarse",
            "approximate"
        ]
    );
    assert_eq!(cell(&out, 1, "gp_acs_income"), "");
}

#[test]
fn a_coarse_table_takes_a_town_centre_but_not_a_weak_answer() {
    let s = Scratch::dir("attach_coarse");
    write(&s, "c.csv", "code,v\nX1,9\n");
    let t = write(
        &s,
        "c.table.json",
        r#"{"table":"c","file":"c.csv","prefix":"c","key":{"column":"code","kind":"county","grain":"coarse"},
        "take":{"v":{"as":"v"}},"edition":"e","publisher":"p","licence":"l","caveat":"c"}"#,
    );
    let rows = write(
        &s,
        "r.csv",
        "k,gp_precision,gp_weak\nX1,city,false\nX1,city,true\n",
    );
    let out = ok(&run(&[
        "attach",
        rows.to_str().unwrap(),
        "--table",
        t.to_str().unwrap(),
        "--on",
        "k",
    ]));
    assert_eq!(cell(&out, 0, "gp_c_status"), "matched");
    assert_eq!(cell(&out, 0, "gp_key_basis"), "point_too_coarse");
    assert_eq!(cell(&out, 1, "gp_c_status"), "no_key");
}

#[test]
fn a_complete_table_that_misses_many_codes_refuses_and_writes_nothing() {
    let s = Scratch::dir("attach_absent");
    let t = tract_table(&s, "");
    let rows = write(
        &s,
        "r.csv",
        "gp_tract\n01001020100\n01001020200\n01001020300\n01001020400\n01001029900\n",
    );
    let out_file = &s.join("out.csv");
    let args = |extra: &[&'static str]| {
        let mut a: Vec<String> = [
            "attach",
            rows.to_str().unwrap(),
            "--table",
            t.to_str().unwrap(),
            "--on",
            "gp_tract",
            "--key-vintage",
            "2020",
            "-o",
            out_file.to_str().unwrap(),
        ]
        .map(String::from)
        .to_vec();
        a.extend(extra.iter().map(|s| s.to_string()));
        a
    };
    let a = args(&[]);
    let e = refused(&Command::new(BIN).args(&a).output().unwrap());
    assert!(
        e.contains("20.0 %") && e.contains("01001029900") && e.contains("--allow-absent"),
        "{e}"
    );
    assert!(!out_file.exists(), "a refusal writes no output");
    let a = args(&["--allow-absent"]);
    let o = Command::new(BIN).args(&a).output().unwrap();
    ok(&o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("warning"));
    assert_eq!(
        std::fs::read_to_string(out_file).unwrap().lines().count(),
        6
    );
    // a partial table never refuses on the share
    let t2 = tract_table(&s, "");
    let text = std::fs::read_to_string(&t2)
        .unwrap()
        .replace("\"complete\"", "\"partial\"");
    std::fs::write(&t2, text).unwrap();
    std::fs::remove_file(out_file).unwrap();
    ok(&Command::new(BIN).args(args(&[])).output().unwrap());
}

#[test]
fn rows_with_dates_need_as_of_and_the_last_day_is_inside() {
    let s = Scratch::dir("attach_dates");
    write(
        &s,
        "z.csv",
        "tract,start,end,zone\n01001020100,2019-01-01,2026-12-31,old\n01001020100,2027-01-01,,new\n",
    );
    let t = write(
        &s,
        "z.table.json",
        r#"{"table":"oz","file":"z.csv","prefix":"oz","key":{"column":"tract","kind":"us_tract","vintage":"2020"},
        "take":{"zone":{"as":"zone"}},"valid":{"from_column":"start","to_column":"end"},
        "edition":"e","publisher":"p","licence":"l","caveat":"c"}"#,
    );
    let rows = write(&s, "r.csv", "gp_tract\n01001020100\n");
    let at = |day: Option<&str>| {
        let mut a = vec![
            "attach",
            rows.to_str().unwrap(),
            "--table",
            t.to_str().unwrap(),
            "--on",
            "gp_tract",
            "--key-vintage",
            "2020",
        ];
        if let Some(d) = day {
            a.extend(["--as-of", d]);
        }
        run(&a)
    };
    assert!(refused(&at(None)).contains("--as-of"));
    assert_eq!(cell(&ok(&at(Some("2026-12-31"))), 0, "gp_oz_zone"), "old");
    assert_eq!(cell(&ok(&at(Some("2027-01-01"))), 0, "gp_oz_zone"), "new");
    assert_eq!(
        cell(&ok(&at(Some("2018-12-31"))), 0, "gp_oz_status"),
        "out_of_dates"
    );
    assert!(refused(&at(Some("31.12.2026"))).contains("YYYY-MM-DD"));
    // overlapping dates of one key
    write(
        &s,
        "z.csv",
        "tract,start,end,zone\n01001020100,2019-01-01,2027-01-01,old\n01001020100,2027-01-01,,new\n",
    );
    assert!(refused(&at(Some("2027-01-01"))).contains("overlap"));
}

#[test]
fn a_table_that_cannot_be_read_says_why() {
    let s = Scratch::dir("attach_bad");
    let rows = write(&s, "r.csv", "gp_tract\n01001020100\n");
    let r = rows.to_str().unwrap();
    let base = r#""prefix":"x","take":{"v":{"as":"v"}},"edition":"e","publisher":"p","licence":"l","caveat":"c""#;
    let passport = |name: &str, key: &str| {
        write(
            &s,
            &format!("{name}.table.json"),
            &format!(r#"{{"table":"{name}","file":"{name}.csv",{base},"key":{key}}}"#),
        )
    };
    // repeated key, no dates
    write(&s, "dup.csv", "k,v\nA,1\nA,2\nB,1\nB,3\n");
    let t = passport("dup", r#"{"column":"k","kind":"k"}"#);
    let e = refused(&run(&[
        "attach",
        r,
        "--table",
        t.to_str().unwrap(),
        "--on",
        "gp_tract",
    ]));
    assert!(e.contains("2 keys repeat") && e.contains("A, B"), "{e}");
    // no key column: the columns are listed
    write(&s, "nokey.csv", "code,v\nA,1\n");
    let t = passport("nokey", r#"{"column":"k","kind":"k"}"#);
    let e = refused(&run(&[
        "attach",
        r,
        "--table",
        t.to_str().unwrap(),
        "--on",
        "gp_tract",
    ]));
    assert!(
        e.contains("no column \"k\"") && e.contains("code, v"),
        "{e}"
    );
    // empty table
    write(&s, "empty.csv", "k,v\n");
    let t = passport("empty", r#"{"column":"k","kind":"k"}"#);
    assert!(refused(&run(&[
        "attach",
        r,
        "--table",
        t.to_str().unwrap(),
        "--on",
        "gp_tract"
    ]))
    .contains("no rows"));
    // a computed key has no year of boundaries
    write(&s, "cell.csv", "k,v\nA,1\n");
    let t = passport(
        "cell",
        r#"{"column":"k","kind":"cell:eu1km","vintage":"2021"}"#,
    );
    assert!(
        refused(&run(&["attach", r, "--table", t.to_str().unwrap()])).contains("\"vintage\": null")
    );
    // an unknown field of the passport is a typo, not ignored
    let t = write(
        &s,
        "typo.table.json",
        &format!(
            r#"{{"table":"t","file":"cell.csv",{base},"key":{{"column":"k","kind":"k"}},"vintgae":"2020"}}"#
        ),
    );
    assert!(refused(&run(&[
        "attach",
        r,
        "--table",
        t.to_str().unwrap(),
        "--on",
        "gp_tract"
    ]))
    .contains("vintgae"));
    // rows without the --on column
    let t = passport("cell2", r#"{"column":"k","kind":"k"}"#);
    write(&s, "cell2.csv", "k,v\nA,1\n");
    let e = refused(&run(&[
        "attach",
        r,
        "--table",
        t.to_str().unwrap(),
        "--on",
        "tract",
    ]));
    assert!(e.contains("gp_tract"), "{e}");
}

#[test]
fn a_cell_key_is_computed_from_the_point_and_an_answer_without_precision_is_not_trusted() {
    let s = Scratch::dir("attach_cell");
    write(&s, "jp.csv", "code,pop\n53394611,100\n");
    let t = write(
        &s,
        "jp.table.json",
        r#"{"table":"jp","file":"jp.csv","prefix":"jp","key":{"column":"code","kind":"cell:jp3"},
        "take":{"pop":{"as":"population"}},"edition":"e","publisher":"p","licence":"l","caveat":"c"}"#,
    );
    let given = write(
        &s,
        "p.csv",
        "lat,lon\n35.681236,139.767125\n48.85,2.35\n,\n",
    );
    let out = ok(&run(&[
        "attach",
        given.to_str().unwrap(),
        "--table",
        t.to_str().unwrap(),
    ]));
    assert_eq!(cell(&out, 0, "gp_jp_population"), "100");
    assert_eq!(cell(&out, 0, "gp_key_basis"), "given");
    assert_eq!(cell(&out, 1, "gp_jp_status"), "no_key");
    assert_eq!(cell(&out, 2, "gp_key_basis"), "");
    // gp_lat and gp_lon of an answer whose precision the rows dropped
    let answered = write(&s, "a.csv", "gp_lat,gp_lon\n35.681236,139.767125\n");
    let o = run(&[
        "attach",
        answered.to_str().unwrap(),
        "--table",
        t.to_str().unwrap(),
    ]);
    let out = ok(&o);
    assert_eq!(cell(&out, 0, "gp_key_basis"), "basis_unknown");
    assert_eq!(cell(&out, 0, "gp_jp_status"), "no_key");
    assert!(String::from_utf8_lossy(&o.stderr).contains("gridpin-pro batch --table"));
    // --on makes no sense for a computed key
    assert!(refused(&run(&[
        "attach",
        given.to_str().unwrap(),
        "--table",
        t.to_str().unwrap(),
        "--on",
        "lat"
    ]))
    .contains("computed"));
}

#[test]
fn a_report_carries_the_counts_the_edition_and_the_caveat() {
    let s = Scratch::dir("attach_report");
    let t = tract_table(&s, "");
    let rows = write(&s, "r.csv", "gp_tract\n01001020100\n");
    let report = &s.join("rep.json");
    ok(&run(&[
        "attach",
        rows.to_str().unwrap(),
        "--table",
        t.to_str().unwrap(),
        "--on",
        "gp_tract",
        "--key-vintage",
        "2020",
        "--report",
        report.to_str().unwrap(),
    ]));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(v["status"]["matched"], 1);
    assert_eq!(v["caveat"], "A value of the area.");
    assert_eq!(v["edition"], "ACS test; us_tract boundaries 2020");
}

/// The fixture sheet of `cli_distance`: two houses of one street.
fn sheet(dir: &Path) -> PathBuf {
    let csv = write(
        dir,
        "in.csv",
        "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n\
         rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\nrue b,001,ville,10000,2,,7.50,43.80,Rue B,Ville\n",
    );
    let man = write(
        dir,
        "m.json",
        r#"{"country":"mc","layer":"addresses","license":"t","source_release":"test"}"#,
    );
    let bin = dir.join("s.bin");
    ok(&run(&[
        "build",
        csv.to_str().unwrap(),
        bin.to_str().unwrap(),
        "--meta",
        man.to_str().unwrap(),
    ]));
    bin
}

#[test]
fn the_output_of_batch_with_method_and_cell_is_attached_by_the_cell_of_its_point() {
    let s = Scratch::dir("attach_batch");
    let bin = sheet(&s);
    let input = write(&s, "q.csv", "address\nRue A 1 Ville\nVille\n");
    let answered = &s.join("a.csv");
    ok(&run(&[
        "batch",
        bin.to_str().unwrap(),
        input.to_str().unwrap(),
        answered.to_str().unwrap(),
        "--with",
        "method,cell:eu1km",
    ]));
    let a = std::fs::read_to_string(answered).unwrap();
    let code = cell(&a, 0, "gp_cell_eu1km").to_string();
    assert!(code.starts_with("CRS3035RES1000mN"), "{a}");
    write(&s, "g.csv", &format!("GRD_ID,T\n{code},42\n"));
    let t = write(
        &s,
        "g.table.json",
        r#"{"table":"g","file":"g.csv","prefix":"grid","key":{"column":"GRD_ID","kind":"cell:eu1km"},
        "take":{"T":{"as":"population"}},"edition":"e","publisher":"p","licence":"l","caveat":"c"}"#,
    );
    let out = ok(&run(&[
        "attach",
        answered.to_str().unwrap(),
        "--table",
        t.to_str().unwrap(),
    ]));
    assert_eq!(cell(&out, 0, "gp_grid_population"), "42");
    assert_eq!(cell(&out, 0, "gp_key_basis"), "point");
    // the town alone: the engine calls the answer weak, and its centre fixes no square kilometre anyway
    assert_eq!(cell(&out, 1, "gp_key_basis"), "weak_answer");
    assert_eq!(cell(&out, 1, "gp_grid_status"), "no_key");
}

/// The example of the docs card (`docs.html#attach`, `code/examples/attach`): the points the card shows, joined by the
/// excerpt it ships, give the values the card prints. The passport and the table of the example are read as they lie.
#[test]
fn the_example_of_the_docs_card_gives_the_values_the_card_prints() {
    let s = Scratch::dir("attach_example");
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/attach");
    let answers = write(
        &s,
        "answers.csv",
        "id,gp_lat,gp_lon,gp_precision,gp_weak,gp_point_method\n\
         A1,52.5158714,13.3780289,house,false,registry_point\n\
         A2,50.9413017,6.9571828,house,false,registry_point\n\
         A3,50.1106797,8.6818637,house,false,registry_point\n\
         A4,52.4078611,13.0633883,city,true,place_table\n",
    );
    let table = example.join("zensus2022_1km.table.json");
    let out = ok(&run(&[
        "attach",
        answers.to_str().unwrap(),
        "--table",
        table.to_str().unwrap(),
    ]));
    let population: Vec<String> = (0..4)
        .map(|i| cell(&out, i, "gp_zensus_population"))
        .collect();
    assert_eq!(population, ["3408", "2263", "4899", ""]);
    assert_eq!(cell(&out, 3, "gp_key_basis"), "weak_answer");
    // the excerpt of the Pro card: the tract codes it prints
    let rows = write(
        &s,
        "codes.csv",
        "id,gp_tract,gp_precision,gp_weak\nC1,11001980000,house,false\nC2,36061007600,house,false\n",
    );
    let acs = example.join("acs5_2024_tract.table.json");
    let out = ok(&run(&[
        "attach",
        rows.to_str().unwrap(),
        "--table",
        acs.to_str().unwrap(),
        "--on",
        "gp_tract",
        "--key-vintage",
        "2020",
    ]));
    assert_eq!(cell(&out, 0, "gp_acs_median_household_income"), "");
    assert_eq!(cell(&out, 0, "gp_acs_population"), "17");
    assert_eq!(cell(&out, 1, "gp_acs_median_household_income"), "193644");
}
