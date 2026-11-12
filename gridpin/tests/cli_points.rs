//! `gridpin nearest`, `gridpin join`, `--aliases` and `batch --reverse` on a small sheet and small files.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

fn sheet(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("points-{name}"));
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!(
            "{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\nrue b,001,ville,10000,2,,7.50,43.80,Rue B,Ville\n"
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
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (dir, bin)
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn table(path: &Path) -> Vec<std::collections::HashMap<String, String>> {
    let mut r = csv::Reader::from_path(path).unwrap();
    let head: Vec<String> = r.headers().unwrap().iter().map(String::from).collect();
    r.records()
        .map(|x| {
            head.iter()
                .cloned()
                .zip(x.unwrap().iter().map(String::from))
                .collect()
        })
        .collect()
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

const DEPOTS: &str = "id,name,lat,lon\nnorth,N,44.0,7.0\nsouth,S,43.0,7.0\neast,E,43.5,8.0\n";

#[test]
fn every_client_gets_its_nearest_depot_at_the_known_distance() {
    let (dir, _bin) = sheet("near");
    // one degree of latitude on the project's sphere is 111 194.9 m
    let clients = write(
        &dir,
        "c.csv",
        "client,lat,lon\na,44.5,7.0\nb,42.5,7.0\nc,43.5,8.5\nd,44.0,7.0\n",
    );
    let depots = write(&dir, "d.csv", DEPOTS);
    let out = dir.join("out.csv");
    ok(&run(&[
        "nearest",
        clients.to_str().unwrap(),
        depots.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]));
    let rows = table(&out);
    let ids: Vec<&str> = rows.iter().map(|r| r["nearest_id"].as_str()).collect();
    assert_eq!(ids, ["north", "south", "east", "north"]);
    assert_eq!(rows[0]["client"], "a", "the row's own columns stay");
    assert_eq!(rows[0]["nearest_distance_m"], "55597.5");
    assert_eq!(rows[3]["nearest_distance_m"], "0.0");
    assert_eq!(
        (
            rows[1]["nearest_lat"].as_str(),
            rows[1]["nearest_lon"].as_str()
        ),
        ("43", "7")
    );
    assert!(!rows[0].contains_key("nearest_rank"), "no rank at --k 1");
    assert!(!rows[0].contains_key("gp_lat"), "nothing was geocoded");
}

#[test]
fn k_gives_a_long_table_and_a_limit_leaves_a_row_empty_but_there() {
    let (dir, _bin) = sheet("near-k");
    let clients = write(
        &dir,
        "c.csv",
        "client,lat,lon\na,44.0,7.0\nfar,10.0,10.0\n,,\n",
    );
    let depots = write(&dir, "d.csv", DEPOTS);
    let out = dir.join("out.csv");
    ok(&run(&[
        "nearest",
        clients.to_str().unwrap(),
        depots.to_str().unwrap(),
        "--k",
        "2",
        "-o",
        out.to_str().unwrap(),
    ]));
    let rows = table(&out);
    assert_eq!(rows.iter().filter(|r| r["client"] == "a").count(), 2);
    assert_eq!(rows[0]["nearest_rank"], "1");
    assert_eq!(rows[1]["nearest_rank"], "2");
    let done = run(&[
        "nearest",
        clients.to_str().unwrap(),
        depots.to_str().unwrap(),
        "--max-km",
        "50",
        "-o",
        out.to_str().unwrap(),
    ]);
    ok(&done);
    let rows = table(&out);
    assert_eq!(rows.len(), 3, "N rows in, N rows out");
    assert_eq!(rows[0]["nearest_id"], "north");
    assert_eq!(rows[1]["nearest_id"], "", "beyond the limit");
    assert_eq!(rows[2]["nearest_id"], "", "no point in the row");
    assert!(
        String::from_utf8_lossy(&done.stderr).contains("2 of 3 rows"),
        "{done:?}"
    );
}

#[test]
fn a_distance_limit_must_be_a_distance() {
    let (dir, _bin) = sheet("near-limit");
    let clients = write(&dir, "c.csv", "lat,lon\n43.0,7.0\n");
    let depots = write(&dir, "d.csv", DEPOTS);
    for limit in ["0", "-5", "NaN", "inf"] {
        let done = run(&[
            "nearest",
            clients.to_str().unwrap(),
            depots.to_str().unwrap(),
            "--max-km",
            limit,
        ]);
        assert!(!done.status.success(), "{limit}");
        assert!(
            String::from_utf8_lossy(&done.stderr).contains("--max-km"),
            "{limit}"
        );
    }
}

#[test]
fn addresses_are_geocoded_on_the_sheet_and_the_row_says_how_it_was_placed() {
    let (dir, bin) = sheet("near-geo");
    let clients = write(&dir, "c.csv", "address\n\"rue a 1, ville\"\nzzzz qqqq\n");
    let depots = write(
        &dir,
        "d.csv",
        "address,code\n\"rue b 2, ville\",D1\n\"rue a 1, ville\",D2\n",
    );
    let out = dir.join("out.csv");
    ok(&run(&[
        "nearest",
        clients.to_str().unwrap(),
        depots.to_str().unwrap(),
        "--sheet",
        bin.to_str().unwrap(),
        "--id-column",
        "code",
        "-o",
        out.to_str().unwrap(),
    ]));
    let rows = table(&out);
    assert_eq!(rows[0]["nearest_id"], "D2");
    assert_eq!(rows[0]["nearest_distance_m"], "0.0");
    assert_eq!(rows[0]["gp_precision"], "house");
    assert_eq!(rows[1]["gp_lat"], "");
    assert_eq!(rows[1]["nearest_id"], "");
    let without = run(&[
        "nearest",
        clients.to_str().unwrap(),
        depots.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(!without.status.success());
    assert!(String::from_utf8_lossy(&without.stderr).contains("--sheet"));
}

#[test]
fn a_named_address_column_outranks_coordinates_in_the_same_file() {
    let (dir, bin) = sheet("near-column");
    let clients = write(&dir, "c.csv", "address,lat,lon\n\"rue a 1, ville\",10,10\n");
    let depots = write(&dir, "d.csv", "lat,lon\n43.73,7.42\n10,10\n");
    let out = dir.join("out.csv");
    ok(&run(&[
        "nearest",
        clients.to_str().unwrap(),
        depots.to_str().unwrap(),
        "--sheet",
        bin.to_str().unwrap(),
        "--rows-column",
        "address",
        "-o",
        out.to_str().unwrap(),
    ]));
    assert_eq!(
        table(&out)[0]["nearest_id"],
        "1",
        "placed by the address, not by the columns"
    );
}

#[test]
fn without_an_id_column_the_answer_is_the_line_of_the_list() {
    let (dir, _bin) = sheet("near-id");
    let clients = write(&dir, "c.csv", "lat,lon\n43.0,7.0\n");
    let depots = write(&dir, "d.csv", "lat,lon\n44.0,7.0\n43.0,7.0\n");
    let out = dir.join("out.csv");
    ok(&run(&[
        "nearest",
        clients.to_str().unwrap(),
        depots.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]));
    assert_eq!(table(&out)[0]["nearest_id"], "2");
}

const ZONES: &str = r#"{"type":"FeatureCollection","features":[
 {"type":"Feature","properties":{"name":"West","code":7},"geometry":{"type":"Polygon","coordinates":[[[0,0],[5,0],[5,5],[0,5],[0,0]]]}},
 {"type":"Feature","properties":{"name":"East"},"geometry":{"type":"Polygon","coordinates":[[[5,0],[10,0],[10,5],[5,5],[5,0]]]}}]}"#;

#[test]
fn join_gives_each_row_the_properties_of_its_zone() {
    let (dir, _bin) = sheet("join");
    let rows_in = write(&dir, "p.csv", "id,lat,lon\n1,2,2\n2,2,7\n3,20,20\n");
    let zones = write(&dir, "z.geojson", ZONES);
    let out = dir.join("out.csv");
    let done = run(&[
        "join",
        rows_in.to_str().unwrap(),
        zones.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    ok(&done);
    let rows = table(&out);
    assert_eq!(
        (rows[0]["zone_name"].as_str(), rows[0]["zone_code"].as_str()),
        ("West", "7")
    );
    assert_eq!(
        (rows[1]["zone_name"].as_str(), rows[1]["zone_code"].as_str()),
        ("East", "")
    );
    assert_eq!(rows[2]["zone_name"], "", "in no zone, and the row stays");
    assert!(
        String::from_utf8_lossy(&done.stderr).contains("1 of 3 rows"),
        "{done:?}"
    );
    ok(&run(&[
        "join",
        rows_in.to_str().unwrap(),
        zones.to_str().unwrap(),
        "--property",
        "name",
        "-o",
        out.to_str().unwrap(),
    ]));
    assert!(
        !table(&out)[0].contains_key("zone_code"),
        "only what was asked"
    );
    assert!(!run(&[
        "join",
        rows_in.to_str().unwrap(),
        zones.to_str().unwrap(),
        "--property",
        "nope",
        "-o",
        out.to_str().unwrap(),
    ])
    .status
    .success());
}

#[test]
fn an_alias_answers_as_its_address_with_the_flag_and_the_name() {
    let (dir, bin) = sheet("alias");
    let aliases = write(&dir, "a.csv", "name,address\nHQ,\"rue a 1, ville\"\n");
    let plain = |q: &str, extra: &[&str]| -> Vec<serde_json::Value> {
        let mut args = vec!["query", bin.to_str().unwrap(), q, "-k", "1"];
        args.extend_from_slice(extra);
        let out = run(&args);
        ok(&out);
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    let flag = ["--aliases", aliases.to_str().unwrap()];
    let direct = &plain("rue a 1, ville", &[])[0];
    let named = &plain("hq", &flag)[0];
    assert_eq!(named["lat"], direct["lat"]);
    assert_eq!(named["housenumber"], direct["housenumber"]);
    assert_eq!(named["alias_name"], "HQ");
    assert!(named["flags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f == "alias"));
    // any other line goes the ordinary way, byte for byte
    assert_eq!(plain("rue b 2 ville", &flag), plain("rue b 2 ville", &[]));
    assert!(
        plain("hq", &[]).is_empty(),
        "without the table HQ is nothing"
    );
}

#[test]
fn a_batch_reads_aliases_in_both_formats_and_a_csv_names_them_in_a_column() {
    let (dir, bin) = sheet("alias-batch");
    let aliases = write(&dir, "a.csv", "name,address\nHQ,\"rue a 1, ville\"\n");
    let a = aliases.to_str().unwrap();
    let input = write(&dir, "i.csv", "address\nhq\nrue b 2 ville\n");
    let out = dir.join("o.csv");
    ok(&run(&[
        "batch",
        bin.to_str().unwrap(),
        input.to_str().unwrap(),
        out.to_str().unwrap(),
        "--aliases",
        a,
    ]));
    let rows = table(&out);
    assert_eq!(
        (
            rows[0]["gp_street"].as_str(),
            rows[0]["gp_alias_name"].as_str()
        ),
        ("Rue A", "HQ")
    );
    assert_eq!(
        (
            rows[1]["gp_street"].as_str(),
            rows[1]["gp_alias_name"].as_str()
        ),
        ("Rue B", "")
    );
    ok(&run(&[
        "batch",
        bin.to_str().unwrap(),
        input.to_str().unwrap(),
        out.to_str().unwrap(),
    ]));
    assert!(
        !table(&out)[0].contains_key("gp_alias_name"),
        "no table, no column"
    );
    let lines = write(
        &dir,
        "i.jsonl",
        "{\"q\":\"HQ\"}\n{\"q\":\"rue b 2 ville\"}\n",
    );
    let out = dir.join("o.jsonl");
    ok(&run(&[
        "batch",
        bin.to_str().unwrap(),
        lines.to_str().unwrap(),
        out.to_str().unwrap(),
        "-k",
        "1",
        "--aliases",
        a,
    ]));
    let text = std::fs::read_to_string(&out).unwrap();
    let rows: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows[0]["results"][0]["alias_name"], "HQ");
    assert_eq!(rows[0]["results"][0]["street"], "Rue A");
    assert!(rows[1]["results"][0].get("alias_name").is_none());
    let bad = write(&dir, "bad.csv", "name,address\nHQ,a\nHQ,b\n");
    assert!(!run(&[
        "batch",
        bin.to_str().unwrap(),
        input.to_str().unwrap(),
        dir.join("x.csv").to_str().unwrap(),
        "--aliases",
        bad.to_str().unwrap()
    ])
    .status
    .success());
}

#[test]
fn a_csv_of_points_is_answered_by_reverse_with_the_distance() {
    let (dir, bin) = sheet("reverse");
    let input = write(&dir, "p.csv", "id,lat,lon\n1,43.73,7.42\n2,,\n3,999,7\n");
    let out = dir.join("o.csv");
    let done = run(&[
        "batch",
        bin.to_str().unwrap(),
        input.to_str().unwrap(),
        out.to_str().unwrap(),
        "--reverse",
    ]);
    ok(&done);
    let rows = table(&out);
    assert_eq!(rows[0]["gp_street"], "Rue A");
    assert_eq!(rows[0]["gp_housenumber"], "1");
    assert_eq!(rows[0]["gp_distance_m"], "0");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1]["gp_lat"], "");
    assert_eq!(rows[1]["gp_match_status"], "U");
    assert_eq!(
        rows[2]["gp_lat"], "",
        "not a coordinate: no answer, the row stays"
    );
    let spelled = write(&dir, "s.csv", "Latitude,LNG\n43.73,7.42\n");
    ok(&run(&[
        "batch",
        bin.to_str().unwrap(),
        spelled.to_str().unwrap(),
        out.to_str().unwrap(),
        "--reverse",
    ]));
    assert_eq!(
        table(&out)[0]["gp_street"],
        "Rue A",
        "the names nearest and join read"
    );
    let renamed = write(&dir, "q.csv", "Y,X\n43.73,7.42\n");
    ok(&run(&[
        "batch",
        bin.to_str().unwrap(),
        renamed.to_str().unwrap(),
        out.to_str().unwrap(),
        "--reverse",
        "--lat-column",
        "y",
        "--lon-column",
        "x",
    ]));
    assert_eq!(table(&out)[0]["gp_street"], "Rue A");
    assert!(!run(&[
        "batch",
        bin.to_str().unwrap(),
        renamed.to_str().unwrap(),
        out.to_str().unwrap(),
        "--reverse"
    ])
    .status
    .success());
}

/// The answer of `gridpin query`, as (street, number, commune), on a sheet with Plaza España beside Calle Muro.
fn country_tail_sheet(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("points-{name}"));
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!(
            "{HDR}calle muro,001,valladolid,47004,13,,-4.72,41.65,Calle Muro,Valladolid\n\
             plaza espana,001,valladolid,47004,13,,-4.73,41.66,Plaza España,Valladolid\n"
        ),
    )
    .unwrap();
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        r#"{"country":"es","layer":"addresses","license":"t","source_release":"test"}"#,
    )
    .unwrap();
    let bin = dir.join("s.bin");
    assert!(Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            bin.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap()
        ])
        .output()
        .unwrap()
        .status
        .success());
    (dir, bin)
}

fn top(bin: &Path, q: &str) -> (String, String, String) {
    let out = run(&["query", bin.to_str().unwrap(), q, "-k", "1"]);
    ok(&out);
    let text = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(text.lines().next().unwrap_or("{}")).unwrap();
    let cell = |k: &str| v[k].as_str().unwrap_or("").to_string();
    (cell("street"), cell("housenumber"), cell("commune"))
}

fn flags(bin: &Path, q: &str, extra: &[&str]) -> Vec<String> {
    let mut args = vec!["query", bin.to_str().unwrap(), q, "-k", "1"];
    args.extend_from_slice(extra);
    let out = run(&args);
    ok(&out);
    let text = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(text.lines().next().unwrap_or("{}")).unwrap();
    v["flags"]
        .as_array()
        .map(|f| f.iter().map(|x| x.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

fn muro() -> (String, String, String) {
    ("Calle Muro".into(), "13".into(), "Valladolid".into())
}

#[test]
fn a_country_closing_the_query_is_not_a_street() {
    let (_dir, bin) = country_tail_sheet("country-tail");
    for tail in ["España", "ESPAÑA", "Espanya", "Spain"] {
        let q = format!("Calle Muro, 13, 47004 Valladolid, {tail}");
        assert_eq!(top(&bin, &q), muro(), "{q}");
        // on this small sheet the old path reaches the same house too, by dropping the tail as a fallback
        // (flag dropped_suffix); on the real sheet it stopped earlier, at Plaza España. The flag shows the path.
        assert!(
            !flags(&bin, &q, &[]).contains(&"dropped_suffix".to_string()),
            "{q}"
        );
    }
}

#[test]
fn the_near_path_reads_the_closing_country_the_same_way() {
    let (_dir, bin) = country_tail_sheet("country-near");
    let out = run(&[
        "query",
        bin.to_str().unwrap(),
        "Calle Muro, 13, 47004 Valladolid, España",
        "-k",
        "1",
        "--near",
        "41.65,-4.72",
    ]);
    ok(&out);
    let v: serde_json::Value = serde_json::from_str(
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(v["street"], "Calle Muro");
    let path = flags(
        &bin,
        "Calle Muro, 13, 47004 Valladolid, España",
        &["--near", "41.65,-4.72"],
    );
    assert!(!path.contains(&"dropped_suffix".to_string()), "{path:?}");
}

#[test]
fn a_name_that_is_not_the_closing_segment_stays_a_name() {
    let (_dir, bin) = country_tail_sheet("country-name");
    let plaza = (
        "Plaza España".to_string(),
        "13".to_string(),
        "Valladolid".to_string(),
    );
    assert_eq!(top(&bin, "Plaza España 13, Valladolid"), plaza);
    assert_eq!(top(&bin, "Valladolid, Plaza España 13"), plaza);
    // the street itself may close the query after its number
    assert_eq!(top(&bin, "13, Plaza España"), plaza);
}

#[test]
fn a_commune_with_the_name_of_its_own_country_stays_a_commune() {
    // a Spanish sheet with a commune called Spain: the segment names the commune
    let dir = Scratch::dir("points-country-commune");
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!("{HDR}rua a,002,spain,10000,1,,-4.80,41.70,Rua A,Spain\n"),
    )
    .unwrap();
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        r#"{"country":"es","layer":"addresses","license":"t","source_release":"test"}"#,
    )
    .unwrap();
    let bin = dir.join("s.bin");
    assert!(Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            bin.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap()
        ])
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(
        top(&bin, "Rua A 1, Spain"),
        ("Rua A".to_string(), "1".to_string(), "Spain".to_string())
    );
    assert!(flags(&bin, "Rua A 1, Spain", &[]).contains(&"commune_exact".to_string()));
}
