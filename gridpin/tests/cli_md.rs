//! `--format md` of `query` and `reverse`, `batch --output-format md`: the Markdown an agent reads holds the JSON
//! answer's fields and values in its order, says "weak" in words, keeps `|` inside its cell and stops at 200 rows.
use std::path::PathBuf;
use std::process::Command;

mod support;
use support::Scratch;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";
const ROWS: &str = "rue des lilas,001,ville,10000,1,,7.42,43.73,Rue des Lilas,Ville\n\
                    rue des lilas,001,ville,10000,3,,7.4205,43.7302,Rue des Lilas,Ville\n";

fn sheet(name: &str) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("md-{name}"));
    let csv = dir.join("in.csv");
    std::fs::write(&csv, format!("{HDR}{ROWS}")).unwrap();
    let man = dir.join("m.json");
    std::fs::write(&man, r#"{"country":"mc","layer":"addresses","license":"t","source_release":"test-release","attribution":"fixture data"}"#)
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

fn run(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(BIN).args(args).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The `| field | value |` rows of one answer's table, in order.
fn table_rows(md: &str) -> Vec<(String, String)> {
    md.lines()
        .filter(|l| l.starts_with("| ") && !l.starts_with("| field |"))
        .map(|l| {
            let inner = &l[2..l.len() - 2];
            let (k, v) = inner.split_once(" | ").unwrap();
            (k.to_string(), v.to_string())
        })
        .collect()
}

/// The JSON answer as the md table writes it: keys in order, text values as written, lists joined by commas.
fn json_rows(line: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = line.trim();
    // keys in written order: read them with a streaming parse of the object
    let v: serde_json::Value = serde_json::from_str(rest).unwrap();
    let obj = v.as_object().unwrap();
    while let Some(start) = rest.find("\"") {
        let after = &rest[start + 1..];
        let end = after.find('"').unwrap();
        let key = &after[..end];
        let tail = &after[end + 1..];
        if tail.starts_with(':') && obj.contains_key(key) && !out.iter().any(|(k, _)| k == key) {
            let value = match &obj[key] {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(a) => a
                    .iter()
                    .map(|x| {
                        x.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| x.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                other => other.to_string(),
            };
            out.push((key.to_string(), value));
        }
        rest = tail;
    }
    out
}

#[test]
fn query_md_holds_the_json_answer_field_for_field_under_a_heading() {
    let (_d, bin) = sheet("query");
    let b = bin.to_str().unwrap();
    let (ok, json, _) = run(&["query", b, "1 rue des lilas ville", "-k", "1"]);
    assert!(ok);
    let (ok, md, _) = run(&[
        "query",
        b,
        "1 rue des lilas ville",
        "-k",
        "1",
        "--format",
        "md",
    ]);
    assert!(ok, "{md}");
    assert!(
        md.starts_with("## 1. 1 Rue des Lilas, 10000 Ville\n\n| field | value |\n|---|---|\n"),
        "{md}"
    );
    assert_eq!(table_rows(&md), json_rows(json.lines().next().unwrap()));
    assert!(
        !md.contains("weak answer"),
        "a firm answer carries no note: {md}"
    );
}

#[test]
fn a_weak_answer_says_so_in_words_in_its_heading_and_its_table() {
    let (_d, bin) = sheet("weak");
    let (ok, md, _) = run(&[
        "query",
        bin.to_str().unwrap(),
        "1 rue des lila",
        "-k",
        "1",
        "--format",
        "md",
    ]);
    assert!(ok);
    assert!(
        md.starts_with("## 1. 1 Rue des Lilas, 10000 Ville (weak)\n"),
        "{md}"
    );
    let rows = table_rows(&md);
    assert_eq!(
        rows[0],
        ("note".into(), "weak answer: check it before use".into())
    );
    assert!(rows.contains(&("weak".into(), "true".into())));
}

#[test]
fn reverse_md_and_no_answer_read_as_sentences_and_tables() {
    let (_d, bin) = sheet("reverse");
    let b = bin.to_str().unwrap();
    let (ok, json, _) = run(&["reverse", b, "43.73", "7.42", "-k", "2"]);
    assert!(ok);
    let (ok, md, _) = run(&["reverse", b, "43.73", "7.42", "-k", "2", "--format", "md"]);
    assert!(ok);
    assert_eq!(
        md.matches("\n## ").count() + usize::from(md.starts_with("## ")),
        json.lines().count()
    );
    let second = md.split("\n## 2. ").nth(1).unwrap();
    assert_eq!(table_rows(second), json_rows(json.lines().nth(1).unwrap()));
    let (_, none, _) = run(&["query", b, "qqqq zzzz", "--format", "md"]);
    assert_eq!(none, "No answer for \"qqqq zzzz\".\n");
}

#[test]
fn batch_md_is_one_table_with_a_note_column_and_a_pipe_kept_in_its_cell() {
    let (dir, bin) = sheet("batch");
    let input = dir.join("in3.csv");
    std::fs::write(
        &input,
        "id,address\n1,1 rue des lilas ville\n2,1 rue des lila\n3,qqqq zzzz\n4,\"Gate | 7, Bloc A\"\n",
    )
    .unwrap();
    let output = dir.join("out.md");
    let (ok, _, err) = run(&[
        "batch",
        bin.to_str().unwrap(),
        input.to_str().unwrap(),
        output.to_str().unwrap(),
        "--output-format",
        "md",
    ]);
    assert!(ok, "{err}");
    let md = std::fs::read_to_string(&output).unwrap();
    let lines: Vec<&str> = md.lines().collect();
    assert!(
        lines[0].starts_with("| id | address | gp_lat | gp_lon |")
            && lines[0].ends_with("| note |"),
        "{md}"
    );
    assert_eq!(lines.len(), 2 + 4, "{md}");
    assert!(lines[5].contains("| Gate \\| 7, Bloc A |"), "{md}");
    assert!(
        lines[3].ends_with("| weak answer: check it before use |"),
        "{md}"
    );
    assert!(
        lines[2].ends_with("|  |"),
        "a firm answer has an empty note: {md}"
    );
    // cell borders: every `|` that is not escaped as `\|`
    let cells = |l: &str| {
        let b = l.as_bytes();
        (0..b.len())
            .filter(|&i| b[i] == b'|' && (i == 0 || b[i - 1] != b'\\'))
            .count()
    };
    assert!(
        lines.iter().all(|l| cells(l) == cells(lines[0])),
        "every row as wide as the header: {md}"
    );
}

#[test]
fn batch_md_refuses_more_than_200_rows_before_writing_and_a_jsonl_input() {
    let (dir, bin) = sheet("limit");
    let b = bin.to_str().unwrap();
    let ok200 = dir.join("ok.csv");
    let big = dir.join("big.csv");
    let body = |n: usize| {
        (0..n)
            .map(|i| format!("{i},1 rue des lilas ville\n"))
            .collect::<String>()
    };
    std::fs::write(&ok200, format!("id,address\n{}", body(200))).unwrap();
    std::fs::write(&big, format!("id,address\n{}", body(201))).unwrap();
    let out = dir.join("ok.md");
    let (ok, _, err) = run(&[
        "batch",
        b,
        ok200.to_str().unwrap(),
        out.to_str().unwrap(),
        "--output-format",
        "md",
    ]);
    assert!(ok, "200 rows fit: {err}");
    assert_eq!(std::fs::read_to_string(&out).unwrap().lines().count(), 202);
    let out = dir.join("big.md");
    let (ok, _, err) = run(&[
        "batch",
        b,
        big.to_str().unwrap(),
        out.to_str().unwrap(),
        "--output-format",
        "md",
    ]);
    assert!(!ok);
    assert!(
        err.contains("this file has more than 200") && err.contains("--output-format csv"),
        "{err}"
    );
    assert!(!out.exists(), "nothing is written for a refused run");
    let jsonl = dir.join("in.jsonl");
    std::fs::write(&jsonl, "{\"q\":\"1 rue des lilas ville\"}\n").unwrap();
    let (ok, _, err) = run(&[
        "batch",
        b,
        jsonl.to_str().unwrap(),
        dir.join("j.md").to_str().unwrap(),
        "--output-format",
        "md",
    ]);
    assert!(!ok);
    assert!(err.contains("needs a CSV/TSV input"), "{err}");
}

#[test]
fn batch_md_counts_the_rows_it_writes_when_k_gives_several_answers_a_row() {
    let (dir, bin) = sheet("k");
    let input = dir.join("in.csv");
    // a point by the two houses: reverse with -k 2 answers both
    let body: String = (0..120).map(|i| format!("{i},43.73,7.42\n")).collect();
    std::fs::write(&input, format!("id,lat,lon\n{body}")).unwrap();
    let out = dir.join("k.md");
    let (ok, _, err) = run(&[
        "batch",
        bin.to_str().unwrap(),
        input.to_str().unwrap(),
        out.to_str().unwrap(),
        "--output-format",
        "md",
        "--reverse",
        "-k",
        "2",
    ]);
    assert!(!ok, "120 rows of two answers each are 240 table rows");
    assert!(err.contains("one row per answer (-k)"), "{err}");
    assert!(!out.exists());
}
