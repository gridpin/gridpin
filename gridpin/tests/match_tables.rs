//! `gridpin match A B OUT`: two tables joined by the address their rows name (plan K4.2, 17.11.2026). The key is
//! that of `gridpin same`; weak answers make no key unless `--lenient`.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

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
    let dir = Scratch::dir(&format!("match-{name}"));
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
    // the same street name and numbers in a second town: one key per town
    for n in 1..=3 {
        rows.push(format!(
            "rue de la paix,13201,marseille,13001,{n},,{:.6},43.2960,Rue de la Paix,Marseille",
            5.37 + n as f64 * 0.0001
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

fn table(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
    let p = dir.join(name);
    let mut s = String::from("id,address,note\n");
    for (i, l) in lines.iter().enumerate() {
        s.push_str(&format!("{},\"{}\",x\n", i + 1, l.replace('"', "\"\"")));
    }
    std::fs::write(&p, s).unwrap();
    p
}

/// (a_row, b_row, level, a_matches, b_matches, reason) of the output.
fn rows(bin: &Path, a: &Path, b: &Path, extra: &[&str]) -> Vec<Vec<String>> {
    let out = a.with_file_name("out.csv");
    let mut args = vec![
        "match",
        bin.to_str().unwrap(),
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        out.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    let o = Command::new(BIN).args(&args).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut r = csv::Reader::from_path(&out).unwrap();
    assert_eq!(
        r.headers().unwrap().iter().collect::<Vec<_>>(),
        [
            "a_row",
            "b_row",
            "level",
            "a_matches",
            "b_matches",
            "reason",
            "a_address",
            "b_address"
        ]
    );
    r.records()
        .map(|x| x.unwrap().iter().take(6).map(str::to_string).collect())
        .collect()
}

#[test]
fn two_writings_of_one_house_pair_and_other_houses_do_not() {
    let (d, bin) = sheet("0");
    let a = table(
        &d,
        "a.csv",
        &[
            "10 Rue de la Paix, Paris",
            "3 Rue de la Paix, Paris",
            "2 Rue A, Lyon",
        ],
    );
    let b = table(
        &d,
        "b.csv",
        &[
            "rue de la paix 10 paris",
            "4 Rue de la Paix, Paris",
            "Rue A 2, 69001 Lyon",
        ],
    );
    let r = rows(&bin, &a, &b, &[]);
    assert_eq!(
        r,
        vec![
            vec!["1", "1", "house", "1", "1", ""],
            vec!["3", "3", "house", "1", "1", ""],
        ]
    );
    // at the street level the two houses of one street pair too
    let s = rows(&bin, &a, &b, &["--level", "street"]);
    assert!(
        s.contains(&vec![
            "2".to_string(),
            "2".into(),
            "street".into(),
            "2".into(),
            "2".into(),
            "".into()
        ]),
        "{s:?}"
    );
}

#[test]
fn rows_without_a_pair_say_why_only_when_asked() {
    let (d, bin) = sheet("1");
    let a = table(
        &d,
        "a.csv",
        &[
            "10 Rue de la Paix, Paris",
            "Paix, Lyon",
            "xyzzy nowhere",
            "Rue de la Paix, Paris",
        ],
    );
    let b = table(&d, "b.csv", &["9 Rue de la Paix, Paris"]);
    assert!(rows(&bin, &a, &b, &[]).is_empty(), "no pair");
    let u = rows(&bin, &a, &b, &["--unmatched"]);
    let why: Vec<(&str, &str, &str)> = u
        .iter()
        .map(|r| (r[0].as_str(), r[1].as_str(), r[5].as_str()))
        .collect();
    assert_eq!(
        why,
        vec![
            ("1", "", "no_pair"),
            ("2", "", "weak"),
            ("3", "", "no_answer"),
            ("4", "", "no_number"),
            ("", "1", "no_pair")
        ]
    );
}

#[test]
fn a_weak_answer_pairs_only_with_lenient() {
    let (d, bin) = sheet("2");
    // "BP 12, Rue de la Paix, Paris" is answered weakly by a house of Rue de la Paix ("near", fuzzy street)
    let a = table(&d, "a.csv", &["BP 12, Rue de la Paix, Paris"]);
    let lenient = rows(&bin, &a, &a, &["--lenient", "--level", "street"]);
    assert_eq!(lenient.len(), 1, "{lenient:?}");
    assert!(rows(&bin, &a, &a, &["--level", "street"]).is_empty());
}

#[test]
fn one_to_many_is_written_as_it_is_with_its_counts() {
    let (d, bin) = sheet("3");
    let a = table(&d, "a.csv", &["10 Rue de la Paix, Paris"]);
    let b = table(
        &d,
        "b.csv",
        &[
            "10 Rue de la Paix, Paris",
            "Rue de la Paix 10, Paris",
            "1 Rue A, Lyon",
        ],
    );
    let r = rows(&bin, &a, &b, &[]);
    assert_eq!(
        r,
        vec![
            vec!["1", "1", "house", "2", "1", ""],
            vec!["1", "2", "house", "2", "1", ""]
        ]
    );
}

#[test]
fn every_pair_is_a_pair_that_same_calls_the_same_level() {
    let (d, bin) = sheet("4");
    let la = [
        "10 Rue de la Paix, Paris",
        "3 Rue de la Paix, Paris",
        "2 Rue A, Lyon",
        "5 Rue A",
    ];
    let lb = [
        "rue de la paix 10 paris",
        "Rue de la Paix 3, Paris",
        "Rue A 2, 69001 Lyon",
        "4 Rue A, Lyon",
    ];
    let (a, b) = (table(&d, "a.csv", &la), table(&d, "b.csv", &lb));
    for (level, ok) in [
        ("house", &["house"][..]),
        ("street", &["house", "street"][..]),
    ] {
        let r = rows(&bin, &a, &b, &["--level", level]);
        assert!(!r.is_empty());
        for p in r {
            let (i, j): (usize, usize) = (p[0].parse().unwrap(), p[1].parse().unwrap());
            let o = Command::new(BIN)
                .args(["same", bin.to_str().unwrap(), la[i - 1], lb[j - 1]])
                .output()
                .unwrap();
            let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
            assert!(
                ok.contains(&v["level"].as_str().unwrap()),
                "{level}: {} / {}: {v}",
                la[i - 1],
                lb[j - 1]
            );
        }
    }
}

#[test]
fn an_unknown_level_is_refused() {
    let (d, bin) = sheet("5");
    let a = table(&d, "a.csv", &["10 Rue de la Paix, Paris"]);
    let o = Command::new(BIN)
        .args([
            "match",
            bin.to_str().unwrap(),
            a.to_str().unwrap(),
            a.to_str().unwrap(),
            d.join("o.csv").to_str().unwrap(),
            "--level",
            "commune",
        ])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("house or street"));
}

#[test]
fn one_street_and_number_in_two_towns_is_two_keys() {
    let (d, bin) = sheet("6");
    let a = table(&d, "a.csv", &["2 Rue de la Paix, Paris"]);
    let b = table(
        &d,
        "b.csv",
        &["2 Rue de la Paix, Marseille", "Rue de la Paix 2, Paris"],
    );
    assert_eq!(
        rows(&bin, &a, &b, &[]),
        vec![vec!["1", "2", "house", "1", "1", ""]]
    );
    assert_eq!(rows(&bin, &a, &b, &["--level", "street"]).len(), 1);
}
