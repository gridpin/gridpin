//! `gridpin extract` on a three-street fixture sheet: addresses in a letter, two in one paragraph, a phone
//! beside a street, a line break inside an address, a text without addresses, and `--verify` naming an address
//! the sheet does not have.
use std::path::PathBuf;
use std::process::Command;

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

fn sheet(name: &str) -> (Scratch, PathBuf) {
    // the builder takes rows sorted by street key
    let mut rows = String::from(HDR);
    for (i, n) in [2, 4, 6].iter().enumerate() {
        rows +=
            &format!("avenue du port,001,ville,10000,{n},,7.43{i},43.740,Avenue du Port,Ville\n");
    }
    for (i, n) in [1, 3, 5, 7, 11].iter().enumerate() {
        rows += &format!("rue des lilas,001,ville,10000,{n},,7.42{i},43.730,Rue des Lilas,Ville\n");
    }
    // a commune split into districts: "Quartier Nord" lies inside the town people call "Metropole"
    rows += "rue du port,005,metropole,20000,1,,7.448,43.762,Rue du Port,Metropole\n";
    for (i, n) in [10, 12].iter().enumerate() {
        rows += &format!("rue haute,002,bourg,20000,{n},,7.45{i},43.760,Rue Haute,Bourg\n");
    }
    build(name, &rows)
}

fn build(name: &str, rows: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("extract-{name}"));
    let csv = dir.join("in.csv");
    std::fs::write(&csv, rows).unwrap();
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

fn extract(bin: &std::path::Path, text: &str, verify: bool) -> Vec<serde_json::Value> {
    let path = bin.with_file_name(format!("t{}.txt", text.len()));
    std::fs::write(&path, text).unwrap();
    let mut args = vec![
        "extract",
        bin.to_str().unwrap(),
        "--text",
        path.to_str().unwrap(),
    ];
    if verify {
        args.push("--verify");
    }
    let out = Command::new(BIN).args(&args).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn found(v: &[serde_json::Value]) -> Vec<(String, String)> {
    v.iter()
        .filter(|f| f["verdict"] == "found")
        .map(|f| {
            (
                f["hit"]["street"].as_str().unwrap().to_string(),
                f["level"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn a_letter_with_one_address_gives_that_house_and_its_offsets() {
    let (_d, bin) = sheet("letter");
    let text = "Bonjour,\nmerci d'envoyer le colis au 5 rue des Lilas, Ville avant vendredi.\nCordialement";
    let v = extract(&bin, text, false);
    assert_eq!(
        found(&v),
        [("Rue des Lilas".to_string(), "house".to_string())]
    );
    let (s, e) = (
        v[0]["start"].as_u64().unwrap() as usize,
        v[0]["end"].as_u64().unwrap() as usize,
    );
    assert_eq!(&text[s..e], v[0]["text"].as_str().unwrap());
    assert!(v[0]["text"].as_str().unwrap().contains("5 rue des Lilas"));
    assert_eq!(v[0]["hit"]["housenumber"], "5");
}

#[test]
fn two_addresses_in_one_paragraph_are_both_found_and_do_not_overlap() {
    let (_d, bin) = sheet("invoice");
    let text = "Facture: livraison 3 rue des Lilas, Ville; facturation 4 avenue du Port, Ville. Total 120 euros.";
    let v = extract(&bin, text, false);
    let f = found(&v);
    assert_eq!(f.len(), 2, "{v:?}");
    assert!(f.contains(&("Rue des Lilas".into(), "house".into())));
    assert!(f.contains(&("Avenue du Port".into(), "house".into())));
    assert!(
        v[0]["end"].as_u64() <= v[1]["start"].as_u64(),
        "kept windows do not overlap"
    );
}

#[test]
fn a_phone_beside_a_street_is_not_a_house_number() {
    let (_d, bin) = sheet("phone");
    // 11 is a house of the street: the last group of a phone must not lend it as a number
    for text in [
        "Tel +381 11 1234567 rue des Lilas",
        "Tel. 01 22 33 44 11 rue des Lilas, Ville",
    ] {
        let v = extract(&bin, text, false);
        assert!(v.iter().all(|f| f["level"] != "house"), "{text}: {v:?}");
    }
}

#[test]
fn a_line_break_ends_a_window_but_the_line_with_the_address_is_found() {
    let (_d, bin) = sheet("newline");
    let v = extract(&bin, "Adresse:\n7 rue des Lilas\nVille", false);
    assert_eq!(
        found(&v),
        [("Rue des Lilas".to_string(), "house".to_string())]
    );
    assert!(!v[0]["text"].as_str().unwrap().contains('\n'));
}

#[test]
fn a_street_name_without_a_number_or_a_type_word_is_not_a_window() {
    let (_d, bin) = sheet("anchor");
    // the engine alone reads "des Lilas, Ville" as the street; without an anchor the window is never asked
    assert!(extract(&bin, "Nous aimons des Lilas, Ville.", false).is_empty());
}

#[test]
fn a_text_without_addresses_prints_nothing_and_exits_zero() {
    let (_d, bin) = sheet("none");
    assert!(extract(
        &bin,
        "Rendez-vous demain a 10 heures, merci beaucoup.",
        false
    )
    .is_empty());
}

#[test]
fn verify_names_an_address_the_sheet_does_not_have() {
    let (_d, bin) = sheet("verify");
    let v = extract(
        &bin,
        "Livrer au 3 rue des Lilas, Ville. Ou bien au 99 rue Inconnue, Ville.",
        true,
    );
    let ok: Vec<_> = v.iter().filter(|f| f["verdict"] == "found").collect();
    let bad: Vec<_> = v.iter().filter(|f| f["verdict"] == "not_found").collect();
    assert_eq!(ok.len(), 1, "{v:?}");
    assert_eq!(bad.len(), 1, "{v:?}");
    assert!(bad[0]["text"].as_str().unwrap().contains("99"));
}

#[test]
fn verify_says_the_number_is_not_on_a_real_street() {
    let (_d, bin) = sheet("reason");
    let v = extract(&bin, "Envoyer au 99999 rue des Lilas, Ville.", true);
    let bad: Vec<_> = v.iter().filter(|f| f["verdict"] == "not_found").collect();
    assert_eq!(bad.len(), 1, "{v:?}");
    assert_eq!(bad[0]["reason"], "number_not_on_street", "{v:?}");
}

#[test]
fn a_long_text_without_addresses_stops_at_the_window_cap_and_says_so() {
    let (_d, bin) = sheet("cap");
    let text = "avenue ".repeat(400);
    let path = bin.with_file_name("long.txt");
    std::fs::write(&path, &text).unwrap();
    let out = Command::new(BIN)
        .args([
            "extract",
            bin.to_str().unwrap(),
            "--text",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cap"),
        "the cap is logged, not an error"
    );
}

/// A street named by its type word alone in Ville, Rue Haute both in Bourg (10, 12) and in a commune whose name
/// starts with "Cite" (14, 16): the house 10 the text asks in Cite is only in Bourg. Rue Basse: the house 7 is
/// only in Bourg, and the Bourg rows carry no postcode, so only the commune can tell the places apart.
fn place_sheet(name: &str) -> (Scratch, PathBuf) {
    let mut rows = String::from(HDR);
    // a street named by two type words, as "Court Street" of the US sheet
    rows += "allee chemin,007,bourg,20000,5,,7.461,43.771,Allee Chemin,Bourg\n";
    rows += "rue,001,ville,10000,20,,7.400,43.700,Rue,Ville\n";
    rows += "rue basse,002,bourg,,7,,7.460,43.770,Rue Basse,Bourg\n";
    rows += "rue basse,003,cite 2e arrondissement,30000,2,,7.910,44.210,Rue Basse,Cite 2e Arrondissement\n";
    rows += "rue blanche,006,port neuf centre,20000,1,,7.452,43.763,Rue Blanche,Port Neuf Centre\n";
    for (i, n) in [1, 3, 5].iter().enumerate() {
        rows += &format!("rue des lilas,001,ville,10000,{n},,7.42{i},43.730,Rue des Lilas,Ville\n");
    }
    for (i, n) in [10, 12].iter().enumerate() {
        rows += &format!("rue haute,002,bourg,20000,{n},,7.45{i},43.760,Rue Haute,Bourg\n");
    }
    for (i, n) in [14, 16].iter().enumerate() {
        rows += &format!(
            "rue haute,003,cite 2e arrondissement,30000,{n},,7.90{i},44.200,Rue Haute,Cite 2e Arrondissement\n"
        );
    }
    rows += "rue verte,004,quartier nord,20000,4,,7.455,43.765,Rue Verte,Quartier Nord\n";
    build(name, &rows)
}

fn communes_found(v: &[serde_json::Value]) -> Vec<String> {
    v.iter()
        .filter(|f| f["verdict"] == "found")
        .map(|f| f["hit"]["commune"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_street_named_by_its_type_word_alone_is_not_found_from_a_cut_window() {
    let (_d, bin) = place_sheet("typeword");
    // "20 rue" of "20 rue des Pins" is the street "Rue" 20 of Ville: the text named another street
    let v = extract(&bin, "Livrer au 20 rue des Pins merci", false);
    assert!(communes_found(&v).is_empty(), "{v:?}");
    let v = extract(&bin, "Livrer au 20 rue des Pins merci", true);
    assert!(
        v.iter().all(|f| f["hit"]["street"] != "Rue"),
        "nor is it the report of the missing address: {v:?}"
    );
}

#[test]
fn a_house_in_another_postcode_than_the_line_names_is_not_found() {
    let (_d, bin) = place_sheet("postcode");
    // the window "10 rue Haute" alone is a house of Bourg (20000); the line says 30000
    let v = extract(&bin, "Livrer au 10 rue Haute 30000 merci", false);
    assert!(!communes_found(&v).contains(&"Bourg".to_string()), "{v:?}");
}

#[test]
fn a_house_in_another_commune_than_the_line_names_is_not_found() {
    let (_d, bin) = place_sheet("city");
    // "Cite" is the leading word of the commune "Cite 2e Arrondissement"
    let v = extract(&bin, "Livrer au 10 rue Haute, Cite merci", false);
    assert!(!communes_found(&v).contains(&"Bourg".to_string()), "{v:?}");
    assert_eq!(
        communes_found(&v),
        ["Cite 2e Arrondissement".to_string()],
        "the street of the named place is found instead: {v:?}"
    );
}

#[test]
fn a_window_that_names_its_own_place_is_not_judged_by_the_next_address_of_the_line() {
    let (_d, bin) = place_sheet("own");
    // a list joined by commas: "Cite" right after "…, Ville" is the place of the next address
    let v = extract(
        &bin,
        "Livrer au 3 rue des Lilas, Ville, Cite 14 rue Haute",
        false,
    );
    assert_eq!(
        communes_found(&v),
        ["Ville".to_string(), "Cite 2e Arrondissement".to_string()],
        "{v:?}"
    );
    // and from the address itself, not from the words before it that the engine dropped
    assert_eq!(v[0]["text"], "3 rue des Lilas, Ville", "{v:?}");
}

#[test]
fn the_commune_written_after_the_postcode_is_the_place_of_the_line() {
    let (_d, bin) = place_sheet("after-code");
    // the Bourg answer has no postcode to differ by: "Cite" after "30000" says it is the wrong place
    let v = extract(&bin, "Livrer au 7 rue Basse 30000 Cite merci", false);
    assert_eq!(
        communes_found(&v),
        ["Cite 2e Arrondissement".to_string()],
        "{v:?}"
    );
    // control: without a place on the line the house of Bourg is the answer
    let v = extract(&bin, "Livrer au 7 rue Basse merci", false);
    assert_eq!(communes_found(&v), ["Bourg".to_string()], "{v:?}");
}

#[test]
fn verify_reports_a_missing_address_from_its_number_to_its_place() {
    let (_d, bin) = place_sheet("report");
    let v = extract(
        &bin,
        "Client rappele, adresse : 99 rue des Pins 30000 Cite, code 4512",
        true,
    );
    assert_eq!(v.len(), 1, "one report, not one per piece: {v:?}");
    let text = v[0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("99 rue des Pins") && text.contains("Cite"),
        "{v:?}"
    );
    assert_eq!(v[0]["verdict"], "not_found");
}

#[test]
fn verify_reports_a_number_missing_on_a_found_street_with_that_street() {
    let (_d, bin) = place_sheet("street");
    for (text, window) in [
        ("Livrer au 10 rue Haute, Cite merci", "10 rue Haute, Cite"),
        // the street window holds the postcode and the commune; a shorter one would drop the commune
        (
            "Livrer au 10 rue Haute 30000 Cite merci",
            "10 rue Haute 30000 Cite",
        ),
    ] {
        let v = extract(&bin, text, true);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0]["text"], window, "{v:?}");
        assert_eq!(v[0]["reason"], "number_not_on_street", "{v:?}");
        assert_eq!(v[0]["hit"]["commune"], "Cite 2e Arrondissement", "{v:?}");
    }
}

#[test]
fn the_place_of_the_next_address_after_a_semicolon_does_not_judge_this_one() {
    let (_d, bin) = place_sheet("semicolon");
    // "Cite" after ";" names where the next address is, not where "10 rue Haute" (Bourg) is
    let v = extract(&bin, "Livraisons : 10 rue Haute ; Cite 14 rue Haute", false);
    assert!(communes_found(&v).contains(&"Bourg".to_string()), "{v:?}");
}

#[test]
fn the_postcode_of_the_line_is_the_first_number_after_the_window_not_a_next_address() {
    let (_d, bin) = place_sheet("firstnumber");
    // two addresses joined by a word: 30000 is the code of "14 rue Haute", not of "10 rue Haute" (Bourg)
    let v = extract(
        &bin,
        "Livrer 10 rue Haute et 14 rue Haute 30000 merci",
        false,
    );
    assert!(communes_found(&v).contains(&"Bourg".to_string()), "{v:?}");
}

#[test]
fn a_district_of_the_town_the_line_names_is_in_that_town() {
    let (_d, bin) = place_sheet("district");
    // the sheet keeps "Quartier Nord" as a commune; the line calls the town "Metropole", 1 km away
    let v = extract(&bin, "Livrer au 4 rue Verte Metropole merci", false);
    assert_eq!(communes_found(&v), ["Quartier Nord".to_string()], "{v:?}");
    // the house is found from the window the engine read whole, not from the longer one it trimmed
    assert_eq!(v[0]["text"], "4 rue Verte", "{v:?}");
    // the town named by the leading words of the sheet's communes ("Port Neuf" of "Port Neuf Centre")
    let v = extract(&bin, "Livrer au 4 rue Verte Port Neuf merci", false);
    assert_eq!(communes_found(&v), ["Quartier Nord".to_string()], "{v:?}");
    assert_eq!(v[0]["text"], "4 rue Verte", "{v:?}");
}

/// Bourg has "Rue Haute" with an 8 and code 20000; Cite (30000) has "Rue Hautes Bourg" without one
fn toledo_sheet(name: &str) -> (Scratch, PathBuf) {
    build(
        name,
        &format!(
            "{HDR}rue haute,002,bourg,20000,8,,7.450,43.760,Rue Haute,Bourg\n\
             rue hautes bourg,003,cite,30000,2,,7.905,44.205,Rue Hautes Bourg,Cite\n"
        ),
    )
}

#[test]
fn the_postcode_of_the_line_outranks_a_town_named_inside_the_street() {
    // "C. Pedro de Toledo, 4, 29015 Málaga": the window reads "Toledo" as its town and guesses a street there
    // ("Calle Pedro Alcocer"); here "Hauts" is guessed as "Rue Haute" of Bourg, the line's code says Cite, whose
    // "Rue Hautes Bourg" has no 8
    let (_d, bin) = toledo_sheet("toledo");
    let v = extract(&bin, "Livrer au 8 rue Hauts Bourg, 30000 merci", false);
    assert!(!communes_found(&v).contains(&"Bourg".to_string()), "{v:?}");
}

#[test]
fn the_exact_street_of_the_town_the_window_names_stands_against_a_postcode_of_its_own_area() {
    // "110 Schloss Ln Moraine OH 45439": street and town written exactly, the sheet keeps another code of the
    // same area (45418)
    let (_d, bin) = toledo_sheet("moraine");
    let v = extract(&bin, "Livrer au 8 rue Haute Bourg 20100 merci", false);
    assert_eq!(communes_found(&v), ["Bourg".to_string()], "{v:?}");
    assert_eq!(v[0]["hit"]["housenumber"], "8", "{v:?}");
}

#[test]
fn the_exact_street_of_a_town_written_as_a_town_stands_against_a_postcode_of_another_area() {
    // "808 S Hobart Blvd, Los Angeles, CA 60603": street and town written, the code is of another state
    let (_d, bin) = toledo_sheet("town-written");
    let v = extract(&bin, "Livrer au 8 rue Haute, Bourg, 30000 merci", false);
    assert_eq!(communes_found(&v), ["Bourg".to_string()], "{v:?}");
    assert_eq!(v[0]["hit"]["housenumber"], "8", "{v:?}");
}

#[test]
fn the_exact_street_of_the_town_the_window_names_yields_to_a_postcode_of_another_area() {
    // "C. San Fernando, 55, 03001 Alicante": the window reads the town San Fernando and a street "C" there (11100);
    // a code of another area is another place, not a code the sheet keeps otherwise
    let (_d, bin) = toledo_sheet("other-area");
    let v = extract(&bin, "Livrer au 8 rue Haute Bourg 30000 merci", false);
    assert!(!communes_found(&v).contains(&"Bourg".to_string()), "{v:?}");
}

/// Borgo has a street named after the town Alba (30000, 50 km away), without a code
fn oldenburg_sheet(name: &str) -> (Scratch, PathBuf) {
    build(
        name,
        &format!(
            "{HDR}alba,002,borgo,,17,,7.450,43.760,Alba,Borgo\n\
             lindenweg,003,alba,30000,17,,7.905,44.205,Lindenweg,Alba\n"
        ),
    )
}

/// One street "Zeta" in Borgo (20000): asked with 20100, the engine still answers it (the same first digits)
fn zeta_sheet(name: &str) -> (Scratch, PathBuf) {
    build(
        name,
        &format!("{HDR}zeta,002,borgo,20000,17,,7.452,43.762,Zeta,Borgo\n"),
    )
}

#[test]
fn a_postcode_inside_the_window_judges_its_answer() {
    // "Borsdorfer Str. 9, 04318" answered in Naunhof (04683): the window holds the code the answer lacks; the
    // window "Zeta 17, 20100" ends the line, so only the code inside it can judge it
    let (_d, bin) = zeta_sheet("code-inside");
    let v = extract(&bin, "Livrer: Zeta 17, 20100", false);
    assert!(communes_found(&v).is_empty(), "{v:?}");
    // control: with the code of the answer the house is found
    let v = extract(&bin, "Livrer: Zeta 17, 20000", false);
    assert_eq!(communes_found(&v), ["Borgo".to_string()], "{v:?}");
}

#[test]
fn the_town_after_a_postcode_inside_the_window_judges_its_answer() {
    // "17, 26121 Oldenburg" answered as the street "Oldenburg" of Brakel, 170 km away
    let (_d, bin) = oldenburg_sheet("town-inside");
    let v = extract(&bin, "Livrer: 17, 30000 Alba", false);
    assert!(!communes_found(&v).contains(&"Borgo".to_string()), "{v:?}");
}

#[test]
fn the_town_at_the_head_of_a_later_part_of_the_line_is_its_place() {
    // "1001 Woodward Ave #505, Detroit, MI 48226": the town is not right after the window
    let (_d, bin) = place_sheet("later-part");
    let v = extract(&bin, "Livrer au 10 rue Haute (bis), au fond, Cite", false);
    assert!(!communes_found(&v).contains(&"Bourg".to_string()), "{v:?}");
}

#[test]
fn the_town_a_line_writes_last_is_its_town() {
    // "…, 56, Salamanca, 28006 Madrid": a district written first, the town last
    let (_d, bin) = place_sheet("last-town");
    let v = extract(&bin, "Livrer au 10 rue Haute Bourg, Cite", false);
    assert!(!communes_found(&v).contains(&"Bourg".to_string()), "{v:?}");
}

#[test]
fn the_town_of_a_next_address_on_the_line_is_not_the_place_of_this_one() {
    // the town written after the number and street of the next address is that address's town
    let (_d, bin) = place_sheet("next-town");
    let v = extract(
        &bin,
        "Livrer au 3 rue des Lilas, Ville, 14 rue Haute, Cite",
        false,
    );
    assert_eq!(
        communes_found(&v),
        ["Ville".to_string(), "Cite 2e Arrondissement".to_string()],
        "{v:?}"
    );
}

#[test]
fn a_window_that_names_its_own_place_yields_only_to_a_town_in_address_form() {
    // "…, 56, Salamanca, 28006 Madrid": after a comma or a postcode the town is the place of the line
    let (_d, bin) = place_sheet("own-place-form");
    let v = extract(&bin, "Livrer au 3 rue des Lilas Ville, Cite", false);
    assert!(!communes_found(&v).contains(&"Ville".to_string()), "{v:?}");
    // a word that happens to name a town, written after the window without a comma, does not
    let v = extract(&bin, "Livrer au 3 rue des Lilas Ville Cite merci", false);
    assert!(communes_found(&v).contains(&"Ville".to_string()), "{v:?}");
}

/// "Rue Double" 3 both in Bourg and, far away, in Cite; "Ville" lies far from both
fn double_sheet(name: &str) -> (Scratch, PathBuf) {
    let mut rows = String::from(HDR);
    rows += "rue des lilas,001,ville,10000,1,,8.900,45.300,Rue des Lilas,Ville\n";
    rows += "rue double,002,bourg,20000,3,,7.450,43.760,Rue Double,Bourg\n";
    rows += "rue double,003,cite 2e arrondissement,30000,3,,7.900,44.200,Rue Double,Cite 2e Arrondissement\n";
    for (i, n) in [5, 7, 9, 11].iter().enumerate() {
        rows += &format!(
            "rue double,003,cite 2e arrondissement,30000,{n},,7.90{i},44.20{i},Rue Double,Cite 2e Arrondissement\n"
        );
    }
    build(name, &rows)
}

#[test]
fn a_window_that_names_its_own_place_keeps_its_bounds_before_a_loose_town_word() {
    // "3 rue Double" alone is the twin in Cite; "3 rue Double Bourg" names its town, and "Ville" written after it
    // without a comma is no place of the line: the address found is the window without that word
    let (_d, bin) = double_sheet("loose-word");
    let v = extract(&bin, "Livrer au 3 rue Double Bourg Ville merci", false);
    assert_eq!(communes_found(&v), ["Bourg".to_string()], "{v:?}");
    assert_eq!(v[0]["text"], "3 rue Double Bourg", "{v:?}");
}

#[test]
fn the_place_follows_a_long_window_however_far_its_start() {
    // "Merci de livrer au 37 Rue Sainte-Croix de la Bretonnerie, 75004 Paris": the code is more than ten words
    // after the start of a window that took the greeting in
    let (_d, bin) = place_sheet("long-window");
    let v = extract(
        &bin,
        "Merci de bien vouloir livrer au 10 rue Haute de la cour 30000 merci",
        false,
    );
    assert!(!communes_found(&v).contains(&"Bourg".to_string()), "{v:?}");
}

#[test]
fn a_district_written_before_an_agreeing_postcode_is_not_checked_as_a_town() {
    let (_d, bin) = place_sheet("district-before-code");
    // "Cite, 20000": the code agrees with Bourg, "Cite" is a district name of the writer's own
    let v = extract(&bin, "Livrer au 12 rue Haute, Cite, 20000 merci", false);
    assert_eq!(communes_found(&v), ["Bourg".to_string()], "{v:?}");
    assert_eq!(v[0]["hit"]["housenumber"], "12", "{v:?}");
}

#[test]
fn a_street_named_by_two_type_words_is_a_street() {
    let (_d, bin) = place_sheet("two-types");
    let v = extract(&bin, "Livrer au 5 allee Chemin merci", false);
    assert_eq!(communes_found(&v), ["Bourg".to_string()], "{v:?}");
}

#[test]
fn the_town_of_a_hyphenated_district_after_the_postcode_is_read() {
    let (_d, bin) = place_sheet("hyphen");
    // "Cite-Ouest" is a district of the town "Cite": Rue Basse 7 of Bourg is the wrong place
    let v = extract(&bin, "Livrer au 7 rue Basse 30000 Cite-Ouest merci", false);
    assert!(!communes_found(&v).contains(&"Bourg".to_string()), "{v:?}");
}

/// A street named like a district ("Arganzuela" of Madrid), and "Rue Haute" in Bourg
fn district_sheet(name: &str) -> (Scratch, PathBuf) {
    build(
        name,
        &format!(
            "{HDR}arganzuela,001,metropole,20000,10,,7.450,43.760,Arganzuela,Metropole\n\
             boulevard,001,metropole,20000,4,,7.451,43.761,Boulevard,Metropole\n\
             f 2,001,metropole,20000,1,,7.452,43.762,F 2,Metropole\n\
             rue haute,002,bourg,30000,10,,7.900,44.200,Rue Haute,Bourg\n\
             rue large,009,vaste,40000,1,,7.000,43.000,Rue Large,Vaste\n\
             rue large,009,vaste,40000,3,,9.000,45.000,Rue Large,Vaste\n\
             rue large,009,vaste,40000,5,,9.001,45.001,Rue Large,Vaste\n\
             rue x,010,brasil novo,50000,1,,5.000,41.000,Rue X,Brasil Novo\n"
        ),
    )
}

#[test]
fn a_number_that_ends_its_part_of_the_line_belongs_to_the_street_before_it() {
    // "Quorvalix, 10, Arganzuela, 28045 Madrid": the street the sheet lacks is written before the number; the
    // district after it is no street of that number
    let (_d, bin) = district_sheet("number-part");
    let v = extract(
        &bin,
        "Livrer: Quorvalix, 10, Arganzuela, 20000 Metropole",
        false,
    );
    assert!(v.iter().all(|f| f["level"] != "house"), "{v:?}");
}

#[test]
fn the_street_before_the_number_or_a_type_word_after_its_comma_keeps_the_house() {
    let (_d, bin) = district_sheet("number-part-kept");
    for (text, commune) in [
        // the street written before its number, as Spain writes it
        ("Livrer: Arganzuela, 10, 20000 Metropole", "Metropole"),
        // no comma after the number: the street follows it
        ("Livrer: 10 Arganzuela, 20000 Metropole", "Metropole"),
        // "10, rue de Rivoli": the type word after the comma starts the street
        ("Livrer au 10, rue Haute, 30000 Bourg", "Bourg"),
        // a street named by its type word alone: "Boulevard 4, 33613 Bielefeld"
        ("Livrer: Boulevard 4, 20000 Metropole", "Metropole"),
        // a block written as one word: "F2 1, 68159 Mannheim"
        ("Livrer: F2 1, 20000 Metropole", "Metropole"),
    ] {
        let v = extract(&bin, text, false);
        let houses: Vec<_> = v.iter().filter(|f| f["level"] == "house").collect();
        assert_eq!(houses.len(), 1, "{text}: {v:?}");
        assert_eq!(houses[0]["hit"]["commune"], commune, "{text}: {v:?}");
    }
}

#[test]
fn a_country_written_last_is_no_town_of_the_line() {
    // "…, Natal - RN, Brasil": "Brasil" begins the commune "Brasil Novo" far away, and is the country; no code on
    // the line, so the town alone judges the answer
    let (_d, bin) = district_sheet("country-last");
    let v = extract(&bin, "Livrer: Arganzuela, 10, Metropole, Brasil", false);
    let houses: Vec<_> = v.iter().filter(|f| f["level"] == "house").collect();
    assert_eq!(houses.len(), 1, "{v:?}");
    assert_eq!(houses[0]["hit"]["commune"], "Metropole", "{v:?}");
}

#[test]
fn an_answer_in_the_commune_the_line_names_is_in_its_town_however_wide_the_commune() {
    // "R. Turiano Meira, 246 - Centro, Santarém - PA": the municipality reaches far beyond the radius
    let (_d, bin) = district_sheet("wide-commune");
    let v = extract(&bin, "Livrer: Rue Large, 1 - Centre, Vaste", false);
    let houses: Vec<_> = v.iter().filter(|f| f["level"] == "house").collect();
    assert_eq!(houses.len(), 1, "{v:?}");
    assert_eq!(houses[0]["hit"]["commune"], "Vaste", "{v:?}");
}
