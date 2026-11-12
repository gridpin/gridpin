//! Query forms read before the address path (`forms`): coordinates, special addresses, crossings, and the
//! `--strict` / `--lenient` / `--abbrev` options, through the built binary on a fixture town.
//!
//! Main Street runs east along 43.7300 (numbers 1-10, 10 m apart from 7.4200); Elm Street runs north along
//! 7.42045 from 43.7302; Far Road lies 1 km north, Near Lane 120 m north (parallel). Main and Elm meet; Main and
//! Far Road or Near Lane do not. 2 km east,
//! Ramon Street and Cajal Street cross, and Ramon Y Cajal Street is a street of its own.
use std::path::{Path, PathBuf};
use std::process::Command;

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";
const STEP: f64 = 0.000125; // ~10 m of longitude at this latitude

fn town(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("forms-{name}"));
    // the builder wants streets in name order
    let mut rows = String::new();
    for n in 1..=10 {
        rows += &format!(
            "cajal street,001,ville,10000,{n},,7.4450000,{:.7},Cajal Street,Ville\n",
            43.7302 + n as f64 * 0.00009
        );
    }
    for n in 1..=10 {
        rows += &format!(
            "calle apartado,001,ville,10000,{n},,{:.7},43.7500000,Calle Apartado,Ville\n",
            7.42 + n as f64 * STEP
        );
    }
    for n in 1..=10 {
        rows += &format!(
            "elm street,001,ville,10000,{n},,7.4204500,{:.7},Elm Street,Ville\n",
            43.7302 + n as f64 * 0.00009
        );
    }
    for n in 1..=10 {
        rows += &format!(
            "far road,001,ville,10000,{n},,{:.7},43.7400000,Far Road,Ville\n",
            7.42 + n as f64 * STEP
        );
    }
    for n in 1..=10 {
        rows += &format!(
            "main street,001,ville,10000,{n},,{:.7},43.7300000,Main Street,Ville\n",
            7.42 + n as f64 * STEP
        );
    }
    for n in 1..=10 {
        rows += &format!(
            "near lane,001,ville,10000,{n},,{:.7},43.7310800,Near Lane,Ville\n",
            7.42 + n as f64 * STEP
        );
    }
    for n in 1240..=1260 {
        rows += &format!(
            "oak street,001,ville,10000,{n},,{:.7},43.7600000,Oak Street,Ville\n",
            7.42 + (n - 1240) as f64 * STEP
        );
    }
    for n in 1..=10 {
        rows += &format!(
            "ramon street,001,ville,10000,{n},,{:.7},43.7300000,Ramon Street,Ville\n",
            7.4445 + n as f64 * STEP
        );
    }
    for n in 1..=10 {
        rows += &format!("ramon y cajal street,001,ville,10000,{n},,{:.7},43.7200000,Ramon Y Cajal Street,Ville\n", 7.4445 + n as f64 * STEP);
    }
    std::fs::write(dir.join("in.csv"), format!("{HDR}{rows}")).unwrap();
    std::fs::write(
        dir.join("m.json"),
        r#"{"country":"mc","layer":"addresses","license":"t","source_release":"t","attribution":"t"}"#,
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

fn query(bin: &Path, q: &str, extra: &[&str]) -> Vec<serde_json::Value> {
    let mut args = vec!["query", bin.to_str().unwrap(), q, "-k", "1"];
    args.extend_from_slice(extra);
    let out = Command::new(BIN).args(&args).output().unwrap();
    assert!(
        out.status.success(),
        "{q}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn flags(h: &serde_json::Value) -> Vec<String> {
    h["flags"].as_array().map_or(vec![], |f| {
        f.iter().map(|x| x.as_str().unwrap().to_string()).collect()
    })
}

#[test]
fn a_coordinate_in_any_written_form_answers_the_house_there() {
    let (_d, bin) = town("coords");
    let house5 = (43.73, 7.42 + 5.0 * STEP);
    let dms = |v: f64, pos: char| {
        let s = (v.abs() * 3600.0 * 10.0).round() / 10.0;
        format!(
            "{}°{}'{:.1}\"{pos}",
            (s / 3600.0) as u32,
            ((s % 3600.0) / 60.0) as u32,
            s % 60.0
        )
    };
    for (q, flag) in [
        (format!("{}, {}", house5.0, house5.1), "input_decimal"),
        (
            format!("{} {}", dms(house5.0, 'N'), dms(house5.1, 'E')),
            "input_dms",
        ),
    ] {
        let hit = &query(&bin, &q, &[])[0];
        assert_eq!(hit["housenumber"], "5", "{q}: {hit}");
        assert_eq!(hit["street"], "Main Street");
        assert!(
            flags(hit).contains(&"coords_in".to_string()) && flags(hit).contains(&flag.to_string()),
            "{hit}"
        );
    }
}

#[test]
fn the_address_path_still_reads_an_address() {
    let (_d, bin) = town("address");
    let hit = &query(&bin, "Main Street 5, Ville", &[])[0];
    assert_eq!(hit["housenumber"], "5");
    assert!(
        !flags(hit)
            .iter()
            .any(|f| f.starts_with("input_") || f == "intersection"),
        "{hit}"
    );
}

#[test]
fn two_streets_that_meet_answer_their_crossing() {
    let (_d, bin) = town("crossing");
    for q in [
        "Main Street & Elm Street, Ville",
        "Main Street and Elm Street, Ville",
        "corner of Elm Street and Main Street, Ville",
    ] {
        let hit = &query(&bin, q, &[])[0];
        assert_eq!(hit["precision"], "intersection", "{q}: {hit}");
        assert_eq!(hit["match_level"], "StreetInt");
        assert!(hit.get("housenumber").is_none());
        // the closest pair is Main 4 (7.4205) and Elm 1 (43.73029): the middle of them
        let (lat, lon) = (hit["lat"].as_f64().unwrap(), hit["lon"].as_f64().unwrap());
        assert!(
            (lat - 43.730145).abs() < 0.00002 && (lon - 7.420475).abs() < 0.00003,
            "{q}: {lat},{lon}"
        );
    }
}

#[test]
fn two_streets_that_never_meet_keep_the_old_answer() {
    let (_d, bin) = town("apart");
    let hit = &query(&bin, "Main Street & Far Road, Ville", &[])[0];
    assert_ne!(hit["precision"], "intersection", "1 km apart: {hit}");
    let hit = &query(&bin, "Main Street & Near Lane, Ville", &[])[0];
    assert_ne!(
        hit["precision"], "intersection",
        "120 m apart, more than the 80 m of a crossing: {hit}"
    );
}

#[test]
fn a_street_with_the_connector_in_its_name_is_that_street() {
    let (_d, bin) = town("one-street");
    let hit = &query(&bin, "Ramon y Cajal Street, Ville", &[])[0];
    assert_eq!(hit["street"], "Ramon Y Cajal Street", "{hit}");
    assert_ne!(hit["precision"], "intersection");
    let crossing = &query(&bin, "Ramon Street & Cajal Street, Ville", &[])[0];
    assert_eq!(
        crossing["precision"], "intersection",
        "the two streets do cross: {crossing}"
    );
}

#[test]
fn lenient_takes_a_second_edit_on_a_name_of_nine_to_eleven_letters() {
    let (_d, bin) = town("lenient");
    // one slip in the name and one in the type: two edits from "elm street" (10 letters), each word within one
    let q = "Elk Streat 3, Ville";
    assert!(
        query(&bin, q, &[])
            .iter()
            .all(|h| h["street"] != "Elm Street" || h["precision"] == "city"),
        "default: one edit"
    );
    let hit = &query(&bin, q, &["--lenient"])[0];
    assert_eq!(
        (hit["street"].as_str(), hit["housenumber"].as_str()),
        (Some("Elm Street"), Some("3")),
        "{hit}"
    );
}

#[test]
fn a_box_answers_its_town_and_care_of_answers_the_address_after_the_name() {
    let (_d, bin) = town("box");
    let hit = &query(&bin, "PO Box 12, Ville", &[])[0];
    assert_eq!(hit["precision"], "city", "{hit}");
    assert!(flags(hit).contains(&"po_box".to_string()));
    assert_eq!(hit["weak"], true, "a box has no door");
    let with_street = query(&bin, "PO Box 12, Main Street 5, Ville", &[]);
    assert!(
        with_street.iter().all(|h| h["precision"] == "city"),
        "a box is never a house: {with_street:?}"
    );
    // the line read whole already reaches the house: that answer stands, with the flag
    let hit = &query(&bin, "c/o Jane Doe, Main Street 5, Ville", &[])[0];
    assert_eq!(hit["housenumber"], "5", "{hit}");
    assert!(flags(hit).contains(&"care_of".to_string()));
    assert!(query(&bin, "Postfachstraße 3", &[])
        .iter()
        .all(|h| !flags(h).contains(&"po_box".to_string())));
    // a street named with the box word is a street: "Apartado 5" is Calle Apartado 5 (Mexico City has one)
    let street = &query(&bin, "Apartado 5, Ville", &[])[0];
    assert_eq!(
        (street["street"].as_str(), street["housenumber"].as_str()),
        (Some("Calle Apartado"), Some("5")),
        "{street}"
    );
    assert!(!flags(street).contains(&"po_box".to_string()));
}

#[test]
fn care_of_does_not_answer_a_weak_rest_in_place_of_the_old_way() {
    // pin 0.9.0, DE 5k dirty row 3271: "c/o Jacobs University Bremen Campusring 1 Bremen, 28759Bremen" — the
    // part after the first comma gave a weak "Bremen" street 190 km away where the line used to stay silent.
    // Here the rest after the comma reads only a town (a weak answer): the c/o shortcut stands back, and
    // whatever the line gives read the old way carries no care_of flag. Mutation: drop the weak-rest rule — red.
    let (_d, bin) = town("careof-weak");
    let hits = query(&bin, "c/o Jane Doe, Ville", &[]);
    assert!(
        hits.iter()
            .all(|h| !flags(h).contains(&"care_of".to_string())),
        "a weak answer to the rest is not the c/o answer: {hits:?}"
    );
    // a strong answer to the rest still is
    let hit = &query(&bin, "c/o Jane Doe, Elm Street 3, Ville", &[])[0];
    assert_eq!(hit["street"], "Elm Street", "{hit}");
    assert!(flags(hit).contains(&"care_of".to_string()), "{hit}");
}

#[test]
fn strict_drops_a_fuzzy_street_that_the_default_takes() {
    let (_d, bin) = town("strict");
    let fuzzy = query(&bin, "Main Streat 5, Ville", &[]);
    assert!(
        !fuzzy.is_empty() && flags(&fuzzy[0]).contains(&"street_fuzzy".to_string()),
        "{fuzzy:?}"
    );
    assert!(query(&bin, "Main Streat 5, Ville", &["--strict"]).is_empty());
    assert_eq!(
        query(&bin, "Main Street 5, Ville", &["--strict"])[0]["housenumber"],
        "5"
    );
}

#[test]
fn abbreviations_are_written_out_before_the_query() {
    let (d, bin) = town("abbrev");
    let tsv = d.join("abbrev.tsv");
    std::fs::write(&tsv, "Mn\tMain\nElmst\tElm Street\n").unwrap();
    let hit = &query(
        &bin,
        "Mn Street 5, Ville",
        &["--abbrev", tsv.to_str().unwrap()],
    )[0];
    assert_eq!(
        (hit["street"].as_str(), hit["housenumber"].as_str()),
        (Some("Main Street"), Some("5")),
        "{hit}"
    );
}

#[test]
fn a_block_answers_the_middle_of_its_hundred() {
    let (_d, bin) = town("block");
    let hit = &query(&bin, "1200 block of Oak Street, Ville", &[])[0];
    assert_eq!(hit["housenumber"], "1250", "{hit}");
    assert!(flags(hit).contains(&"block".to_string()));
}
