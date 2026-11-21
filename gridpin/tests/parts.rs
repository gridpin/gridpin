//! `gridpin format --out parts` and `--out fhir`: the parts of a line from the reading of ISO 20022, and the HL7 FHIR
//! `Address` built on them (plan K4.0-K4.1, 17.11.2026). The structure is never richer than ISO 20022 for the same line.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

/// A French sheet: rue de la paix 1-10 in Paris 75002, rue a 1-5 in Lyon 1er Arrondissement 69001, and a street of 75 letters in Paris.
fn sheet(name: &str) -> (Scratch, PathBuf) {
    sheet_of(name, "fr")
}

/// The same rows on a sheet that says it is of `country`: the postal layout (and so the order of the town line)
/// comes from the sheet's country.
fn sheet_of(name: &str, country: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("parts-{name}"));
    let mut rows: Vec<String> = Vec::new();
    for n in 1..=10 {
        rows.push(format!(
            "rue de la paix,75102,paris,75002,{n},,{:.6},48.8690,Rue de la Paix,Paris",
            2.33 + n as f64 * 0.0001
        ));
    }
    for n in 1..=5 {
        rows.push(format!(
            "rue a,69381,lyon 1er arrondissement,69001,{n},,{:.6},45.7600,Rue A,Lyon 1er Arrondissement",
            4.83 + n as f64 * 0.0001
        ));
    }
    let long = "a".repeat(64) + " longue";
    rows.push(format!(
        "rue {long},75102,paris,75002,3,,2.3400,48.8700,Rue {long},Paris"
    ));
    // the builder takes the rows in the order of their key (street, commune code)
    rows.sort_by_key(|r| r.splitn(3, ',').take(2).collect::<Vec<_>>().join(","));
    std::fs::write(dir.join("in.csv"), format!("{HDR}{}\n", rows.join("\n"))).unwrap();
    std::fs::write(
        dir.join("m.json"),
        format!(r#"{{"country":"{country}","layer":"addresses","license":"t","source_release":"t","attribution":"t"}}"#),
    )
    .unwrap();
    let bin = dir.join("s.bin");
    let st = Command::new(BIN)
        .args([
            "build",
            dir.join("in.csv").to_str().unwrap(),
            bin.to_str().unwrap(),
            "--meta",
            dir.join("m.json").to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(st.success());
    (dir, bin)
}

fn run(bin: &Path, q: &str, out: &str) -> Value {
    let o = Command::new(BIN)
        .args(["format", bin.to_str().unwrap(), q, "--out", out])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

fn keys(v: &Value) -> Vec<&str> {
    let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    k.sort_unstable();
    k
}

#[test]
fn a_confirmed_house_gives_every_part_and_a_point() {
    let (_d, bin) = sheet("0");
    let p = run(&bin, "10 Rue de la Paix, Paris", "parts");
    assert_eq!(p["mode"], "structured", "{p}");
    let parts = &p["parts"];
    assert_eq!(
        (
            parts["number"].as_str(),
            parts["street_name"].as_str(),
            parts["postcode"].as_str(),
            parts["locality"].as_str(),
            parts["country"].as_str()
        ),
        (
            Some("10"),
            Some("Rue de la Paix"),
            Some("75002"),
            Some("Paris"),
            Some("FR")
        )
    );
    // a sheet whose street type comes first keeps the street whole: a split is not invented
    assert!(
        parts["street_type"].is_null() && parts["pre_direction"].is_null(),
        "{p}"
    );
    let f = run(&bin, "10 Rue de la Paix, Paris", "fhir");
    let a = &f["address"];
    assert_eq!(a["type"], "physical");
    // `text` from the parts (FHIR: no content in the text that is not found in a part), not the line as written
    assert_eq!(a["text"], "10 Rue de la Paix, 75002 Paris");
    assert_eq!(a["line"], serde_json::json!(["10 Rue de la Paix"]), "{f}");
    assert_eq!(
        (
            a["city"].as_str(),
            a["postalCode"].as_str(),
            a["country"].as_str()
        ),
        (Some("Paris"), Some("75002"), Some("FR"))
    );
    let ext = &a["extension"][0];
    assert_eq!(
        ext["url"],
        "http://hl7.org/fhir/StructureDefinition/geolocation"
    );
    assert_eq!(ext["extension"][0]["url"], "latitude");
    assert!(
        (ext["extension"][0]["valueDecimal"].as_f64().unwrap() - 48.869).abs() < 1e-6,
        "{f}"
    );
    assert_eq!(f["mode"], "structured");
}

#[test]
fn a_number_the_street_does_not_hold_is_as_written_without_a_point() {
    let (_d, bin) = sheet("1");
    let f = run(&bin, "77 Rue de la Paix, Paris", "fhir");
    assert_eq!(f["mode"], "as_written", "{f}");
    // no part confirmed beyond the town: the line as written alone, since a country beside it would be a part the text
    // does not hold
    assert_eq!(keys(&f["address"]), ["text"], "{f}");
    assert_eq!(f["address"]["text"], "77 Rue de la Paix, Paris");
    let p = run(&bin, "77 Rue de la Paix, Paris", "parts");
    assert!(
        p["parts"]["number"].is_null() && p["parts"]["street_name"].is_null(),
        "{p}"
    );
    assert!(!p["reasons"].as_array().unwrap().is_empty(), "{p}");
}

#[test]
fn nothing_confirmed_gives_the_text_alone() {
    let (_d, bin) = sheet("2");
    let f = run(&bin, "Impasse Inconnue 4, Marseille", "fhir");
    assert_eq!(f["mode"], "none", "{f}");
    assert_eq!(keys(&f["address"]), ["text"], "{f}");
    assert_eq!(f["address"]["text"], "Impasse Inconnue 4, Marseille");
    let p = run(&bin, "Impasse Inconnue 4, Marseille", "parts");
    assert!(
        p["parts"].as_object().unwrap().values().all(Value::is_null),
        "{p}"
    );
}

#[test]
fn a_po_box_is_a_postal_address_without_a_point() {
    let (_d, bin) = sheet("3");
    let f = run(&bin, "BP 7, 69001 Lyon", "fhir");
    let a = &f["address"];
    assert_eq!(
        (f["mode"].as_str(), a["type"].as_str()),
        (Some("structured"), Some("postal")),
        "{f}"
    );
    assert_eq!(a["line"], serde_json::json!(["PO Box 7"]), "{f}");
    assert_eq!(a["text"], "PO Box 7, 69001 Lyon", "{f}");
    assert!(
        a.get("extension").is_none(),
        "a post office box has no point of its own: {f}"
    );
    // a box beside a house the sheet answers (not weak) and beside a weak answer: the reading keeps the box, not the
    // house, so neither answer is its point
    for q in ["BP 7, Rue A 2, Lyon", "BP 12, Rue de la Paix, Paris"] {
        let g = run(&bin, q, "fhir");
        assert_eq!(g["address"]["type"], "postal", "{q}: {g}");
        assert!(g["address"].get("extension").is_none(), "{q}: {g}");
    }
}

#[test]
fn a_weak_answer_is_never_a_point() {
    let (_d, bin) = sheet("4");
    let f = run(&bin, "Paix, Lyon", "fhir");
    assert!(f["address"].get("extension").is_none(), "{f}");
    assert_ne!(f["mode"], "structured", "{f}");
}

#[test]
fn the_district_of_a_town_kept_by_districts_is_the_hit_s_commune() {
    let (_d, bin) = sheet("5");
    let f = run(&bin, "2 Rue A, Lyon", "fhir");
    assert_eq!(
        (
            f["address"]["city"].as_str(),
            f["address"]["district"].as_str()
        ),
        (Some("Lyon"), Some("Lyon 1er Arrondissement")),
        "{f}"
    );
    // no town written: the town is the hit's commune, and no district repeats it
    let g = run(&bin, "2 Rue A", "fhir");
    assert_eq!(g["address"]["city"], "Lyon 1er Arrondissement", "{g}");
    assert!(g["address"].get("district").is_none(), "{g}");
}

#[test]
fn every_word_of_a_structured_text_is_in_a_part() {
    let (_d, bin) = sheet("7");
    for q in [
        "10 Rue de la Paix, Paris, France",
        "Rue A 9, Paris",
        "BP 7, 69001 Lyon",
        "2 Rue A, Lyon",
        "10 rue de la paix 75002 paris",
    ] {
        let f = run(&bin, q, "fhir");
        if f["mode"] != "structured" {
            continue;
        }
        let a = &f["address"];
        let parts: Vec<String> = ["line", "city", "district", "state", "postalCode", "country"]
            .iter()
            .flat_map(|k| match &a[*k] {
                Value::Array(v) => v
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
                Value::String(s) => vec![s.clone()],
                _ => vec![],
            })
            .collect();
        let words = parts.join(" ");
        for w in a["text"]
            .as_str()
            .unwrap()
            .split([' ', ','])
            .filter(|w| !w.is_empty())
        {
            assert!(
                words.split(' ').any(|x| x == w),
                "{q}: {w:?} of the text is in no part: {f}"
            );
        }
    }
}

#[test]
fn the_structure_is_never_richer_than_iso20022() {
    let (_d, bin) = sheet("6");
    for q in [
        "10 Rue de la Paix, Paris",
        "77 Rue de la Paix, Paris",
        "Paix, Lyon",
        "BP 7, 69001 Lyon",
        "Impasse Inconnue 4, Marseille",
    ] {
        let iso = run(&bin, q, "iso20022");
        let p = run(&bin, q, "parts");
        let want = match iso["mode"].as_str().unwrap() {
            "structured" => "structured",
            "hybrid" => "as_written",
            _ => "none",
        };
        assert_eq!(p["mode"], want, "{q}: {iso} {p}");
        assert_eq!(p["reasons"], iso["checks"], "{q}");
    }
}

#[test]
fn with_fhir_adds_the_mode_and_the_resource_to_a_csv_beside_iso20022() {
    let (d, bin) = sheet("8");
    let input = d.join("a.csv");
    std::fs::write(
        &input,
        "address\n\"10 Rue de la Paix, Paris\"\n\"99 Rue de la Paix, Paris\"\nxyzzy nowhere\n\"Impasse Inconnue 4, Paris\"\n",
    )
    .unwrap();
    let out = d.join("o.csv");
    let st = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            out.to_str().unwrap(),
            "--with",
            "iso20022,fhir",
        ])
        .status()
        .unwrap();
    assert!(st.success());
    let mut rdr = csv::Reader::from_path(&out).unwrap();
    let head: Vec<String> = rdr.headers().unwrap().iter().map(String::from).collect();
    let col = |name: &str| head.iter().position(|h| h == name).unwrap();
    let rows: Vec<csv::StringRecord> = rdr.records().map(Result::unwrap).collect();
    let address =
        |r: usize| -> Value { serde_json::from_str(&rows[r][col("gp_fhir_address")]).unwrap() };
    assert_eq!(&rows[0][col("gp_fhir_mode")], "structured");
    assert_eq!(address(0)["line"], serde_json::json!(["10 Rue de la Paix"]));
    assert!(address(0).get("extension").is_some());
    assert_eq!(&rows[1][col("gp_fhir_mode")], "as_written");
    assert_eq!(keys(&address(1)), ["text"]);
    // rows without an answer still get both: ISO 20022 and FHIR from the same reading
    for r in [2, 3] {
        assert_eq!(&rows[r][col("gp_lat")], "");
        assert_eq!(address(r)["text"], rows[r][col("address")].to_string());
    }
    assert_eq!(
        (&rows[2][col("gp_fhir_mode")], &rows[2][col("gp_iso_mode")]),
        ("none", "none")
    );
    assert_eq!(
        (&rows[3][col("gp_fhir_mode")], &rows[3][col("gp_iso_mode")]),
        ("as_written", "hybrid")
    );
}

#[test]
fn the_town_line_of_the_text_follows_the_sheet_s_postal_layout() {
    // France: postcode before the town; the United States: the town, then the code
    let (_d, fr) = sheet("9");
    assert_eq!(
        run(&fr, "10 Rue de la Paix, Paris", "fhir")["address"]["text"],
        "10 Rue de la Paix, 75002 Paris"
    );
    let (_e, us) = sheet_of("10", "us");
    let f = run(&us, "10 Rue de la Paix, Paris", "fhir");
    assert_eq!(f["mode"], "structured", "{f}");
    assert_eq!(
        f["address"]["text"], "10 Rue de la Paix, Paris 75002",
        "{f}"
    );
}

/// A Polish sheet: before 17.11.2026 it had no postal layout, so ISO 20022 and FHIR said `country_unknown` and stayed
/// empty; its digits-only postcode is written with the dash of the Polish post ("75-002" here: the rows' code).
#[test]
fn a_polish_sheet_has_a_layout_and_its_postcode_is_written_with_the_dash() {
    let (_d, pl) = sheet_of("pl", "pl");
    let iso = run(&pl, "10 Rue de la Paix, Paris", "iso20022");
    assert_eq!(iso["mode"], "structured", "{iso}");
    assert_eq!(iso["PstlAdr"]["Ctry"], "PL", "{iso}");
    assert_eq!(iso["PstlAdr"]["PstCd"], "75-002", "{iso}");
    let f = run(&pl, "10 Rue de la Paix, Paris", "fhir");
    assert_eq!(f["address"]["postalCode"], "75-002", "{f}");
    assert_eq!(
        f["address"]["text"], "Rue de la Paix 10, 75-002 Paris",
        "{f}"
    );
}

/// A line listing two doors of one place ("A ; B, town"): the door the sheet holds answers, read with the town of the
/// tail (17.11.2026, French Mérimée set +37). The line read whole does not reach the house here.
#[test]
fn of_the_doors_a_line_lists_the_one_the_sheet_holds_answers() {
    let (_d, fr) = sheet("doors");
    let q = "7 impasse Quorvalix ; 3 Rue de la Paix, Paris";
    let o = Command::new(BIN)
        .args(["query", fr.to_str().unwrap(), q, "-k", "1"])
        .output()
        .unwrap();
    let line = String::from_utf8_lossy(&o.stdout);
    let hit: Value =
        serde_json::from_str(line.lines().next().unwrap_or("{}")).unwrap_or(Value::Null);
    assert_eq!(hit["street"], "Rue de la Paix", "{line}");
    assert_eq!(hit["housenumber"], "3", "{line}");
    assert_eq!(hit["precision"], "house", "{line}");
}

/// "3 Eighth St., Sheboygan": the spelled ordinal with an abbreviated type finds no street read whole, and the line
/// read with the number ("3 8th St.") answers the house (17.11.2026, US NRHP messy set).
#[test]
fn a_spelled_ordinal_with_an_abbreviated_type_answers_as_the_number() {
    let dir = Scratch::dir("parts-ordinal");
    let rows: Vec<String> = (1..=10)
        .map(|n| {
            format!(
                "8th street,55001,sheboygan,53081,{n},,{:.4},43.75,8th Street,Sheboygan",
                -87.71 - n as f64 * 0.0001
            )
        })
        .collect();
    // the builder wants the numbers of a street in order: 1..=10 already is
    std::fs::write(dir.join("in.csv"), format!("{HDR}{}\n", rows.join("\n"))).unwrap();
    std::fs::write(
        dir.join("m.json"),
        r#"{"country":"us","layer":"addresses","license":"t","source_release":"t","attribution":"t"}"#,
    )
    .unwrap();
    let bin = dir.join("s.bin");
    let st = Command::new(BIN)
        .args([
            "build",
            dir.join("in.csv").to_str().unwrap(),
            bin.to_str().unwrap(),
            "--meta",
            dir.join("m.json").to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(st.success());
    let o = Command::new(BIN)
        .args([
            "query",
            bin.to_str().unwrap(),
            "3 Eighth St., Sheboygan",
            "-k",
            "1",
        ])
        .output()
        .unwrap();
    let line = String::from_utf8_lossy(&o.stdout);
    let hit: Value =
        serde_json::from_str(line.lines().next().unwrap_or("{}")).unwrap_or(Value::Null);
    assert_eq!(hit["street"], "8th Street", "{line}");
    assert_eq!(hit["housenumber"], "3", "{line}");
    assert_eq!(hit["precision"], "house", "{line}");
}

/// Plan K2.1: a range written with a typographic dash and spaces is answered as the range written with a hyphen.
#[test]
fn a_range_with_a_typographic_dash_is_answered_as_with_a_hyphen() {
    let (_d, bin) = sheet("dash");
    let answer = |q: &str| {
        let o = Command::new(BIN)
            .args(["query", bin.to_str().unwrap(), q])
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    let hyphen = answer("Rue de la Paix 4-6, Paris");
    assert!(hyphen.contains("\"housenumber\":\"4\""), "{hyphen}");
    assert_eq!(answer("Rue de la Paix 4 \u{2013} 6, Paris"), hyphen);
}
