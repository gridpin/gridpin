//! Publishing without points: `batch --aggregate` and `batch --mask`, through the built binary. The town has
//! Main Street (numbers 1-40, 10 m apart along 43.7300) and Far Road 20 km north (numbers 1-3).
use std::path::{Path, PathBuf};
use std::process::Command;

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

fn town(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("publish-{name}"));
    let mut rows = String::new();
    for n in 1..=3 {
        rows += &format!(
            "far road,001,ville,10000,{n},,{:.7},43.9100000,Far Road,Ville\n",
            7.42 + n as f64 * 0.000125
        );
    }
    for n in 1..=40 {
        rows += &format!(
            "main street,001,ville,10000,{n},,{:.7},43.7300000,Main Street,Ville\n",
            7.42 + n as f64 * 0.000125
        );
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

/// 20 addresses: 12 on Main Street, 3 on Far Road, 5 that do not exist.
fn input(dir: &Path) -> PathBuf {
    let mut csv = String::from("id,address\n");
    for n in 1..=12 {
        csv += &format!("{n},\"{n} Main Street, Ville\"\n");
    }
    for n in 1..=3 {
        csv += &format!("f{n},\"{n} Far Road, Ville\"\n");
    }
    for n in 1..=5 {
        csv += &format!("x{n},\"{n} Nowhere Lane, Atlantis\"\n");
    }
    let p = dir.join("addresses.csv");
    std::fs::write(&p, csv).unwrap();
    p
}

fn batch(bin: &Path, input: &Path, out: &Path, extra: &[&str]) -> std::process::Output {
    let mut args = vec![
        "batch",
        bin.to_str().unwrap(),
        input.to_str().unwrap(),
        out.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    Command::new(BIN).args(&args).output().unwrap()
}

fn rows(path: &Path) -> Vec<Vec<String>> {
    let mut r = csv::Reader::from_path(path).unwrap();
    let mut out = vec![r
        .headers()
        .unwrap()
        .iter()
        .map(String::from)
        .collect::<Vec<_>>()];
    out.extend(
        r.records()
            .map(|x| x.unwrap().iter().map(String::from).collect()),
    );
    out
}

#[test]
fn the_aggregate_writes_counts_and_no_point_and_small_cells_merge() {
    let (dir, bin) = town("aggregate");
    let input = input(&dir);
    let out = dir.join("cells.csv");
    let run = batch(&bin, &input, &out, &["--aggregate", "geohash:5"]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(
        !text.contains("gp_lat") && !text.contains("43.73") && !text.contains("Main Street"),
        "{text}"
    );
    let t = rows(&out);
    assert_eq!(t[0], ["gp_cell", "rows", "at_house", "weak", "cells"]);
    let main = gridpin::codes::geohash_encode(43.73, 7.4215, 5);
    assert_eq!(t[1], [main.as_str(), "12", "12", "0", "1"], "{t:?}");
    assert_eq!(
        t[2],
        ["other", "3", "3", "0", "1"],
        "Far Road has 3 rows, under the bar of 5"
    );
    assert_eq!(t[3][..2], ["no answer".to_string(), "5".to_string()]);
    // with a bar of 3 Far Road is a cell of its own
    assert!(batch(
        &bin,
        &input,
        &out,
        &["--aggregate", "geohash:5", "--min-count", "3"]
    )
    .status
    .success());
    assert_eq!(rows(&out).len(), 4, "two cells and no answer, no other");
}

#[test]
fn zones_name_the_cells() {
    let (dir, bin) = town("zones");
    let input = input(&dir);
    let zones = dir.join("z.geojson");
    std::fs::write(
        &zones,
        r#"{"type":"FeatureCollection","features":[
          {"type":"Feature","properties":{"name":"South"},"geometry":{"type":"Polygon","coordinates":[[[7.4,43.7],[7.5,43.7],[7.5,43.8],[7.4,43.8],[7.4,43.7]]]}}]}"#,
    )
    .unwrap();
    let out = dir.join("zones.csv");
    let z = zones.to_str().unwrap();
    assert!(
        batch(&bin, &input, &out, &["--aggregate", z, "--min-count", "1"])
            .status
            .success()
    );
    let t = rows(&out);
    let names: Vec<&str> = t[1..].iter().map(|r| r[0].as_str()).collect();
    assert_eq!(names, ["South", "outside the zones", "no answer"]);
    assert_eq!((t[1][1].as_str(), t[2][1].as_str()), ("12", "3"));
}

#[test]
fn the_mask_moves_every_point_within_the_ring_the_same_way_for_one_seed() {
    let (dir, bin) = town("mask");
    let input = input(&dir);
    let (a, b) = (dir.join("a.csv"), dir.join("b.csv"));
    for out in [&a, &b] {
        let run = batch(
            &bin,
            &input,
            out,
            &["--mask", "donut:200-500", "--seed", "42"],
        );
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
    assert_eq!(
        std::fs::read(&a).unwrap(),
        std::fs::read(&b).unwrap(),
        "one seed, one output"
    );
    let other = dir.join("c.csv");
    assert!(batch(
        &bin,
        &input,
        &other,
        &["--mask", "donut:200-500", "--seed", "43"]
    )
    .status
    .success());
    assert_ne!(std::fs::read(&a).unwrap(), std::fs::read(&other).unwrap());
    let t = rows(&a);
    let col = |name: &str| t[0].iter().position(|h| h == name).unwrap();
    let (lat, lon, moved, street, number) = (
        col("gp_lat"),
        col("gp_lon"),
        col("gp_masked_m"),
        col("gp_street"),
        col("gp_housenumber"),
    );
    let mut seen = 0;
    for (i, r) in t[1..].iter().enumerate() {
        if r[lat].is_empty() {
            continue;
        }
        seen += 1;
        let truth = if i < 12 {
            (43.73, 7.42 + (i + 1) as f64 * 0.000125)
        } else {
            (43.91, 7.42 + (i - 11) as f64 * 0.000125)
        };
        let d =
            gridpin::codes::haversine_m(truth, (r[lat].parse().unwrap(), r[lon].parse().unwrap()));
        assert!((195.0..=505.0).contains(&d), "row {i}: moved {d} m");
        let said: f64 = r[moved].parse().unwrap();
        assert!((said - d).abs() < 5.0, "row {i}: said {said}, moved {d}");
        assert!(
            r[street].is_empty() && r[number].is_empty(),
            "row {i}: the street and the number are left out"
        );
    }
    assert_eq!(seen, 15);
}

#[test]
fn with_k_the_move_reaches_k_addresses_of_the_sheet() {
    let (dir, bin) = town("mask-k");
    let input = input(&dir);
    let out = dir.join("k.csv");
    // a ring of 1-2 m alone would leave the point among its neighbours; k:30 pushes it past 30 addresses of
    // Main Street (10 m apart); Far Road has only 3 addresses, so its rows say how many it found
    let run = batch(
        &bin,
        &input,
        &out,
        &["--mask", "donut:1-2,k:30", "--seed", "1"],
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let t = rows(&out);
    let col = |name: &str| t[0].iter().position(|h| h == name).unwrap();
    let (moved, k) = (col("gp_masked_m"), col("gp_mask_k"));
    for r in &t[1..13] {
        assert!(r[moved].parse::<f64>().unwrap() > 100.0, "{r:?}");
        assert!(r[k].parse::<usize>().unwrap() >= 30, "{r:?}");
    }
    for r in &t[13..16] {
        assert_eq!(
            r[k], "3",
            "a sparse place says how many addresses it found: {r:?}"
        );
    }
}

#[test]
fn aggregate_and_mask_refuse_what_would_leak_or_mean_nothing() {
    let (dir, bin) = town("refuse");
    let input = input(&dir);
    let out = dir.join("o.csv");
    for (extra, want) in [
        (
            vec!["--aggregate", "h3:8", "--mask", "donut:1-2"],
            "cannot be used",
        ),
        (vec!["--aggregate", "h3:8", "--with", "h3"], "--with"),
        (vec!["--mask", "donut:1-2", "-k", "3"], "-k"),
        (vec!["--mask", "k:5"], "donut"),
        (vec!["--aggregate", "h3:99"], "0 to 15"),
    ] {
        let run = batch(&bin, &input, &out, &extra);
        let err = String::from_utf8_lossy(&run.stderr);
        assert!(
            !run.status.success() && err.contains(want),
            "{extra:?}: {err}"
        );
    }
    let jsonl = dir.join("q.jsonl");
    std::fs::write(&jsonl, "{\"q\": \"1 Main Street, Ville\"}\n").unwrap();
    let run = batch(&bin, &jsonl, &out, &["--aggregate", "h3:8"]);
    assert!(!run.status.success() && String::from_utf8_lossy(&run.stderr).contains("CSV/TSV"));
    // without the flags the table is the one of before: the point is there
    assert!(batch(&bin, &input, &out, &[]).status.success());
    assert!(std::fs::read_to_string(&out).unwrap().contains("43.73"));
}
