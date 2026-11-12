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

#[test]
fn exists_says_yes_maybe_or_no_with_the_reason() {
    let (_dir, bin) = sheet("exists");
    let s = bin.to_str().unwrap();
    let e = |q: &str| {
        let v = json(&["exists", s, q]);
        (
            v["exists"].as_str().unwrap().to_string(),
            v["reason"].as_str().unwrap().to_string(),
        )
    };
    let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
    assert_eq!(e("rue a 1, ville"), pair("yes", "house_found"));
    assert_eq!(e("rue a 999, ville"), pair("no", "number_not_on_street"));
    assert_eq!(e("rue a, ville"), pair("maybe", "no_number"));
    assert_eq!(e("xyzzy plugh 5"), pair("no", "empty"));
}

#[test]
fn exists_says_wrong_city_when_the_street_is_in_another_town_than_the_named_one() {
    let dir = Scratch::dir("exists-city");
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!("{HDR}avenue du port,002,bourg,20000,1,,7.500,43.800,Avenue du Port,Bourg\nrue des lilas,001,ville,10000,1,,7.420,43.730,Rue des Lilas,Ville\n"),
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
            csv.to_str().unwrap(),
            bin.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    let v = json(&["exists", bin.to_str().unwrap(), "rue des lilas 1, bourg"]);
    assert_eq!(
        (v["exists"].as_str(), v["reason"].as_str()),
        (Some("no"), Some("wrong_city")),
        "{v}"
    );
}

#[test]
fn explain_names_the_reading_the_verdicts_and_the_candidates_or_the_reason_of_a_silence() {
    let (_dir, bin) = sheet("explain");
    let s = bin.to_str().unwrap();
    let text = |q: &str| {
        let out = Command::new(BIN).args(["explain", s, q]).output().unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let t = text("rue a 3 ville");
    assert!(
        t.contains("street: confirmed") && t.contains("number: confirmed"),
        "{t}"
    );
    assert!(t.contains("1. Rue A 3, Ville (house)"), "{t}");
    let two = text("rue 1 ville");
    assert!(
        two.contains("1. Rue ") && two.contains("2. Rue ") && two.contains("lost on"),
        "candidates: {two}"
    );
    let silent = text("xyzzy plugh 999");
    assert!(silent.contains("No answer: "), "{silent}");
    let j = json(&["explain", s, "rue a 3 ville", "--format", "json"]);
    assert_eq!(j["exists"]["exists"], "yes");
    assert_eq!(j["reading"]["house_number"], 3);
}

fn build_rows(name: &str, rows: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(name);
    let csv = dir.join("in.csv");
    std::fs::write(&csv, format!("{HDR}{rows}")).unwrap();
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        r#"{"country":"it","layer":"addresses","license":"t","source_release":"test"}"#,
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
    (dir, bin)
}

#[test]
fn an_exonym_names_the_local_town_only_when_the_sheet_has_no_town_of_that_name() {
    let zaragoza = "calle mayor,001,zaragoza,50001,1,,-0.88,41.65,Calle Mayor,Zaragoza\n";
    let saragossa = "calle mayor,002,saragossa,30100,1,,-97.5,30.9,Calle Mayor,Saragossa\n";
    let (_d, both) = build_rows("exonym", &format!("{zaragoza}{saragossa}"));
    // a commune "Saragossa" of its own: the name is read as written
    let q = "Calle Mayor 1, Saragossa";
    assert_eq!(
        json(&["query", both.to_str().unwrap(), q])["commune"],
        "Saragossa"
    );
    // no commune "Saragossa": the exonym names Zaragoza
    // and Huesca has a Calle Mayor 1 too: without the exonym the town named would not choose
    let huesca = "calle mayor,003,huesca,22001,1,,-0.41,42.14,Calle Mayor,Huesca\n";
    let zaragoza_last = "calle mayor,009,zaragoza,50001,1,,-0.88,41.65,Calle Mayor,Zaragoza\n";
    let (_d2, only) = build_rows("exonym2", &format!("{huesca}{zaragoza_last}"));
    assert_eq!(
        json(&["query", only.to_str().unwrap(), q])["commune"],
        "Zaragoza"
    );
}

/// Revision 0.9.0,(E1): "majorca → palma" put every address of the island in Palma ("Carrer de sa Lluna 5,
/// Sóller, Majorca" → a house 16 km away). The table holds towns only; these islands and regions are the ones people
/// write as the last segment of an address in the languages of the table.
#[test]
fn the_exonym_table_holds_towns_not_islands_or_regions() {
    let table = include_str!("../src/exonyms.tsv");
    let names: Vec<String> = table
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .flat_map(|(e, local)| [e.to_lowercase(), local.to_lowercase()])
        .collect();
    for place in [
        "majorca",
        "mallorca",
        "minorca",
        "menorca",
        "ibiza",
        "tenerife",
        "sicily",
        "sicilia",
        "sardinia",
        "sardegna",
        "corsica",
        "corse",
        "crete",
        "bavaria",
        "bayern",
        "tuscany",
        "toscana",
        "catalonia",
        "cataluña",
        "andalusia",
        "andalucía",
        "brittany",
        "bretagne",
        "normandy",
        "flanders",
        "lombardy",
        "lombardia",
        "saxony",
        "sachsen",
        "galicia",
        "vojvodina",
    ] {
        assert!(!names.contains(&place.to_string()), "{place} is not a town");
    }
    // the near miss: a town of the same table stays
    assert!(names.contains(&"zaragoza".to_string()));
}

#[test]
fn an_island_written_last_does_not_pull_the_house_into_one_town_of_it() {
    // the street in Sóller and in Palma: "…, Majorca" names neither, so it must not choose Palma
    let soller = "carrer de sa lluna,001,soller,07100,5,,2.715,39.767,Carrer de sa Lluna,Sóller\n";
    let palma = "carrer de sa lluna,002,palma,07001,5,,2.650,39.570,Carrer de sa Lluna,Palma\n";
    let (_d, bin) = build_rows("exonym-island", &format!("{soller}{palma}"));
    let out = Command::new(BIN)
        .args([
            "query",
            bin.to_str().unwrap(),
            "Carrer de sa Lluna 5, Majorca",
            "-k",
            "1",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) {
        assert!(
            !v["flags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f == "commune_exact")
                || v["commune"] != "Palma",
            "the island read as Palma: {v}"
        );
    }
    // the near miss: an exonym of a town still names it
    let q = json(&[
        "query",
        bin.to_str().unwrap(),
        "Carrer de sa Lluna 5, Palma",
    ]);
    assert_eq!(q["commune"], "Palma");
}

#[test]
fn a_sheet_from_bytes_answers_as_the_same_sheet_from_its_file() {
    let (_dir, bin) = sheet("from-bytes");
    let file = gridpin::query::Index::open_address(&bin).unwrap();
    let bytes = gridpin::query::Index::from_bytes(std::fs::read(&bin).unwrap(), "s.bin").unwrap();
    for q in ["rue a 1 ville", "rue a 3 ville", "rue 1 ville", "xyzzy"] {
        let a = serde_json::to_string(&gridpin::query::query_cascade(&file, None, q, 3)).unwrap();
        let b = serde_json::to_string(&gridpin::query::query_cascade(&bytes, None, q, 3)).unwrap();
        assert_eq!(a, b, "{q}");
    }
    assert!(gridpin::query::Index::from_bytes(vec![0; 16], "broken.bin").is_err());
}

#[test]
fn exists_counts_a_district_house_for_the_city_the_sheet_records_by_districts() {
    // ANSWERS §284 p.5: "Berlin" holds a handful of houses, its district Mitte thousands; the house of the district,
    // 5 km from the city's point, is in the city named. Next to it, a town of its own stays wrong_city (test above).
    let dir = Scratch::dir("exists-districts");
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!(
            "{HDR}place de la ville,001,grande ville,10000,1,,7.420,43.730,Place de la Ville,Grande Ville\n\
             rue du quartier,002,quartier,10001,1,,7.420,43.775,Rue du Quartier,Quartier\n\
             rue du quartier,002,quartier,10001,2,,7.4205,43.775,Rue du Quartier,Quartier\n\
             rue du quartier,002,quartier,10001,3,,7.421,43.775,Rue du Quartier,Quartier\n\
             rue lointaine,003,lointain,20000,1,,7.420,44.200,Rue Lointaine,Lointain\n\
             rue lointaine,003,lointain,20000,2,,7.4205,44.200,Rue Lointaine,Lointain\n"
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
            csv.to_str().unwrap(),
            bin.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    let v = json(&[
        "exists",
        bin.to_str().unwrap(),
        "rue du quartier 2, grande ville",
    ]);
    assert_eq!(
        (v["exists"].as_str(), v["reason"].as_str()),
        (Some("yes"), Some("house_found")),
        "{v}"
    );
    // a bigger commune 52 km away is not a district of the city (the engine drops it before exists sees it)
    let far = json(&[
        "exists",
        bin.to_str().unwrap(),
        "rue lointaine 1, grande ville",
    ]);
    assert_eq!(far["exists"].as_str(), Some("no"), "{far}");
}

#[test]
fn exists_does_not_confirm_a_house_when_the_named_town_is_not_on_the_sheet() {
    // "Sparkassenstraße 5, 80331 München" on a sheet without Bavaria answered a house 500 km away (ANSWERS §308)
    let (_dir, bin) = sheet("exists-unknown-town");
    let s = bin.to_str().unwrap();
    let v = json(&["exists", s, "rue a 1, lointainville"]);
    assert_eq!(
        (v["exists"].as_str(), v["reason"].as_str()),
        (Some("maybe"), Some("town_not_on_sheet")),
        "{v}"
    );
    // the postcode of the line places the house: then the house is enough
    let v = json(&["exists", s, "rue a 1, 10000 lointainville"]);
    assert_eq!(v["exists"], "yes", "{v}");
    // the near miss: the town of the sheet
    let v = json(&["exists", s, "rue a 1, ville"]);
    assert_eq!(v["exists"], "yes", "{v}");
}

/// A sheet for the forms of a named town (revision of 0.9.0): "Grande Ville" with a few houses, a bigger commune
/// "Voisin" 21 km away, "Sarstedt, Stadt" (a commune whose name holds a comma) and "Quartier" 5 km from the city.
fn towns_sheet(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("exists-towns-{name}"));
    let mut rows = vec![
        "place de la ville,001,grande ville,10000,1,,7.420,43.730,Place de la Ville,Grande Ville".to_string(),
        "rue du quartier,002,quartier,10001,1,,7.420,43.775,Rue du Quartier,Quartier".to_string(),
        "rue du quartier,002,quartier,10001,2,,7.4205,43.775,Rue du Quartier,Quartier".to_string(),
        "rue du quartier,002,quartier,10001,3,,7.421,43.775,Rue du Quartier,Quartier".to_string(),
        "rue de la gare,005,sarstedt stadt,30000,4,,7.900,44.500,Rue de la Gare,\"Sarstedt, Stadt\"".to_string(),
        // "Stadt" alone names a commune's start: without reading "Sarstedt, Stadt" as one name it would be the town
        "rue du lac,006,stadt am see,40000,1,,8.300,45.000,Rue du Lac,Stadt am See".to_string(),
        // a commune of two letters, the way a state code is written
        "rue de ab,009,ab,70000,1,,9.500,46.000,Rue de AB,AB".to_string(),
    ];
    for n in 1..=6 {
        rows.push(format!(
            "rue voisine,003,voisin,20000,{n},,{:.4},43.920,Rue Voisine,Voisin",
            7.420 + f64::from(n) * 0.0005
        ));
    }
    rows.sort();
    std::fs::write(dir.join("in.csv"), format!("{HDR}{}\n", rows.join("\n"))).unwrap();
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
            dir.join("in.csv").to_str().unwrap(),
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

#[test]
fn exists_reads_the_town_in_the_forms_people_write_it() {
    let (_dir, bin) = towns_sheet("forms");
    let s = bin.to_str().unwrap();
    let e = |q: &str| {
        let v = json(&["exists", s, q]);
        format!(
            "{} {}",
            v["exists"].as_str().unwrap(),
            v["reason"].as_str().unwrap()
        )
    };
    // a district 5 km from the city: in the city (ANSWERS §284 p.5)
    assert_eq!(e("rue du quartier 1, grande ville"), "yes house_found");
    // a bigger commune 21 km away is not a district of it: the engine's own test reaches 25 km
    assert_eq!(e("rue voisine 1, grande ville"), "no wrong_city");
    // the town before its district ("Köln-Nippes")
    assert_eq!(e("rue voisine 1, voisin-centre"), "yes house_found");
    // a commune whose name holds a comma: two segments are one town
    assert_eq!(e("rue de la gare 4, sarstedt, stadt"), "yes house_found");
}

#[test]
fn iso20022_takes_the_named_town_not_the_house_elsewhere() {
    let (_dir, bin) = towns_sheet("iso");
    let s = bin.to_str().unwrap();
    let iso = |q: &str| json(&["format", s, q, "--out", "iso20022"]);
    // the house of Voisin for a line naming Grande Ville: hybrid in the town the line names
    let v = iso("rue voisine 1, grande ville");
    assert_eq!(
        (v["mode"].as_str(), v["PstlAdr"]["TwnNm"].as_str()),
        (Some("hybrid"), Some("grande ville")),
        "{v}"
    );
    assert!(v["PstlAdr"].get("StrtNm").is_none(), "{v}");
    // the house with another postcode than the line's: exists maybe, so not structured
    let p = iso("rue voisine 1, 20001 voisin");
    assert_eq!(p["exists"]["reason"], "postcode_differs", "{p}");
    assert_eq!(p["mode"], "hybrid", "{p}");
    // a two-letter segment is a state code, never TwnNm, even when a commune of the sheet spells it
    let w = iso("rue inconnue 5, grande ville, AB 10000");
    assert_eq!(w["PstlAdr"]["TwnNm"], "grande ville", "{w}");
}

/// Revision 0.9.0: "The line was read as…" described the first reading tried, so the town was always "not named"
/// next to "city: confirmed". The reading named is the one that agrees with the answer.
#[test]
fn explain_names_the_reading_that_gave_the_answer() {
    let (_dir, bin) = sheet("explain-reading");
    let s = bin.to_str().unwrap();
    for q in ["rue a 3, ville", "rue a 3 ville", "Rue A 3, Ville"] {
        let j = json(&["explain", s, q, "--format", "json"]);
        assert_eq!(j["reading"]["answered_by"], "answer", "{q}: {j}");
        assert_eq!(j["reading"]["town"], "ville", "{q}: {}", j["reading"]);
        assert_eq!(j["reading"]["house_number"], 3, "{q}");
        let out = Command::new(BIN).args(["explain", s, q]).output().unwrap();
        let t = String::from_utf8(out.stdout).unwrap();
        assert!(t.contains("town \"ville\""), "{q}: {t}");
        assert!(!t.contains("\"null\""), "{q}: {t}");
    }
    // no answer: the first reading, and an empty field is said, not printed as null
    let none = Command::new(BIN)
        .args(["explain", s, "xyzzy plugh 999"])
        .output()
        .unwrap();
    let t = String::from_utf8(none.stdout).unwrap();
    assert!(!t.contains("null"), "{t}");
}
