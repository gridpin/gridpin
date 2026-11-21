//! A sheet of one's own places: `gridpin build --places` and `--also` on query, batch and serve, through the built
//! binary. The town has Main Street (numbers 1-10 along 43.7300) and Library Road; the own sheet has a campus with
//! "Main Library" (alias "Old Library", code "B12") and "Hall Seven".
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn ok(out: std::process::Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The town sheet and the own sheet.
fn sheets(name: &str) -> (Scratch, PathBuf, PathBuf) {
    let dir = Scratch::dir(&format!("places-{name}"));
    let mut rows = String::new();
    for n in 1..=10 {
        rows += &format!(
            "library road,001,ville,10000,{n},,{:.7},43.7400000,Library Road,Ville\n",
            7.42 + n as f64 * 0.000125
        );
    }
    for n in 1..=10 {
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
    let town = dir.join("town.bin");
    let (input, meta) = (dir.join("in.csv"), dir.join("m.json"));
    ok(run(&[
        "build",
        input.to_str().unwrap(),
        town.to_str().unwrap(),
        "--meta",
        meta.to_str().unwrap(),
    ]));
    std::fs::write(
        dir.join("campus.csv"),
        "name,lat,lon,group,code,aliases\n\
         Main Library,43.7500,7.4300,North Campus,B12,Old Library\n\
         Hall Seven,43.7510,7.4310,North Campus,,\n\
         Main Street Hall,43.7520,7.4320,North Campus,,\n\
         Student Reception and Welcome Desk,43.7530,7.4330,North Campus,,\n",
    )
    .unwrap();
    let own = dir.join("campus.bin");
    let built = run(&[
        "build",
        "--places",
        dir.join("campus.csv").to_str().unwrap(),
        own.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() && stderr.contains("4 places"),
        "{stderr}"
    );
    (dir, town, own)
}

fn query(town: &Path, q: &str, extra: &[&str]) -> Option<serde_json::Value> {
    let mut args = vec!["query", town.to_str().unwrap(), q, "-k", "1"];
    args.extend_from_slice(extra);
    ok(run(&args))
        .lines()
        .next()
        .map(|l| serde_json::from_str(l).unwrap())
}

fn own_flag(h: &serde_json::Value) -> bool {
    h["flags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f == "own_sheet")
}

#[test]
fn a_place_is_answered_by_name_code_alias_and_a_near_spelling() {
    let (_d, town, own) = sheets("names");
    let also = ["--also", own.to_str().unwrap()];
    for q in [
        "Main Library",
        "main library",
        "B12",
        "Old Library",
        "Main Librray",
        "Main Library, Somewhere Town",
    ] {
        let h = query(&town, q, &also).unwrap_or_else(|| panic!("{q}: no answer"));
        assert!(own_flag(&h), "{q}: {h}");
        assert_eq!(h["street"], "Main Library", "{q}");
        assert_eq!(
            (h["lat"].as_f64().unwrap(), h["lon"].as_f64().unwrap()),
            (43.75, 7.43),
            "{q}"
        );
    }
    let exact = query(&town, "Main Library", &also).unwrap();
    let fuzzy = query(&town, "Main Librray", &also).unwrap();
    assert_eq!(
        (exact["confidence"].as_f64(), exact["weak"].as_bool()),
        (Some(0.9), Some(false))
    );
    assert_eq!(
        (fuzzy["confidence"].as_f64(), fuzzy["weak"].as_bool()),
        (Some(0.55), Some(true)),
        "a person confirms a typo"
    );
    assert_eq!(
        query(&town, "Hall Seven", &also).unwrap()["street"],
        "Hall Seven"
    );
    // a town the own sheet does not know is dropped: the whole line finds nothing there
    let far_town =
        query(&town, "Hall Seven, Saint-Martin-d'Hères", &also).expect("the town is dropped");
    assert!(
        own_flag(&far_town) && far_town["street"] == "Hall Seven",
        "{far_town}"
    );
    // the own sheet is asked first even where the country's sheet has an answer of its own
    let plain = query(&town, "Main Street Hall", &[]).unwrap();
    assert_eq!(
        plain["street"], "Main Street",
        "the town alone reads it as the street"
    );
    assert_eq!(
        query(&town, "Main Street Hall", &also).unwrap()["street"],
        "Main Street Hall"
    );
}

#[test]
fn an_address_and_one_word_of_a_name_go_the_old_way() {
    let (_d, town, own) = sheets("addresses");
    let also = ["--also", own.to_str().unwrap()];
    // one word of a long name is not the name: "Reception" is not "Student Reception and Welcome Desk"
    for q in [
        "5 Main Street, Ville",
        "3 Library Road, Ville",
        "the library, Ville",
        "Library",
        "Reception, North Campus",
        "Library, North Campus",
    ] {
        assert_eq!(query(&town, q, &also), query(&town, q, &[]), "{q}");
    }
    assert!(
        query(&town, "Main Library", &[]).is_none_or(|h| !own_flag(&h)),
        "without --also the own sheet is not asked"
    );
}

#[test]
fn batch_takes_the_own_sheet_too() {
    let (dir, town, own) = sheets("surfaces");
    let input = dir.join("q.jsonl");
    std::fs::write(
        &input,
        "{\"q\": \"Old Library\"}\n{\"q\": \"5 Main Street, Ville\"}\n",
    )
    .unwrap();
    let out = dir.join("out.jsonl");
    ok(run(&[
        "batch",
        town.to_str().unwrap(),
        input.to_str().unwrap(),
        out.to_str().unwrap(),
        "-k",
        "1",
        "--also",
        own.to_str().unwrap(),
    ]));
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        own_flag(&lines[0]["results"][0]) && lines[0]["results"][0]["street"] == "Main Library"
    );
    assert_eq!(lines[1]["results"][0]["housenumber"], "5");
    assert!(!own_flag(&lines[1]["results"][0]));
    // an address sheet is not an own sheet: the wrong file is an error naming the flag
    let wrong = run(&[
        "query",
        town.to_str().unwrap(),
        "x",
        "--also",
        town.to_str().unwrap(),
    ]);
    assert!(!wrong.status.success() && String::from_utf8_lossy(&wrong.stderr).contains("--also"));
}

#[test]
fn a_bad_table_names_its_line() {
    let dir = Scratch::dir("places-bad");
    for (body, want) in [
        ("name,lat,lon\nA,1,2\na,3,4\n", "line 3"),
        ("name,lat,lon\nA,95,2\n", "line 2"),
        ("name,lat\nA,1\n", "name, lat, lon"),
        ("name,lat,lon\n,1,2\n", "no name"),
    ] {
        let input = dir.join("t.csv");
        std::fs::write(&input, body).unwrap();
        let out = run(&[
            "build",
            "--places",
            input.to_str().unwrap(),
            dir.join("t.bin").to_str().unwrap(),
        ]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && err.contains(want),
            "{body:?}: {err}"
        );
    }
}

#[test]
fn the_server_takes_the_own_sheet_too() {
    let (_d, town, own) = sheets("serve");
    let mut child = Command::new(BIN)
        .args([
            "serve",
            town.to_str().unwrap(),
            "--also",
            own.to_str().unwrap(),
            "--port",
            "0",
            "--threads",
            "2",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    // the server dies with the test even when a request below panics (09.11.2026, see gridpin-pro/tests/cli.rs)
    let server = KillOnDrop(child);
    let addr = line
        .trim()
        .strip_prefix("listening on http://")
        .unwrap()
        .to_string();
    let get = |target: &str| -> serde_json::Value {
        let mut s = TcpStream::connect(&addr).unwrap();
        s.write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .unwrap();
        let mut text = String::new();
        s.read_to_string(&mut text).unwrap();
        serde_json::from_str(text.split_once("\r\n\r\n").unwrap().1).unwrap()
    };
    let place = get("/v1/geocode?q=B12");
    let address = get("/v1/geocode?q=5+Main+Street,+Ville");
    drop(server);
    assert!(
        own_flag(&place["results"][0]) && place["results"][0]["street"] == "Main Library",
        "{place}"
    );
    assert!(!own_flag(&address["results"][0]), "{address}");
}

/// A world sheet of towns beside the town sheet: Lyon, Springfield twice (two regions), and a town named as a
/// street of the sheet ("Main Street"), which must never take that street's houses.
fn world(dir: &std::path::Path) -> PathBuf {
    std::fs::write(
        dir.join("world.csv"),
        "name,lat,lon,group,code,aliases\n\
         Lyon,45.7600,4.8400,FR-ARA,,Lione\n\
         Springfield,39.8000,-89.6400,US-IL,,\n\
         Springfield,42.1000,-72.5900,US-MA,,\n\
         Springfield,42.1000,-72.5900,US-MA,,\n\
         Main Street,10.0000,10.0000,ZZ-01,,\n\
         Paris,28.0000,84.0000,NP-P7,,\n\
         Gaun 1,28.0001,84.1000,NP-P7,,\n\
         Gaun 2,28.0002,84.1000,NP-P7,,\n\
         Gaun 3,28.0003,84.1000,NP-P7,,\n\
         Gaun 4,28.0004,84.1000,NP-P7,,\n\
         Gaun 5,28.0005,84.1000,NP-P7,,\n\
         Gaun 6,28.0006,84.1000,NP-P7,,\n\
         Gaun 7,28.0007,84.1000,NP-P7,,\n\
         Gaun 8,28.0008,84.1000,NP-P7,,\n\
         Paris,48.8566,2.3522,FR-IDF,,Parigi|Parijs|Paryż\n\
         Berlin,54.0000,61.0000,RU-CHE,,\n\
         Berlin,52.5200,13.4050,DE-BE,,Berlín|Berlino|Berlijn\n",
    )
    .unwrap();
    let out = dir.join("world.bin");
    ok(run(&[
        "build",
        "--places",
        "--first-wins",
        dir.join("world.csv").to_str().unwrap(),
        out.to_str().unwrap(),
    ]));
    out
}

#[test]
fn also_after_answers_a_town_the_sheet_lacks_and_never_takes_its_houses() {
    let (dir, town, _) = sheets("also-after");
    let w = world(&dir);
    let (t, w) = (town.to_str().unwrap(), w.to_str().unwrap());
    let lyon: serde_json::Value =
        serde_json::from_str(ok(run(&["query", t, "Lyon", "--also-after", w])).trim()).unwrap();
    assert_eq!(
        (lyon["street"].as_str(), lyon["precision"].as_str()),
        (Some("Lyon"), Some("city"))
    );
    assert!(lyon["flags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f == "world_place"));
    let two = ok(run(&[
        "query",
        t,
        "Springfield",
        "-k",
        "5",
        "--also-after",
        w,
    ]));
    assert_eq!(
        two.lines().count(),
        2,
        "one per region; the duplicate row left out by --first-wins"
    );
    for q in ["main street 3 ville", "Main Street"] {
        let with = ok(run(&["query", t, q, "--also-after", w]));
        assert_eq!(
            with,
            ok(run(&["query", t, q])),
            "{q}: the sheet's own answer stands"
        );
    }
    // a name one letter off is not a town of the world sheet
    let off = run(&["query", t, "Lyom", "--also-after", w]);
    assert!(!String::from_utf8_lossy(&off.stdout).contains("world_place"));
    // namesakes: the town known by more names wins; a country at the end keeps only its towns (09.11.2026, §294:
    // "Paris" answered a village of Nepal, "Berlin" one of Russia, "Berlin, Germany" nothing)
    let region = |q: &str| {
        let out = ok(run(&["query", t, q, "-k", "1", "--also-after", w]));
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_default();
        v["commune"].as_str().unwrap_or("").to_string()
    };
    assert_eq!(region("Paris"), "FR-IDF");
    assert_eq!(region("Berlin"), "DE-BE");
    assert_eq!(region("Berlin, Germany"), "DE-BE");
    assert_eq!(region("Berlin, Deutschland"), "DE-BE");
    assert_eq!(
        region("Berlin, Russia"),
        "RU-CHE",
        "the country beats the more famous namesake"
    );
    // a town of a country the world sheet does not hold gets no answer, not the address sheet's own guess
    assert_eq!(
        ok(run(&[
            "query",
            t,
            "Toronto, Canada",
            "-k",
            "1",
            "--also-after",
            w
        ]))
        .trim(),
        ""
    );
}

/// A child process killed and reaped when the test ends, passed or failed.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Revision 0.9.0: `--with provenance` named the country's sheet and licence for an answer of another sheet.
#[test]
fn provenance_names_the_sheet_that_gave_the_answer() {
    let (dir, town, own) = sheets("provenance");
    let world_csv = dir.join("w.csv");
    std::fs::write(
        &world_csv,
        "name,lat,lon,group\nLyon,45.7600,4.8400,FR-ARA\n",
    )
    .unwrap();
    let world = dir.join("w.bin");
    ok(run(&[
        "build",
        "--places",
        world_csv.to_str().unwrap(),
        world.to_str().unwrap(),
        "--license",
        "ODbL-1.0 (test world)",
    ]));
    let (t, o, w) = (
        town.to_str().unwrap(),
        own.to_str().unwrap(),
        world.to_str().unwrap(),
    );
    let card = |q: &str| {
        let h = query(
            &town,
            q,
            &["--also", o, "--also-after", w, "--with", "provenance"],
        )
        .unwrap();
        (h["extras"]["provenance"].clone(), h["flags"].clone())
    };
    let (own_card, flags) = card("Old Library");
    assert!(flags.to_string().contains("own_sheet"), "{flags}");
    assert_eq!(own_card["license"], "the owner's own data", "{own_card}");
    assert_eq!(own_card["country"], "own", "{own_card}");
    let (world_card, flags) = card("Lyon");
    assert!(flags.to_string().contains("world_place"), "{flags}");
    assert_eq!(
        world_card["license"], "ODbL-1.0 (test world)",
        "{world_card}"
    );
    let (town_card, _) = card("5 Main Street, Ville");
    assert_eq!(town_card["license"], "t", "{town_card}");
    assert_eq!(town_card["country"], "mc", "{town_card}");
    // the three cards hash three files
    let sha = |c: &serde_json::Value| c["sha256"].as_str().unwrap().to_string();
    assert_ne!(sha(&own_card), sha(&town_card));
    assert_ne!(sha(&world_card), sha(&town_card));
    // CSV: the same card in gp_license
    let input = dir.join("q.csv");
    std::fs::write(&input, "id,address\n1,Lyon\n2,5 Main Street Ville\n").unwrap();
    let out = dir.join("o.csv");
    ok(run(&[
        "batch",
        t,
        input.to_str().unwrap(),
        out.to_str().unwrap(),
        "--also-after",
        w,
        "--with",
        "provenance",
    ]));
    let mut r = csv::Reader::from_path(&out).unwrap();
    let head = r.headers().unwrap().clone();
    let at = head.iter().position(|h| h == "gp_license").unwrap();
    let licences: Vec<String> = r.records().map(|x| x.unwrap()[at].to_string()).collect();
    assert_eq!(licences, ["ODbL-1.0 (test world)", "t"]);
}

/// Revision 0.9.0: `build --places same.csv same.csv` wrote the sheet over the user's table.
#[test]
fn build_places_never_writes_over_its_own_table() {
    let dir = Scratch::dir("places-same");
    let body = "name,lat,lon\nDepot,43.7,7.4\n";
    let table = dir.join("same.csv");
    std::fs::write(&table, body).unwrap();
    let link = dir.join("link.csv");
    std::fs::hard_link(&table, &link).unwrap();
    let helper = dir.join("t.places-input.csv");
    std::fs::write(&helper, body).unwrap();
    // A second hard link of the table is told apart only on unix (dev + ino, as `batch` does):
    // std has no stable file identity on Windows. There the sheet replaces the link by an atomic
    // rename, and the table itself must still come through untouched (v0.9.0 Windows CI).
    for (input, out, refused_everywhere) in [
        (&table, table.clone(), true),
        (&table, link.clone(), false),
        (&helper, dir.join("t.bin"), true),
    ] {
        let o = run(&[
            "build",
            "--places",
            input.to_str().unwrap(),
            out.to_str().unwrap(),
        ]);
        let err = String::from_utf8_lossy(&o.stderr);
        if refused_everywhere || cfg!(unix) {
            assert!(
                !o.status.success() && err.contains("destroy"),
                "{out:?}: {err}"
            );
        }
        assert_eq!(std::fs::read_to_string(input).unwrap(), body, "{out:?}");
    }
    // another name builds
    ok(run(&[
        "build",
        "--places",
        table.to_str().unwrap(),
        dir.join("ok.bin").to_str().unwrap(),
    ]));
}

/// A town sheet with Ville, its village Hameau 3 km east and a far town Autre, and a place layer with chapels of one
/// name: in Hameau, in Autre and, when `edge` is set, only one 8 km east of Ville instead of the one in Hameau.
fn town_and_chapels(name: &str, edge: bool) -> (Scratch, PathBuf, PathBuf) {
    let dir = Scratch::dir(&format!("chapels-{name}"));
    let mut rows = String::new();
    for (street, code, town, lon, lat) in [
        ("rue basse", "002", "hameau", 7.4572, 43.7300),
        ("rue haute", "001", "ville", 7.4200, 43.7300),
        ("rue neuve", "003", "autre", 8.4200, 43.7300),
    ] {
        // Autre is the big town, so the layer's ordinary choice among the namesakes is its chapel
        for n in 1..=(if town == "autre" { 60 } else { 5 }) {
            rows += &format!(
                "{street},{code},{town},10000,{n},,{:.7},{lat:.7},{street},{town}\n",
                lon + n as f64 * 0.0001
            );
        }
    }
    std::fs::write(dir.join("in.csv"), format!("{HDR}{rows}")).unwrap();
    std::fs::write(
        dir.join("m.json"),
        r#"{"country":"mc","layer":"addresses","license":"t","source_release":"t","attribution":"t"}"#,
    )
    .unwrap();
    let town = dir.join("town.bin");
    ok(run(&[
        "build",
        dir.join("in.csv").to_str().unwrap(),
        town.to_str().unwrap(),
        "--meta",
        dir.join("m.json").to_str().unwrap(),
    ]));
    let village = if edge {
        "chapelle sainte anne,P2,lisiere,,1,,7.5200000,43.7300000,Chapelle Sainte Anne,Lisiere\n"
    } else {
        "chapelle sainte anne,P2,hameau,,1,,7.4600000,43.7300000,Chapelle Sainte Anne,Hameau\n"
    };
    std::fs::write(
        dir.join("poi.csv"),
        // Autre is the layer's biggest town too, so the layer's ordinary choice among the namesakes is its chapel
        format!(
            "{HDR}{village}chapelle sainte anne,P3,autre,,1,,8.4250000,43.7300000,Chapelle Sainte Anne,Autre\n\
             ecole des pins,P3,autre,,1,,8.4260000,43.7310000,Ecole des Pins,Autre\n\
             gare d autre,P3,autre,,1,,8.4270000,43.7320000,Gare d Autre,Autre\n\
             musee du port,P3,autre,,1,,8.4280000,43.7330000,Musee du Port,Autre\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("pm.json"),
        r#"{"country":"mc","layer":"poi","license":"t","source_release":"t","attribution":"t"}"#,
    )
    .unwrap();
    let poi = dir.join("poi.bin");
    ok(run(&[
        "build",
        dir.join("poi.csv").to_str().unwrap(),
        poi.to_str().unwrap(),
        "--meta",
        dir.join("pm.json").to_str().unwrap(),
    ]));
    (dir, town, poi)
}

#[test]
fn a_place_in_a_village_of_the_named_town_is_found_and_a_namesake_beyond_reach_is_not() {
    // «Station Vinkeveen, De Ronde Venen»: the place is recorded under a village of the town the query names
    let (_dir, town, poi) = town_and_chapels("village", false);
    let h = query(
        &town,
        "Chapelle Sainte Anne Ville",
        &["--poi", poi.to_str().unwrap()],
    )
    .unwrap();
    assert_eq!(h["commune"], "Hameau", "{h}");
    // a tail that only begins like the town names no town
    let h = query(
        &town,
        "Chapelle Sainte Anne Vil",
        &["--poi", poi.to_str().unwrap()],
    )
    .unwrap();
    assert_eq!(h["commune"], "Autre", "{h}");
    // without the town the layer keeps its ordinary choice, the chapel of the big town far away
    let h = query(
        &town,
        "Chapelle Sainte Anne",
        &["--poi", poi.to_str().unwrap()],
    )
    .unwrap();
    assert_eq!(h["commune"], "Autre", "{h}");
    // the reach (5 km) is held by the measurement, not here: a namesake 8 km away is also the ordinary answer of the
    // layer in this fixture (at 15 km the sets lost 4 Italian rows, at 5 km none, 16.11.2026)
}

/// The town sheet of `town_and_chapels` (Ville, its village Hameau 3 km east, the far big town Autre) and a place layer
/// of the given records (display name, commune, longitude; all on the latitude of the towns).
fn town_and_layer(name: &str, records: &[(&str, &str, f64)]) -> (Scratch, PathBuf, PathBuf) {
    let (dir, town, _) = town_and_chapels(name, false);
    // Autre is the layer's biggest town, so the layer's ordinary choice among namesakes is there
    let mut all: Vec<(String, String, f64)> = vec![
        ("Ecole des Pins".into(), "Autre".into(), 8.426),
        ("Gare d Autre".into(), "Autre".into(), 8.427),
    ];
    all.extend(
        records
            .iter()
            .map(|(n, c, lon)| (n.to_string(), c.to_string(), *lon)),
    );
    all.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase())); // the builder takes rows in key order
    let mut rows = String::new();
    for (i, (shown, commune, lon)) in all.iter().enumerate() {
        rows += &format!(
            "{},P{i},{},,1,,{lon:.7},43.7300000,{shown},{commune}\n",
            shown.to_lowercase(),
            commune.to_lowercase()
        );
    }
    std::fs::write(dir.join("poi.csv"), format!("{HDR}{rows}")).unwrap();
    let poi = dir.join("poi.bin");
    let _ = std::fs::remove_file(&poi);
    ok(run(&[
        "build",
        dir.join("poi.csv").to_str().unwrap(),
        poi.to_str().unwrap(),
        "--meta",
        dir.join("pm.json").to_str().unwrap(),
    ]));
    (dir, town, poi)
}

fn layer_answer(records: &[(&str, &str, f64)], tag: &str, q: &str) -> serde_json::Value {
    let (_dir, town, poi) = town_and_layer(tag, records);
    query(&town, q, &["--poi", poi.to_str().unwrap()]).unwrap_or(serde_json::Value::Null)
}

fn by_words(h: &serde_json::Value) -> bool {
    h["flags"].to_string().contains("place_words")
}

#[test]
fn a_place_written_otherwise_is_found_by_its_telling_words_near_the_named_town() {
    // «chiesa di San Giacomo, Soragna» ↔ «Chiesa S. Giacomo Apostolo»: the record holds the query's telling words
    let campo = [("Pieve di San Lorenzo in Campo", "Hameau", 7.45)];
    let h = layer_answer(&campo, "words-found", "Chiesa di San Lorenzo Ville");
    assert!(h["commune"] == "Hameau" && by_words(&h), "{h}");
    // «chiesa di San Gaetano da Thiene, Serle» ↔ «Parrocchia di San Gaetano»: the shorter name holds half of them
    let plain = [("Pieve di San Lorenzo", "Hameau", 7.45)];
    let h = layer_answer(&plain, "words-half", "Chiesa di San Lorenzo in Campo Ville");
    assert!(h["commune"] == "Hameau" && by_words(&h), "{h}");
    // one of three is not enough («Instituto São José» is not «Escola José Anacleto Gomes»)
    let h = layer_answer(
        &plain,
        "words-third",
        "Chiesa di San Lorenzo in Campo Verde Ville",
    );
    assert!(!by_words(&h) && h["commune"] != "Hameau", "{h}");
    // a record of another kind with the same saint is not the church
    let cafe = [("Caffetteria San Lorenzo in Campo", "Hameau", 7.45)];
    let h = layer_answer(&cafe, "words-kind", "Chiesa di San Lorenzo Ville");
    assert!(!by_words(&h) && h["commune"] != "Hameau", "{h}");
}

#[test]
fn the_words_of_the_named_town_tell_no_place_apart() {
    // «Church In Arlington» is no answer to «Arlington Methodist Church (Arlington, Alabama), Arlington»
    let ville = [("Pieve di Ville", "Hameau", 7.45)];
    let h = layer_answer(&ville, "words-town", "Chiesa di San Rocco di Ville Ville");
    assert!(!by_words(&h) && h["commune"] != "Hameau", "{h}");
    // nor of the query («Fornaci romane di Lonato del Garda, Lonato del Garda» ↔ «Le Fornaci Romane»)
    let plain = [("Pieve di San Lorenzo", "Hameau", 7.45)];
    let h = layer_answer(
        &plain,
        "words-query-town",
        "Chiesa di San Lorenzo di Ville in Campo Ville",
    );
    assert!(h["commune"] == "Hameau" && by_words(&h), "{h}");
    // and a record that carries the town's name in its own is read without it
    let named = [("Pieve di San Lorenzo di Ville", "Hameau", 7.45)];
    let h = layer_answer(
        &named,
        "words-named",
        "Chiesa di San Lorenzo in Campo Ville",
    );
    assert!(h["commune"] == "Hameau" && by_words(&h), "{h}");
}

#[test]
fn two_qualifying_places_apart_an_exact_name_elsewhere_or_a_far_one_keep_the_words_out() {
    // two churches of the saint in two villages of the town: neither is taken
    let two = [
        ("Pieve di San Lorenzo in Campo", "Hameau", 7.45),
        ("Pieve di San Lorenzo in Campo", "Hameau", 7.432),
    ];
    let h = layer_answer(&two, "words-two", "Chiesa di San Lorenzo Ville");
    assert!(!by_words(&h) && h["commune"] != "Hameau", "{h}");
    // the layer knows the name exactly, far away: its answer stands (a big city whose place lies far from its point)
    let exact = [
        ("Pieve di San Lorenzo in Campo", "Hameau", 7.45),
        ("Chiesa di San Lorenzo", "Autre", 8.425),
    ];
    let h = layer_answer(&exact, "words-exact", "Chiesa di San Lorenzo Ville");
    assert!(h["commune"] == "Autre" && !by_words(&h), "{h}");
    // beyond the reach of the words (3 km) nothing is taken
    let far = [("Pieve di San Lorenzo in Campo", "Hameau", 7.47)];
    let h = layer_answer(&far, "words-reach", "Chiesa di San Lorenzo Ville");
    assert!(!by_words(&h) && h["commune"] != "Hameau", "{h}");
}

fn by_district(h: &serde_json::Value) -> bool {
    h["flags"].to_string().contains("place_district")
}

/// Three records of the layer name the quarter «Quartier Bas», a district the town sheet has no town for, around
/// 7.45 (between Ville and Hameau); the extra records are added to them.
fn district_answer(extra: &[(&str, &str, f64)], tag: &str, q: &str) -> serde_json::Value {
    let mut records = vec![
        ("Mairie Quartier Bas", "Ville", 7.449),
        ("Poste Quartier Bas", "Ville", 7.450),
        ("Ecole Quartier Bas", "Ville", 7.451),
    ];
    records.extend_from_slice(extra);
    layer_answer(&records, tag, q)
}

#[test]
fn a_place_in_a_district_the_sheet_does_not_know_is_found_near_the_records_naming_it() {
    // «Godesberger Stadthalle, Bad Godesberg»: the sheet knows Bonn only, and a namesake elsewhere won
    let lake = [
        ("Musee du Lac", "Ville", 7.452),
        ("Musee du Lac", "Autre", 8.428),
    ];
    let h = district_answer(&lake, "district-found", "Musee du Lac Quartier Bas");
    assert!(h["lon"].as_f64().unwrap() < 8.0 && by_district(&h), "{h}");
    // without the district the layer keeps its ordinary choice, the namesake in the big town
    let h = district_answer(&lake, "district-none", "Musee du Lac");
    assert!(h["lon"].as_f64().unwrap() > 8.0 && !by_district(&h), "{h}");
    // only a place of exactly that name is taken, not one written nearly so
    let longer = [
        ("Musee du Lacs", "Ville", 7.452),
        ("Musee du Lac", "Autre", 8.428),
    ];
    let h = district_answer(&longer, "district-exact", "Musee du Lac Quartier Bas");
    assert!(!by_district(&h) && h["lon"].as_f64().unwrap() > 8.0, "{h}");
    // beyond the reach of the district (3 km) nothing is taken
    let far = [
        ("Musee du Lac", "Ville", 7.495),
        ("Musee du Lac", "Autre", 8.428),
    ];
    let h = district_answer(&far, "district-reach", "Musee du Lac Quartier Bas");
    assert!(!by_district(&h), "{h}");
}

#[test]
fn a_district_named_by_too_few_or_by_scattered_records_gives_no_point() {
    let lake = [
        ("Musee du Lac", "Ville", 7.452),
        ("Musee du Lac", "Autre", 8.428),
    ];
    // two records naming it are not enough
    let mut two = vec![
        ("Mairie Quartier Bas", "Ville", 7.449),
        ("Poste Quartier Bas", "Ville", 7.450),
    ];
    two.extend_from_slice(&lake);
    let h = layer_answer(&two, "district-two", "Musee du Lac Quartier Bas");
    assert!(!by_district(&h), "{h}");
    // four records of which one lies in the far town: three of four within 5 km is below the 80 % the point needs
    let scattered = [
        ("Musee du Lac", "Ville", 7.452),
        ("Musee du Lac", "Autre", 8.428),
        ("Gare Quartier Bas", "Autre", 8.429),
    ];
    let h = district_answer(
        &scattered,
        "district-scattered",
        "Musee du Lac Quartier Bas",
    );
    assert!(!by_district(&h), "{h}");
    // a word of two letters is no word of the district: «6e» does not narrow «arrondissement»
    let short = [
        ("Musee du Lac", "Ville", 7.452),
        ("Musee du Lac", "Autre", 8.428),
    ];
    let h = district_answer(&short, "district-short", "Musee du Lac Le Quartier Bas");
    assert!(by_district(&h), "{h}");
}

#[test]
fn a_place_already_in_the_district_or_known_exactly_in_the_named_town_keeps_its_answer() {
    // the ordinary answer already lies in the district: it stands, unflagged
    let only = [("Musee du Lac", "Ville", 7.452)];
    let h = district_answer(&only, "district-ordinary", "Musee du Lac Quartier Bas");
    assert!(h["lon"].as_f64().unwrap() < 8.0 && !by_district(&h), "{h}");
    // the layer knows the name exactly in the town the query names («Van der Valk Hotel Eindhoven»): a tail reaching
    // into the name makes no district of it
    let named = [
        ("Musee du Lac", "Ville", 7.452),
        ("Musee du Lac", "Quartier Bas", 8.428),
    ];
    let h = district_answer(&named, "district-named", "Musee du Lac Quartier Bas");
    assert!(h["lon"].as_f64().unwrap() > 8.0 && !by_district(&h), "{h}");
}
