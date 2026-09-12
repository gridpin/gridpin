//! Germany F3: live-pattern micro-index tests.
//!
//! Query/base pairs exercise realistic German address forms. The tiny synthetic
//! sheet isolates parser behavior from the production data pipeline; these tests
//! do not measure the quality or coverage of a released country sheet.

use gridpin::query::{Hit, Index};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// Explicit witness vocabulary: no dependency on the private release rule tree.
fn write_fixture_rules(dir: &Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    for (name, rows) in [
        (
            "city_alias.tsv",
            "de\tmunich\tmunchen\nde\tcologne\tkoln\nde\tnuremberg\tnurnberg\nde\tfrankfurt main\tfrankfurt am main\n",
        ),
        (
            "abbrev2.tsv",
            "de\ta\tm\tam main\nde\ta\td\tan der\nde\ta\trh\tam rhein\nde\ti\tbay\tin bayern\n",
        ),
        (
            "street_types_latin.tsv",
            "de\tstraße\nde\tstrasse\nde\tstr\nde\tweg\nde\tallee\nde\tplatz\nde\tgasse\nde\tdamm\nde\tufer\n",
        ),
        ("countries_mid.tsv", "de\tdeutschland\nde\tgermany\n"),
        ("countries_tail.tsv", "de\tdeutschland\nde\tgermany\n"),
        ("place_prefix.tsv", "de\tot\nde\tortsteil\n"),
        ("place_type_strip.tsv", "de\tot\nde\tortsteil\n"),
    ] {
        std::fs::write(dir.join(name), rows).unwrap();
    }
    dir.to_path_buf()
}

fn fixture_rules() -> &'static Path {
    static RULES: OnceLock<PathBuf> = OnceLock::new();
    RULES.get_or_init(|| {
        write_fixture_rules(
            &std::env::temp_dir()
                .join(format!("gridpin-de-f3-rule-fixture-{}", std::process::id())),
        )
    })
}

const HEADER: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,code_postal_display,numero,rep,lon,lat,nom_voie,nom_commune\n";

fn fixture_rows() -> Vec<String> {
    vec![
        // Q475708 / Q178619 — seam and leading-zero postcode.
        "augustusstraße,DR01,dresden,1067,01067,1,,13.739590,51.051800,Augustusstraße,Dresden",
        "st petersburger straße,DR01,dresden,1069,01069,24,a,13.737500,51.044100,St. Petersburger Straße,Dresden",
        // Q138888 — Str. abbreviation.
        "bahnhofstraße,RE01,regensburg,93047,93047,18,,12.099691,49.011766,Bahnhofstraße,Regensburg",
        // Q94670 — a. Rh.
        "rochusstraße,BI01,bingen am rhein,55411,55411,8,,7.899084,49.967476,Rochusstraße,Bingen am Rhein",
        // Q950 — ae/oe/ß plus range 29-33.
        "cacilienstraße,KO01,koln,50676,50676,29,33,6.95143889,50.93471389,Cäcilienstraße,Köln",
        // Q1954 — exact `gasse` must beat the lossy ss -> ß retry.
        "trankgasse,KO01,koln,50667,50667,11,,6.95805555,50.94250000,Trankgasse,Köln",
        // Q167195 — reverse ß -> ss source spelling.
        "charles de gaulle strasse,BO01,bonn,53113,53113,20,,7.130037,50.715512,Charles-de-Gaulle-Strasse,Bonn",
        // Q322769 — a. d.
        "bahnhof,GE01,geislingen an der steige,73312,73312,1,,9.842220,48.618900,Bahnhof,Geislingen an der Steige",
        // Q112903 / Q296838 — source literals omit commune/postcode.  The QID
        // labels are fixture-only partition keys and never appear in a query.
        "hauptstraße,Q296838,q296838,0,,17,12,10.184400,48.058200,Hauptstraße,Q296838 fixture",
        "hauptstraße,Q112903,q112903,0,,164,,9.251770,49.699600,Hauptstraße,Q112903 fixture",
        // Q1263900 — live base is `Krahnstraße 1 /2` without locality.
        "krahnstraße,Q1263900,q1263900,0,,1,12,8.04118000,52.27720000,Krahnstraße,Q1263900 fixture",
        // Q160385 — joined Allee and spaced suffix.
        "kirschallee,LO01,lobau,2708,02708,1,b,14.659400,51.100400,Kirschallee,Löbau",
        // Q4024 — Frankfurt (Oder), deliberately separate from Main.
        "logenstraße,FFO1,frankfurt oder,15230,15230,8,,14.55166667,52.34208333,Logenstraße,Frankfurt (Oder)",
        // Q1194100 — source has Alsterufer; query splits it.
        "alsterufer,HH01,hamburg,20354,20354,21,,9.99736111,53.56077778,Alsterufer,Hamburg",
        // Q50730 / Q44562 / Q478695 — Weg, Platz and Gasse split witnesses.
        "schulweg,HE02,helgoland,27498,27498,648,,7.88475000,54.18302778,Schulweg,Helgoland",
        "munsterplatz,UL01,ulm,89073,89073,1,,9.99250000,48.39861111,Münsterplatz,Ulm",
        "kartausergasse,NU01,nurnberg,90402,90402,1,,11.07555600,49.44833300,Kartäusergasse,Nürnberg",
        // Q16435 — parser-only Bavaria witness; public sheet remains 15/16.
        "osterwaldstraße,MU01,munchen,80805,80805,10,,11.59873889,48.16300000,Osterwaldstraße,München",
        // Q458915 — Nuremberg exonym, also parser-only because Bavaria is absent.
        "lessingstraße,NU01,nurnberg,90443,90443,6,,11.07444400,49.44555600,Lessingstraße,Nürnberg",
        // Q322286 — exact `ue` in Uelzen must not be treated as an umlaut retry.
        "friedensreich hundertwasser platz,UE01,uelzen,29525,29525,1,,10.55310000,52.96970000,Friedensreich-Hundertwasser-Platz,Uelzen",
        // Q1311392 — accepted B row `Binz - OT Prora` pattern.
        "proraer allee,BI02,binz,18609,18609,119,,13.56833333,54.44305556,Proraer Allee,Binz",
        // Q154996 — source type split, query joined.
        "spandauer damm,BE01,berlin,14059,14059,10,22,13.295833,52.521111,Spandauer Damm,Berlin",
        // Q162222 — Frankfurt am Main; never alias bare Frankfurt globally.
        "wilhelm epstein straße,FFM1,frankfurt am main,60431,60431,14,,8.65972222,50.13388889,Wilhelm-Epstein-Straße,Frankfurt am Main",
        // Q638130 — i. Bay. abbreviation; parser-only Bavaria witness.
        "am romerbad,WE01,weißenburg in bayern,91781,91781,17,a,10.95861944,49.03058611,Am Römerbad,Weißenburg in Bayern",
        // Q534914 / Q575202 / Q695267 — exact live country/prefix witnesses.
        "bertolt brecht platz,BE02,berlin,10117,10117,1,,13.38611111,52.52166694,Bertolt-Brecht-Platz,Berlin",
        "lausitzerstraße,BE03,berlin,10999,10999,10,,13.42858000,52.49738000,Lausitzerstraße,Berlin",
        "meyerhofstraße,HE01,heidelberg,69117,69117,1,,8.71031940,49.38480000,Meyerhofstraße,Heidelberg",
        // Frozen dirty-5k ordinal 788: exact recipient-prefix + Str. composition.
        "max dohrn straße,BE04,charlottenburg nord,10589,10589,5,,13.3017350,52.5311096,Max-Dohrn-Straße,Charlottenburg-Nord",
        // The live sheet also contains the lossy dropped-prefix competitor.
        "dohrnstraße,SH04,wesselburen,25764,25764,5,,8.9242217,54.2144121,Dohrnstraße,Wesselburen",
        // Independent clean-5k witnesses for exact three-field address selection.
        "bergheimer straße,HD02,heidelberg,69115,69115,147,,8.6840000,49.4070000,Bergheimer Straße,Heidelberg",
        "knobelsdorffallee,DE02,dessau roßlau,06847,06847,2,3,12.2330000,51.8380000,Knobelsdorffallee,Dessau-Roßlau",
        "guttin,RU02,dreschvitz,18573,18573,66,,13.3100000,54.4030000,Güttin,Dreschvitz",
        // Frozen independent clean-5k official long-form commune witnesses.  The
        // production source uses the official abbreviated commune spellings.
        "jacobistraße,BH01,bad homburg v d hohe,0,,37,,8.6102292,50.2224935,Jacobistraße,Bad Homburg v. d. Höhe",
        "auf der steinkaut,BH01,bad homburg v d hohe,0,,1,,8.6355595,50.2252517,Auf der Steinkaut,Bad Homburg v. d. Höhe",
        "dorotheenstraße,BH01,bad homburg v d hohe,0,,24,,8.6148267,50.2262365,Dorotheenstraße,Bad Homburg v. d. Höhe",
        "am wingertsberg,BH01,bad homburg v d hohe,0,,4,,8.6272261,50.2310019,Am Wingertsberg,Bad Homburg v. d. Höhe",
        "domplatz,LI01,limburg a d lahn,0,,2,,8.0667120,50.3887948,Domplatz,Limburg a. d. Lahn",
        "lispenhauser straße,RO01,rotenburg a d fulda,0,,41,,9.7711211,51.0233648,Lispenhäuser Straße,Rotenburg a. d. Fulda",
        // Commune-mismatch negative: a hard official alias must never borrow an
        // otherwise exact house from a different commune.
        "fremdweg,HS01,homburg saar,66424,66424,7,,7.3380000,49.3160000,Fremdweg,Homburg (Saar)",
        // Frozen independent clean-5k parenthetical-subaddress witnesses.
        // These coordinates are the exact base-sheet rows used by the offline
        // @150 analysis; Schramberg is deliberately retained as a >150 m
        // nonclaim, not counted among the five expected benchmark gains.
        "unter den eichen,PS01,schwarmstedt,0,,2,,9.6181178,52.6768132,Unter den Eichen,Schwarmstedt",
        "am markt,PS02,zeven stadt,0,,4,,9.2794954,53.2953853,Am Markt,\"Zeven, Stadt\"",
        "kopernikusstraße,PS03,aachen,0,,16,,6.0632200,50.7793188,Kopernikusstraße,Aachen",
        "universitatsstr,PS04,bochum,0,,105,,7.2274548,51.4709319,Universitätsstr.,Bochum",
        "kohlweg,PS05,saarbrucken,66123,66123,7,,7.0208505,49.2406745,Kohlweg,Saarbrücken",
        "bahnhofstraße,PS06,schramberg,0,,1,,8.3842066,48.2285506,Bahnhofstraße,Schramberg",
        // Exact source rows whose locality qualifiers are intentionally not
        // accepted by this pass's commune-core equality gate.
        "albertstraße,PS07,freiburg im breisgau,79104,79104,25,,7.8490000,48.0060000,Albertstraße,Freiburg im Breisgau",
        "marktplatz,PS08,weilheim an der teck,73235,73235,4,,9.5370000,48.6150000,Marktplatz,Weilheim an der Teck",
        // Exact street with no exact house: parenthetical cleanup must not
        // promote an interpolation/near/street result.
        "interpolationsweg,PS10,teststadt,12345,12345,1,,8.0000000,50.0000000,Interpolationsweg,Teststadt",
        "interpolationsweg,PS10,teststadt,12345,12345,9,,8.0010000,50.0010000,Interpolationsweg,Teststadt",
        // Q150928 — the live B row carries an actual D- postcode prefix.
        "strobelallee,DO01,dortmund,44139,44139,50,,7.45166700,51.49250000,Strobelallee,Dortmund",
        // Q1435538 — fused/spaced alphabetic house suffix.
        "altenbaustraße,BN01,bad neuenahr ahrweiler,53474,53474,12,a,7.09288000,50.54140000,Altenbaustraße,Bad Neuenahr-Ahrweiler",
        // Q176751 / Q828981 / Q450048 — exact spellings that must win over
        // over-broad ae/oe/ue/ss fallback candidates (Essen/Neuss/Neues).
        "messepl,ES01,essen,45131,45131,2,,6.99767000,51.43100000,Messepl.,Essen",
        "rheinstraße,NE01,neuss,41460,41460,3,,6.69217000,51.20120000,Rheinstraße,Neuss",
        "neues kloster,BS01,bad schussenried,88427,88427,1,,9.65858000,48.00730000,Neues Kloster,Bad Schussenried",
        // Q263645 — `Land` is part of a live municipality name, not removable
        // administrative noise.
        "friedensstraße,ML01,milower land,14715,14715,86,,12.31122222,52.51941667,Friedensstraße,Milower Land",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn build_fixture_with_rules(country: &str, tag: &str, rules: &Path) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gridpin-de-f3-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("addresses.csv");
    let mut rows = fixture_rows();
    rows.sort_by_key(|row| {
        let mut fields = row.split(',');
        (
            fields.next().unwrap().to_string(),
            fields.next().unwrap().to_string(),
        )
    });
    std::fs::write(&csv, format!("{HEADER}{}\n", rows.join("\n"))).unwrap();
    let manifest = dir.join("manifest.json");
    std::fs::write(
        &manifest,
        format!(
            r#"{{"country":"{country}","layer":"addresses","license":"test","source_release":"test"}}"#
        ),
    )
    .unwrap();
    let bin = dir.join("addresses.bin");
    gridpin::builder::build(&csv, &bin, None, None, Some(rules), None, Some(&manifest)).unwrap();
    bin
}

fn build_fixture(country: &str, tag: &str) -> PathBuf {
    build_fixture_with_rules(country, tag, fixture_rules())
}

fn build_official_commune_alias_target_absent_fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gridpin-de-official-commune-target-absent-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("addresses.csv");
    std::fs::write(
        &csv,
        format!(
            "{HEADER}domplatz,LI02,limburg weilburg,65549,65549,2,,8.0667,50.3888,Domplatz,Limburg-Weilburg\n"
        ),
    )
    .unwrap();
    let manifest = dir.join("manifest.json");
    std::fs::write(
        &manifest,
        r#"{"country":"de","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let rules = fixture_rules();
    let bin = dir.join("addresses.bin");
    gridpin::builder::build(&csv, &bin, None, None, Some(rules), None, Some(&manifest)).unwrap();
    bin
}

fn build_postcode_capital_prior_fixture(country: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gridpin-{tag}-postcode-capital-prior-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("addresses.csv");
    let mut rows = vec![
        "alphaweg,AN01,anchorstadt,10115,10115,1,,13.4000,52.5000,Alphaweg,Anchorstadt",
        "betaweg,AN01,anchorstadt,10115,10115,1,,13.4010,52.5010,Betaweg,Anchorstadt",
        "gammaweg,AN01,anchorstadt,10115,10115,1,,13.4020,52.5020,Gammaweg,Anchorstadt",
        "deltaweg,AN01,anchorstadt,10115,10115,1,,13.4030,52.5030,Deltaweg,Anchorstadt",
        "epsilonweg,AN01,anchorstadt,10115,10115,1,,13.4040,52.5040,Epsilonweg,Anchorstadt",
        "sicherweg,AN01,anchorstadt,0,,1,,13.4060,52.5060,Sicherweg,Anchorstadt",
        "sicherweg,FE01,fernstadt,0,,4,,9.7100,53.4700,Sicherweg,Fernstadt",
        "zielstraße,AN01,anchorstadt,0,,1,,13.4050,52.5050,Zielstraße,Anchorstadt",
        "zielstraße,FE01,fernstadt,12357,12357,35,,9.7000,53.4700,Zielstraße,Fernstadt",
    ];
    rows.sort_unstable();
    std::fs::write(&csv, format!("{HEADER}{}\n", rows.join("\n"))).unwrap();
    let manifest = dir.join("manifest.json");
    std::fs::write(
        &manifest,
        format!(
            r#"{{"country":"{country}","layer":"addresses","license":"test","source_release":"test"}}"#
        ),
    )
    .unwrap();
    let bin = dir.join("addresses.bin");
    gridpin::builder::build(&csv, &bin, None, None, None, None, Some(&manifest)).unwrap();
    bin
}

fn build_missing_postcode_comma_fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gridpin-de-missing-postcode-comma-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("addresses.csv");
    let mut rows = [
        "august sonntag straße,BR01,brandenburg,14770,14770,5,,12.5056039,52.4166629,August-Sonntag-Straße,Brandenburg",
        "burghof,BR01,brandenburg,14776,14776,9,,12.5668239,52.4154182,Burghof,Brandenburg",
        "hauptstraße,FE01,fehmarn,23769,23769,32,,11.1459396,54.4507021,Hauptstraße,Fehmarn",
        "kirchgasse,BE01,neukolln,12043,12043,5,,13.4455457,52.4753414,Kirchgasse,Neukölln",
    ];
    rows.sort_unstable();
    std::fs::write(&csv, format!("{HEADER}{}\n", rows.join("\n"))).unwrap();
    let manifest = dir.join("manifest.json");
    std::fs::write(
        &manifest,
        r#"{"country":"de","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let rules = fixture_rules();
    let bin = dir.join("addresses.bin");
    gridpin::builder::build(&csv, &bin, None, None, Some(rules), None, Some(&manifest)).unwrap();
    bin
}

fn build_retained_locality_fixture(country: &str, tag: &str, ambiguous: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gridpin-{tag}-retained-locality-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("addresses.csv");
    let mut rows: Vec<String> = vec![
        // Live BNetzA family: query b8f19a65... (`Hamburger Allee 2, 60486 Frankfurt`).
        // Extra streets make Zippendorf the fallback anchor before retained locality wins.
        "hamburger allee,ZI01,zippendorf,0,,2,,11.4465,53.5975,Hamburger Allee,Zippendorf",
        "alphaweg,ZI01,zippendorf,0,,1,,11.4466,53.5976,Alphaweg,Zippendorf",
        "betaweg,ZI01,zippendorf,0,,1,,11.4467,53.5977,Betaweg,Zippendorf",
        "gammaweg,ZI01,zippendorf,0,,1,,11.4468,53.5978,Gammaweg,Zippendorf",
        "deltaweg,ZI01,zippendorf,0,,1,,11.4469,53.5979,Deltaweg,Zippendorf",
        "hamburger allee,HA01,hannover landeshauptstadt,0,,2,,9.7433,52.3798,Hamburger Allee,Hannover Landeshauptstadt",
        "hamburger allee,WI01,wincheringen,0,,2,,6.4324,49.6147,Hamburger Allee,Wincheringen",
        "hamburger allee,HO01,holzwickede,0,,2,,7.6211,51.5011,Hamburger Allee,Holzwickede",
        "hamburger allee,FF01,frankfurt am main,0,,2,,8.6504,50.1145,Hamburger Allee,Frankfurt am Main",
        "oderweg,FO01,frankfurt am oder,0,,1,,14.5517,52.3421,Oderweg,Frankfurt am Oder",
        // Independent UBA family: query bfee507e... (`Brandstr. 21, 49393 Lohne`).
        "brandstraße,GA01,gardelegen hansestadt,0,,21,,11.1452,52.4593,Brandstraße,Gardelegen Hansestadt",
        "brandstraße,LO01,lohne oldenburg stadt,0,,21,,8.2118,52.6645,Brandstraße,\"Lohne (Oldenburg), Stadt\"",
        // Raw-before-variant negative: the orthographic lookup variant rewrites
        // `Muehlenweg ... Koeln` to `muhlenweg ... koln`, but retained locality must
        // remain the user's raw-normalized `koeln` and therefore cannot promote Köln.
        "muhlenweg,ZI01,zippendorf,0,,4,,11.4465,53.5975,Mühlenweg,Zippendorf",
        "muhlenweg,KO01,koln stadt,0,,4,,6.9570,50.9360,Mühlenweg,\"Köln, Stadt\"",
        // Negative: the discarded token `roge` alone must not manufacture a match for
        // `Roge Stadt`; the retained full locality is `Gross Roge` and must still reject it.
        "meierei,JU01,julchendorf,0,,9,,11.4465,53.5975,Meierei,Jülchendorf",
        "meierei,RS01,roge stadt,0,,9,,12.5100,53.7600,Meierei,Roge Stadt",
        "juweg eins,JU01,julchendorf,0,,1,,12.5001,53.7501,Juweg Eins,Jülchendorf",
        "juweg zwei,JU01,julchendorf,0,,1,,12.5002,53.7502,Juweg Zwei,Jülchendorf",
        "juweg drei,JU01,julchendorf,0,,1,,12.5003,53.7503,Juweg Drei,Jülchendorf",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    // Reproduce the live >40 commune-prefix cap: `Frankfurt am Main` exists, but the
    // ordinary full query cannot attach it before c2 removes the locality suffix.
    for ordinal in 0..45 {
        rows.push(format!(
            "dummyweg {ordinal},FA{ordinal:02},frankfurt aa{ordinal:02},0,,1,,8.0,50.0,Dummyweg {ordinal},Frankfurt Aa{ordinal:02}"
        ));
        rows.push(format!(
            "nebenweg {ordinal},LA{ordinal:02},lohne aa{ordinal:02},0,,1,,8.1,52.6,Nebenweg {ordinal},Lohne Aa{ordinal:02}"
        ));
        rows.push(format!(
            "rogeseitenweg {ordinal},RA{ordinal:02},roge aa{ordinal:02},0,,1,,12.6,53.7,Rogeseitenweg {ordinal},Roge Aa{ordinal:02}"
        ));
        rows.push(format!(
            "kolnseitenweg {ordinal},KA{ordinal:02},koln aa{ordinal:02},0,,1,,7.0,50.9,Kolnseitenweg {ordinal},Köln Aa{ordinal:02}"
        ));
    }
    if ambiguous {
        rows.push("hamburger allee,FO01,frankfurt am oder,0,,2,,14.5517,52.3421,Hamburger Allee,Frankfurt am Oder".to_owned());
    }
    rows.sort_unstable();
    std::fs::write(&csv, format!("{HEADER}{}\n", rows.join("\n"))).unwrap();
    let manifest = dir.join("manifest.json");
    std::fs::write(
        &manifest,
        format!(
            r#"{{"country":"{country}","layer":"addresses","license":"test","source_release":"test"}}"#
        ),
    )
    .unwrap();
    let bin = dir.join("addresses.bin");
    gridpin::builder::build(&csv, &bin, None, None, None, None, Some(&manifest)).unwrap();
    bin
}

fn write_postal_tail_rank(dir: &Path) -> PathBuf {
    // Deliberately place pc_exact below otherwise identical homonyms so the post-ranking
    // move is observable.  This valid tiny SEC_RANK table lives only in fixture binaries.
    let rank = dir.join("rank.bin");
    let weights: [f32; 10] = [2.0, -2.0, 0.0, 0.0, -4.0, 0.0, 0.0, 1.0, 1.0, 0.0];
    let mut rank_bytes = b"GPRK".to_vec();
    rank_bytes.push(weights.len() as u8);
    rank_bytes.extend_from_slice(&0.0f32.to_le_bytes());
    for weight in weights {
        rank_bytes.extend_from_slice(&weight.to_le_bytes());
    }
    std::fs::write(&rank, rank_bytes).unwrap();
    rank
}

fn build_postal_tail_fixture(
    country: &str,
    tag: &str,
    wrong_count: usize,
    exact_count: usize,
    weaken_exact_house: bool,
    far_exact: bool,
) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("gridpin-{tag}-postal-tail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("addresses.csv");
    let wrong_rep = if weaken_exact_house { "" } else { "b" };
    let mut rows = Vec::new();
    for ordinal in 0..wrong_count {
        rows.push(format!(
            "postallee,W{ordinal:02},wrongstadt {ordinal},12346,12346,15,{wrong_rep},{:.5},{:.5},Postallee,Wrongstadt {ordinal}",
            8.00000 + ordinal as f64 / 10_000.0,
            50.00000 + ordinal as f64 / 10_000.0,
        ));
    }
    for ordinal in 0..exact_count {
        let exact_lon = if far_exact { 8.75 } else { 8.00100 };
        rows.push(format!(
            "postallee,T{ordinal:02},zielstadt {ordinal},12345,12345,15,a,{:.5},{:.5},Postallee,Zielstadt {ordinal}",
            exact_lon + ordinal as f64 / 10_000.0,
            50.00100 + ordinal as f64 / 10_000.0,
        ));
    }
    rows.sort_unstable();
    std::fs::write(&csv, format!("{HEADER}{}\n", rows.join("\n"))).unwrap();

    let rank = write_postal_tail_rank(&dir);

    let manifest = dir.join("manifest.json");
    std::fs::write(
        &manifest,
        format!(
            r#"{{"country":"{country}","layer":"addresses","license":"test","source_release":"test"}}"#
        ),
    )
    .unwrap();
    let bin = dir.join("addresses.bin");
    gridpin::builder::build(&csv, &bin, None, Some(&rank), None, None, Some(&manifest)).unwrap();
    bin
}

fn build_postal_tail_named_fixture(
    tag: &str,
    street_norm: &str,
    street_display: &str,
    number: u32,
    rep: &str,
) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gridpin-{tag}-postal-tail-house-effect-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("addresses.csv");
    let mut rows = Vec::new();
    for ordinal in 0..4 {
        rows.push(format!(
            "{street_norm},W{ordinal:02},wrongstadt {ordinal},12346,12346,{number},{rep},{:.5},{:.5},{street_display},Wrongstadt {ordinal}",
            8.00000 + ordinal as f64 / 10_000.0,
            50.00000 + ordinal as f64 / 10_000.0,
        ));
    }
    rows.push(format!(
        "{street_norm},T00,zielstadt,12345,12345,{number},{rep},8.00100,50.00100,{street_display},Zielstadt"
    ));
    rows.sort_unstable();
    std::fs::write(&csv, format!("{HEADER}{}\n", rows.join("\n"))).unwrap();
    let rank = write_postal_tail_rank(&dir);
    let manifest = dir.join("manifest.json");
    std::fs::write(
        &manifest,
        r#"{"country":"de","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let bin = dir.join("addresses.bin");
    let rules = fixture_rules();
    gridpin::builder::build(
        &csv,
        &bin,
        None,
        Some(&rank),
        Some(rules),
        None,
        Some(&manifest),
    )
    .unwrap();
    bin
}

fn build_postal_tail_house_effect_fixture(tag: &str, number: u32, rep: &str) -> PathBuf {
    build_postal_tail_named_fixture(tag, "postallee", "Postallee", number, rep)
}

fn build_letter_range_tie_fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gridpin-letter-range-tie-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("addresses.csv");
    std::fs::write(
        &csv,
        format!(
            "{HEADER}{}\n",
            [
                "postallee,P00,zielstadt,12345,12345,11,,8.00000,50.00000,Postallee,Zielstadt",
                "postallee,P00,zielstadt,12345,12345,11,c,8.00010,50.00010,Postallee,Zielstadt",
            ]
            .join("\n")
        ),
    )
    .unwrap();
    let manifest = dir.join("manifest.json");
    std::fs::write(
        &manifest,
        r#"{"country":"de","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let bin = dir.join("addresses.bin");
    gridpin::builder::build(&csv, &bin, None, None, None, None, Some(&manifest)).unwrap();
    bin
}

fn build_postal_capital_order_fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gridpin-postal-capital-order-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("addresses.csv");
    let mut rows: Vec<String> = vec![
        // Initial rank 1: stronger exact house, but far from the capital anchor.
        "postallee,F00,fernstadt,12346,12346,15,,10.00000,50.00000,Postallee,Fernstadt",
        // CAPITAL moves this equal-street candidate first. Its non-postal evidence is
        // deliberately identical to the postal target below.
        "postallee,CAP00,capitalstadt,12347,12347,15,b,8.00000,50.00000,Postallee,Capitalstadt",
        // Postal tail can win only after CAPITAL has installed the compatible top.
        "postallee,P00,zielstadt,12345,12345,15,a,10.00100,50.00100,Postallee,Zielstadt",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for ordinal in 0..6 {
        rows.push(format!(
            "dummyweg {ordinal},CAP00,capitalstadt,0,,1,,{:.5},{:.5},Dummyweg {ordinal},Capitalstadt",
            8.0 + ordinal as f64 / 10_000.0,
            50.0 + ordinal as f64 / 10_000.0,
        ));
    }
    rows.sort_unstable();
    std::fs::write(&csv, format!("{HEADER}{}\n", rows.join("\n"))).unwrap();
    let rank = write_postal_tail_rank(&dir);
    let manifest = dir.join("manifest.json");
    std::fs::write(
        &manifest,
        r#"{"country":"de","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let bin = dir.join("addresses.bin");
    gridpin::builder::build(&csv, &bin, None, Some(&rank), None, None, Some(&manifest)).unwrap();
    bin
}

fn mutant_rules(tag: &str, removed_lines: &[&str]) -> PathBuf {
    let source = fixture_rules();
    let target = std::env::temp_dir().join(format!(
        "gridpin-de-f3-mutant-rules-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&target).unwrap();
    for entry in std::fs::read_dir(source).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("tsv") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let filtered = text
            .lines()
            .filter(|line| !removed_lines.contains(line))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(target.join(path.file_name().unwrap()), filtered).unwrap();
    }
    target
}

fn de_index() -> Index {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    Index::open(BIN.get_or_init(|| build_fixture("de", "de"))).unwrap()
}

fn top(query: &str) -> Hit {
    de_index()
        .query(query, 1)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no result for live-pattern query {query:?}"))
}

fn assert_close(hit: &Hit, lat: f64, lon: f64) {
    assert!(
        (hit.lat - lat).abs() < 0.000_01,
        "lat: {} != {lat}",
        hit.lat
    );
    assert!(
        (hit.lon - lon).abs() < 0.000_01,
        "lon: {} != {lon}",
        hit.lon
    );
}

#[test]
fn de_delivery_noise_and_locality_first_variants_recover_the_same_house() {
    let cases = [
        (
            "z. Hd. Empfang, Trankgasse 11, 50667 Köln",
            "de_recipient_prefix",
        ),
        (
            "für den Empfang, Trankgasse 11, 50667 Köln",
            "de_recipient_prefix",
        ),
        (
            "An die Poststelle, Trankgasse 11, 50667 Köln",
            "de_recipient_prefix",
        ),
        (
            "Trankgasse 11, 50667 Köln, Hinterhaus 1. OG",
            "de_subaddress_tail",
        ),
        (
            "Trankgasse 11, 50667 Köln, Aufgang B 3. OG",
            "de_subaddress_tail",
        ),
        ("Trankgasse 11, 50667 Köln, 2. OG", "de_subaddress_tail"),
        (
            "Trankgasse 11, 50667 Köln, Erdgeschoss",
            "de_subaddress_tail",
        ),
    ];
    for (query, flag) in cases {
        let hit = top(query);
        assert_close(&hit, 50.9425, 6.95805555);
        assert!(
            hit.flags.contains(&flag),
            "{query}: expected {flag}, got {:?}",
            hit.flags
        );
    }

    let composed = top("für den Empfang, Max-Dohrn-Str. 5");
    assert_close(&composed, 52.5311096, 13.3017350);
    for flag in [
        "street_exact",
        "house_rep",
        "de_recipient_prefix",
        "de_abbrev",
    ] {
        assert!(
            composed.flags.contains(&flag),
            "recipient + abbreviation composition must retain {flag}: {:?}",
            composed.flags
        );
    }

    let locality_first = top("Frankfurt am Main, Wilhelm-Epstein-Straße 14, 60431");
    assert_close(&locality_first, 50.13388889, 8.65972222);
    assert!(locality_first.flags.contains(&"de_locality_first"));

    let street_abbreviation = top("Proraer al. 119, 18609 Binz");
    assert_close(&street_abbreviation, 54.44305556, 13.56833333);
    assert!(street_abbreviation.flags.contains(&"de_abbrev"));
}

#[test]
fn de_three_field_address_variant_selects_the_only_bounded_address_field() {
    for (query, lat, lon) in [
        (
            "Bergheimer Straße 147, Gebäude C, 69115 Heidelberg",
            49.407,
            8.684,
        ),
        (
            "Schloss Mosigkau, Knobelsdorffallee 2-3, 06847 Dessau-Roßlau",
            51.838,
            12.233,
        ),
        (
            "Flugplatz Rügen, Güttin 66, 18573 Dreschvitz",
            54.403,
            13.31,
        ),
    ] {
        let hit = top(query);
        assert_close(&hit, lat, lon);
        assert!(
            hit.flags.contains(&"de_address_field"),
            "{query}: expected the exact address-field observer, got {:?}",
            hit.flags
        );
    }
}

fn distance_metres(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let earth_radius_metres = 6_371_000.0_f64;
    let lat1 = lat1.to_radians();
    let lat2 = lat2.to_radians();
    let delta_lat = lat2 - lat1;
    let delta_lon = (lon2 - lon1).to_radians();
    let a =
        (delta_lat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (delta_lon / 2.0).sin().powi(2);
    2.0 * earth_radius_metres * a.sqrt().asin()
}

#[test]
fn parenthetical_subaddress_five_frozen_witnesses_have_exact_source_candidates() {
    let cases = [
        (
            3292,
            "Unter den Eichen 2 (Uhle-Hof), 29690 Schwarmstedt",
            "Unter den Eichen 2, 29690 Schwarmstedt",
            52.6768132,
            9.6181178,
            52.67682,
            9.61808,
            false,
        ),
        (
            3419,
            "Am Markt 4 (Rathaus), 27404 Zeven",
            "Am Markt 4, 27404 Zeven",
            53.2953853,
            9.2794954,
            53.29562,
            9.27974,
            false,
        ),
        (
            3421,
            "Kopernikusstr. 16 (Bauteil Ost Verfügungszentrum), 52074 Aachen",
            "Kopernikusstr. 16, 52074 Aachen",
            50.7793188,
            6.0632200,
            50.77946,
            6.06286,
            true,
        ),
        (
            3817,
            "Universitätsstr. 105 (Raum 2.22), 44801 Bochum",
            "Universitätsstr. 105, 44801 Bochum",
            51.4709319,
            7.2274548,
            51.47086,
            7.22731,
            false,
        ),
        (
            3963,
            "Kohlweg 7 (Villa Europa), 66123 Saarbrücken",
            "Kohlweg 7, 66123 Saarbrücken",
            49.2406745,
            7.0208505,
            49.24067,
            7.02079,
            true,
        ),
    ];

    for (
        ordinal,
        raw_query,
        cleaned_query,
        source_lat,
        source_lon,
        truth_lat,
        truth_lon,
        fixture_ordinary_is_empty,
    ) in cases
    {
        // The cleaned exact candidate is the source-backed half of the
        // contract. Admission itself is independently tested as fill-empty;
        // this small fixture intentionally documents when its ordinary fuzzy
        // path is nonempty instead of weakening that production guard.
        let hit = de_index()
            .query(cleaned_query, 5)
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                panic!("PARENTHETICAL_SUBADDRESS_CANDIDATE_OBSERVER: {ordinal}: {cleaned_query}")
            });
        assert_close(&hit, source_lat, source_lon);
        assert_eq!(hit.precision, "house", "ordinal {ordinal}");
        assert!(
            hit.flags.contains(&"street_exact"),
            "ordinal {ordinal}: {:?}",
            hit.flags
        );
        assert!(hit.flags.contains(&"house_rep"), "ordinal {ordinal}");
        assert!(
            distance_metres(hit.lat, hit.lon, truth_lat, truth_lon) < 150.0,
            "ordinal {ordinal} is a predeclared @150 gain"
        );

        let raw = de_index().query(raw_query, 5);
        if fixture_ordinary_is_empty {
            assert!(
                raw.first()
                    .is_some_and(|hit| hit.flags.contains(&"de_parenthetical_subaddress")),
                "PARENTHETICAL_SUBADDRESS_RUNTIME_OBSERVER: ordinal {ordinal}"
            );
        } else {
            assert!(
                raw.iter()
                    .all(|hit| !hit.flags.contains(&"de_parenthetical_subaddress")),
                "PARENTHETICAL_SUBADDRESS_FILL_EMPTY_OBSERVER: ordinal {ordinal}"
            );
        }
    }
}

#[test]
fn parenthetical_subaddress_schramberg_is_an_explicit_over_150m_nonclaim() {
    let hit = de_index()
        .query("Bahnhofstraße 1, 78713 Schramberg", 5)
        .into_iter()
        .next()
        .expect("the exact source address remains a semantic product recovery");
    assert_close(&hit, 48.2285506, 8.3842066);
    assert!(hit.flags.contains(&"street_exact"));
    assert!(hit.flags.contains(&"house_rep"));
    let metres = distance_metres(hit.lat, hit.lon, 48.22991, 8.38381);
    assert!(
        metres > 150.0,
        "PARENTHETICAL_SUBADDRESS_NONCLAIM_OBSERVER: {metres} m"
    );
}

#[test]
fn parenthetical_subaddress_runtime_gate_is_exact_and_fill_empty_only() {
    for query in [
        // Exact street+house exists, but the explicit terminal commune differs.
        "Kohlweg 7 (Villa Europa), 66123 Aachen",
        // Qualifier aliases are deliberately outside this exact-core pass.
        "Albertstr. 25 (Otto-Krayer-Haus), 79104 Freiburg/Breisgau",
        "Marktplatz 4 (Weilheimer Bürgerhaus), 73235 Weilheim/Teck",
        // An interpolation/near/street result is not exact house evidence.
        "Interpolationsweg 5 (Haus Mitte), 12345 Teststadt",
    ] {
        let hits = de_index().query(query, 5);
        assert!(
            hits.iter()
                .all(|hit| !hit.flags.contains(&"de_parenthetical_subaddress")),
            "PARENTHETICAL_SUBADDRESS_FAIL_CLOSED_OBSERVER: {query}: {} hits",
            hits.len()
        );
    }

    let already_nonempty = de_index().query("Unter den Eichen 2 (Uhle-Hof), 29690 Schwarmstedt", 5);
    assert!(
        !already_nonempty.is_empty(),
        "the fill-empty negative must exercise an existing ordinary result"
    );
    assert!(
        already_nonempty
            .iter()
            .all(|hit| !hit.flags.contains(&"de_parenthetical_subaddress")),
        "PARENTHETICAL_SUBADDRESS_FILL_EMPTY_OBSERVER: {} hits",
        already_nonempty.len()
    );
}

#[test]
fn umlaut_and_eszett_work_in_both_directions() {
    let ascii = top("Caecilienstrasse 29-33 50676 Koeln");
    assert_close(&ascii, 50.93471389, 6.95143889);
    assert!(
        ascii.flags.contains(&"de_umlaut"),
        "ORTHOGRAPHY_OBSERVER: ASCII digraph retry did not win"
    );

    let reverse = top("Charles-de-Gaulle-Straße 20");
    assert_close(&reverse, 50.715512, 7.130037);
    assert!(
        reverse.flags.contains(&"de_umlaut"),
        "ORTHOGRAPHY_OBSERVER: reverse eszett retry did not win"
    );
}

#[test]
fn german_street_types_join_split_and_expand_exactly() {
    for (query, lat, lon) in [
        ("Haupt Strasse 164", 49.699600, 9.251770),
        ("Bahnhofstr. 18 Regensburg", 49.011766, 12.099691),
        ("Spandauerdamm 10-22 14059 Berlin", 52.521111, 13.295833),
        ("Alster Ufer 21 20354 Hamburg", 53.56077778, 9.99736111),
        ("Kirsch Allee 1 B 02708 Löbau", 51.100400, 14.659400),
        ("Schul Weg 648 27498 Helgoland", 54.18302778, 7.88475000),
        ("Münster Platz 1 89073 Ulm", 48.39861111, 9.99250000),
        ("Kartäuser Gasse 1 90402 Nürnberg", 49.44833300, 11.07555600),
    ] {
        let hit = top(query);
        assert_close(&hit, lat, lon);
        assert!(
            hit.flags.contains(&"de_street_type"),
            "{query}: {:?}",
            hit.flags
        );
    }
}

#[test]
fn exonym_alias_is_real_but_does_not_pretend_bavaria_is_in_public_sheet() {
    let cologne = top("Caecilienstrasse 29-33 50676 Cologne");
    assert_close(&cologne, 50.93471389, 6.95143889);
    assert!(
        cologne.flags.contains(&"de_city_alias"),
        "CITY_ALIAS_OBSERVER: Cologne did not constrain Köln"
    );
    assert!(!cologne.flags.contains(&"dropped_suffix"));

    let hit = top("Osterwaldstrasse 10 80805 Munich");
    assert_close(&hit, 48.16300000, 11.59873889);
    assert!(hit.flags.contains(&"de_city_alias"));

    let nuremberg = top("Lessingstrasse 6 90443 Nuremberg");
    assert_close(&nuremberg, 49.44555600, 11.07444400);
    assert!(nuremberg.flags.contains(&"de_city_alias"));

    // An exonym-looking token away from the city tail is never rewritten.
    let embedded = top("Cologne Cäcilienstraße 29-33 50676 Köln");
    assert_close(&embedded, 50.93471389, 6.95143889);
    assert!(!embedded.flags.contains(&"de_city_alias"));
    // This tiny witness proves the parser only.  The F4 acceptance report must
    // still count this as not covered by the public 15/16 sheet.
}

#[test]
fn official_commune_alias_is_postcode_bound_exact_house_and_fail_closed() {
    for (query, lat, lon) in [
        (
            "Jacobistraße 37, 61348 Bad Homburg vor der Höhe",
            50.2224935,
            8.6102292,
        ),
        (
            "Auf der Steinkaut 1–15, 61352 Bad Homburg vor der Höhe",
            50.2252517,
            8.6355595,
        ),
        (
            "Dorotheenstr. 24, 61348 Bad Homburg vor der Höhe",
            50.2262365,
            8.6148267,
        ),
        (
            "Am Wingertsberg 4, 61348 Bad Homburg vor der Höhe",
            50.2310019,
            8.6272261,
        ),
        (
            "Domplatz 2, 65549 Limburg an der Lahn",
            50.3887948,
            8.0667120,
        ),
        (
            "Lispenhäuser Straße 41, 36199 Rotenburg an der Fulda",
            51.0233648,
            9.7711211,
        ),
    ] {
        let hit = top(query);
        assert_close(&hit, lat, lon);
        assert_eq!(hit.precision, "house", "{query}");
        assert!(
            hit.flags.contains(&"street_exact"),
            "{query}: {:?}",
            hit.flags
        );
        assert!(hit.flags.contains(&"house_rep"), "{query}: {:?}", hit.flags);
        assert!(
            hit.flags.contains(&"de_official_commune_alias"),
            "OFFICIAL_COMMUNE_ALIAS_RUNTIME_OBSERVER: {query}: {:?}",
            hit.flags
        );
    }

    // The alias target exists, but none of these candidates proves the exact
    // street+house predicate inside that exact commune.
    for query in [
        "Domplatzz 2, 65549 Limburg an der Lahn",
        "Domplatz 999, 65549 Limburg an der Lahn",
        "Domplatz, 65549 Limburg an der Lahn",
        "Fremdweg 7, 61348 Bad Homburg vor der Höhe",
    ] {
        assert!(
            de_index().query(query, 5).is_empty(),
            "OFFICIAL_COMMUNE_ALIAS_FAIL_CLOSED_OBSERVER: {query}"
        );
    }

    // No target in this exact sheet: the required_commune FST guard must reject
    // the retry instead of borrowing the same street+house from another commune.
    let absent = Index::open(&build_official_commune_alias_target_absent_fixture()).unwrap();
    assert!(
        absent
            .query("Domplatz 2, 65549 Limburg an der Lahn", 5)
            .is_empty(),
        "OFFICIAL_COMMUNE_ALIAS_FST_OBSERVER: absent target must fail closed"
    );
}

#[test]
fn frankfurt_main_and_oder_are_never_collapsed_to_one_alias() {
    let main = top("Wilhelm-Epstein-Straße 14 Frankfurt a. M.");
    assert_close(&main, 50.13388889, 8.65972222);
    assert!(main.flags.contains(&"de_abbrev"));

    let slash_main = top("Wilhelm-Epstein-Straße 14 Frankfurt/Main");
    assert_close(&slash_main, 50.13388889, 8.65972222);
    assert!(slash_main.flags.contains(&"de_city_alias"));
    assert!(
        slash_main.flags.contains(&"de_frankfurt"),
        "FRANKFURT_OBSERVER: Main qualifier was not a hard constraint"
    );

    let oder = top("Logenstraße 8 Frankfurt/Oder");
    assert_close(&oder, 52.34208333, 14.55166667);
    assert!(
        oder.flags.contains(&"de_frankfurt"),
        "FRANKFURT_OBSERVER: Oder qualifier was not a hard constraint"
    );
    assert_ne!(main.commune, oder.commune);

    let conflict = de_index().query("Wilhelm-Epstein-Straße 14 Frankfurt Oder", 1);
    assert!(
        conflict.is_empty() || conflict[0].commune != "Frankfurt am Main",
        "explicit Oder must never be silently rewritten to Main"
    );
}

#[test]
fn official_place_abbreviations_preserve_the_name_tail() {
    for (query, commune) in [
        ("Rochusstraße 8 Bingen a. Rh.", "Bingen am Rhein"),
        (
            "Bahnhof 1 Geislingen a. d. Steige",
            "Geislingen an der Steige",
        ),
        ("Am Römerbad 17a Weißenburg i. Bay.", "Weißenburg in Bayern"),
    ] {
        let hit = top(query);
        assert_eq!(hit.commune, commune, "{query}");
        assert!(
            hit.flags.contains(&"de_abbrev"),
            "ABBREVIATION_OBSERVER: {query}"
        );
    }
}

#[test]
fn typed_subdivision_tail_keeps_the_parent_municipality() {
    let hit = top("Proraer Allee 119 18609 Binz - OT Prora");
    assert_eq!(hit.commune, "Binz");
    assert_close(&hit, 54.44305556, 13.56833333);
    assert!(
        hit.flags.contains(&"de_admin_tail"),
        "ADMIN_TAIL_OBSERVER: OT tail did not preserve Binz"
    );
    assert!(!hit.flags.contains(&"dropped_suffix"));

    let land = top("Friedensstraße 86 14715 Milower Land");
    assert_eq!(land.commune, "Milower Land");
    assert_close(&land, 52.51941667, 12.31122222);
    assert!(!land.flags.contains(&"de_admin_tail"));
}

#[test]
fn country_tokens_and_d_postcode_prefix_are_contextual() {
    for (query, lat, lon) in [
        (
            "Bertolt-Brecht-Platz 1, 10117 Berlin, Germany",
            52.52166694,
            13.38611111,
        ),
        (
            "Lausitzerstraße 10,10999 Berlin, Deutschland",
            52.49738000,
            13.42858000,
        ),
        (
            "Meyerhofstraße 1, DE-69117 Heidelberg",
            49.38480000,
            8.71031940,
        ),
        ("Strobelallee 50, D-44139 Dortmund", 51.49250000, 7.45166700),
    ] {
        let hit = top(query);
        assert_close(&hit, lat, lon);
        assert!(
            hit.flags.contains(&"de_country"),
            "{query}: {:?}",
            hit.flags
        );
    }
}

#[test]
fn house_suffix_fused_and_spaced_forms_are_equivalent() {
    let suffix = top("Kirschallee 1 B 02708 Löbau");
    assert_close(&suffix, 51.100400, 14.659400);
    assert_eq!(
        suffix.housenumber.as_deref(),
        Some("1b"),
        "HOUSE_SUFFIX_SPACED_OBSERVER: spaced suffix was not consumed"
    );
    assert!(
        suffix.flags.contains(&"house_rep"),
        "HOUSE_SUFFIX_SPACED_OBSERVER: spaced suffix was not matched exactly"
    );

    for query in [
        "Altenbaustraße 12a 53474 Bad Neuenahr-Ahrweiler",
        "Altenbaustraße 12 A 53474 Bad Neuenahr-Ahrweiler",
    ] {
        let suffix = top(query);
        assert_close(&suffix, 50.54140000, 7.09288000);
        assert_eq!(
            suffix.housenumber.as_deref(),
            Some("12a"),
            "HOUSE_SUFFIX_OBSERVER: {query}"
        );
    }
}

#[test]
fn house_range_words_preserve_the_source_tail() {
    let symbol = top("Cäcilienstraße 29-33 50676 Köln");
    assert_close(&symbol, 50.93471389, 6.95143889);
    assert_eq!(symbol.housenumber.as_deref(), Some("2933"));
    assert!(
        symbol.flags.contains(&"de_house_range"),
        "HOUSE_RANGE_SYMBOL_OBSERVER: punctuation range retry did not win"
    );

    let range = top("Cäcilienstraße 29 bis 33 50676 Köln");
    assert_close(&range, 50.93471389, 6.95143889);
    assert!(
        range.flags.contains(&"de_house_range"),
        "HOUSE_RANGE_OBSERVER: word range retry did not win"
    );
}

#[test]
fn house_fraction_shorthand_maps_to_live_mixed_fraction_suffixes() {
    let fraction = top("Hauptstraße 17/2");
    assert_close(&fraction, 48.058200, 10.184400);
    assert_eq!(fraction.housenumber.as_deref(), Some("1712"));
    assert!(
        fraction.flags.contains(&"de_house_slash"),
        "HOUSE_SLASH_OBSERVER: mixed-fraction retry did not win"
    );

    let half = top("Krahnstraße 1/2");
    assert_close(&half, 52.27720000, 8.04118000);
    assert_eq!(half.housenumber.as_deref(), Some("112"));
    assert!(
        half.flags.contains(&"de_house_slash"),
        "HOUSE_SLASH_OBSERVER: 1/2 retry did not win"
    );
}

#[test]
fn postcode_city_seam_is_split_without_losing_the_leading_zero() {
    let seam = top("St Petersburger Straße 24a 01069Dresden");
    assert_eq!(seam.postcode, "01069");
    assert!(
        seam.flags.contains(&"pc_exact"),
        "POSTCODE_SEAM_OBSERVER: glued PLZ was not parsed exactly"
    );
}

#[test]
fn exact_five_digit_postcode_keeps_its_display_zero() {
    let exact = top("Augustusstraße 1 01067 Dresden");
    assert_eq!(exact.postcode, "01067");
    assert!(
        exact.flags.contains(&"pc_exact"),
        "POSTCODE_FIVE_OBSERVER: exact leading-zero PLZ was not parsed"
    );
}

#[test]
fn lost_postcode_zero_retry_requires_an_exact_five_digit_match() {
    let lost_zero = top("Augustusstraße 1 1067 Dresden");
    assert_eq!(lost_zero.postcode, "01067");
    assert!(
        lost_zero.flags.contains(&"de_postcode"),
        "POSTCODE_ZERO_OBSERVER: lost-zero retry did not win"
    );

    // The same four digits can be a house number.  The ordinary parse must
    // keep priority over a padded-postcode street fallback.
    let four_digit_house = top("Augustusstraße 1067 Dresden");
    assert!(
        !four_digit_house.flags.contains(&"de_postcode"),
        "POSTCODE_ZERO_COLLISION: a four-digit house was reclassified as PLZ"
    );
}

#[test]
fn exact_german_words_beat_lossy_orthographic_retries() {
    for (query, lat, lon) in [
        ("Trankgasse 11 50667 Köln", 50.94250000, 6.95805555),
        (
            "Friedensreich-Hundertwasser-Platz 1 29525 Uelzen",
            52.96970000,
            10.55310000,
        ),
        ("Messepl. 2 45131 Essen", 51.43100000, 6.99767000),
        ("Rheinstraße 3 41460 Neuss", 51.20120000, 6.69217000),
        (
            "Neues Kloster 1 88427 Bad Schussenried",
            48.00730000,
            9.65858000,
        ),
    ] {
        let hit = top(query);
        assert_close(&hit, lat, lon);
        assert!(
            !hit.flags.contains(&"de_umlaut"),
            "canonical spelling must keep tie priority for {query}: {:?}",
            hit.flags
        );
    }
}

#[test]
fn exact_german_house_blocks_the_weak_cityless_capital_prior() {
    let bin = build_postcode_capital_prior_fixture("de", "de");
    let index = Index::open(&bin).unwrap();

    let postcode = index.query("Zielstraße 35 12357 Unbekannt", 2);
    assert_eq!(postcode[0].commune, "Fernstadt");
    assert_eq!(postcode[0].postcode, "12357");
    assert!(postcode[0].flags.contains(&"pc_exact"));
    assert!(postcode[0].flags.contains(&"dropped_suffix"));
    assert!(postcode[0].score > postcode[1].score);

    let cityless = index.query("Zielstraße 35", 2);
    assert_eq!(cityless[0].commune, "Fernstadt");
    assert_eq!(cityless[1].commune, "Anchorstadt");
    assert!(cityless[0].score > cityless[1].score);

    let weak_postcode = index.query("Sicherweg 4 21423 Unbekannt", 2);
    assert_eq!(weak_postcode[0].commune, "Fernstadt");
    assert_eq!(weak_postcode[1].commune, "Anchorstadt");
    assert!(!weak_postcode[0].flags.contains(&"pc_exact"));
    assert!(weak_postcode[0].score > weak_postcode[1].score);

    let foreign_bin = build_postcode_capital_prior_fixture("uz", "foreign-control");
    let foreign = Index::open(&foreign_bin)
        .unwrap()
        .query("Zielstraße 35 12357 Unbekannt", 2);
    assert_eq!(foreign[0].commune, "Anchorstadt");
    assert_eq!(foreign[1].commune, "Fernstadt");
    assert!(foreign[1].flags.contains(&"pc_exact"));
    assert!(foreign[1].flags.contains(&"dropped_suffix"));
    assert!(foreign[0].score < foreign[1].score);
}

#[test]
fn removed_postcode_commas_recover_the_same_exact_german_house() {
    let bin = build_missing_postcode_comma_fixture();
    let index = Index::open(&bin).unwrap();
    for (dirty, clean) in [
        (
            "Hauptstraße 32   23769 Landkirchen",
            "Hauptstraße 32, 23769 Landkirchen",
        ),
        (
            "Burghof 9   14776 Brandenburg an der Havel",
            "Burghof 9, 14776 Brandenburg an der Havel",
        ),
        (
            "August-Sonntag-Straße 5   14770 Brandenburg an der Havel",
            "August-Sonntag-Straße 5, 14770 Brandenburg an der Havel",
        ),
        ("Kirchgasse 5   12043 Berlin", "Kirchgasse 5, 12043 Berlin"),
    ] {
        let dirty_hit = index.query(dirty, 1).remove(0);
        let clean_hit = index.query(clean, 1).remove(0);
        assert_eq!(dirty_hit.street, clean_hit.street, "{dirty}");
        assert_eq!(dirty_hit.housenumber, clean_hit.housenumber, "{dirty}");
        assert_eq!(dirty_hit.postcode, clean_hit.postcode, "{dirty}");
        assert_eq!(dirty_hit.lat, clean_hit.lat, "{dirty}");
        assert_eq!(dirty_hit.lon, clean_hit.lon, "{dirty}");
        for flag in [
            "street_exact",
            "house_rep",
            "pc_exact",
            "de_postcode_house",
            "de_missing_postcode_comma",
        ] {
            assert!(dirty_hit.flags.contains(&flag), "{dirty}: missing {flag}");
        }
    }
}

#[test]
fn retained_de_locality_breaks_only_equal_suffix_recovery_ties_for_k1_and_k5() {
    let bin = build_retained_locality_fixture("de", "de-positive", false);
    let index = Index::open(&bin).unwrap();

    let frankfurt_k5 = index.query("Hamburger Allee 2 60486 Frankfurt", 5);
    let frankfurt_k1 = index.query("Hamburger Allee 2 60486 Frankfurt", 1);
    assert_eq!(frankfurt_k5[0].commune, "Frankfurt am Main");
    assert_eq!(frankfurt_k1[0].commune, frankfurt_k5[0].commune);
    assert!(frankfurt_k1[0].flags.contains(&"de_retained_locality"));
    assert!(frankfurt_k5[0].flags.contains(&"de_retained_locality"));
    assert!(frankfurt_k5[0].flags.contains(&"dropped_suffix"));
    assert!(frankfurt_k5[0].flags.contains(&"ambiguous_far"));
    assert_eq!(frankfurt_k5[0].score, frankfurt_k5[1].score);
    assert_eq!(frankfurt_k5[0].housenumber, frankfurt_k5[1].housenumber);

    let lohne = index.query("Brandstraße 21 49393 Lohne", 5);
    assert_eq!(lohne[0].commune, "Lohne (Oldenburg), Stadt");
    assert!(lohne[0].flags.contains(&"de_retained_locality"));
    assert!(lohne[0].flags.contains(&"dropped_suffix"));
}

#[test]
fn retained_de_locality_fails_closed_on_ambiguous_or_shared_tail_and_foreign_sheets() {
    let ambiguous_bin = build_retained_locality_fixture("de", "de-ambiguous", true);
    let ambiguous = Index::open(&ambiguous_bin)
        .unwrap()
        .query("Hamburger Allee 2 60486 Frankfurt", 6);
    assert!(ambiguous
        .iter()
        .any(|hit| hit.commune == "Frankfurt am Main"));
    assert!(ambiguous
        .iter()
        .any(|hit| hit.commune == "Frankfurt am Oder"));
    assert_ne!(ambiguous[0].commune, "Frankfurt am Main");
    assert_ne!(ambiguous[0].commune, "Frankfurt am Oder");
    assert!(!ambiguous[0].flags.contains(&"de_retained_locality"));

    let index = Index::open(&build_retained_locality_fixture("de", "de-negative", false)).unwrap();
    let shared_tail = index.query("Meierei 9 17166 Groß Roge", 2);
    assert_eq!(shared_tail[0].commune, "Jülchendorf");
    assert_eq!(shared_tail[1].commune, "Roge Stadt");
    assert_eq!(shared_tail[0].score, shared_tail[1].score);
    assert_eq!(shared_tail[0].housenumber, shared_tail[1].housenumber);
    assert_eq!(shared_tail[0].street, shared_tail[1].street);
    assert!(shared_tail[0].flags.contains(&"dropped_suffix"));
    assert!(!shared_tail[0].flags.contains(&"de_retained_locality"));

    let raw_before_variant = index.query("Muehlenweg 4 50667 Koeln", 2);
    assert_eq!(raw_before_variant[0].commune, "Zippendorf");
    assert_eq!(raw_before_variant[1].commune, "Köln, Stadt");
    assert_eq!(raw_before_variant[0].score, raw_before_variant[1].score);
    assert_eq!(
        raw_before_variant[0].housenumber,
        raw_before_variant[1].housenumber
    );
    assert!(raw_before_variant[0].flags.contains(&"de_umlaut"));
    assert!(raw_before_variant[0].flags.contains(&"dropped_suffix"));
    assert!(!raw_before_variant[0]
        .flags
        .contains(&"de_retained_locality"));

    let foreign_bin = build_retained_locality_fixture("uz", "foreign-negative", false);
    let foreign = Index::open(&foreign_bin)
        .unwrap()
        .query("Hamburger Allee 2 60486 Frankfurt", 5);
    assert_ne!(foreign[0].commune, "Frankfurt am Main");
    assert!(!foreign[0].flags.contains(&"de_retained_locality"));

    let focused = index
        .query_near("Hamburger Allee 2 60486 Frankfurt", 5, 53.5975, 11.4465)
        .unwrap();
    assert_eq!(focused[0].commune, "Zippendorf");
    assert!(!focused[0].flags.contains(&"de_retained_locality"));
}

#[test]
fn exact_postcode_c2_tail_promotes_ranks_two_three_and_five_for_small_k() {
    for (wrong_count, expected_rank) in [(1, 2), (2, 3), (4, 5)] {
        let bin = build_postal_tail_fixture(
            "de",
            &format!("rank-{expected_rank}"),
            wrong_count,
            1,
            false,
            false,
        );
        let index = Index::open(&bin).unwrap();

        let ordinary = index.query("Postallee 15 12345", 5);
        assert_eq!(ordinary[expected_rank - 1].commune, "Zielstadt 0");
        assert!(ordinary[expected_rank - 1].flags.contains(&"pc_exact"));
        assert!(ordinary
            .iter()
            .all(|hit| !hit.flags.contains(&"de_postal_tail")));

        for k in [1, 2, 5] {
            let promoted = index.query("Postallee 15 12345 Unbekannt", k);
            assert_eq!(
                promoted[0].commune, "Zielstadt 0",
                "rank {expected_rank}, k={k}"
            );
            assert_eq!(promoted[0].postcode, "12345");
            assert_eq!(promoted[0].housenumber.as_deref(), Some("15a"));
            assert!(promoted[0].flags.contains(&"pc_exact"));
            assert!(promoted[0].flags.contains(&"de_postal_tail"));
            assert!(promoted[0].flags.contains(&"dropped_suffix"));
            assert!(!promoted[0].flags.contains(&"house_rep"));
        }
    }
}

#[test]
fn house_range_and_slash_inputs_freeze_out_only_postal_tail() {
    for (tag, number, rep, special_query, effect_flag, expected_house) in [
        (
            "range-freeze",
            20,
            "38",
            "Postallee 20-38 12345 Unbekannt",
            "de_house_range",
            "2038",
        ),
        (
            "slash-freeze",
            17,
            "12",
            "Postallee 17/2 12345 Unbekannt",
            "de_house_slash",
            "1712",
        ),
    ] {
        let index = Index::open(&build_postal_tail_house_effect_fixture(tag, number, rep)).unwrap();

        // Same c2 candidate set without a range/slash effect proves the postal rule would
        // otherwise fire; the exclusion is frozen from the original raw input.
        let control = index.query(&format!("Postallee {number} 12345 Unbekannt"), 5);
        assert_eq!(control[0].commune, "Zielstadt");
        assert!(control[0].flags.contains(&"de_postal_tail"));
        assert_eq!(
            control
                .iter()
                .map(|hit| hit.commune.as_str())
                .collect::<Vec<_>>(),
            [
                "Zielstadt",
                "Wrongstadt 3",
                "Wrongstadt 2",
                "Wrongstadt 1",
                "Wrongstadt 0",
            ]
        );

        let baseline_query = special_query.strip_suffix(" Unbekannt").unwrap();
        let baseline = index.query(baseline_query, 5);
        let protected = index.query(special_query, 5);
        let expected_order = [
            "Wrongstadt 3",
            "Wrongstadt 2",
            "Wrongstadt 1",
            "Wrongstadt 0",
            "Zielstadt",
        ];
        assert_eq!(baseline.len(), 5);
        assert_eq!(protected.len(), 5);
        assert_eq!(
            baseline
                .iter()
                .map(|hit| hit.commune.as_str())
                .collect::<Vec<_>>(),
            expected_order
        );
        assert_eq!(
            protected
                .iter()
                .map(|hit| hit.commune.as_str())
                .collect::<Vec<_>>(),
            expected_order
        );
        for (position, (before, after)) in baseline.iter().zip(&protected).enumerate() {
            assert_eq!(after.lat, before.lat, "{tag}: latitude at {position}");
            assert_eq!(after.lon, before.lon, "{tag}: longitude at {position}");
            assert_eq!(after.score, before.score, "{tag}: score at {position}");
            assert_eq!(after.street, before.street, "{tag}: street at {position}");
            assert_eq!(
                after.housenumber, before.housenumber,
                "{tag}: house at {position}"
            );
            assert_eq!(
                after.housenumber.as_deref(),
                Some(expected_house),
                "{tag}: rendered structural house at {position}"
            );
            assert_eq!(
                after.postcode, before.postcode,
                "{tag}: postcode at {position}"
            );
            assert_eq!(
                after.precision, before.precision,
                "{tag}: precision at {position}"
            );
            assert!(before.flags.contains(&effect_flag));
            assert!(after.flags.contains(&effect_flag));
            assert!(before.flags.contains(&"house_rep"));
            assert!(after.flags.contains(&"house_rep"));
            assert!(!before.flags.contains(&"dropped_suffix"));
            assert!(after.flags.contains(&"dropped_suffix"));
            let mut protected_baseline_flags = after.flags.clone();
            protected_baseline_flags.retain(|flag| *flag != "dropped_suffix");
            assert_eq!(
                protected_baseline_flags, before.flags,
                "{tag}: only c2 explainability may differ at {position}"
            );
            assert_eq!(
                before.flags.contains(&"pc_exact"),
                position == 4,
                "{tag}: baseline pc_exact at {position}"
            );
            assert_eq!(
                after.flags.contains(&"pc_exact"),
                position == 4,
                "{tag}: protected pc_exact at {position}"
            );
        }
        assert!(protected
            .iter()
            .all(|hit| !hit.flags.contains(&"de_postal_tail")));
    }
}

#[test]
fn letter_suffixed_house_range_resolves_the_first_house_and_freezes_postal_tail() {
    let observer = "HOUSE_RANGE_LETTER_SUFFIX_OBSERVER";
    let index = Index::open(&build_postal_tail_house_effect_fixture(
        "letter-range-freeze",
        3,
        "b",
    ))
    .unwrap();

    let control = index.query("Postallee 3b 12345 Unbekannt", 5);
    assert_eq!(control[0].commune, "Zielstadt", "{observer}: control");
    assert!(
        control[0].flags.contains(&"de_postal_tail"),
        "{observer}: control activation"
    );

    let baseline = index.query("Postallee 3b 12345", 5);
    let protected = index.query("Postallee 3b-3c 12345 Unbekannt", 5);
    assert_eq!(baseline.len(), 5, "{observer}: baseline window");
    assert_eq!(protected.len(), 5, "{observer}: protected window");
    assert_eq!(
        baseline
            .iter()
            .map(|hit| hit.commune.as_str())
            .collect::<Vec<_>>(),
        protected
            .iter()
            .map(|hit| hit.commune.as_str())
            .collect::<Vec<_>>(),
        "{observer}: ordering"
    );
    for (position, (before, after)) in baseline.iter().zip(&protected).enumerate() {
        assert_eq!(after.lat, before.lat, "{observer}: latitude at {position}");
        assert_eq!(after.lon, before.lon, "{observer}: longitude at {position}");
        assert_eq!(after.score, before.score, "{observer}: score at {position}");
        assert_eq!(
            after.street, before.street,
            "{observer}: street at {position}"
        );
        assert_eq!(
            after.housenumber, before.housenumber,
            "{observer}: house at {position}"
        );
        assert_eq!(
            after.housenumber.as_deref(),
            Some("3b"),
            "{observer}: the second source endpoint must not become another house token at {position}"
        );
        assert_eq!(
            after.postcode, before.postcode,
            "{observer}: postcode at {position}"
        );
        assert_eq!(
            after.precision, before.precision,
            "{observer}: precision at {position}"
        );
        assert!(
            after.flags.contains(&"de_house_range"),
            "{observer}: structural effect at {position}"
        );
        assert!(
            !after.flags.contains(&"de_postal_tail"),
            "{observer}: postal-tail exclusion at {position}"
        );
    }
}

#[test]
fn right_suffixed_house_range_wins_an_exact_raw_tie_with_the_first_endpoint() {
    let observer = "HOUSE_RANGE_RIGHT_SUFFIX_TIE_OBSERVER";
    let index = Index::open(&build_letter_range_tie_fixture()).unwrap();

    let hits = index.query("Postallee 11-13c 12345", 5);

    assert!(!hits.is_empty(), "{observer}: expected a house result");
    assert_eq!(
        hits[0].housenumber.as_deref(),
        Some("11"),
        "{observer}: raw normalization must not reinterpret the range as house 11c"
    );
    assert!(
        hits[0].flags.contains(&"de_house_range"),
        "{observer}: the winning first-endpoint retry must remain explainable"
    );
}

#[test]
fn house_effect_postal_freeze_propagates_through_the_production_c0_retry() {
    for (tag, number, rep, special_query, effect_flag) in [
        (
            "range-c0-freeze",
            20,
            "38",
            "Via Postallee 20-38 12345 Unbekannt",
            "de_house_range",
        ),
        (
            "slash-c0-freeze",
            17,
            "12",
            "Via Postallee 17/2 12345 Unbekannt",
            "de_house_slash",
        ),
    ] {
        let index = Index::open(&build_postal_tail_house_effect_fixture(tag, number, rep)).unwrap();

        let control = index.query(&format!("Via Postallee {number} 12345 Unbekannt"), 5);
        assert_eq!(control.len(), 5, "{tag}: c0 control candidate window");
        assert_eq!(control[0].commune, "Zielstadt");
        assert!(control[0].flags.contains(&"de_postal_tail"));

        let protected = index.query(special_query, 5);
        assert_eq!(protected.len(), 5, "{tag}: c0 protected candidate window");
        assert_eq!(protected[0].commune, "Wrongstadt 3");
        assert!(protected[0].flags.contains(&effect_flag));
        assert!(protected
            .iter()
            .all(|hit| !hit.flags.contains(&"de_postal_tail")));
    }
}

#[test]
fn orthography_effect_does_not_suppress_postal_tail() {
    let bin = build_postal_tail_named_fixture(
        "orthography-postal",
        "caecilienstrasse",
        "Caecilienstrasse",
        15,
        "a",
    );
    let hits = Index::open(&bin)
        .unwrap()
        .query("Cäcilienstraße 15 12345 Unbekannt", 5);
    assert_eq!(hits.len(), 5);
    assert_eq!(hits[0].commune, "Zielstadt");
    assert!(hits[0].flags.contains(&"de_umlaut"));
    assert!(hits[0].flags.contains(&"de_postal_tail"));
    assert!(hits.iter().all(|hit| hit.flags.contains(&"de_umlaut")));
}

#[test]
fn house_range_freeze_keeps_retained_locality_active() {
    let index = Index::open(&build_retained_locality_fixture(
        "de",
        "retained-range-freeze",
        false,
    ))
    .unwrap();
    let hits = index.query("Hamburger Allee 2-4 60486 Frankfurt", 5);
    assert_eq!(hits[0].commune, "Frankfurt am Main");
    assert!(hits[0].flags.contains(&"de_retained_locality"));
    assert!(hits
        .iter()
        .all(|hit| !hit.flags.contains(&"de_postal_tail")));
}

#[test]
fn exact_postcode_c2_tail_runs_before_ambiguity_recalibration() {
    let index = Index::open(&build_postal_tail_fixture(
        "de",
        "ambiguity-order",
        1,
        1,
        false,
        true,
    ))
    .unwrap();
    let ordinary = index.query("Postallee 15 12345", 2);
    assert_eq!(ordinary[0].commune, "Wrongstadt 0");
    assert_eq!(ordinary[1].commune, "Zielstadt 0");

    let recovered = index.query("Postallee 15 12345 Unbekannt", 2);
    assert_eq!(recovered[0].commune, "Zielstadt 0");
    assert!(recovered[0].flags.contains(&"de_postal_tail"));
    assert!(recovered[0].flags.contains(&"ambiguous_far"));
    assert!(recovered[0].confidence <= 0.2);
}

#[test]
fn exact_postcode_c2_tail_runs_after_capital_prior() {
    let index = Index::open(&build_postal_capital_order_fixture()).unwrap();
    let capital_only = index.query("Postallee 15 12345", 3);
    assert_eq!(capital_only[0].commune, "Capitalstadt");
    assert_eq!(capital_only[1].commune, "Fernstadt");
    assert_eq!(capital_only[2].commune, "Zielstadt");
    assert!(!capital_only[0].flags.contains(&"de_postal_tail"));

    let recovered = index.query("Postallee 15 12345 Unbekannt", 3);
    assert_eq!(recovered[0].commune, "Zielstadt");
    assert_eq!(recovered[1].commune, "Capitalstadt");
    assert_eq!(recovered[2].commune, "Fernstadt");
    assert!(recovered[0].flags.contains(&"de_postal_tail"));
    assert!(recovered[0].flags.contains(&"ambiguous_far"));
}

#[test]
fn exact_postcode_c2_tail_fails_closed_at_rank_six_duplicates_and_context_gates() {
    let rank_six = Index::open(&build_postal_tail_fixture(
        "de", "rank-six", 5, 1, false, false,
    ))
    .unwrap();
    let ordinary_six = rank_six.query("Postallee 15 12345", 6);
    assert_eq!(ordinary_six[5].commune, "Zielstadt 0");
    let recovered_six = rank_six.query("Postallee 15 12345 Unbekannt", 6);
    assert_ne!(recovered_six[0].commune, "Zielstadt 0");
    assert_eq!(recovered_six[5].commune, "Zielstadt 0");
    assert!(!recovered_six[0].flags.contains(&"de_postal_tail"));

    let duplicate = Index::open(&build_postal_tail_fixture(
        "de",
        "duplicate",
        1,
        2,
        false,
        false,
    ))
    .unwrap()
    .query("Postallee 15 12345 Unbekannt", 3);
    assert_ne!(duplicate[0].commune, "Zielstadt 0");
    assert_eq!(
        duplicate
            .iter()
            .filter(|hit| hit.flags.contains(&"pc_exact"))
            .count(),
        2
    );
    assert!(!duplicate[0].flags.contains(&"de_postal_tail"));

    let foreign = Index::open(&build_postal_tail_fixture(
        "uz", "foreign", 1, 1, false, false,
    ))
    .unwrap()
    .query("Postallee 15 12345 Unbekannt", 2);
    assert_ne!(foreign[0].commune, "Zielstadt 0");
    assert!(foreign
        .iter()
        .all(|hit| !hit.flags.contains(&"de_postal_tail")));

    let context = Index::open(&build_postal_tail_fixture(
        "de", "context", 1, 1, false, false,
    ))
    .unwrap();
    let non_c2 = context.query("Postallee 15 12345", 2);
    assert_ne!(non_c2[0].commune, "Zielstadt 0");
    assert!(!non_c2[0].flags.contains(&"de_postal_tail"));
    let focused = context
        .query_near("Postallee 15 12345 Unbekannt", 2, 50.001, 8.001)
        .unwrap();
    assert!(focused
        .iter()
        .all(|hit| !hit.flags.contains(&"de_postal_tail")));

    let already_exact = Index::open(&build_postal_tail_fixture(
        "de",
        "already-exact",
        0,
        1,
        false,
        false,
    ))
    .unwrap()
    .query("Postallee 15 12345 Unbekannt", 1);
    assert_eq!(already_exact[0].commune, "Zielstadt 0");
    assert!(already_exact[0].flags.contains(&"pc_exact"));
    assert!(!already_exact[0].flags.contains(&"de_postal_tail"));
}

#[test]
fn exact_postcode_c2_tail_rejects_altvater_style_house_evidence_loss() {
    let index = Index::open(&build_postal_tail_fixture(
        "de",
        "house-evidence-loss",
        1,
        1,
        true,
        false,
    ))
    .unwrap();
    let ordinary = index.query("Postallee 15 12345", 2);
    assert_eq!(ordinary[0].housenumber.as_deref(), Some("15"));
    assert!(ordinary[0].flags.contains(&"house_rep"));
    assert_eq!(ordinary[1].housenumber.as_deref(), Some("15a"));
    assert!(!ordinary[1].flags.contains(&"house_rep"));

    let recovered = index.query("Postallee 15 12345 Unbekannt", 2);
    assert_eq!(recovered[0].housenumber.as_deref(), Some("15"));
    assert!(recovered[0].flags.contains(&"house_rep"));
    assert!(!recovered[0].flags.contains(&"de_postal_tail"));
}

#[test]
fn structured_de_input_gets_the_same_country_scoped_fallbacks() {
    let hits = de_index().query_structured(
        "Caecilienstrasse",
        Some("29-33"),
        "Cologne",
        Some("50676"),
        1,
    );
    let hit = &hits.first().expect("structured DE alias result").0;
    assert_close(hit, 50.93471389, 6.95143889);
    assert!(hit.flags.contains(&"de_city_alias"));
    assert!(hit.flags.contains(&"de_umlaut"));

    let hits = de_index().query_structured("Münster Platz", Some("1"), "Ulm", Some("89073"), 1);
    let hit = &hits.first().expect("structured DE street-type result").0;
    assert_close(hit, 48.39861111, 9.99250000);
    assert!(hit.flags.contains(&"de_street_type"));
}

#[test]
fn germany_fallbacks_do_not_run_on_another_country_sheet() {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    let index = Index::open(BIN.get_or_init(|| build_fixture("nl", "non-de"))).unwrap();
    let hits = index.query("Caecilienstrasse 29-33 50676 Koeln", 3);
    assert!(
        hits.iter()
            .all(|hit| !hit.flags.iter().any(|flag| flag.starts_with("de_"))),
        "DE explainability flags leaked into a non-DE sheet"
    );
    assert!(
        hits.iter()
            .all(|hit| (hit.lat - 50.93471389).abs() > 0.000_01),
        "DE orthographic fallback leaked into a non-DE sheet"
    );
}

#[test]
fn reverse_rule_table_mutations_break_the_same_positive_observer() {
    type RuleCase<'a> = (&'a str, &'a [&'a str], &'a str, &'a str, f64, f64);
    let cases: &[RuleCase<'_>] = &[
        (
            "street",
            &[
                "de\tstraße",
                "de\tstrasse",
                "de\tstr",
                "de\tweg",
                "de\tallee",
                "de\tplatz",
                "de\tgasse",
                "de\tdamm",
                "de\tufer",
            ],
            "Alster Ufer 21 20354 Hamburg",
            "de_street_type",
            53.56077778,
            9.99736111,
        ),
        (
            "city",
            &["de\tcologne\tkoln"],
            "Caecilienstrasse 29-33 50676 Cologne",
            "de_city_alias",
            50.93471389,
            6.95143889,
        ),
        (
            "abbrev",
            &["de\ta\tm\tam main"],
            "Wilhelm-Epstein-Straße 14 Frankfurt a. M.",
            "de_abbrev",
            50.13388889,
            8.65972222,
        ),
        (
            "admin",
            &["de\tot", "de\tortsteil"],
            "Proraer Allee 119 18609 Binz - OT Prora",
            "de_admin_tail",
            54.44305556,
            13.56833333,
        ),
        (
            "country",
            &["de\tdeutschland", "de\tgermany"],
            "Bertolt-Brecht-Platz 1, 10117 Berlin, Germany",
            "de_country",
            52.52166694,
            13.38611111,
        ),
    ];
    for (tag, removed, query, flag, lat, lon) in cases {
        let positive = top(query);
        let observed = |hit: &Hit| {
            hit.flags.contains(flag)
                && (hit.lat - lat).abs() < 0.000_01
                && (hit.lon - lon).abs() < 0.000_01
        };
        assert!(observed(&positive), "positive observer failed for {tag}");
        let rules = mutant_rules(tag, removed);
        let bin = build_fixture_with_rules("de", &format!("mutant-{tag}"), &rules);
        let hits = Index::open(&bin).unwrap().query(query, 1);
        assert!(
            hits.first().is_none_or(|hit| !observed(hit)),
            "removing {tag} rule rows must break the positive coordinate+flag observer"
        );
    }
}
