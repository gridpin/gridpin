//! `gridpin format --out iso20022` and `--with iso20022`: the ISO 20022 `PstlAdr` of a free line, confirmed against
//! the sheet (structured, hybrid or none, with the reasons). Added 10.11.2026.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

/// A French sheet: rue de la paix 1-10 in Paris 75002, rue a 1-5 in Lyon 1er Arrondissement 69001, and a street of 75 letters in Paris.
fn sheet(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("iso20022-{name}"));
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
        r#"{"country":"fr","layer":"addresses","license":"t","source_release":"t","attribution":"t"}"#,
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

fn iso(bin: &Path, q: &str) -> Value {
    let out = Command::new(BIN)
        .args(["format", bin.to_str().unwrap(), q, "--out", "iso20022"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn checks(v: &Value) -> Vec<String> {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_house_with_its_town_is_structured_and_the_town_is_filled_when_the_line_lacks_it() {
    let (_d, bin) = sheet("0");
    let v = iso(&bin, "10 Rue de la Paix, Paris");
    assert_eq!(v["mode"], "structured", "{v}");
    let a = &v["PstlAdr"];
    assert_eq!(
        (
            a["StrtNm"].as_str(),
            a["BldgNb"].as_str(),
            a["PstCd"].as_str(),
            a["TwnNm"].as_str(),
            a["Ctry"].as_str()
        ),
        (
            Some("Rue de la Paix"),
            Some("10"),
            Some("75002"),
            Some("Paris"),
            Some("FR")
        )
    );
    assert!(a.get("AdrLine").is_none() && checks(&v).is_empty(), "{v}");
    assert_eq!(v["exists"]["exists"], "yes");
    // the same house without the town: the sheet fills it
    let w = iso(&bin, "10 Rue de la Paix");
    assert_eq!(
        (w["mode"].as_str(), w["PstlAdr"]["TwnNm"].as_str()),
        (Some("structured"), Some("Paris")),
        "{w}"
    );
}

#[test]
fn a_flat_goes_to_room_and_a_floor_to_flr() {
    let (_d, bin) = sheet("1");
    assert_eq!(
        iso(&bin, "10 Rue de la Paix, apt 4B, Paris")["PstlAdr"]["Room"],
        "4B"
    );
    let f = iso(&bin, "10 Rue de la Paix, 2º B, Paris");
    assert_eq!(f["PstlAdr"]["Flr"], "2º B", "{f}");
    assert!(f["PstlAdr"].get("Room").is_none(), "{f}");
}

#[test]
fn a_number_not_on_the_street_is_hybrid_with_the_line_without_its_town() {
    let (_d, bin) = sheet("2");
    let v = iso(&bin, "99 Rue de la Paix, Paris");
    assert_eq!(v["mode"], "hybrid", "{v}");
    assert_eq!(
        v["PstlAdr"]["AdrLine"],
        serde_json::json!(["99 Rue de la Paix"]),
        "{v}"
    );
    assert_eq!(
        (
            v["PstlAdr"]["TwnNm"].as_str(),
            v["PstlAdr"]["Ctry"].as_str()
        ),
        (Some("Paris"), Some("FR"))
    );
    assert!(
        checks(&v).contains(&"number_not_on_street".to_string()),
        "{v}"
    );
    // the postcode goes into PstCd only when the line's postcode matched the sheet exactly: the sheet knows 75002
    // for this street, the line does not name it
    assert!(v["PstlAdr"].get("PstCd").is_none(), "{v}");
}

#[test]
fn a_town_only_inferred_by_the_sheet_is_not_enough_for_a_hybrid_address() {
    let (_d, bin) = sheet("inferred");
    let v = iso(&bin, "99 Rue de la Paix");
    assert_eq!(v["mode"], "none", "the line names no town: {v}");
    assert!(checks(&v).contains(&"town_unconfirmed".to_string()), "{v}");
}

#[test]
fn one_segment_longer_than_an_address_line_is_not_cut_in_the_middle() {
    let (_d, bin) = sheet("segment");
    let v = iso(
        &bin,
        &format!("99 Rue de la Paix {}, Paris", "y".repeat(60)),
    );
    assert_eq!(v["mode"], "none", "{v}");
    assert!(checks(&v).contains(&"adr_line_overflow".to_string()), "{v}");
}

#[test]
fn a_town_the_sheet_does_not_confirm_gives_none() {
    let (_d, bin) = sheet("3");
    let v = iso(&bin, "99 Rue Inconnue, Marseille");
    assert_eq!(v["mode"], "none", "{v}");
    assert!(v["PstlAdr"].as_object().unwrap().is_empty());
}

#[test]
fn another_country_named_in_the_line_is_a_mismatch_and_the_sheet_s_own_is_read() {
    let (_d, bin) = sheet("4");
    let v = iso(&bin, "10 Rue de la Paix, Paris, Germany");
    assert_eq!(
        (v["mode"].as_str(), checks(&v)),
        (Some("none"), vec!["country_mismatch".to_string()]),
        "{v}"
    );
    let fr = iso(&bin, "10 Rue de la Paix, Paris, France");
    assert_eq!(
        (fr["mode"].as_str(), fr["PstlAdr"]["Ctry"].as_str()),
        (Some("structured"), Some("FR")),
        "{fr}"
    );
}

#[test]
fn a_street_longer_than_seventy_characters_is_not_structured_and_not_cut() {
    let (_d, bin) = sheet("5");
    let long = "a".repeat(64) + " longue";
    let v = iso(&bin, &format!("3 Rue {long}, Paris"));
    assert_ne!(v["mode"], "structured", "{v}");
    assert!(checks(&v).contains(&"too_long:StrtNm".to_string()), "{v}");
    assert!(
        v["PstlAdr"].get("StrtNm").is_none(),
        "the name is not cut: {v}"
    );
}

#[test]
fn address_lines_beyond_two_of_seventy_give_none() {
    let (_d, bin) = sheet("6");
    let seg = "x".repeat(60);
    let v = iso(&bin, &format!("99 Rue de la Paix, {seg}, {seg}, Paris"));
    assert_eq!(v["mode"], "none", "{v}");
    assert!(checks(&v).contains(&"adr_line_overflow".to_string()), "{v}");
}

#[test]
fn a_po_box_fills_pstbx_without_a_street() {
    let (_d, bin) = sheet("7");
    let v = iso(&bin, "BP 1234, Paris");
    assert_eq!(v["PstlAdr"]["PstBx"], "1234", "{v}");
    assert!(v["PstlAdr"].get("StrtNm").is_none(), "{v}");
    assert!(checks(&v).contains(&"po_box".to_string()), "{v}");
}

#[test]
fn a_street_the_sheet_does_not_know_in_a_town_it_knows_is_hybrid() {
    let (_d, bin) = sheet("11");
    let v = iso(&bin, "Impasse Inconnue 4, 75002 Paris");
    assert_eq!(
        (v["mode"].as_str(), v["PstlAdr"]["TwnNm"].as_str()),
        (Some("hybrid"), Some("Paris")),
        "{v}"
    );
    assert_eq!(
        v["PstlAdr"]["AdrLine"],
        serde_json::json!(["Impasse Inconnue 4, 75002"]),
        "{v}"
    );
    // the sheet did not answer, so it did not confirm the code: it stays in the line, not in PstCd
    assert!(v["PstlAdr"].get("PstCd").is_none(), "{v}");
    assert!(checks(&v).contains(&"street_unknown".to_string()), "{v}");
    // a town the sheet does not know: still nothing to confirm
    assert_eq!(iso(&bin, "Impasse Inconnue 4, Marseille")["mode"], "none");
}

#[test]
fn a_po_box_in_a_town_of_the_sheet_is_structured_without_an_answer() {
    let (_d, bin) = sheet("12");
    let v = iso(&bin, "BP 7, 69001 Lyon");
    assert_eq!(v["mode"], "structured", "{v}");
    assert_eq!(
        (
            v["PstlAdr"]["PstBx"].as_str(),
            v["PstlAdr"]["TwnNm"].as_str(),
            v["PstlAdr"]["PstCd"].as_str()
        ),
        (Some("7"), Some("Lyon"), Some("69001")),
        "{v}"
    );
    assert!(v["PstlAdr"].get("AdrLine").is_none(), "{v}");
}

#[test]
fn a_town_kept_by_districts_is_the_town_the_line_names() {
    let (_d, bin) = sheet("13");
    let v = iso(&bin, "2 Rue A, Lyon");
    assert_eq!(
        (v["mode"].as_str(), v["PstlAdr"]["TwnNm"].as_str()),
        (Some("structured"), Some("Lyon")),
        "the payment names Lyon, not its district: {v}"
    );
    // a line without commas: the town is still the one the line ends with
    let flat = iso(&bin, "2 rue a lyon");
    assert_eq!(flat["PstlAdr"]["TwnNm"], "lyon", "{flat}");
    // without a town in the line the sheet's commune stays
    let bare = iso(&bin, "2 Rue A");
    assert_eq!(
        bare["PstlAdr"]["TwnNm"], "Lyon 1er Arrondissement",
        "{bare}"
    );
}

#[test]
fn a_weak_answer_or_a_corrected_street_is_said_in_the_checks() {
    let (_d, bin) = sheet("15");
    // "Paix" alone is a weak answer: it does not confirm the street, the town of the line still makes a hybrid
    let weak = iso(&bin, "Paix, Lyon");
    assert_eq!(weak["mode"], "hybrid", "{weak}");
    assert!(checks(&weak).contains(&"weak_answer".to_string()), "{weak}");
    assert!(weak["PstlAdr"].get("StrtNm").is_none(), "{weak}");
    // "Rue A" in Paris is answered by another street of the sheet: structured, with the warning
    let fixed = iso(&bin, "Rue A 9, Paris");
    assert!(
        checks(&fixed).contains(&"street_corrected".to_string()),
        "{fixed}"
    );
    let exact = iso(&bin, "9 Rue de la Paix, Paris");
    assert!(
        !checks(&exact).contains(&"street_corrected".to_string()),
        "{exact}"
    );
}

#[test]
fn a_town_with_its_region_code_is_the_town() {
    let (_d, bin) = sheet("16");
    let v = iso(&bin, "BP 7, Lyon - RH");
    assert_eq!(v["PstlAdr"]["TwnNm"], "Lyon", "{v}");
}

#[test]
fn the_postal_layout_prints_no_weak_answer() {
    let (_d, bin) = sheet("17");
    let run = |q: &str| {
        Command::new(BIN)
            .args(["format", bin.to_str().unwrap(), q])
            .output()
            .unwrap()
    };
    // "Paix, Lyon" is answered only with a guess: no address, an error naming it
    let weak = run("Paix, Lyon");
    assert!(!weak.status.success());
    let err = String::from_utf8_lossy(&weak.stderr);
    assert!(
        err.contains("no confirmed address") && err.contains("guess"),
        "{err}"
    );
    assert!(weak.stdout.is_empty());
    // a house of the sheet is printed as before
    let ok = run("9 Rue de la Paix, Paris");
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(String::from_utf8_lossy(&ok.stdout).contains("Rue de la Paix"));
}

#[test]
fn batch_with_postal_leaves_the_layout_of_a_weak_answer_empty() {
    // ANSWERS §300 (the rule of §298 for the batch): a weak row keeps its answer columns and gp_weak, but no label text
    let (d, bin) = sheet("postal-batch");
    let input = d.join("in.csv");
    std::fs::write(
        &input,
        "id,address\nw,\"Paix, Lyon\"\nh,\"9 Rue de la Paix, Paris\"\n",
    )
    .unwrap();
    let out = d.join("out.csv");
    let run = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            out.to_str().unwrap(),
            "--with",
            "postal",
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
    let col = |name: &str| head.iter().position(|h| h == name).unwrap();
    let rows: Vec<csv::StringRecord> = r.records().map(|x| x.unwrap()).collect();
    assert_eq!(
        &rows[0][col("gp_weak")],
        "true",
        "the fixture's guess is weak: {:?}",
        rows[0]
    );
    assert_eq!(&rows[0][col("gp_postal")], "", "{:?}", rows[0]);
    assert!(
        !rows[0][col("gp_lat")].is_empty(),
        "the answer itself stays: {:?}",
        rows[0]
    );
    assert!(
        rows[1][col("gp_postal")].contains("Rue de la Paix"),
        "{:?}",
        rows[1]
    );
}

#[test]
fn xml_is_in_iso_order_and_postal_stays_as_it_was() {
    let (_d, bin) = sheet("8");
    let x = Command::new(BIN)
        .args([
            "format",
            bin.to_str().unwrap(),
            "10 Rue de la Paix, Paris",
            "--out",
            "iso20022",
            "--xml",
        ])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&x.stdout).trim(),
        "<PstlAdr><StrtNm>Rue de la Paix</StrtNm><BldgNb>10</BldgNb><PstCd>75002</PstCd><TwnNm>Paris</TwnNm><Ctry>FR</Ctry></PstlAdr>"
    );
    let postal = |extra: &[&str]| {
        let mut a = vec!["format", bin.to_str().unwrap(), "10 Rue de la Paix, Paris"];
        a.extend_from_slice(extra);
        Command::new(BIN).args(&a).output().unwrap()
    };
    assert_eq!(
        postal(&[]).stdout,
        postal(&["--out", "postal"]).stdout,
        "--out postal is the old output"
    );
    let bad = postal(&["--xml"]);
    assert!(
        !bad.status.success() && String::from_utf8_lossy(&bad.stderr).contains("--out iso20022")
    );
}

#[test]
fn with_iso20022_adds_its_columns_to_a_csv() {
    let (d, bin) = sheet("9");
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
            "iso20022",
        ])
        .status()
        .unwrap();
    assert!(st.success());
    let mut rdr = csv::Reader::from_path(&out).unwrap();
    let head: Vec<String> = rdr.headers().unwrap().iter().map(String::from).collect();
    let iso: Vec<&String> = head.iter().filter(|h| h.starts_with("gp_iso_")).collect();
    assert_eq!(iso.len(), 13, "{head:?}");
    let col = |name: &str| head.iter().position(|h| h == name).unwrap();
    let rows: Vec<csv::StringRecord> = rdr.records().map(Result::unwrap).collect();
    assert_eq!(&rows[0][col("gp_iso_mode")], "structured");
    assert_eq!(&rows[1][col("gp_iso_mode")], "hybrid");
    assert_eq!(&rows[1][col("gp_iso_adr_line_1")], "99 Rue de la Paix");
    assert!(rows[1][col("gp_iso_checks")].contains("number_not_on_street"));
    // a row without an answer: the answer columns stay empty, the ISO columns say what can still be made and why
    assert_eq!(&rows[2][col("gp_lat")], "");
    assert_eq!(
        (&rows[2][col("gp_iso_mode")], &rows[2][col("gp_iso_checks")]),
        ("none", "empty")
    );
    assert_eq!(&rows[3][col("gp_lat")], "");
    assert_eq!(
        (
            &rows[3][col("gp_iso_mode")],
            &rows[3][col("gp_iso_twn_nm")],
            &rows[3][col("gp_iso_adr_line_1")]
        ),
        ("hybrid", "Paris", "Impasse Inconnue 4")
    );
}

// Revision of 0.9.0 (ANSWERS §308): structured only for a house the sheet confirms in the town the line names.

#[test]
fn a_house_in_another_town_than_the_named_one_is_not_structured() {
    let (_d, bin) = sheet("other-town");
    // the house is in Paris, the line names Lyon (a town of the sheet, kept by districts, 390 km away)
    let v = iso(&bin, "10 Rue de la Paix, Lyon");
    assert_ne!(v["mode"], "structured", "{v}");
    // the engine itself keeps the named town and answers nothing here; what matters is that nothing confirms it
    assert_eq!(v["exists"]["exists"], "no", "{v}");
    assert!(v["PstlAdr"].get("StrtNm").is_none(), "{v}");
    assert_ne!(
        v["PstlAdr"]["TwnNm"], "Paris",
        "the sheet's town of a house elsewhere is not the payer's: {v}"
    );
    // the near miss: the same house with its own town is structured
    assert_eq!(iso(&bin, "10 Rue de la Paix, Paris")["mode"], "structured");
}

#[test]
fn a_town_the_sheet_does_not_know_gives_no_structured_address() {
    let (_d, bin) = sheet("unknown-town");
    let v = iso(&bin, "10 Rue de la Paix, Marseille");
    assert_ne!(v["mode"], "structured", "{v}");
    assert_eq!(
        (
            v["exists"]["exists"].as_str(),
            v["exists"]["reason"].as_str()
        ),
        (Some("maybe"), Some("town_not_on_sheet")),
        "{v}"
    );
}

#[test]
fn a_state_code_is_not_a_town_and_a_zip_is_not_a_floor() {
    let (_d, bin) = sheet("state");
    // "FL 33040": no floor in Flr, the code once
    let v = iso(&bin, "10 Rue de la Paix, Paris, FL 75002");
    assert!(v["PstlAdr"].get("Flr").is_none(), "{v}");
    // a line whose only town-like segment is a two-letter code: the code is not TwnNm
    let w = iso(&bin, "Rue Inconnue 5, Paris, MN 55082");
    assert_ne!(w["PstlAdr"]["TwnNm"], "MN", "{w}");
}
