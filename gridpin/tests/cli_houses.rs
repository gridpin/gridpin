//! `gridpin list`, `export`, `sample`, `stats`: the built binary on a small sheet whose build CSV is the truth.
//! A row of the output is compared with the row the sheet was built from, not with a hand-written answer.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";
/// Two communes; the same street name in both, and a street with two postcodes.
const ROWS: &str = "\
grand rue,001,ville,10000,1,,7.4200,43.7300,Grand Rue,Ville
grand rue,001,ville,10000,3,,7.4201,43.7301,Grand Rue,Ville
grand rue,001,ville,10000,5,,7.4202,43.7302,Grand Rue,Ville
grand rue,002,autre,20000,1,,7.5000,43.8000,Grand Rue,Autre
grand rue,002,autre,20000,2,,7.5001,43.8001,Grand Rue,Autre
long chemin,001,ville,10000,1,,7.4300,43.7400,Long Chemin,Ville
long chemin,001,ville,10001,9,,7.4400,43.7500,Long Chemin,Ville
petite place,002,autre,20000,4,,7.5100,43.8100,Petite Place,Autre
";
const HOUSES: usize = 8;

fn sheet(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("houses-{name}"));
    let csv = dir.join("in.csv");
    std::fs::write(&csv, format!("{HDR}{ROWS}")).unwrap();
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        r#"{"country":"mc","layer":"addresses","license":"CC BY 4.0","source_release":"test","attribution":"Fixture & Co"}"#,
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

fn list(bin: &Path, area: &[&str]) -> Vec<Vec<String>> {
    let mut args = vec!["list", bin.to_str().unwrap()];
    args.extend_from_slice(area);
    let out = run(&args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    table(&String::from_utf8(out.stdout).unwrap())
}

/// The data rows of a CSV output, after its two licence lines and its header.
fn table(text: &str) -> Vec<Vec<String>> {
    let mut lines = text.lines();
    assert!(lines.next().unwrap().starts_with("# license: "));
    assert!(lines.next().unwrap().starts_with("# attribution: "));
    assert_eq!(
        lines.next().unwrap(),
        "lat,lon,street,housenumber,postcode,commune,region"
    );
    lines
        .map(|l| {
            csv::ReaderBuilder::new()
                .has_headers(false)
                .from_reader(l.as_bytes())
                .records()
                .next()
                .unwrap()
                .unwrap()
                .iter()
                .map(String::from)
                .collect()
        })
        .collect()
}

/// `(street, number, commune)` of each row, sorted.
fn who(rows: &[Vec<String>]) -> Vec<(String, String, String)> {
    let mut all: Vec<_> = rows
        .iter()
        .map(|r| (r[2].clone(), r[3].clone(), r[5].clone()))
        .collect();
    all.sort();
    all
}

fn expect(list: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    let mut all: Vec<_> = list
        .iter()
        .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
        .collect();
    all.sort();
    all
}

#[test]
fn a_street_of_a_city_lists_exactly_its_houses_and_not_its_namesake() {
    let (_d, bin) = sheet("street");
    let rows = list(&bin, &["--street", "grand rue", "--city", "ville"]);
    assert_eq!(
        who(&rows),
        expect(&[
            ("Grand Rue", "1", "Ville"),
            ("Grand Rue", "3", "Ville"),
            ("Grand Rue", "5", "Ville")
        ])
    );
    // the coordinates are those of the build file, with the longitude and latitude in place
    assert_eq!(rows[0][0], "43.73");
    assert_eq!(rows[0][1], "7.42");
    assert_eq!(rows[0][4], "10000");
    let other = list(&bin, &["--street", "grand rue", "--city", "autre"]);
    assert_eq!(other.len(), 2);
    assert!(other.iter().all(|r| r[5] == "Autre"));
}

#[test]
fn a_street_alone_is_refused_and_a_street_that_is_not_there_says_so() {
    let (_d, bin) = sheet("street-no");
    let out = run(&["list", bin.to_str().unwrap(), "--street", "grand rue"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--city or --postcode"));
    let out = run(&[
        "list",
        bin.to_str().unwrap(),
        "--street",
        "zzzz qqqq",
        "--city",
        "nowhere",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no street found"));
    let out = run(&["list", bin.to_str().unwrap()]);
    assert!(!out.status.success(), "no area at all: use export --all");
}

#[test]
fn a_commune_lists_all_its_streets() {
    let (_d, bin) = sheet("commune");
    let rows = list(&bin, &["--city", "AUTRE"]);
    assert_eq!(
        who(&rows),
        expect(&[
            ("Grand Rue", "1", "Autre"),
            ("Grand Rue", "2", "Autre"),
            ("Petite Place", "4", "Autre")
        ])
    );
    let out = run(&["list", bin.to_str().unwrap(), "--city", "nowhere"]);
    assert!(!out.status.success());
}

#[test]
fn a_postcode_keeps_the_houses_that_carry_it_even_on_a_street_with_two() {
    let (_d, bin) = sheet("postcode");
    assert_eq!(list(&bin, &["--postcode", "10001"]).len(), 1);
    assert_eq!(list(&bin, &["--postcode", "10001"])[0][3], "9");
    let main = list(&bin, &["--postcode", "10000"]);
    assert_eq!(main.len(), 4, "three of Grand Rue and one of Long Chemin");
    assert!(list(&bin, &["--postcode", "99999"]).is_empty());
}

#[test]
fn a_circle_and_a_box_keep_what_lies_inside() {
    let (_d, bin) = sheet("area");
    // 50 m around the first house reach its two neighbours (about 14 and 28 m), not the far street
    let near = list(&bin, &["--near", "43.73,7.42,50"]);
    assert_eq!(who(&near).len(), 3);
    assert!(near.iter().all(|r| r[2] == "Grand Rue" && r[5] == "Ville"));
    assert_eq!(
        list(&bin, &["--near", "43.73,7.42,5"]).len(),
        1,
        "the radius counts"
    );
    let boxed = list(&bin, &["--bbox", "7.49,43.79,7.52,43.82"]);
    assert_eq!(who(&boxed).len(), 3);
    assert!(boxed.iter().all(|r| r[5] == "Autre"));
    // the order is lon,lat: the same numbers swapped hold nothing
    assert!(list(&bin, &["--bbox", "43.79,7.49,43.82,7.52"]).is_empty());
}

#[test]
fn a_polygon_file_keeps_its_inside() {
    let (dir, bin) = sheet("polygon");
    let file = dir.join("zone.geojson");
    std::fs::write(
        &file,
        r#"{"type":"Feature","properties":{},"geometry":{"type":"Polygon","coordinates":[[[7.41,43.72],[7.425,43.72],[7.425,43.735],[7.41,43.735],[7.41,43.72]]]}}"#,
    )
    .unwrap();
    assert_eq!(list(&bin, &["--polygon", file.to_str().unwrap()]).len(), 3);
    let bad = dir.join("bad.geojson");
    std::fs::write(&bad, r#"{"type":"Point","coordinates":[7.4,43.7]}"#).unwrap();
    let out = run(&[
        "list",
        bin.to_str().unwrap(),
        "--polygon",
        bad.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
}

#[test]
fn every_option_given_must_hold() {
    let (_d, bin) = sheet("and");
    let rows = list(&bin, &["--city", "ville", "--postcode", "10001"]);
    assert_eq!(who(&rows), expect(&[("Long Chemin", "9", "Ville")]));
    assert!(list(&bin, &["--city", "autre", "--near", "43.73,7.42,50"]).is_empty());
}

#[test]
fn limit_stops_the_list() {
    let (_d, bin) = sheet("limit");
    assert_eq!(list(&bin, &["--city", "ville", "--limit", "2"]).len(), 2);
}

#[test]
fn export_all_writes_every_house_with_the_licence_and_leaves_no_temp() {
    let (dir, bin) = sheet("export");
    let output = dir.join("all.csv");
    let out = run(&[
        "export",
        bin.to_str().unwrap(),
        "--all",
        "-o",
        output.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&output).unwrap();
    assert!(
        text.starts_with("# license: CC BY 4.0\n# attribution: Fixture & Co\n"),
        "{text}"
    );
    assert_eq!(table(&text).len(), HOUSES);
    let names: Vec<String> = std::fs::read_dir(&*dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!names.iter().any(|n| n.contains(".tmp")), "{names:?}");
}

#[test]
fn export_asks_for_an_area_or_all_but_not_both() {
    let (dir, bin) = sheet("export-args");
    let output = dir.join("o.csv");
    let o = output.to_str().unwrap();
    assert!(!run(&["export", bin.to_str().unwrap(), "-o", o])
        .status
        .success());
    assert!(!run(&[
        "export",
        bin.to_str().unwrap(),
        "--all",
        "--city",
        "ville",
        "-o",
        o
    ])
    .status
    .success());
    let out = run(&["export", bin.to_str().unwrap(), "--region", "x", "-o", o]);
    assert!(!out.status.success(), "no polygon file beside the sheet");
    assert!(String::from_utf8_lossy(&out.stderr).contains("_admin.bin"));
    assert!(!output.exists());
}

#[test]
fn a_region_comes_from_the_polygon_file_beside_the_sheet() {
    let (dir, bin) = sheet("region");
    // "WOFA", one region named "Sud" with a box and one square ring around the Autre houses
    let square: [(i32, i32); 5] = [
        (437_900_000, 74_900_000),
        (437_900_000, 75_200_000),
        (438_200_000, 75_200_000),
        (438_200_000, 74_900_000),
        (437_900_000, 74_900_000),
    ];
    let mut wofa = b"WOFA".to_vec();
    wofa.extend_from_slice(&1u32.to_le_bytes());
    wofa.push(3);
    wofa.extend_from_slice(b"Sud");
    for v in [437_900_000i32, 74_900_000, 438_200_000, 75_200_000] {
        wofa.extend_from_slice(&v.to_le_bytes());
    }
    wofa.extend_from_slice(&1u16.to_le_bytes());
    wofa.extend_from_slice(&(square.len() as u16).to_le_bytes());
    for (la, lo) in square {
        wofa.extend_from_slice(&la.to_le_bytes());
        wofa.extend_from_slice(&lo.to_le_bytes());
    }
    std::fs::write(dir.join("s_admin.bin"), &wofa).unwrap();
    let output = dir.join("sud.csv");
    let out = run(&[
        "export",
        bin.to_str().unwrap(),
        "--region",
        "sud",
        "-o",
        output.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows = table(&std::fs::read_to_string(&output).unwrap());
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter().all(|r| r[5] == "Autre" && r[6] == "Sud"),
        "{rows:?}"
    );
    let out = run(&[
        "export",
        bin.to_str().unwrap(),
        "--region",
        "nord",
        "-o",
        output.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Sud"),
        "names what there is"
    );
}

#[test]
fn every_format_opens_with_the_licence_and_holds_the_same_addresses() {
    let (dir, bin) = sheet("formats");
    for (name, count) in [
        ("x.jsonl", |t: &str| t.lines().count() - 1),
        ("x.geojsonl", |t: &str| t.lines().count() - 1),
        ("x.gpx", |t: &str| t.matches("<wpt ").count()),
    ] as [(&str, fn(&str) -> usize); 3]
    {
        let output = dir.join(name);
        let out = run(&[
            "export",
            bin.to_str().unwrap(),
            "--all",
            "-o",
            output.to_str().unwrap(),
        ]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = std::fs::read_to_string(&output).unwrap();
        assert!(text.contains("CC BY 4.0"), "{name}: {text}");
        assert!(text.contains("Fixture &"), "{name}: {text}");
        assert_eq!(count(&text), HOUSES, "{name}");
    }
    let output = dir.join("x.geojson");
    assert!(run(&[
        "export",
        bin.to_str().unwrap(),
        "--all",
        "-o",
        output.to_str().unwrap()
    ])
    .status
    .success());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(v["features"].as_array().unwrap().len(), HOUSES);
    assert_eq!(v["license"], "CC BY 4.0");
}

#[test]
fn a_sample_is_real_rows_and_the_same_seed_gives_the_same_bytes() {
    let (_d, bin) = sheet("sample");
    let args = ["sample", bin.to_str().unwrap(), "--n", "3", "--seed", "42"];
    let (a, b) = (run(&args), run(&args));
    assert!(a.status.success());
    assert_eq!(a.stdout, b.stdout, "the same seed, the same sample");
    let rows = table(&String::from_utf8(a.stdout.clone()).unwrap());
    assert_eq!(rows.len(), 3);
    for r in &rows {
        let row = format!("{},{}", r[3], r[1]);
        assert!(
            ROWS.lines()
                .any(|l| l.contains(&row[..row.find(',').unwrap()])),
            "{r:?}"
        );
    }
    let other: Vec<Vec<u8>> = (0..8)
        .map(|s| {
            run(&[
                "sample",
                bin.to_str().unwrap(),
                "--n",
                "3",
                "--seed",
                &s.to_string(),
            ])
            .stdout
        })
        .collect();
    assert!(
        other.iter().any(|o| *o != a.stdout),
        "another seed, another sample"
    );
}

/// The licence lines a sample carries do not stop the commands that read tables: the output of `sample` is the input
/// of `batch` and `nearest` as it is (found 08.11.2026 while writing the function reference).
#[test]
fn a_sample_with_its_licence_lines_is_read_by_batch_and_nearest() {
    let (d, bin) = sheet("sample-into-batch");
    let sample = d.join("sample.csv");
    let out = run(&[
        "sample",
        bin.to_str().unwrap(),
        "--n",
        "3",
        "--seed",
        "7",
        "-o",
        sample.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert!(std::fs::read_to_string(&sample)
        .unwrap()
        .starts_with("# license: "));
    let answered = d.join("answered.csv");
    let out = run(&[
        "batch",
        bin.to_str().unwrap(),
        sample.to_str().unwrap(),
        answered.to_str().unwrap(),
        "--column",
        "street,housenumber,commune",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&answered).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("lat,lon,street,"), "{}", lines[0]);
    assert_eq!(lines.len(), 4, "{text}");
    let near = run(&[
        "nearest",
        sample.to_str().unwrap(),
        sample.to_str().unwrap(),
    ]);
    assert!(
        near.status.success(),
        "{}",
        String::from_utf8_lossy(&near.stderr)
    );
    assert_eq!(String::from_utf8(near.stdout).unwrap().lines().count(), 4);
}

#[test]
fn a_sample_larger_than_the_area_is_the_area_and_takes_its_filter() {
    let (_d, bin) = sheet("sample-all");
    let out = run(&[
        "sample",
        bin.to_str().unwrap(),
        "--n",
        "100",
        "--seed",
        "1",
        "--city",
        "autre",
    ]);
    assert!(out.status.success());
    let rows = table(&String::from_utf8(out.stdout).unwrap());
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r[5] == "Autre"));
}

#[test]
fn every_address_has_the_same_chance_of_being_sampled() {
    // 8 houses, 2 picked, 400 seeds: each house expected 100 times; a reservoir that favours the head
    // or the tail of the walk leaves this band.
    let (_d, bin) = sheet("sample-fair");
    let mut seen = std::collections::HashMap::new();
    for seed in 0..400 {
        let out = run(&[
            "sample",
            bin.to_str().unwrap(),
            "--n",
            "2",
            "--seed",
            &seed.to_string(),
        ]);
        for r in table(&String::from_utf8(out.stdout).unwrap()) {
            *seen
                .entry((r[2].clone(), r[3].clone(), r[5].clone()))
                .or_insert(0u32) += 1;
        }
    }
    assert_eq!(seen.len(), HOUSES);
    assert!(seen.values().all(|&n| (60..=140).contains(&n)), "{seen:?}");
}

#[test]
fn stats_counts_the_houses_of_each_cell() {
    let (dir, bin) = sheet("stats");
    let out = run(&["stats", bin.to_str().unwrap()]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let mut lines = text.lines();
    assert!(lines.next().unwrap().starts_with("# license: CC BY 4.0"));
    lines.next();
    assert_eq!(lines.next().unwrap(), "lat,lon,houses");
    let cells: Vec<&str> = lines.collect();
    let total: u32 = cells
        .iter()
        .map(|l| l.rsplit(',').next().unwrap().parse::<u32>().unwrap())
        .sum();
    assert_eq!(total as usize, HOUSES);
    // cells are the sheet's own: the floor of (lat + 90) / 0.01 in floating point, as index::cell_of has it
    assert!(cells.contains(&"43.73,7.42,2"), "{cells:?}");
    assert!(cells.contains(&"43.80,7.50,2"), "{cells:?}");
    let output = dir.join("cells.geojson");
    assert!(run(&[
        "stats",
        bin.to_str().unwrap(),
        "-o",
        output.to_str().unwrap()
    ])
    .status
    .success());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(v["features"].as_array().unwrap().len(), cells.len());
    assert_eq!(v["features"][0]["geometry"]["type"], "Polygon");
    let out = run(&["stats", bin.to_str().unwrap(), "--format", "gpx"]);
    assert!(!out.status.success());
}

#[test]
fn range_points_are_not_addresses() {
    // a range point (house suffix "~", prep/us_tiger.py) stays out of every table and is counted aloud
    let dir = Scratch::dir("houses-range");
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!(
            "{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\nrue a,001,ville,10000,9,~,7.43,43.74,Rue A,Ville\n"
        ),
    )
    .unwrap();
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        r#"{"country":"mc","layer":"addresses","license":"t","source_release":"t"}"#,
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
    let out = run(&["list", bin.to_str().unwrap(), "--city", "ville"]);
    assert!(out.status.success());
    assert_eq!(table(&String::from_utf8(out.stdout).unwrap()).len(), 1);
    assert!(String::from_utf8_lossy(&out.stderr).contains("1 points sampled from address ranges"));
}

#[test]
fn a_parquet_output_name_is_refused_rather_than_filled_with_csv() {
    let (dir, bin) = sheet("parquet");
    let output = dir.join("x.parquet");
    let out = run(&[
        "list",
        bin.to_str().unwrap(),
        "--city",
        "ville",
        "-o",
        output.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("DuckDB"),
        "{out:?}"
    );
    assert!(!output.exists());
}

#[test]
fn a_closed_pipe_is_not_an_error() {
    let (_d, bin) = sheet("pipe");
    let mut child = Command::new(BIN)
        .args(["list", bin.to_str().unwrap(), "--city", "ville"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // the walk did not finish, so there is no count to report
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("addresses"),
        "{out:?}"
    );
}

#[test]
fn postcodes_draw_an_outline_that_holds_every_house_of_its_code() {
    let dir = Scratch::dir("postcodes");
    let csv = dir.join("in.csv");
    let mut rows = String::from("nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n");
    for i in 0..12u32 {
        rows += &format!(
            "rue a,001,ville,10000,{},,{:.5},43.730,Rue A,Ville\n",
            i + 1,
            7.420 + 0.002 * f64::from(i)
        );
    }
    for i in 0..5u32 {
        rows += &format!(
            "rue b,002,bourg,20000,{},,7.600,{:.5},Rue B,Bourg\n",
            i + 1,
            43.800 + 0.002 * f64::from(i)
        );
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
    let out = dir.join("pc.geojson");
    assert!(Command::new(BIN)
        .args([
            "postcodes",
            bin.to_str().unwrap(),
            "-o",
            out.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    let fc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let f = fc["features"].as_array().unwrap();
    assert_eq!(f.len(), 2);
    let feature = |code: &str| {
        f.iter()
            .find(|x| x["properties"]["postcode"] == code)
            .unwrap()
    };
    for i in 0..12u32 {
        let lon = 7.420 + 0.002 * f64::from(i);
        assert!(
            gridpin::postcodes::holds(feature("10000"), 43.730, lon),
            "house {i} of 10000"
        );
        assert!(!gridpin::postcodes::holds(feature("20000"), 43.730, lon));
    }
    assert_eq!(feature("20000")["properties"]["addresses"], 5);
}

#[test]
fn slice_keeps_the_houses_of_the_box_answers_them_as_the_source_and_carries_its_passport() {
    let dir = Scratch::dir("slice");
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n\
         rue a,001,ville,10000,1,,7.420,43.730,Rue A,Ville\nrue a,001,ville,10000,3,,7.421,43.731,Rue A,Ville\n\
         rue z,002,bourg,20000,1,,7.600,43.900,Rue Z,Bourg\n",
    )
    .unwrap();
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        r#"{"country":"mc","layer":"addresses","license":"t","source_release":"test"}"#,
    )
    .unwrap();
    let bin = dir.join("s.bin");
    let rules = dir.join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    // a rule of the source: an abbreviation the cut must still read
    std::fs::write(rules.join("abbrev2.tsv"), "zz\tqq\true a\n").unwrap();
    let ok = |args: &[&str]| {
        let o = Command::new(BIN).args(args).output().unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap()
    };
    ok(&[
        "build",
        csv.to_str().unwrap(),
        bin.to_str().unwrap(),
        "--meta",
        man.to_str().unwrap(),
        "--rules",
        rules.to_str().unwrap(),
    ]);
    let part = dir.join("part.bin");
    ok(&[
        "slice",
        bin.to_str().unwrap(),
        "--bbox",
        "7.41,43.72,7.43,43.74",
        "-o",
        part.to_str().unwrap(),
    ]);
    let (s, p) = (bin.to_str().unwrap(), part.to_str().unwrap());
    // the source reads "zz qq" as "rue a" by its own rule; the cut keeps the rule
    assert!(ok(&["query", s, "zz qq 3 ville"]).contains("\"housenumber\":\"3\""));
    for q in ["rue a 1 ville", "rue a 3 ville", "zz qq 3 ville"] {
        assert_eq!(ok(&["query", s, q]), ok(&["query", p, q]), "{q}");
    }
    assert!(
        ok(&["query", p, "rue z 1 bourg"]).trim().is_empty()
            || !ok(&["query", p, "rue z 1 bourg"]).contains("Rue Z")
    );
    let meta = ok(&["meta", p]);
    assert!(
        meta.contains("derived_from:") && meta.contains("slice: bbox 7.41,43.72,7.43,43.74"),
        "{meta}"
    );
}
