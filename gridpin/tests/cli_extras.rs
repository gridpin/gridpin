//! `--with`: the extra answer fields on a fixture street with houses on both sides, through the built binary.
//! The street runs east along 43.7300; odd numbers stand 20 m north of its axis, even numbers 20 m south.
use std::path::{Path, PathBuf};
use std::process::Command;

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";
/// 20 m of latitude
const SIDE: f64 = 20.0 / 111_320.0;

fn sheet(name: &str, rows: &str) -> (Scratch, PathBuf) {
    sheet_of("mc", name, rows)
}

/// A fixture sheet whose passport names `country` (the postal layout of `--with place` and `postal` follows it).
fn sheet_of(country: &str, name: &str, rows: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("extras-{name}"));
    let csv = dir.join("in.csv");
    std::fs::write(&csv, format!("{HDR}{rows}")).unwrap();
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        format!(r#"{{"country":"{country}","layer":"addresses","license":"CC BY 4.0","source_release":"2026-06-17.0","attribution":"Fixture & Co","sources":"fixture"}}"#),
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

/// Both sides of Grand Rue (odd north, even south, 10 m apart along the street) and a one-sided street.
fn rows(across_m: f64) -> String {
    let south = 43.73 - across_m / 2.0 / 111_320.0;
    let north = 43.73 + SIDE;
    // the builder wants numbers ascending within a street
    let mut grand: Vec<(u32, f64, f64)> = (0..5)
        .flat_map(|i| {
            let lon = 7.42 + i as f64 * 0.000125;
            [(2 * i + 1, lon, north), (2 * i + 2, lon, south)]
        })
        .collect();
    grand.sort_by_key(|h| h.0);
    let mut r = String::new();
    for (n, lon, lat) in grand {
        r += &format!("grand rue,001,ville,10000,{n},,{lon:.7},{lat:.7},Grand Rue,Ville\n");
    }
    for (i, n) in [1, 3, 5].iter().enumerate() {
        r += &format!(
            "quai seul,001,ville,10000,{n},,{:.7},43.7400000,Quai Seul,Ville\n",
            7.43 + i as f64 * 0.000125
        );
    }
    r
}

fn query(bin: &Path, q: &str, with: Option<&str>) -> serde_json::Value {
    let mut args = vec!["query", bin.to_str().unwrap(), q, "-k", "1"];
    if let Some(w) = with {
        args.extend(["--with", w]);
    }
    let out = Command::new(BIN).args(&args).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn metres(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dy = (a.0 - b.0) * 111_320.0;
    let dx = (a.1 - b.1) * 111_320.0 * a.0.to_radians().cos();
    (dx * dx + dy * dy).sqrt()
}

#[test]
fn the_street_point_lies_half_way_to_the_houses_across_the_street() {
    let (_d, bin) = sheet("both", &rows(40.0));
    let hit = query(&bin, "grand rue 5 ville", Some("street_point"));
    assert_eq!(hit["housenumber"], "5");
    let p = &hit["extras"]["street_point"];
    let point = (p["lat"].as_f64().unwrap(), p["lon"].as_f64().unwrap());
    // house 5 is north at 7.42025; the houses across are on the south line: the axis is 43.7300
    assert!(metres(point, (43.73, 7.42025)) < 0.5, "{hit}");
    let m = hit["extras"]["street_point_m"].as_f64().unwrap();
    assert!((m - 20.0).abs() < 0.5, "{m}");
}

#[test]
fn no_houses_across_or_too_far_across_means_no_street_point() {
    let (_d, bin) = sheet("one-sided", &rows(40.0));
    let hit = query(&bin, "quai seul 3 ville", Some("street_point"));
    assert_eq!(hit["housenumber"], "3");
    assert!(
        hit["extras"]["street_point"].is_null(),
        "same parity is the same side: {hit}"
    );
    let (_d, wide) = sheet("wide", &rows(260.0));
    let hit = query(&wide, "grand rue 5 ville", Some("street_point"));
    assert!(
        hit["extras"]["street_point"].is_null(),
        "houses 150 m across are not across the street: {hit}"
    );
}

#[test]
fn without_with_the_answer_is_the_answer_of_before() {
    let (_d, bin) = sheet("plain", &rows(40.0));
    let plain = query(&bin, "grand rue 5 ville", None);
    assert!(plain.get("extras").is_none());
    let asked = query(&bin, "grand rue 5 ville", Some("h3,codes"));
    let mut without = asked.clone();
    without.as_object_mut().unwrap().remove("extras");
    assert_eq!(without, plain, "the fields of the answer do not move");
}

#[test]
fn provenance_names_the_sheet_and_its_digest() {
    let (_d, bin) = sheet("card", &rows(40.0));
    let card = &query(&bin, "grand rue 5 ville", Some("provenance"))["extras"]["provenance"];
    assert_eq!(card["data_date"], "2026-06-17.0");
    assert_eq!(card["license"], "CC BY 4.0");
    assert_eq!(card["country"], "mc");
    assert!(card["release"].is_null(), "a fixture is no released sheet");
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(std::fs::read(&bin).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(card["sha256"], digest.as_str());
}

#[test]
fn csv_gets_the_extra_columns_after_the_answer_and_before_the_rank() {
    let (d, bin) = sheet("csv", &rows(40.0));
    let input = d.join("in2.csv");
    std::fs::write(&input, "id,address\n1,grand rue 5 ville\n2,nowhere 1\n").unwrap();
    let out = d.join("out.csv");
    let run = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            out.to_str().unwrap(),
            "-k",
            "2",
            "--with",
            "links,h3:8",
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let text = std::fs::read_to_string(&out).unwrap();
    let head: Vec<&str> = text.lines().next().unwrap().split(',').collect();
    let at = |name: &str| {
        head.iter()
            .position(|h| *h == name)
            .unwrap_or_else(|| panic!("{name} in {head:?}"))
    };
    assert_eq!(
        at("gp_h3"),
        at("gp_match_status") + 1,
        "h3 first, whatever the spec order"
    );
    assert_eq!(at("gp_osm_url"), at("gp_h3") + 1);
    assert_eq!(at("gp_rank"), head.len() - 1);
    let first: Vec<&str> = text.lines().nth(1).unwrap().split(',').collect();
    assert!(first[at("gp_h3")].starts_with("88"), "{first:?}");
    let none: Vec<&str> = text
        .lines()
        .find(|l| l.starts_with("2,"))
        .unwrap()
        .split(',')
        .collect();
    assert_eq!(none.len(), head.len());
    assert!(none[at("gp_h3")].is_empty(), "no answer, no cell: {none:?}");
}

#[test]
fn a_bad_spec_stops_before_any_answer() {
    let (_d, bin) = sheet("bad", &rows(40.0));
    for bad in ["zip", "h3:20", "tz"] {
        let out = Command::new(BIN)
            .args([
                "query",
                bin.to_str().unwrap(),
                "grand rue 5 ville",
                "--with",
                bad,
            ])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{bad}");
        assert!(out.stdout.is_empty(), "{bad}");
    }
}

#[test]
fn place_writes_the_answer_as_a_person_does_at_the_asked_level() {
    let (_d, bin) = sheet_of("de", "place", &rows(40.0));
    let at = |level: &str| {
        query(&bin, "grand rue 5 ville", Some(&format!("place:{level}")))["extras"]["place"].clone()
    };
    let house = at("house");
    assert_eq!(
        house["levels"]["house"], "Grand Rue 5",
        "number after the street where the post writes it so (de)"
    );
    assert_eq!(
        house["short"], "Grand Rue 5",
        "the only Grand Rue around: no town needed"
    );
    assert_eq!(at("street")["short"], "Grand Rue");
    assert_eq!(at("locality")["short"], "Ville");
    let bad = Command::new(BIN)
        .args([
            "query",
            bin.to_str().unwrap(),
            "grand rue 5 ville",
            "--with",
            "place:country",
        ])
        .output()
        .unwrap();
    assert!(!bad.status.success());
}

#[test]
fn place_names_the_town_when_a_street_of_that_name_is_near() {
    let mut rows = rows(40.0);
    // another Grand Rue, in the next town 1 km east
    let other: String = (1..=3)
        .map(|n| {
            format!(
                "grand rue,002,autre,20000,{n},,{:.7},43.7300000,Grand Rue,Autre\n",
                7.4325 + n as f64 * 0.000125
            )
        })
        .collect();
    let at = rows.find("quai seul").unwrap();
    rows.insert_str(at, &other);
    let (_d, bin) = sheet_of("de", "place-near", &rows);
    let place = &query(&bin, "grand rue 5 ville", Some("place"))["extras"]["place"];
    assert_eq!(place["short"], "Grand Rue 5, Ville", "{place}");
}

#[test]
fn verdict_says_what_happened_to_each_field_of_the_query() {
    let (_d, bin) = sheet("verdict", &rows(40.0));
    let asked = query(&bin, "grand rue 5 ville", Some("verdict"));
    let v = &asked["extras"]["verdict"];
    assert_eq!(
        (
            v["street"].as_str(),
            v["number"].as_str(),
            v["city"].as_str()
        ),
        (Some("confirmed"), Some("confirmed"), Some("confirmed")),
        "{v}"
    );
    assert_eq!(
        v["postcode"], "inferred",
        "the query had no postcode, the answer has one"
    );
    let off = query(&bin, "grand ru 5 ville", Some("verdict"));
    assert_eq!(off["extras"]["verdict"]["street"], "corrected", "{off}");
    let reverse = Command::new(BIN)
        .args([
            "reverse",
            bin.to_str().unwrap(),
            "43.7302",
            "7.42",
            "-k",
            "1",
            "--with",
            "verdict",
        ])
        .output()
        .unwrap();
    let back: serde_json::Value = serde_json::from_slice(&reverse.stdout).unwrap();
    assert!(
        back["extras"]["verdict"].is_null(),
        "no query, no verdict: {back}"
    );
}

#[test]
fn bench_counts_hits_like_the_benchmark_tool() {
    let (d, bin) = sheet("bench", &rows(40.0));
    // house 1 of Grand Rue is at 7.42, 43.7301797 (20 m north of the axis): one row on it, one 1 km off, one silent
    let truth = d.join("truth.csv");
    std::fs::write(&truth, "address,lat,lon\ngrand rue 1 ville,43.7301797,7.42\ngrand rue 1 ville,43.7391797,7.42\nnowhere 1,43.73,7.42\nville,43.73,7.42\n").unwrap();
    let out = Command::new(BIN)
        .args(["bench", bin.to_str().unwrap(), truth.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        (
            s["rows"].as_u64(),
            s["answered"].as_u64(),
            s["within_150m"].as_u64()
        ),
        (Some(4), Some(3), Some(1)),
        "{s}"
    );
    assert_eq!(s["by_match_level"]["U"], 1);
    assert_eq!(
        s["match_rate"], 0.5,
        "two of four at an address; the town answer does not count: {s}"
    );
    let strict = Command::new(BIN)
        .args([
            "bench",
            bin.to_str().unwrap(),
            truth.to_str().unwrap(),
            "--min-match-rate",
            "0.9",
        ])
        .output()
        .unwrap();
    assert!(!strict.status.success(), "2 of 4 matched is below 0.9");
}

#[test]
fn format_prints_the_answer_in_the_postal_layout_of_the_sheet() {
    let (_dir, bin) = sheet("format", &rows(20.0));
    let out = std::process::Command::new(BIN)
        .args([
            "format",
            bin.to_str().unwrap(),
            "grand rue 3 ville",
            "--layout",
            "fr",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // the French layout: the number comes first
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim_end(),
        "3 Grand Rue\n10000 Ville"
    );
}

#[test]
fn without_a_code_the_postal_layout_is_the_one_of_the_sheets_country() {
    // ANSWERS §278: `--with postal` and `gridpin format` follow the sheet's country when no layout is named
    let (_dir, bin) = sheet("format-sheet", &rows(20.0));
    let format = |extra: &[&str]| {
        let mut args = vec!["format", bin.to_str().unwrap(), "grand rue 3 ville"];
        args.extend_from_slice(extra);
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    };
    let own = format(&[]);
    assert_eq!(
        own,
        format(&["--layout", "mc"]),
        "the fixture sheet is of Monaco"
    );
    assert_ne!(own, format(&["--layout", "de"]));
    assert_ne!(own, format(&["--layout", "default"]));
    let q = Command::new(BIN)
        .args([
            "query",
            bin.to_str().unwrap(),
            "grand rue 3 ville",
            "-k",
            "1",
            "--with",
            "postal",
        ])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&q.stdout).unwrap();
    let hit = if v.is_array() { v[0].clone() } else { v };
    assert_eq!(hit["extras"]["postal"], own.as_str(), "{hit}");
}

/// The grammar `gridpin grammar` writes for a fixed sheet, kept in `tests/data/grammar_mc.json`. The Python test
/// `tools/test_grammar_schema.py` checks that file against `docs-public/address-grammar.schema.json`: together they check the
/// program's output against the schema without the measurement engine of the tree (`target/release`), which the gate
/// does not rebuild. Regenerate with `GRIDPIN_UPDATE_GOLDEN=1 cargo test --test cli_extras grammar`.
#[test]
fn grammar_of_a_fixed_sheet_is_the_file_the_schema_test_reads() {
    let dir = Scratch::dir("extras-grammar");
    let csv = dir.join("in.csv");
    let mut rows = String::from(HDR);
    for (street, n) in [
        ("avenue du port", 2),
        ("rue des lilas", 1),
        ("rue haute", 3),
    ] {
        let title: String = street
            .split(' ')
            .map(|w| {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ");
        rows += &format!("{street},001,ville,10000,{n},,7.42,43.73,{title},Ville\n");
    }
    std::fs::write(&csv, rows).unwrap();
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
            csv.to_str().unwrap(),
            bin.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    let out = Command::new(BIN)
        .args(["grammar", bin.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut got = got;
    // the sheet's file name differs per run
    got["source"]["sheet"] = "s.bin".into();
    let golden =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/grammar_mc.json");
    if std::env::var_os("GRIDPIN_UPDATE_GOLDEN").is_some() {
        // non-ASCII as \u escapes: the file is exported, and public files carry no Cyrillic outside data literals
        let text: String = serde_json::to_string_pretty(&got)
            .unwrap()
            .chars()
            .flat_map(|c| {
                if c.is_ascii() {
                    vec![c.to_string()]
                } else {
                    let mut buf = [0u16; 2];
                    c.encode_utf16(&mut buf)
                        .iter()
                        .map(|u| format!("\\u{u:04x}"))
                        .collect()
                }
            })
            .collect();
        std::fs::write(&golden, text + "\n").unwrap();
    }
    let want: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&golden).unwrap()).unwrap();
    assert_eq!(
        got, want,
        "gridpin grammar changed: regenerate tests/data/grammar_mc.json and rerun the schema test"
    );
}

/// Revision 0.9.0: on a sheet whose post writes the number first (fr, mc), `place` wrote "Rue … 37".
#[test]
fn place_writes_the_number_where_the_post_of_the_sheets_country_writes_it() {
    for (country, house) in [
        ("fr", "5 Grand Rue"),
        ("mc", "5 Grand Rue"),
        ("de", "Grand Rue 5"),
        ("it", "Grand Rue 5"),
    ] {
        let (_d, bin) = sheet_of(country, &format!("place-{country}"), &rows(40.0));
        let place = &query(&bin, "grand rue 5 ville", Some("place"))["extras"]["place"];
        assert_eq!(place["levels"]["house"], house, "{country}: {place}");
        assert_eq!(place["short"], house, "{country}: {place}");
    }
}

/// Revision 0.9.0: the CSV header followed one order and the cells another, so `--with exists,crs:…` wrote
/// `gp_x=yes`. Every `gp_*` cell of `batch` is the field of the same name in the JSON of `query`.
#[test]
fn every_csv_cell_of_with_is_the_json_field_of_its_own_column() {
    let (d, bin) = sheet("csv-order", &rows(40.0));
    let input = d.join("in.csv");
    std::fs::write(&input, "id,address\n1,grand rue 5 ville\n").unwrap();
    let spec = "exists,crs:EPSG:3035,area,unit,iso20022,verdict,place,postal,provenance,h3,codes";
    let out = d.join("out.csv");
    let run = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            out.to_str().unwrap(),
            "--with",
            spec,
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let mut r = csv::Reader::from_path(&out).unwrap();
    let head = r.headers().unwrap().clone();
    let row = r.records().next().unwrap().unwrap();
    let cell = |name: &str| {
        let at = head
            .iter()
            .position(|h| h == name)
            .unwrap_or_else(|| panic!("{name} in {head:?}"));
        row[at].to_string()
    };
    let e = query(&bin, "grand rue 5 ville", Some(spec))["extras"].clone();
    let s = |v: &serde_json::Value| match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(t) => t.clone(),
        v => v.to_string(),
    };
    for (col, want) in [
        ("gp_x", s(&e["crs"]["x"])),
        ("gp_y", s(&e["crs"]["y"])),
        ("gp_crs", s(&e["crs"]["code"])),
        ("gp_exists", s(&e["exists"]["exists"])),
        ("gp_exists_reason", s(&e["exists"]["reason"])),
        ("gp_area_spread_m", s(&e["area"]["spread_m"])),
        ("gp_iso_mode", s(&e["iso20022"]["mode"])),
        ("gp_iso_strt_nm", s(&e["iso20022"]["PstlAdr"]["StrtNm"])),
        ("gp_verdict_street", s(&e["verdict"]["street"])),
        ("gp_place", s(&e["place"]["short"])),
        ("gp_postal", s(&e["postal"])),
        ("gp_license", s(&e["provenance"]["license"])),
        ("gp_h3", s(&e["h3"])),
        ("gp_plus_code", s(&e["plus_code"])),
    ] {
        assert_eq!(cell(col), want, "{col}");
    }
    assert_eq!(cell("gp_crs"), "EPSG:3035");
    assert!(cell("gp_x").parse::<f64>().is_ok(), "{}", cell("gp_x"));
    assert!(
        ["yes", "maybe", "no"].contains(&cell("gp_exists").as_str()),
        "{}",
        cell("gp_exists")
    );
    assert_eq!(cell("gp_iso_mode"), "structured");
}
