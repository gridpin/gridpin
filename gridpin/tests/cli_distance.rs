//! `gridpin distance`: pairs (row i with row i, or by `--key`) and the matrix, on files of points and on files
//! geocoded on a small sheet. The control distances come from the project's own formula
//! (haversine, radius 6 371 000 m, computed outside this code), not from the code under test.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

fn sheet(dir: &Path) -> PathBuf {
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!("{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\nrue b,001,ville,10000,2,,7.50,43.80,Rue B,Ville\n"),
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
    bin
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn text(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn write(dir: &Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p.to_str().unwrap().to_string()
}

#[test]
fn pairs_take_row_i_with_row_i_and_match_the_project_formula() {
    let dir = Scratch::dir("distance-pairs");
    let a = write(&dir, "a.csv", "name,lat,lon\nparis,48.8566,2.3522\nequator,0,0\nsydney,-33.8688,151.2093\nsame,43.73,7.42\nlast,1,1\n");
    let b = write(
        &dir,
        "b.csv",
        "id,lat,lon\nlon,51.5074,-0.1278\ne1,0,1\nnyc,40.7128,-74.0060\nsame,43.73,7.42\n",
    );
    let out = run(&["distance", &a, &b]);
    let t = text(&out);
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(
        lines[0],
        "name,lat,lon,distance_id,distance_m,distance_note"
    );
    assert_eq!(lines[1], "paris,48.8566,2.3522,lon,343556.1,");
    assert_eq!(lines[2], "equator,0,0,e1,111194.9,");
    assert_eq!(lines[3], "sydney,-33.8688,151.2093,nyc,15988755.5,");
    assert_eq!(lines[4], "same,43.73,7.42,same,0.0,");
    assert_eq!(
        lines[5],
        "last,1,1,,,no row with this number in the second file"
    );
    assert_eq!(lines.len(), 6);
}

#[test]
fn a_key_pairs_rows_that_share_it_whatever_their_order() {
    let dir = Scratch::dir("distance-key");
    let a = write(
        &dir,
        "a.csv",
        "shop,lat,lon\nB,0,0\nA,48.8566,2.3522\nC,1,1\n",
    );
    let b = write(&dir, "b.csv", "shop,lat,lon\nA,51.5074,-0.1278\nB,0,1\n");
    let t = text(&run(&[
        "distance",
        &a,
        &b,
        "--key",
        "shop",
        "--id-column",
        "shop",
    ]));
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(lines[1], "B,0,0,B,111194.9,");
    assert_eq!(lines[2], "A,48.8566,2.3522,A,343556.1,");
    assert_eq!(lines[3], "C,1,1,,,no row with this key in the second file");
    // a key that names two rows of the second file is refused, not paired with one of them by chance
    let twice = write(&dir, "b2.csv", "shop,lat,lon\nA,0,0\nA,1,1\n");
    let out = run(&["distance", &a, &twice, "--key", "shop"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("\"A\" names more than one row"));
}

#[test]
fn the_matrix_is_every_pair_in_long_form_and_has_a_limit() {
    let dir = Scratch::dir("distance-matrix");
    let a = write(&dir, "a.csv", "id,lat,lon\np,48.8566,2.3522\nq,0,0\n");
    let b = write(&dir, "b.csv", "id,lat,lon\nl,51.5074,-0.1278\ne,0,1\nx,,\n");
    let out = run(&["distance", &a, &b, "--matrix"]);
    let t = text(&out);
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(lines[0], "a_id,b_id,distance_m");
    assert!(lines.contains(&"p,l,343556.1"));
    assert!(lines.contains(&"q,e,111194.9"));
    assert_eq!(
        lines.len(),
        5,
        "4 pairs with a point, the row without one is left out: {t}"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("4 distances, 2 pairs without one"));
    let out = run(&["distance", &a, &b, "--matrix", "--max-pairs", "5"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("make 6 pairs, more than --max-pairs 5"));
    let out = run(&["distance", &a, &b, "--matrix", "--key", "id"]);
    assert!(!out.status.success(), "--key and --matrix together");
}

#[test]
fn a_weak_answer_gets_no_distance_and_says_why() {
    let dir = Scratch::dir("distance-weak");
    let sheet = sheet(&dir);
    let a = write(
        &dir,
        "a.csv",
        "address\n1 rue a ville\nrue a\nRue Zzz 1 Ville\n",
    );
    let b = write(
        &dir,
        "b.csv",
        "id,lat,lon\nx,43.73,7.43\ny,43.73,7.43\nz,43.73,7.43\n",
    );
    let out = run(&["distance", &a, &b, "--sheet", sheet.to_str().unwrap()]);
    let t = text(&out);
    let lines: Vec<&str> = t.lines().collect();
    // the house answer is placed; "rue a" is a weak street answer, the third has no answer at all
    let metres: f64 = lines[1].split(',').nth(2).unwrap().parse().unwrap();
    assert!((metres - 803.5).abs() < 0.11, "{}", lines[1]);
    assert_eq!(lines[2], "rue a,y,,weak answer in the first file");
    assert_eq!(lines[3], "Rue Zzz 1 Ville,z,,no point in the first file");
    assert!(String::from_utf8_lossy(&out.stderr)
        .contains("1 distances, 2 pairs without one (1 placed by a weak answer)"));
    // in the matrix the weak row loses all three of its pairs, and the last line says why
    let out = run(&[
        "distance",
        &a,
        &b,
        "--sheet",
        sheet.to_str().unwrap(),
        "--matrix",
    ]);
    assert_eq!(text(&out).lines().count(), 4);
    assert!(String::from_utf8_lossy(&out.stderr)
        .contains("3 distances, 6 pairs without one (3 placed by a weak answer)"));
}

#[test]
fn a_weak_answer_in_the_second_file_is_counted_in_the_matrix() {
    let dir = Scratch::dir("distance-weak-b");
    let sheet = sheet(&dir);
    let a = write(&dir, "a.csv", "id,lat,lon\nx,43.73,7.43\ny,43.74,7.43\n");
    let b = write(&dir, "b.csv", "address\n1 rue a ville\nrue a\n");
    let out = run(&[
        "distance",
        &a,
        &b,
        "--sheet",
        sheet.to_str().unwrap(),
        "--matrix",
    ]);
    assert_eq!(
        text(&out).lines().count(),
        3,
        "two pairs with the house, none with the weak street"
    );
    assert!(String::from_utf8_lossy(&out.stderr)
        .contains("2 distances, 2 pairs without one (2 placed by a weak answer)"));
}

#[test]
fn the_same_rows_give_the_same_distance_as_nearest() {
    // the neighbour path: `nearest -k 1` with a one-row list is the pair distance
    let dir = Scratch::dir("distance-nearest");
    let a = write(&dir, "a.csv", "id,lat,lon\np,48.8566,2.3522\n");
    let b = write(&dir, "b.csv", "id,lat,lon\nl,51.5074,-0.1278\n");
    let pair = text(&run(&["distance", &a, &b]));
    let near = text(&run(&["nearest", &a, &b]));
    let d_pair = pair
        .lines()
        .nth(1)
        .unwrap()
        .split(',')
        .nth(4)
        .unwrap()
        .to_string();
    let d_near = near
        .lines()
        .nth(1)
        .unwrap()
        .split(',')
        .nth(4)
        .unwrap()
        .to_string();
    assert_eq!(d_pair, d_near);
}
