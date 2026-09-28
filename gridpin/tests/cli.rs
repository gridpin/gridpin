//! CLI integration tests: drive the built `gridpin` binary end to end. These cover
//! the batch paths (windowed streaming, malformed exit code) that unit tests cannot
//! reach — the coverage-gap map flagged them as never exercised.
use gridpin::builder;
use gridpin::query::Index;
use std::io::Write;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_gridpin");
const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

fn tmpdir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("gridpin-cli-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn build_sheet(dir: &std::path::Path) -> std::path::PathBuf {
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!("{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n"),
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
        "build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

#[test]
fn query_near_reaches_the_engine_and_injects_a_local_homonym() {
    // A parser-only test would stay green if Cmd::Query silently ignored `near`. Drive the real
    // binary over a sheet where the wanted homonym lies beyond the ordinary 300-row FST cap:
    // without --near it is absent; with --near it must become top-1 via spatial injection.
    let dir = tmpdir("query-near-e2e");
    let csv = dir.join("near.csv");
    let mut rows = String::from(HDR);
    for ordinal in 0..305u32 {
        let (insee, commune, lon, lat) = if ordinal == 304 {
            ("99999".to_string(), "Wanted".to_string(), 7.7455, 48.5839)
        } else {
            (
                format!("{ordinal:05}"),
                format!("Global {ordinal:03}"),
                2.0,
                43.0,
            )
        };
        rows.push_str(&format!(
            "markt,{insee},{},10000,1,,{lon:.4},{lat:.4},Markt,{commune}\n",
            commune.to_lowercase()
        ));
    }
    std::fs::write(&csv, rows).unwrap();
    let manifest = dir.join("near-meta.json");
    std::fs::write(
        &manifest,
        r#"{"country":"fr","layer":"addresses","license":"t","source_release":"test"}"#,
    )
    .unwrap();
    let sheet = dir.join("near.bin");
    let build = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            manifest.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = |near: bool| {
        let mut args = vec!["query", sheet.to_str().unwrap(), "1 markt", "-k", "100"];
        if near {
            args.extend(["--near", "48.5839,7.7455"]);
        }
        Command::new(BIN).args(args).output().unwrap()
    };
    let ordinary = run(false);
    assert!(ordinary.status.success());
    assert!(
        !String::from_utf8_lossy(&ordinary.stdout).contains("\"commune\":\"Wanted\""),
        "fixture must keep Wanted beyond the ordinary prefix cap"
    );
    let focused = run(true);
    assert!(
        focused.status.success(),
        "{}",
        String::from_utf8_lossy(&focused.stderr)
    );
    let first = String::from_utf8(focused.stdout)
        .unwrap()
        .lines()
        .next()
        .map(str::to_owned)
        .expect("focused query must return a row");
    let hit: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(hit["commune"], "Wanted");
}

#[test]
fn batch_malformed_input_exits_1_without_publishing() {
    // a semantic failure (a line with no usable "q") must NOT publish. The previous
    // output stays intact and no partial temp is left behind.
    let dir = tmpdir("malformed");
    let bin = build_sheet(&dir);
    let input = dir.join("q.jsonl");
    std::fs::write(&input, "{\"q\":\"rue a 1 ville\"}\n{\"junk\":1}\n").unwrap();
    let output = dir.join("out.jsonl");
    std::fs::write(&output, "PREVIOUS-OUTPUT\n").unwrap();
    let st = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(st.status.code(), Some(1), "malformed input must exit 1");
    assert!(String::from_utf8_lossy(&st.stderr).contains("no usable"));
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "PREVIOUS-OUTPUT\n",
        "the previous output must be kept, not replaced by an empty/partial file"
    );
    // no leftover temp (.out.jsonl.tmp.<pid>.<seq>)
    let leftover = std::fs::read_dir(&dir).unwrap().flatten().any(|e| {
        e.file_name()
            .to_string_lossy()
            .starts_with(".out.jsonl.tmp")
    });
    assert!(!leftover, "the partial temp must be cleaned up");
}

#[test]
fn batch_preserves_order_and_count_across_window_boundaries() {
    let dir = tmpdir("window");
    let bin = build_sheet(&dir);
    let input = dir.join("big.jsonl");
    let n = 2 * 65_536 + 3;
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&input).unwrap());
        for i in 0..n {
            writeln!(f, "{{\"q\":\"rue a 1 ville\",\"i\":{i}}}").unwrap();
        }
    }
    let output = dir.join("big_out.jsonl");
    let st = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "-k",
            "1",
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let lines = std::fs::read_to_string(&output).unwrap();
    assert_eq!(
        lines.lines().count(),
        n,
        "every input line must yield one output line across windows"
    );
}

#[test]
fn batch_blank_line_is_not_silently_dropped() {
    // a blank line among good lines used to be silently skipped, breaking the
    // input<->output line cardinality (a consumer zipping the two files would misalign). A blank
    // line now counts as malformed -> fail-closed (exit 1, nothing published, no leftover temp).
    let dir = tmpdir("blankline");
    let bin = build_sheet(&dir);
    let input = dir.join("in.jsonl");
    std::fs::write(
        &input,
        "{\"q\":\"rue a 1 ville\"}\n\n{\"q\":\"rue a 1 ville\"}\n",
    )
    .unwrap();
    let output = dir.join("out.jsonl");
    let st = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(
        st.status.code(),
        Some(1),
        "a blank line makes the batch malformed, not silently dropped"
    );
    assert!(!output.exists(), "a malformed batch publishes nothing");

    // and a clean 3-line file yields exactly 3 output lines (cardinality preserved)
    std::fs::write(
        &input,
        "{\"q\":\"rue a 1 ville\"}\n{\"q\":\"rue a 1 ville\"}\n{\"q\":\"rue a 1 ville\"}\n",
    )
    .unwrap();
    let st = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    assert_eq!(
        std::fs::read_to_string(&output).unwrap().lines().count(),
        3,
        "one output line per input line"
    );
}

#[test]
fn corrupt_sheet_query_never_aborts_the_process() {
    // a corrupt sheet must make the CLI exit in a CONTROLLED way (0 with an
    // empty answer, or 1 with an error) — never a panic-abort (101) or a signal (SIGABRT
    // = 134). Sweep u32-aligned corruptions and run a real `gridpin query` subprocess.
    let dir = tmpdir("corrupt");
    let bin = build_sheet(&dir);
    let good = std::fs::read(&bin).unwrap();
    let mut checked = 0;
    for off in (0..good.len().saturating_sub(4)).step_by(17) {
        let mut bytes = good.clone();
        for b in &mut bytes[off..off + 4] {
            *b = 0xFF;
        }
        let path = dir.join(format!("c-{off}.bin"));
        std::fs::write(&path, &bytes).unwrap();
        let st = Command::new(BIN)
            .args(["query", path.to_str().unwrap(), "rue a 1 ville", "-k", "1"])
            .output()
            .unwrap();
        let code = st.status.code();
        assert!(
            code == Some(0) || code == Some(1),
            "corrupt@{off}: expected a controlled exit 0/1, got {:?} (signal/abort = host crash)",
            st.status,
        );
        checked += 1;
    }
    assert!(
        checked > 5,
        "sweep should have exercised several corruptions"
    );
}

#[test]
#[cfg(unix)]
fn batch_refuses_a_hardlinked_input_and_preserves_it() {
    // input and output hardlinked to one inode have different paths, so the
    // canonicalize guard missed them and the input was zeroed. The dev+ino identity check
    // must refuse; and even so, the atomic temp+rename means the input keeps its bytes.
    let dir = tmpdir("hardlink");
    let bin = build_sheet(&dir);
    let input = dir.join("io.jsonl");
    std::fs::write(&input, "{\"q\":\"rue a 1 ville\"}\n").unwrap();
    let before = std::fs::read(&input).unwrap();
    let output = dir.join("out.jsonl");
    std::fs::hard_link(&input, &output).unwrap(); // output is a 2nd name for the same inode
    let st = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_ne!(
        st.status.code(),
        Some(0),
        "a hardlinked input==output must be refused"
    );
    assert_eq!(
        std::fs::read(&input).unwrap(),
        before,
        "the input must not be destroyed"
    );
}

#[test]
fn batch_error_midstream_keeps_the_previous_output() {
    // a read failure after some windows must not clobber an existing output.
    // Invalid UTF-8 in the input makes BufRead::lines() error mid-stream; the old output
    // must survive byte-for-byte because we only rename the temp on full success.
    let dir = tmpdir("partial");
    let bin = build_sheet(&dir);
    let input = dir.join("in.jsonl");
    // a valid line, then invalid UTF-8 (0xFF is not valid UTF-8) — lines() errors on it
    let mut bytes = b"{\"q\":\"rue a 1 ville\"}\n".to_vec();
    bytes.extend_from_slice(&[0xFF, 0xFE, b'\n']);
    std::fs::write(&input, &bytes).unwrap();
    let output = dir.join("out.jsonl");
    std::fs::write(&output, "PREVIOUS-OUTPUT\n").unwrap(); // a valuable prior result
    let st = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_ne!(
        st.status.code(),
        Some(0),
        "a mid-stream read error must fail"
    );
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "PREVIOUS-OUTPUT\n",
        "the previous output must survive an aborted batch"
    );
}

#[test]
fn batch_input_named_like_the_temp_is_not_destroyed() {
    // the temp used to be a fixed "<output>.tmp"; an input literally named
    // that collided and was truncated. The temp is now unique + create_new, so an input
    // named like the old temp survives and still produces correct output.
    let dir = tmpdir("tempname");
    let bin = build_sheet(&dir);
    let output = dir.join("result.jsonl");
    let input = dir.join("result.jsonl.tmp"); // == the OLD fixed temp path for this output
    std::fs::write(&input, "{\"q\":\"rue a 1 ville\"}\n").unwrap();
    let before = std::fs::read(&input).unwrap();
    let st = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "-k",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "batch should succeed: {}",
        String::from_utf8_lossy(&st.stderr)
    );
    assert_eq!(
        std::fs::read(&input).unwrap(),
        before,
        "the input must not be destroyed"
    );
    assert_eq!(
        std::fs::read_to_string(&output).unwrap().lines().count(),
        1,
        "one output line"
    );
}

#[test]
fn repack_rejects_a_corrupt_input_and_writes_no_output() {
    // a structurally-broken sheet (relabeled TOC id) must be REFUSED by repack,
    // not repacked into a "success" v7 that cannot open. No output file is produced.
    let dir = tmpdir("repackbad");
    let good = build_sheet(&dir);
    let mut bytes = std::fs::read(&good).unwrap();
    bytes[6] = 200; // relabel the first TOC entry to an unknown section id
    let input = dir.join("bad.bin");
    std::fs::write(&input, &bytes).unwrap();
    let output = dir.join("packed.bin");
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        r#"{"country":"mc","layer":"addresses","license":"t","source_release":"test"}"#,
    )
    .unwrap();
    let st = Command::new(BIN)
        .args([
            "repack",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!st.status.success(), "repacking a corrupt sheet must fail");
    assert!(
        !output.exists(),
        "no output must be written for a corrupt input"
    );
}

#[test]
fn repack_rejects_v6_without_creating_an_output() {
    let dir = tmpdir("repackv6");
    let v7 = build_sheet(&dir);
    let mut bytes = std::fs::read(&v7).unwrap();
    bytes[4] = 6;
    let input = dir.join("legacy-v6.bin");
    std::fs::write(&input, bytes).unwrap();
    let output = dir.join("must-not-exist.bin");
    let manifest = dir.join("m.json");

    let result = Command::new(BIN)
        .args([
            "repack",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "--meta",
            manifest.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!result.status.success(), "v6 must require a source rebuild");
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("must be rebuilt from source"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!output.exists(), "a rejected repack must publish no output");
}

#[test]
fn batch_over_a_corrupt_sheet_completes_every_line() {
    // a corrupt-but-openable sheet that panics INSIDE the fst crate on some query
    // must NOT kill the whole batch — the per-line panic boundary yields empty for that line and
    // the batch keeps going, producing one output line per input line with a controlled exit.
    let dir = tmpdir("batchcorrupt");
    let bin = build_sheet(&dir);
    let mut bytes = std::fs::read(&bin).unwrap();
    // flip bytes across the middle (the FST/postings region) — stays openable, may panic on query
    let mid = bytes.len() / 2;
    let end = (mid + 16).min(bytes.len());
    for b in &mut bytes[mid..end] {
        *b ^= 0xFF;
    }
    let corrupt = dir.join("corrupt.bin");
    std::fs::write(&corrupt, &bytes).unwrap();
    let input = dir.join("in.jsonl");
    std::fs::write(
        &input,
        "{\"q\":\"rue a\"}\n{\"q\":\"ville\"}\n{\"q\":\"rue a 1 ville\"}\n",
    )
    .unwrap();
    let output = dir.join("out.jsonl");
    let st = Command::new(BIN)
        .args([
            "batch",
            corrupt.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "-k",
            "1",
        ])
        .output()
        .unwrap();
    // a controlled exit (0 = completed; never a signal/abort 101/134)
    assert!(
        matches!(st.status.code(), Some(0) | Some(1)),
        "controlled exit, got {:?}",
        st.status
    );
    if st.status.success() {
        assert_eq!(
            std::fs::read_to_string(&output).unwrap().lines().count(),
            3,
            "the batch completed all lines despite corruption"
        );
    }
}

#[test]
fn batch_blank_street_does_not_hijack_freeform_q() {
    // a blank/whitespace "street" must be treated as ABSENT, so a valid free-form
    // "q" on the same line is NOT discarded into an empty structured result.
    let dir = tmpdir("m09");
    let bin = build_sheet(&dir);
    let input = dir.join("in.jsonl");
    std::fs::write(
        &input,
        "{\"q\":\"rue a 1 ville\",\"street\":\"\"}\n{\"q\":\"rue a 1 ville\",\"street\":\"   \"}\n{\"q\":\"rue a 1 ville\"}\n",
    )
    .unwrap();
    let output = dir.join("out.jsonl");
    let st = Command::new(BIN)
        .args([
            "batch",
            bin.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "-k",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "batch: {}",
        String::from_utf8_lossy(&st.stderr)
    );
    let lines: Vec<String> = std::fs::read_to_string(&output)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(lines.len(), 3);
    // all three route to the free-form path and produce the SAME non-empty result
    assert_eq!(
        lines[0], lines[2],
        "blank street must behave exactly like no street"
    );
    assert_eq!(
        lines[1], lines[2],
        "whitespace street must behave exactly like no street"
    );
    assert!(
        lines[2].contains("\"results\":[{"),
        "the free-form q must resolve, not return empty"
    );
}

#[test]
fn repack_input_named_like_the_temp_is_not_destroyed() {
    // the shared write_atomic used a fixed "<out>.tmp"; a repack whose INPUT is
    // named exactly that would have been truncated by File::create. The writer is now unique +
    // create_new, so the input survives and repack produces a valid re-openable sheet.
    let dir = tmpdir("repacktemp");
    let bin = build_sheet(&dir); // valid sheet at s.bin
    let output = dir.join("packed.bin");
    let input = dir.join("packed.bin.tmp"); // == the OLD fixed temp path for this output
    std::fs::copy(&bin, &input).unwrap();
    let before = std::fs::read(&input).unwrap();
    let man = dir.join("m2.json");
    std::fs::write(
        &man,
        r#"{"country":"mc","layer":"addresses","license":"t2","source_release":"test"}"#,
    )
    .unwrap();
    let st = Command::new(BIN)
        .args([
            "repack",
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "repack should succeed: {}",
        String::from_utf8_lossy(&st.stderr)
    );
    assert_eq!(
        std::fs::read(&input).unwrap(),
        before,
        "the input named like the temp must survive"
    );
    // the repacked output opens and answers (a valid v7 sheet)
    let q = Command::new(BIN)
        .args([
            "query",
            output.to_str().unwrap(),
            "rue a 1 ville",
            "-k",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        q.status.success() && !q.stdout.is_empty(),
        "repacked sheet must be queryable"
    );
}

#[test]
fn format_md_section_table_matches_the_code() {
    // the public FORMAT.md section table must not drift from src/index.rs SEC_* ids, and
    // the keys the writer stamps into SEC_META must be documented. A pure docs-contract gate.
    let md = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../docs-public/FORMAT.md"
    ))
    .expect("read FORMAT.md");
    let expected = [
        (1, "communes_fst"),
        (2, "communes_meta"),
        (3, "commune_postings"),
        (4, "streets_fst"),
        (5, "streets_meta"),
        (6, "house_blocks"),
        (7, "names"),
        (8, "reps"),
        (9, "cells"),
        (10, "parser"),
        (11, "rank"),
        (12, "words"),
        (13, "word_postings"),
        (14, "commune_coords"),
        (15, "rules"),
        (16, "mark"),
        (17, "meta"),
    ];
    assert_eq!(
        expected.len(),
        gridpin::index::N_SECTIONS,
        "the expected section list must cover exactly N_SECTIONS"
    );
    for (id, name) in expected {
        let needle = format!("| {id} | `{name}`");
        assert!(
            md.contains(&needle),
            "FORMAT.md section table is missing/renamed section {id} `{name}`"
        );
    }
    for key in [
        "meta_schema",
        "builder_version",
        "builder_target",
        "builder_git",
        "input_blake2b256",
    ] {
        assert!(
            md.contains(&format!("`{key}`")),
            "FORMAT.md must document the writer-stamped meta key `{key}`"
        );
    }
    // the names-blob doc must match the builder — a >255-byte name is REJECTED at build
    // (builder.rs push_name), not silently truncated. The doc previously promised truncation.
    let names_para = md
        .split("A blob of length-prefixed display strings")
        .nth(1)
        .and_then(|s| s.split("Other sections").next())
        .expect("FORMAT.md names-blob paragraph");
    assert!(
        names_para.contains("rejected at build time"),
        "FORMAT.md must say a >255-byte name is rejected at build time"
    );
    assert!(
        !names_para.contains("bytes are truncated"),
        "FORMAT.md must NOT claim >255-byte names ARE truncated (the builder rejects them)"
    );
}

#[test]
fn kill_during_build_never_publishes_a_corrupt_sheet() {
    // fault/kill acceptance: SIGKILL the builder at assorted moments and assert the
    // INVARIANT that must hold for ANY kill timing — the published path either still holds the
    // intact OLD sheet or a COMPLETE new one, and always opens cleanly; a partial write may exist
    // only as a hidden unique temp, never under the final name. The atomic publish is
    // write-temp -> fsync -> rename -> fsync(dir), so a kill can only land between whole steps.
    let dir = tmpdir("killbuild");
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        r#"{"country":"mc","layer":"addresses","license":"t","source_release":"test"}"#,
    )
    .unwrap();
    // the OLD published sheet, with a distinctive street
    let old_csv = dir.join("old.csv");
    std::fs::write(
        &old_csv,
        format!("{HDR}rue ancienne,001,ville,10000,1,,7.42,43.73,Rue Ancienne,Ville\n"),
    )
    .unwrap();
    let out = dir.join("out.bin");
    let st = Command::new(BIN)
        .args([
            "build",
            old_csv.to_str().unwrap(),
            out.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(st.status.success(), "old sheet builds");
    let old_bytes = std::fs::read(&out).unwrap();

    // a bigger NEW input so the rebuild has a real window to be killed in
    let new_csv = dir.join("new.csv");
    let mut rows = String::from(HDR);
    for n in 1..=30_000 {
        rows.push_str(&format!(
            "rue nouvelle,001,ville,10000,{n},,7.42,43.73,Rue Nouvelle,Ville\n"
        ));
    }
    std::fs::write(&new_csv, rows).unwrap();

    for delay_ms in [5u64, 25, 60, 120, 250] {
        let mut child = Command::new(BIN)
            .args([
                "build",
                new_csv.to_str().unwrap(),
                out.to_str().unwrap(),
                "--meta",
                man.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        let _ = child.kill(); // SIGKILL — no cleanup handlers run
        let _ = child.wait();

        // INVARIANT 1: the published path always opens cleanly (old or complete new, never junk)
        let q = Command::new(BIN)
            .args(["query", out.to_str().unwrap(), "rue"])
            .output()
            .unwrap();
        assert!(
            q.status.success(),
            "after a {delay_ms}ms kill the published sheet must still open cleanly"
        );
        // INVARIANT 2: whatever is at the final name is either the old bytes or a VALID new sheet
        let now = std::fs::read(&out).unwrap();
        if now != old_bytes {
            let m = Command::new(BIN)
                .args(["meta", out.to_str().unwrap()])
                .output()
                .unwrap();
            assert!(
                m.status.success(),
                "a replaced sheet must be complete (meta opens)"
            );
        }
        // INVARIANT 3: any leftover partial is a HIDDEN unique temp, never a visible artifact
        for e in std::fs::read_dir(&dir).unwrap() {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            let known = ["m.json", "old.csv", "new.csv", "out.bin"];
            if !known.contains(&name.as_str()) {
                assert!(
                    name.starts_with('.'),
                    "unexpected VISIBLE leftover after a {delay_ms}ms kill: {name}"
                );
            }
        }
    }

    // and an uninterrupted run publishes the new sheet, which answers for the new street
    let st = Command::new(BIN)
        .args([
            "build",
            new_csv.to_str().unwrap(),
            out.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(st.status.success(), "final clean rebuild succeeds");
    let q = Command::new(BIN)
        .args(["query", out.to_str().unwrap(), "rue nouvelle 7 ville"])
        .output()
        .unwrap();
    assert!(
        q.status.success() && !q.stdout.is_empty(),
        "the new sheet answers"
    );
}

#[test]
fn huge_reverse_k_is_capped_on_every_interface() {
    // reverse passed RAW k through (`-k usize::MAX` returned 15k+ rows on a real
    // sheet) while forward was already capped. Every public reverse entry must respect MAX_K.
    let dir = tmpdir("hugek");
    let csv = dir.join("many.csv");
    let mut body = String::from(HDR);
    for i in 0..300 {
        // 300 distinct streets around one point -> plenty of reverse candidates in the rings.
        // Zero-padded names keep the CSV in FST (lexicographic) order, as export_build guarantees.
        body.push_str(&format!(
            "rue n{i:03},001,ville,10000,1,,7.4{i:03},43.7{i:03},Rue N{i:03},Ville\n"
        ));
    }
    std::fs::write(&csv, body).unwrap();
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        r#"{"country":"mc","layer":"addresses","license":"t","source_release":"test"}"#,
    )
    .unwrap();
    let bin = dir.join("many.bin");
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
    // CLI reverse with an absurd k: output rows must be capped at MAX_K (100)
    let rev = Command::new(BIN)
        .args([
            "reverse",
            bin.to_str().unwrap(),
            "43.75",
            "7.45",
            "-k",
            "18446744073709551615", // usize::MAX
        ])
        .output()
        .unwrap();
    assert!(
        rev.status.success(),
        "{}",
        String::from_utf8_lossy(&rev.stderr)
    );
    let rows = String::from_utf8_lossy(&rev.stdout).lines().count();
    assert!(
        rows <= 100,
        "reverse must cap k at MAX_K=100, got {rows} rows"
    );
    assert!(rows > 0, "the cap must not silence real results");
}

#[test]
fn batch_diagnosis_observes_empty_and_nonempty_without_changing_results() {
    let dir = tmpdir("diagnosis-observer");
    let sheet = build_sheet(&dir);
    let input = dir.join("queries.jsonl");
    // Repeat alternating requests to exercise per-line isolation on Rayon workers.
    let queries = [
        r#"{"q":"1 rue a ville 10000"}"#,
        r#"{"q":"qzxwvutqqqq ville"}"#,
        r#"{"q":"qzxwvutqqqq zxqwvttqqqq"}"#,
        r#"{"street":"rue a","housenumber":"1","city":"ville","postcode":"10000"}"#,
    ];
    std::fs::write(
        &input,
        (0..32)
            .map(|i| queries[i % 4])
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let run = |name: &str, diagnose: bool, threads: &str| {
        let output = dir.join(name);
        let mut cmd = Command::new(BIN);
        cmd.args([
            "batch",
            sheet.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "-k",
            "3",
        ]);
        if diagnose {
            cmd.arg("--diagnose");
        }
        let status = cmd.env("GRIDPIN_THREADS", threads).output().unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        std::fs::read_to_string(output).unwrap()
    };
    let plain = run("plain.jsonl", false, "1");
    let diagnostic = run("diagnostic.jsonl", true, "4");
    let single = run("single.jsonl", true, "1");
    assert_eq!(
        diagnostic, single,
        "trace leaked between workers or requests"
    );
    assert_eq!(plain.lines().count(), 32);
    for (i, (before, after)) in plain.lines().zip(diagnostic.lines()).enumerate() {
        let value: serde_json::Value = serde_json::from_str(after).unwrap();
        let results = &value["results"];
        assert_eq!(
            before,
            serde_json::json!({"results": results}).to_string(),
            "results bytes changed at {i}"
        );
        let normal: serde_json::Value = serde_json::from_str(before).unwrap();
        assert_eq!(
            normal.as_object().unwrap().len(),
            1,
            "flag-off schema changed"
        );
        let trace = &value["diagnosis"];
        assert!(trace.is_object());
        let stage = trace["stop_stage"].as_str().expect("missing stop_stage");
        assert!(!stage.is_empty());
        if i % 4 == 0 || i % 4 == 3 {
            assert!(!results.as_array().unwrap().is_empty());
            assert_eq!(stage, "returned");
            assert!(trace["street_candidates"]["count"].as_u64().unwrap() > 0);
            assert!(
                trace["house_candidates"]["resolved_count"]
                    .as_u64()
                    .unwrap()
                    > 0
            );
            assert!(trace["before_threshold"]["count"].as_u64().unwrap() > 0);
            assert_eq!(results[0]["precision"], "house");
            assert_eq!(results[0]["housenumber"], "1");
        } else {
            assert!(results.as_array().unwrap().is_empty());
            assert_ne!(stage, "returned");
            assert_eq!(trace["street_candidates"]["count"], 0);
            assert_eq!(trace["before_threshold"]["count"], 0);
        }
    }
}

#[test]
fn prefix_place_centroid_excludes_distant_places_from_strongest_anchor() {
    let dir = tmpdir("prefix-place-centroid");
    let csv = dir.join("places.csv");
    let mut rows = String::from(HDR);
    // The strongest entry is neither first nor last in name order.
    for (code, place, count, lat, lon) in [
        ("001", "testoria alpha", 1, 43.0, 7.0),
        ("002", "testoria middle", 5, 48.0, 2.0),
        ("003", "testoria omega", 2, 45.0, 10.0),
    ] {
        for number in 1..=count {
            rows.push_str(&format!(
                "road,{code},{place},10000,{number},,{lon},{lat},Road,{place}\n"
            ));
        }
    }
    std::fs::write(&csv, rows).unwrap();
    let sheet = dir.join("places.bin");
    let build = Command::new(BIN)
        .args(["build", csv.to_str().unwrap(), sheet.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let query = Command::new(BIN)
        .args(["query", sheet.to_str().unwrap(), "testoria", "-k", "1"])
        .output()
        .unwrap();
    assert!(
        query.status.success(),
        "{}",
        String::from_utf8_lossy(&query.stderr)
    );
    let text = String::from_utf8(query.stdout).unwrap();
    let hit: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(hit["precision"], "city");
    assert!(
        (hit["lat"].as_f64().unwrap() - 48.0).abs() < 1e-9
            && (hit["lon"].as_f64().unwrap() - 2.0).abs() < 1e-9,
        "prefix centroid must equal the strongest anchor, not the mean of distant places: {hit}"
    );
}

#[test]
fn exact_place_anchor_survives_a_distant_stronger_prefix_group() {
    let dir = tmpdir("exact-place-anchor");
    let csv = dir.join("places.csv");
    let mut rows = String::from(HDR);
    for (code, place, count, lat, lon) in [
        ("001", "testoria minor", 2, 48.0, 2.0),
        ("002", "testoria major", 5, 43.0, 7.0),
        ("003", "umbrella", 10, 43.01, 7.01),
    ] {
        for number in 1..=count {
            rows.push_str(&format!(
                "road,{code},{place},10000,{number},,{lon},{lat},Road,{place}\n"
            ));
        }
    }
    std::fs::write(&csv, rows).unwrap();
    let sheet = dir.join("places.bin");
    let build = Command::new(BIN)
        .args(["build", csv.to_str().unwrap(), sheet.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    for (q, lat, lon) in [
        ("Unknown venue, testoria, testoria minor", 48.0, 2.0),
        // An exact, stronger umbrella still excludes the distant exact namesake.
        ("Unknown venue, testoria minor, umbrella", 43.01, 7.01),
    ] {
        let output = Command::new(BIN)
            .args(["query", sheet.to_str().unwrap(), q, "-k", "1"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let hit: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(hit["precision"], "city");
        assert!(
            (hit["lat"].as_f64().unwrap() - lat).abs() < 1e-9
                && (hit["lon"].as_f64().unwrap() - lon).abs() < 1e-9,
            "wrong place anchor for {q}: {hit}"
        );
    }
}

// Queries are existing development-corpus inputs; tiny sheets isolate the two
// resolution branches without depending on installed country data.
fn place_provenance_query(tag: &str, commune: &str, query: &str) -> serde_json::Value {
    let dir = tmpdir(tag);
    let csv = dir.join("places.csv");
    std::fs::write(
        &csv,
        format!("{HDR}road,001,{commune},10000,1,,2.0,48.0,Road,{commune}\n"),
    )
    .unwrap();
    let sheet = dir.join("places.bin");
    let build = Command::new(BIN)
        .args(["build", csv.to_str().unwrap(), sheet.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let out = Command::new(BIN)
        .args(["query", sheet.to_str().unwrap(), query, "-k", "1"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let hit: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("city hit")).unwrap();
    assert_eq!(hit["precision"], "city");
    assert_eq!(hit["lat"], 48.0);
    assert_eq!(hit["lon"], 2.0);
    hit
}

#[test]
fn place_provenance_exact_name() {
    // ES Wikidata row 1506: the fallback must carry the selected exact name.
    let hit = place_provenance_query(
        "place-exact-flag",
        "santa eulalia",
        "Santiago Ramon y Cajal, s/n, Santa Eul\u{00e0}lia",
    );
    assert_eq!(hit["flags"], serde_json::json!(["place_exact"]));
}

#[test]
fn place_provenance_prefix_group() {
    // FR Wikidata row 1496: the query names only the commune prefix.
    let hit = place_provenance_query("place-prefix-flag", "entremont le vieux", "Entremont");
    assert_eq!(hit["flags"], serde_json::json!(["place_prefix_group"]));
}

fn guard_drop_sheet(name: &str) -> std::path::PathBuf {
    let dir = tmpdir(name);
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!("{HDR}rue rivoli,001,ville,10000,1,,2.35,48.86,Rue Rivoli,Ville\n"),
    )
    .unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        r#"{"country":"fr","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let sheet = dir.join("sheet.bin");
    let built = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            meta.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    sheet
}

// The foreign Zqx street prevents the full query from bypassing the drop guards.
// After one drop Cannes yields a fuzzy local street; after two, Alpha Beta is exact.
fn guard_drop_longer_prefix_sheet(name: &str, houses: &[u32]) -> std::path::PathBuf {
    let dir = tmpdir(name);
    let csv = dir.join("in.csv");
    let mut rows = HDR.to_string();
    for (i, number) in houses.iter().enumerate() {
        rows.push_str(&format!(
            "alpha beta,06029,cannes,06400,{number},,{},{},Alpha Beta,Cannes\n",
            7.017 + i as f64 * 0.0001,
            43.553 + i as f64 * 0.0001,
        ));
    }
    rows.push_str(
        "rue de cannes,06029,cannes,06400,2,,6.971133,43.553354,Rue de Cannes,Cannes\n\
         zqx,75056,paris,75000,2,,2.35,48.86,Zqx,Paris\n",
    );
    std::fs::write(&csv, rows).unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        r#"{"country":"fr","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let sheet = dir.join("sheet.bin");
    let built = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            meta.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    sheet
}

fn guard_drop_assert_local_prefix_premise(
    hit: &serde_json::Value,
    street: &str,
    precision: &str,
    exact: bool,
) {
    assert_eq!(hit["street"], street, "{hit}");
    assert_eq!(hit["precision"], precision, "{hit}");
    let flags = hit["flags"].as_array().unwrap();
    for flag in [
        "commune_exact",
        "pc_exact",
        if exact {
            "street_exact"
        } else {
            "street_fuzzy"
        },
    ] {
        assert!(flags.iter().any(|f| f == flag), "missing {flag}: {hit}");
    }
    assert!(
        !flags
            .iter()
            .any(|f| f == "dropped_prefix" || f == "dropped_suffix"),
        "{hit}"
    );
}

fn guard_drop_longer_prefix_pair(
    name: &str,
    houses: &[u32],
    number: u32,
    street_query: &str,
    precision: &str,
    exact: bool,
    longer_wins: bool,
) {
    let sheet = guard_drop_longer_prefix_sheet(name, houses);
    let remainder = format!("{number} {street_query} 06400 Cannes");
    let first = guard_drop_query(&sheet, &format!("Cannes {remainder}"));
    let longer = guard_drop_query(&sheet, &remainder);
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(longer.len(), 1, "{longer:?}");
    guard_drop_assert_local_prefix_premise(
        &first[0],
        "Rue de Cannes",
        if number == 2 { "house" } else { "near" },
        false,
    );
    guard_drop_assert_local_prefix_premise(&longer[0], "Alpha Beta", precision, exact);
    let hits = guard_drop_query(&sheet, &format!("zqx Cannes {remainder}"));
    let mut expected = if longer_wins {
        longer[0].clone()
    } else {
        first[0].clone()
    };
    let confidence = expected["confidence"].as_f64().unwrap().min(0.6);
    expected["confidence"] = serde_json::json!(confidence);
    expected["flags"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("dropped_prefix"));
    assert_eq!(
        hits,
        vec![expected],
        "the first bypass must survive unless the longer drop passes the ordinary guard"
    );
}

#[test]
fn guard_drop_longer_prefix_exact_house_overrides_first_bypass() {
    guard_drop_longer_prefix_pair(
        "guard-drop-longer-house",
        &[2],
        2,
        "Alpha Beta",
        "house",
        true,
        true,
    );
}

#[test]
fn guard_drop_longer_prefix_fuzzy_house_keeps_first_bypass() {
    guard_drop_longer_prefix_pair(
        "guard-drop-longer-fuzzy",
        &[2],
        2,
        "Alpha Btea",
        "house",
        false,
        false,
    );
}

#[test]
fn guard_drop_longer_prefix_interpolation_keeps_first_bypass() {
    guard_drop_longer_prefix_pair(
        "guard-drop-longer-interp",
        &[1, 3],
        2,
        "Alpha Beta",
        "interp",
        true,
        false,
    );
}

#[test]
fn guard_drop_longer_prefix_nearest_house_keeps_first_bypass() {
    guard_drop_longer_prefix_pair(
        "guard-drop-longer-near",
        &[1, 99],
        50,
        "Alpha Beta",
        "near",
        true,
        false,
    );
}

// The first remainder finds Rue de Cannes through the locality bypass. Zqx in
// Paris blocks a direct answer to the full query, keeping the prefix loop live.
fn guard_drop_deferred_branch_sheet(
    name: &str,
    street_type: bool,
    umbrella: Option<&str>,
) -> std::path::PathBuf {
    let dir = tmpdir(name);
    let target = if street_type {
        "rue alpha beta"
    } else {
        "alpha beta"
    };
    let display = if street_type {
        "Rue Alpha Beta"
    } else {
        "Alpha Beta"
    };
    let mut rows = vec![
        format!("{target},06029,cannes,06400,2,,7.017,43.553,{display},Cannes"),
        "rue de cannes,06029,cannes,06400,2,,6.971133,43.553354,Rue de Cannes,Cannes".into(),
        "zqx,75056,paris,75000,2,,2.35,48.86,Zqx,Paris".into(),
    ];
    if let Some(commune) = umbrella {
        rows.push(format!(
            "omega,99001,{commune},99900,1,,2.4,48.9,Omega,{commune}"
        ));
    }
    rows.sort();
    let csv = dir.join("in.csv");
    std::fs::write(&csv, format!("{HDR}{}\n", rows.join("\n"))).unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        r#"{"country":"fr","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let sheet = dir.join("sheet.bin");
    let built = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            meta.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    sheet
}

fn guard_drop_deferred_branch_pair(
    name: &str,
    street_type: bool,
    umbrella: Option<&str>,
    exact: bool,
    longer_wins: bool,
    penalized: bool,
) {
    let sheet = guard_drop_deferred_branch_sheet(name, street_type, umbrella);
    let target = if street_type {
        "Rue Alpha Beta"
    } else {
        "Alpha Beta"
    };
    let query_street = if exact {
        target.to_string()
    } else {
        target.replace("Beta", "Btea")
    };
    let remainder = format!("{query_street} 06400 Cannes");
    let first = guard_drop_query(&sheet, &format!("Cannes {remainder}"));
    let longer = guard_drop_query(&sheet, &remainder);
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(longer.len(), 1, "{longer:?}");
    guard_drop_assert_local_prefix_premise(&first[0], "Rue de Cannes", "street", false);
    guard_drop_assert_local_prefix_premise(&longer[0], target, "street", exact);
    // Distinct public outcomes prevent a vacuous priority check; neither is a house.
    assert!(first[0]["housenumber"].is_null(), "{first:?}");
    assert!(longer[0]["housenumber"].is_null(), "{longer:?}");
    assert_eq!(first[0]["lat"], serde_json::json!(43.553354));
    assert_eq!(first[0]["lon"], serde_json::json!(6.971133));
    assert_eq!(longer[0]["lat"], serde_json::json!(43.553));
    assert_eq!(longer[0]["lon"], serde_json::json!(7.017));
    let hits = guard_drop_query(&sheet, &format!("zqx Cannes {remainder}"));
    let mut expected = if longer_wins {
        longer[0].clone()
    } else {
        first[0].clone()
    };
    if penalized {
        expected["confidence"] =
            serde_json::json!(expected["confidence"].as_f64().unwrap().min(0.6));
        expected["flags"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("dropped_prefix"));
    } else {
        // An exact umbrella must retain the direct answer's uncapped confidence.
        assert!(expected["confidence"].as_f64().unwrap() > 0.6, "{expected}");
    }
    assert_eq!(hits, vec![expected], "deferred prefix branch: {name}");
}

#[test]
fn guard_drop_deferred_exact_street_after_type_overrides_first_bypass() {
    guard_drop_deferred_branch_pair("guard-drop-deferred-type", true, None, true, true, true);
}

#[test]
fn guard_drop_deferred_fuzzy_street_after_type_keeps_first_bypass() {
    guard_drop_deferred_branch_pair(
        "guard-drop-deferred-type-fuzzy",
        true,
        None,
        false,
        false,
        true,
    );
}

#[test]
fn guard_drop_deferred_exact_commune_overrides_without_penalty() {
    guard_drop_deferred_branch_pair(
        "guard-drop-deferred-commune",
        false,
        Some("zqx cannes"),
        true,
        true,
        false,
    );
}

#[test]
fn guard_drop_deferred_prefix_only_commune_keeps_first_bypass() {
    guard_drop_deferred_branch_pair(
        "guard-drop-deferred-commune-prefix",
        false,
        Some("zqx cannes nord"),
        true,
        false,
        true,
    );
}

#[test]
fn guard_drop_deferred_second_locality_bypass_keeps_first() {
    guard_drop_deferred_branch_pair("guard-drop-deferred-second", false, None, true, false, true);
}

fn guard_drop_tail_priority_sheet(name: &str, with_house: bool) -> std::path::PathBuf {
    guard_drop_tail_locality_sheet(
        name,
        with_house.then_some(("37261", "tours", "37000", "Tours")),
    )
}

fn guard_drop_tail_locality_sheet(
    name: &str,
    house_locality: Option<(&str, &str, &str, &str)>,
) -> std::path::PathBuf {
    let dir = tmpdir(name);
    let csv = dir.join("in.csv");
    // N3 uses a prefix-only earlier locality (Tour), while the final Tours
    // still gives the prefix bypass an exact locality. Its fallback street
    // contains Tour, so the prefix bypass is reached before the tail pass.
    let prefix_only = house_locality.is_some_and(|(_, key, _, _)| key == "tours nord");
    let fallback = if prefix_only { "tour" } else { "tours" };
    let fallback_display = if prefix_only { "Tour" } else { "Tours" };
    let mut rows = format!(
        "{HDR}place gregoire de {fallback},37261,tours,37000,1,,0.695386,47.3957945,Place Gregoire de {fallback_display},Tours\n"
    );
    // The foreign street contains every word of the repeated-tail parse, so its
    // subset intersection fails the Tours locality filter. Neither variant may
    // answer before the drop guards; the shortened exact street remains local.
    if let Some((code, key, postcode, display)) = house_locality {
        rows.push_str(&format!(
            "rue victor hugo,{code},{key},{postcode},10,,0.688152,47.389005,Rue Victor Hugo,{display}\n",
        ));
    }
    rows.push_str(
        "rue victor hugo tour tours,75056,paris,75000,10,,2.35,48.86,Rue Victor Hugo Tour Tours,Paris\n",
    );
    rows.push_str(
        "rue victor hugo tours,75056,paris,75000,10,,2.35,48.86,Rue Victor Hugo Tours,Paris\n",
    );
    std::fs::write(&csv, rows).unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        r#"{"country":"fr","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let sheet = dir.join("sheet.bin");
    let built = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            meta.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    sheet
}

#[test]
fn guard_drop_defers_locality_bypass_to_exact_tail_house() {
    let sheet = guard_drop_tail_priority_sheet("guard-drop-tail-house", true);
    let hits = guard_drop_query(
        &sheet,
        "10 rue Victor Hugo, 37000 Tours, France, Tours, France",
    );
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit["precision"], "house", "{hit}");
    assert_eq!(hit["street"], "Rue Victor Hugo", "{hit}");
    assert_eq!(hit["housenumber"], "10", "{hit}");
    let flags = hit["flags"].as_array().unwrap();
    for flag in [
        "street_exact",
        "commune_exact",
        "pc_exact",
        "dropped_suffix",
    ] {
        assert!(flags.iter().any(|f| f == flag), "missing {flag}: {hit}");
    }
    assert!(!flags.iter().any(|f| f == "dropped_prefix"), "{hit}");
}

#[test]
fn guard_drop_keeps_locality_bypass_when_tail_has_no_house() {
    let sheet = guard_drop_tail_priority_sheet("guard-drop-tail-no-house", false);
    let hits = guard_drop_query(
        &sheet,
        "10 rue Victor Hugo, 37000 Tours, France, Tours, France",
    );
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit["precision"], "street", "{hit}");
    assert_eq!(hit["street"], "Place Gregoire de Tours", "{hit}");
    let flags = hit["flags"].as_array().unwrap();
    for flag in [
        "street_fuzzy",
        "commune_exact",
        "pc_exact",
        "dropped_prefix",
    ] {
        assert!(flags.iter().any(|f| f == flag), "missing {flag}: {hit}");
    }
    assert!(!flags.iter().any(|f| f == "dropped_suffix"), "{hit}");
}

fn guard_drop_assert_weak_tail_keeps_prefix(name: &str, locality: (&str, &str, &str, &str)) {
    let sheet = guard_drop_tail_locality_sheet(name, Some(locality));
    let query = if locality.1 == "tours nord" {
        "10 rue Victor Hugo, 37000 Tour, France, Tours, France"
    } else {
        "10 rue Victor Hugo, 37000 Tours, France, Tours, France"
    };
    let hits = guard_drop_query(&sheet, query);
    assert_eq!(hits.len(), 1, "{hits:?}");
    let hit = &hits[0];
    let fallback_street = if locality.1 == "tours nord" {
        "Place Gregoire de Tour"
    } else {
        "Place Gregoire de Tours"
    };
    assert_eq!(hit["street"], fallback_street, "{hit}");
    assert_eq!(hit["commune"], "Tours", "{hit}");
    assert_eq!(hit["postcode"], "37000", "{hit}");
    let flags = hit["flags"].as_array().unwrap();
    for flag in ["commune_exact", "pc_exact", "dropped_prefix"] {
        assert!(flags.iter().any(|f| f == flag), "missing {flag}: {hit}");
    }
    assert!(!flags.iter().any(|f| f == "dropped_suffix"), "{hit}");
}

#[test]
fn guard_drop_tail_exact_commune_department_postcode_keeps_prefix() {
    guard_drop_assert_weak_tail_keeps_prefix(
        "guard-drop-tail-commune-exact-pc-dept",
        ("37261", "tours", "37100", "Tours"),
    );
}

#[test]
fn guard_drop_tail_exact_commune_without_postcode_keeps_prefix() {
    guard_drop_assert_weak_tail_keeps_prefix(
        "guard-drop-tail-commune-exact-no-pc",
        ("37261", "tours", "0", "Tours"),
    );
}

#[test]
fn guard_drop_tail_prefix_commune_exact_postcode_keeps_prefix() {
    guard_drop_assert_weak_tail_keeps_prefix(
        "guard-drop-tail-commune-prefix-pc-exact",
        ("37262", "tours nord", "37000", "Tours Nord"),
    );
}

#[test]
fn guard_drop_tail_no_commune_department_postcode_keeps_prefix() {
    guard_drop_assert_weak_tail_keeps_prefix(
        "guard-drop-tail-no-commune-pc-dept",
        ("37263", "courtry", "37100", "Courtry"),
    );
}

fn guard_drop_de_subaddress_priority_sheet(name: &str, with_campus_e: bool) -> std::path::PathBuf {
    let dir = tmpdir(name);
    let csv = dir.join("in.csv");
    let mut rows = vec![
        "campus a,10041,saarbrucken,66123,11,,7.037,49.252,Campus A,Saarbrücken",
        "campus aufgang b,10041,saarbrucken,66123,3,,7.036,49.251,Campus Aufgang B,Saarbrücken",
        "campus b,10041,saarbrucken,66123,1,,7.037,49.252,Campus B,Saarbrücken",
        "campus b,10041,saarbrucken,66123,9,,7.039,49.253,Campus B,Saarbrücken",
        "campus c,10041,saarbrucken,66123,11,,7.041,49.254,Campus C,Saarbrücken",
        "campus d,10041,saarbrucken,66123,11,,7.043,49.255,Campus D,Saarbrücken",
    ];
    if with_campus_e {
        rows.push("campus e,10041,saarbrucken,66123,11,,7.045,49.256,Campus E,Saarbrücken");
    }
    std::fs::write(&csv, format!("{HDR}{}\n", rows.join("\n"))).unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        r#"{"country":"de","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let sheet = dir.join("sheet.bin");
    let rank = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ml/rank_v0.bin");
    let built = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            meta.to_str().unwrap(),
            "--rank",
            rank.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    sheet
}

#[test]
fn guard_drop_de_subaddress_answer_beats_locality_only_prefix_bypass() {
    let sheet = guard_drop_de_subaddress_priority_sheet("de-subaddress-priority", true);
    let observed = guard_drop_diagnose(&sheet, "Campus E1 5, 66123 Saarbrücken, Aufgang B 3. OG");
    let guards = observed["diagnosis"]["fallback_guards"]["examples"]
        .as_array()
        .unwrap();
    assert!(
        guards.iter().any(|g| g["stage"] == "prefix_drop"
            && g["evidence"]["house_exact"] == false
            && g["evidence"]["dropped_is_commune"] == false
            && g["evidence"]["street_after_type"] == false),
        "prefix bypass must actually be reached: {observed}"
    );
    let hits = observed["results"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0]["street"], "Campus E", "{hits:?}");
    assert_eq!(hits[0]["housenumber"], "11", "{hits:?}");
    assert!(
        hits[0]["flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "de_subaddress_tail"),
        "{hits:?}"
    );
    assert!(
        !hits[0]["flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "dropped_prefix"),
        "{hits:?}"
    );
}

// The parser model is material here: without it this fixture takes a prefix
// path and cannot observe the suffix guard's locality-only bypass.
fn guard_drop_de_modeled_sheet(name: &str, rows: &[&str]) -> std::path::PathBuf {
    let dir = tmpdir(name);
    let csv = dir.join("in.csv");
    let mut rows = rows.to_vec();
    rows.sort_unstable();
    std::fs::write(&csv, format!("{HDR}{}\n", rows.join("\n"))).unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        r#"{"country":"de","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let model = root.join("../ml/parser_v0.bin");
    let rank = root.join("../ml/rank_v0.bin");
    let sheet = dir.join("sheet.bin");
    let built = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            meta.to_str().unwrap(),
            "--model",
            model.to_str().unwrap(),
            "--rank",
            rank.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    sheet
}

fn guard_drop_diagnose(sheet: &std::path::Path, query: &str) -> serde_json::Value {
    let dir = sheet.parent().unwrap();
    let input = dir.join("diagnose-input.jsonl");
    let output = dir.join("diagnose-output.jsonl");
    std::fs::write(&input, format!("{}\n", serde_json::json!({"q": query}))).unwrap();
    let run = Command::new(BIN)
        .args([
            "batch",
            sheet.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "-k",
            "5",
            "--diagnose",
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    serde_json::from_str(&std::fs::read_to_string(output).unwrap()).unwrap()
}

#[test]
fn guard_drop_de_subaddress_beats_locality_only_suffix_at_guard_site() {
    let sheet = guard_drop_de_modeled_sheet(
        "guard-drop-de-suffix-site",
        &[
            "campus a,10041,saarbrucken,66123,11,,7.037,49.252,Campus A,Saarbrücken",
            "campus b,10041,saarbrucken,66123,1,,7.037,49.252,Campus B,Saarbrücken",
            "campus b,10041,saarbrucken,66123,9,,7.039,49.253,Campus B,Saarbrücken",
            "campus c,10041,saarbrucken,66123,11,,7.041,49.254,Campus C,Saarbrücken",
            "campus d,10041,saarbrucken,66123,11,,7.043,49.255,Campus D,Saarbrücken",
            "campus e,10041,saarbrucken,66123,11,,7.045,49.256,Campus E,Saarbrücken",
        ],
    );
    let observed = guard_drop_diagnose(&sheet, "Campus E1 5, 66123 Saarbrücken, Aufgang B 3. OG");
    let guards = observed["diagnosis"]["fallback_guards"]["examples"]
        .as_array()
        .unwrap();
    assert!(
        guards.iter().any(|g| {
            let evidence = &g["evidence"];
            let flags = evidence["top"]["flags"].as_array();
            g["stage"] == "suffix_drop"
                && evidence["exact_street_noncity"] == false
                && evidence["top"]["street"] == "Campus B"
                && flags.is_some_and(|fs| {
                    ["street_fuzzy", "commune_exact", "pc_exact"]
                        .iter()
                        .all(|f| fs.iter().any(|v| v == f))
                })
        }),
        "suffix bypass must actually be reached: {observed}"
    );
    let hits = observed["results"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{observed}");
    assert_eq!(hits[0]["street"], "Campus E", "{observed}");
    assert_eq!(hits[0]["housenumber"], "11", "{observed}");
    assert!(
        hits[0]["flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "de_subaddress_tail"),
        "{observed}"
    );
}

#[test]
fn guard_drop_de_exact_suffix_keeps_priority_at_guard_site() {
    let sheet = guard_drop_de_modeled_sheet(
        "guard-drop-de-exact-suffix-site",
        &[
            "an der havel,12063,ketzin,14669,7,,12.9,52.5,An der Havel,Ketzin",
            "rathausstrasse,12063,ketzin,14669,7,,12.8,52.4,Rathausstraße,Ketzin",
        ],
    );
    let observed = guard_drop_diagnose(&sheet, "Rathausstraße 7, 14669 Ketzin/Havel");
    let guards = observed["diagnosis"]["fallback_guards"]["examples"]
        .as_array()
        .unwrap();
    assert!(
        guards.iter().any(|g| g["stage"] == "suffix_drop"
            && g["evidence"]["exact_street_noncity"] == true
            && g["evidence"]["top"]["street"] == "Rathausstraße"),
        "exact suffix must actually be reached: {observed}"
    );
    let hit = &observed["results"][0];
    assert_eq!(hit["street"], "Rathausstraße", "{observed}");
    assert_eq!(hit["housenumber"], "7", "{observed}");
    assert_eq!(hit["precision"], "house", "{observed}");
    for flag in [
        "street_exact",
        "commune_exact",
        "pc_exact",
        "dropped_suffix",
    ] {
        assert!(
            hit["flags"].as_array().unwrap().iter().any(|f| f == flag),
            "exact suffix must retain {flag}: {observed}"
        );
    }
}

#[test]
fn guard_drop_de_exact_prefix_keeps_priority_at_guard_site() {
    let sheet = guard_drop_de_modeled_sheet(
        "guard-drop-de-exact-prefix-site",
        &[
            "dorfstrasse,10553,gransee,16775,29,,10.0553,50.0553,Dorfstraße,Gransee",
            "unter den linden,10813,frankfurt am main,0,13,,10.0813,50.0813,Unter den Linden,Frankfurt am Main",
        ],
    );
    let observed = guard_drop_diagnose(
        &sheet,
        "für den Empfang, Meseberger Dorfstraße 29, 16775 Gransee",
    );
    let guards = observed["diagnosis"]["fallback_guards"]["examples"]
        .as_array()
        .unwrap();
    assert!(
        guards.iter().any(|g| g["stage"] == "prefix_drop"
            && g["evidence"]["house_exact"] == true
            && g["evidence"]["dropped_is_commune"] == false
            && g["evidence"]["top"]["street"] == "Dorfstraße"),
        "exact prefix must actually be reached: {observed}"
    );
    let hit = &observed["results"][0];
    assert_eq!(hit["street"], "Dorfstraße", "{observed}");
    assert_eq!(hit["housenumber"], "29", "{observed}");
    assert_eq!(hit["precision"], "house", "{observed}");
    for flag in [
        "street_exact",
        "commune_exact",
        "pc_exact",
        "dropped_prefix",
    ] {
        assert!(
            hit["flags"].as_array().unwrap().iter().any(|f| f == flag),
            "exact prefix must retain {flag}: {observed}"
        );
    }
}

#[test]
fn guard_drop_accepts_fuzzy_street_with_exact_commune_and_postcode() {
    let sheet = guard_drop_sheet("guard-drop-both-exact");
    for (query, dropped) in [
        ("zqx rue rivloi 1 10000 ville", "dropped_prefix"),
        ("rue rivloi 1 10000 ville zqx", "dropped_suffix"),
    ] {
        let out = Command::new(BIN)
            .args(["query", sheet.to_str().unwrap(), query, "-k", "1"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "{query}: exact locality must allow the fuzzy candidate"
        );
        let hit: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(hit["street"], "Rue Rivoli", "{query}");
        assert_eq!(hit["commune"], "Ville", "{query}");
        assert_eq!(hit["postcode"], "10000", "{query}");
        assert_eq!(hit["precision"], "house", "{query}");
        assert_eq!(hit["housenumber"], "1", "{query}");
        let flags = hit["flags"].as_array().unwrap();
        for flag in ["street_fuzzy", "commune_exact", "pc_exact", dropped] {
            assert!(
                flags.iter().any(|f| f == flag),
                "{query}: missing {flag}: {hit}"
            );
        }
        assert!(
            !flags.iter().any(|f| f == "street_exact"),
            "{query}: fixture must stay fuzzy"
        );
        assert!(
            hit["confidence"].as_f64().unwrap() <= 0.6,
            "{query}: drop must remain capped"
        );
    }
}

#[test]
fn guard_drop_rejects_fuzzy_street_with_exact_commune_without_postcode() {
    let sheet = guard_drop_sheet("guard-drop-commune-only");
    for query in ["zqx rue rivloi 1 ville", "rue rivloi 1 ville zqx"] {
        let out = Command::new(BIN)
            .args(["query", sheet.to_str().unwrap(), query, "-k", "1"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty(),
            "{query}: a commune alone must not bypass the guard: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn guard_drop_rejects_fuzzy_street_with_exact_commune_and_department_postcode() {
    let sheet = guard_drop_sheet("guard-drop-commune-dept");
    for query in [
        "zqx rue rivloi 1 10001 ville",
        "rue rivloi 1 10001 ville zqx",
    ] {
        let out = Command::new(BIN)
            .args(["query", sheet.to_str().unwrap(), query, "-k", "1"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty(),
            "{query}: a department-only postcode must not bypass the guard: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

fn guard_drop_typefree_sheet(name: &str) -> std::path::PathBuf {
    let dir = tmpdir(name);
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!(
            "{HDR}alpha beta,001,ville,10000,1,,2.35,48.86,Alpha Beta,Ville\n\
             road,002,umbrella,20000,1,,4.35,45.86,Road,Umbrella\n"
        ),
    )
    .unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        r#"{"country":"fr","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let sheet = dir.join("sheet.bin");
    let built = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            meta.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    sheet
}

fn guard_drop_query(sheet: &std::path::Path, query: &str) -> Vec<serde_json::Value> {
    let out = Command::new(BIN)
        .args(["query", sheet.to_str().unwrap(), query, "-k", "1"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn guard_drop_rejects_exact_street_level_without_house_or_type() {
    let sheet = guard_drop_typefree_sheet("guard-drop-typefree-street");
    let direct = guard_drop_query(&sheet, "alpha beta");
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0]["precision"], "street");
    assert_eq!(direct[0]["street"], "Alpha Beta");
    assert!(direct[0]["flags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f == "street_exact"));
    // The street is exact, but has neither a requested house nor a street-type word.
    for query in ["zqx alpha beta", "zqx qzx alpha beta"] {
        let hits = guard_drop_query(&sheet, query);
        assert!(
            hits.is_empty(),
            "{query}: an untyped street cannot justify dropping a prefix: {hits:?}"
        );
    }
}

#[test]
fn guard_drop_rejects_exact_postcode_with_near_commune() {
    let sheet = guard_drop_sheet("guard-drop-postcode-near-commune");
    let direct = guard_drop_query(&sheet, "rue rivloi 1 10000");
    assert_eq!(direct.len(), 1);
    let flags = direct[0]["flags"].as_array().unwrap();
    assert!(flags.iter().any(|f| f == "street_fuzzy"));
    assert!(flags.iter().any(|f| f == "pc_exact"));
    assert!(!flags.iter().any(|f| f == "commune_exact"));
    // Villo is a near miss of Ville, not an omitted locality or an exact commune.
    for query in ["villo rue rivloi 1 10000", "rue rivloi 1 10000 villo"] {
        let hits = guard_drop_query(&sheet, query);
        assert!(
            hits.is_empty(),
            "{query}: an exact postcode alone cannot bypass the guard: {hits:?}"
        );
    }
}

#[test]
fn guard_drop_never_discards_numeric_suffix() {
    let sheet = guard_drop_typefree_sheet("guard-drop-numeric-tail");
    let word_tail = guard_drop_query(&sheet, "alpha beta zqx");
    assert_eq!(word_tail.len(), 1);
    assert_eq!(word_tail[0]["precision"], "street");
    let flags = word_tail[0]["flags"].as_array().unwrap();
    assert!(flags.iter().any(|f| f == "street_exact"));
    assert!(flags.iter().any(|f| f == "dropped_suffix"));
    // A numeric postcode is address evidence, unlike the disposable word above.
    for query in ["alpha beta 99999", "alpha beta ville 99999"] {
        let hits = guard_drop_query(&sheet, query);
        assert!(
            hits.is_empty(),
            "{query}: a numeric suffix must not be dropped: {hits:?}"
        );
    }
}

#[test]
fn guard_drop_accepts_real_umbrella_commune_without_noise_penalty() {
    let sheet = guard_drop_typefree_sheet("guard-drop-real-umbrella");
    let dir = sheet.parent().unwrap();
    let input = dir.join("queries.jsonl");
    let output = dir.join("answers.jsonl");
    std::fs::write(&input, "{\"q\":\"umbrella alpha beta\"}\n").unwrap();
    let run = Command::new(BIN)
        .args([
            "batch",
            sheet.to_str().unwrap(),
            input.to_str().unwrap(),
            output.to_str().unwrap(),
            "-k",
            "1",
            "--diagnose",
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let text = std::fs::read_to_string(output).unwrap();
    let answer: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    let hits = answer["results"].as_array().unwrap();
    assert_eq!(
        hits.len(),
        1,
        "a real umbrella commune must preserve the street: {answer}"
    );
    assert_eq!(hits[0]["street"], "Alpha Beta");
    assert_eq!(hits[0]["commune"], "Ville");
    assert_eq!(hits[0]["precision"], "street");
    let flags = hits[0]["flags"].as_array().unwrap();
    assert!(flags.iter().any(|f| f == "street_exact"));
    assert!(!flags
        .iter()
        .any(|f| f == "dropped_prefix" || f == "dropped_suffix"));
    let guards = answer["diagnosis"]["fallback_guards"]["examples"]
        .as_array()
        .unwrap();
    assert!(
        guards.iter().any(|g| {
            g["stage"] == "prefix_drop"
                && g["accepted"] == true
                && g["evidence"]["dropped_is_commune"] == true
                && g["evidence"]["house_exact"] == false
                && g["evidence"]["street_after_type"] == false
        }),
        "the real-commune branch, not another attempt, must accept: {answer}"
    );
    let ordinary_output = dir.join("ordinary-answers.jsonl");
    let ordinary_run = Command::new(BIN)
        .args([
            "batch",
            sheet.to_str().unwrap(),
            input.to_str().unwrap(),
            ordinary_output.to_str().unwrap(),
            "-k",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        ordinary_run.status.success(),
        "{}",
        String::from_utf8_lossy(&ordinary_run.stderr)
    );
    let ordinary_text = std::fs::read_to_string(ordinary_output).unwrap();
    let ordinary: serde_json::Value = serde_json::from_str(ordinary_text.trim()).unwrap();
    assert_eq!(
        ordinary["results"], answer["results"],
        "diagnosis must not change the returned hits"
    );
}

// The drop guard must use the first locality candidate even when a valid rank
// section puts an exact house on a fuzzy, differently localized street first.
fn guard_drop_ordered_sheet(name: &str) -> std::path::PathBuf {
    let dir = tmpdir(name);
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!(
            "{HDR}rue rivoli,001,ville,10000,2,,2.3502,48.8602,Rue Rivoli,Ville\n\
             rue rivoli ville,002,autre,10001,1,,2.35,48.86,Rue Rivoli Ville,Autre\n"
        ),
    )
    .unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        r#"{"country":"fr","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    // Public --rank input, not a patched implementation. At house 1 the exact
    // house scores 9 and the localized near-snap 7. At house 2 their order flips.
    // Feature order: street exact/fuzzy, commune exact/prefix, postcode exact/dept,
    // parser, house found/exact suffix, number present.
    let weights = [3.0f32, 2.0, 3.0, 2.0, 2.0, 0.0, 0.0, 0.0, 6.0, 0.0];
    let mut rank = b"GPRK".to_vec();
    rank.push(weights.len() as u8);
    rank.extend_from_slice(&0.0f32.to_le_bytes());
    for weight in weights {
        rank.extend_from_slice(&weight.to_le_bytes());
    }
    let rank_path = dir.join("rank.bin");
    std::fs::write(&rank_path, rank).unwrap();
    let sheet = dir.join("sheet.bin");
    let built = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            meta.to_str().unwrap(),
            "--rank",
            rank_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    sheet
}

fn guard_drop_query_k2(sheet: &std::path::Path, query: &str) -> Vec<serde_json::Value> {
    // k=1 cannot distinguish first() from any(): both must see two candidates.
    let out = Command::new(BIN)
        .args(["query", sheet.to_str().unwrap(), query, "-k", "2"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn guard_drop_first_locality_pair_k2(name: &str, prefix: bool) {
    let sheet = guard_drop_ordered_sheet(name);
    let has_flag = |hit: &serde_json::Value, flag: &str| {
        hit["flags"].as_array().unwrap().iter().any(|f| f == flag)
    };
    let direct = guard_drop_query_k2(&sheet, "rue rivloi 1 10000 ville");
    assert_eq!(direct.len(), 2, "the near miss needs the second candidate");
    assert_eq!(direct[0]["street"], "Rue Rivoli Ville");
    assert_eq!(direct[0]["commune"], "Autre");
    assert_eq!(direct[0]["precision"], "house");
    assert_eq!(direct[1]["street"], "Rue Rivoli");
    assert_eq!(direct[1]["commune"], "Ville");
    assert_eq!(direct[1]["precision"], "near");
    assert!(direct[0]["score"].as_f64().unwrap() > direct[1]["score"].as_f64().unwrap());
    for hit in &direct {
        assert!(has_flag(hit, "street_fuzzy"));
        assert!(!has_flag(hit, "street_exact"));
    }
    for flag in ["commune_exact", "pc_exact"] {
        assert!(
            !has_flag(&direct[0], flag),
            "first candidate has {flag}: {direct:?}"
        );
        assert!(
            has_flag(&direct[1], flag),
            "second candidate lacks {flag}: {direct:?}"
        );
    }
    let (yes, no, dropped) = if prefix {
        (
            "zqx rue rivloi 2 10000 ville",
            "zqx rue rivloi 1 10000 ville",
            "dropped_prefix",
        )
    } else {
        (
            "rue rivloi 2 10000 ville zqx",
            "rue rivloi 1 10000 ville zqx",
            "dropped_suffix",
        )
    };
    let accepted = guard_drop_query_k2(&sheet, yes);
    assert_eq!(accepted.len(), 2, "{yes}: {accepted:?}");
    assert_eq!(accepted[0]["commune"], "Ville");
    assert_eq!(accepted[0]["housenumber"], "2");
    for flag in ["street_fuzzy", "commune_exact", "pc_exact", dropped] {
        assert!(
            has_flag(&accepted[0], flag),
            "{yes}: missing {flag}: {accepted:?}"
        );
    }
    assert!(accepted.iter().all(|hit| !has_flag(hit, "street_exact")));
    let rejected = guard_drop_query_k2(&sheet, no);
    assert!(
        rejected.is_empty(),
        "{no}: a later locality match must not authorize the first: {rejected:?}"
    );
}

#[test]
fn guard_drop_prefix_uses_only_first_locality_candidate_k2() {
    guard_drop_first_locality_pair_k2("guard-drop-prefix-first-k2", true);
}

#[test]
fn guard_drop_suffix_uses_only_first_locality_candidate_k2() {
    guard_drop_first_locality_pair_k2("guard-drop-suffix-first-k2", false);
}

#[test]
fn guard_drop_rejects_interpolation_on_exact_untyped_street() {
    let dir = tmpdir("guard-drop-exact-interpolation");
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!(
            "{HDR}alpha beta,001,ville,10000,1,,2.35,48.86,Alpha Beta,Ville\n\
             alpha beta,001,ville,10000,3,,2.3502,48.8602,Alpha Beta,Ville\n"
        ),
    )
    .unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        r#"{"country":"fr","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let sheet = dir.join("sheet.bin");
    let built = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            meta.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    // The same k=1 and street are used throughout. The missing number is bracketed
    // by nearby houses, so this tests interpolation, not a missing street or house.
    let direct = guard_drop_query(&sheet, "alpha beta 2");
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0]["precision"], "interp");
    assert_eq!(direct[0]["housenumber"], "2");
    assert_eq!(direct[0]["flags"], serde_json::json!(["street_exact"]));
    let accepted = guard_drop_query(&sheet, "zqx alpha beta 1");
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0]["precision"], "house");
    assert_eq!(accepted[0]["street"], "Alpha Beta");
    assert_eq!(accepted[0]["housenumber"], "1");
    assert_eq!(
        accepted[0]["flags"],
        serde_json::json!(["street_exact", "house_rep", "dropped_prefix"])
    );
    let rejected = guard_drop_query(&sheet, "zqx alpha beta 2");
    assert!(
        rejected.is_empty(),
        "interpolation must not justify dropping noise: {rejected:?}"
    );
}

#[test]
fn guard_drop_rejects_nearest_house_snap_on_exact_untyped_street() {
    let dir = tmpdir("guard-drop-exact-nearest-house");
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!(
            "{HDR}alpha beta,001,ville,10000,1,,2.35,48.86,Alpha Beta,Ville\n\
             alpha beta,001,ville,10000,99,,2.36,48.87,Alpha Beta,Ville\n"
        ),
    )
    .unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        r#"{"country":"fr","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let sheet = dir.join("sheet.bin");
    let built = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            meta.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    // The number gap prevents interpolation. The direct query fixes the near-snap
    // precondition, so a changed interpolation rule cannot make this vacuous.
    let direct = guard_drop_query(&sheet, "alpha beta 50");
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0]["precision"], "near");
    assert_eq!(direct[0]["housenumber"], "1");
    assert_eq!(direct[0]["flags"], serde_json::json!(["street_exact"]));
    assert_eq!(direct[0]["street"], "Alpha Beta");
    assert_eq!(direct[0]["score"], 3.0);
    let accepted = guard_drop_query(&sheet, "zqx alpha beta 1");
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0]["precision"], "house");
    assert_eq!(accepted[0]["street"], "Alpha Beta");
    assert_eq!(accepted[0]["housenumber"], "1");
    assert_eq!(
        accepted[0]["flags"],
        serde_json::json!(["street_exact", "house_rep", "dropped_prefix"])
    );
    let rejected = guard_drop_query(&sheet, "zqx alpha beta 50");
    assert!(
        rejected.is_empty(),
        "a nearest-house snap must not justify dropping noise: {rejected:?}"
    );
}

fn guard_drop_commune_prefix_pair(name: &str, prefix: bool) {
    let dir = tmpdir(name);
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!(
            "{HDR}rue rivoli,001,villeneuve,10000,1,,2.35,48.86,Rue Rivoli,Villeneuve\n\
             rue rivoli,001,villeneuve,10000,2,,2.3502,48.8602,Rue Rivoli,Villeneuve\n"
        ),
    )
    .unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        r#"{"country":"fr","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let sheet = dir.join("sheet.bin");
    let built = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            meta.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let has_flag = |hit: &serde_json::Value, flag: &str| {
        hit["flags"].as_array().unwrap().iter().any(|f| f == flag)
    };
    // Keep the near miss reachable: a fuzzy street, prefix-only commune and
    // exact postcode. Neither an exact street nor a missing commune can stand in.
    let direct = guard_drop_query(&sheet, "rue rivloi 1 10000 ville");
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0]["precision"], "house");
    assert_eq!(direct[0]["street"], "Rue Rivoli");
    assert_eq!(direct[0]["commune"], "Villeneuve");
    assert_eq!(direct[0]["postcode"], "10000");
    assert_eq!(direct[0]["housenumber"], "1");
    for flag in ["street_fuzzy", "commune_prefix", "pc_exact"] {
        assert!(has_flag(&direct[0], flag), "missing {flag}: {direct:?}");
    }
    assert!(!has_flag(&direct[0], "street_exact"));
    assert!(!has_flag(&direct[0], "commune_exact"));
    let (positive, negative, dropped) = if prefix {
        (
            "zqx rue rivloi 1 10000 villeneuve",
            "zqx rue rivloi 1 10000 ville",
            "dropped_prefix",
        )
    } else {
        (
            "rue rivloi 1 10000 villeneuve zqx",
            "rue rivloi 1 10000 ville zqx",
            "dropped_suffix",
        )
    };
    let accepted = guard_drop_query(&sheet, positive);
    assert_eq!(accepted.len(), 1, "{positive}: {accepted:?}");
    assert_eq!(accepted[0]["precision"], "house");
    assert_eq!(accepted[0]["commune"], "Villeneuve");
    assert_eq!(accepted[0]["housenumber"], "1");
    for flag in ["street_fuzzy", "commune_exact", "pc_exact", dropped] {
        assert!(has_flag(&accepted[0], flag), "missing {flag}: {accepted:?}");
    }
    assert!(!has_flag(&accepted[0], "street_exact"));
    assert!(!has_flag(&accepted[0], "commune_prefix"));
    let rejected = guard_drop_query(&sheet, negative);
    assert!(
        rejected.is_empty(),
        "{negative}: a prefix-only commune must not justify dropping noise: {rejected:?}"
    );
}

#[test]
fn guard_drop_prefix_rejects_prefix_only_commune_with_exact_postcode() {
    guard_drop_commune_prefix_pair("guard-drop-prefix-commune-prefix", true);
}

#[test]
fn guard_drop_suffix_rejects_prefix_only_commune_with_exact_postcode() {
    guard_drop_commune_prefix_pair("guard-drop-suffix-commune-prefix", false);
}

// Public CSV and rank inputs keep the weaker, exact-house tail first. The
// foreign longest street blocks the full-query subset path before the guards.
fn guard_drop_tail_choice_sheet(name: &str, later_exact_tail: bool) -> std::path::PathBuf {
    let dir = tmpdir(name);
    let (number, weak_tail, foreign_tail) = if later_exact_tail {
        (10, "Tours Tours", "Tours Tours Tours")
    } else {
        (11, "Tours", "Tours Tours")
    };
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!(
            "{HDR}place gregoire de tours,37261,tours,37000,1,,0.695386,47.3957945,Place Gregoire de Tours,Tours\n\
             rue victor hugo,37261,tours,37000,{number},,0.688252,47.389105,Rue Victor Hugo,Tours\n\
             rue victor hugo {weak_key},37263,courtry,37100,10,,0.688152,47.389005,Rue Victor Hugo {weak_tail},Courtry\n\
             rue victor hugo {foreign_key},75056,paris,75000,10,,2.35,48.86,Rue Victor Hugo {foreign_tail},Paris\n",
            weak_key = weak_tail.to_lowercase(),
            foreign_key = foreign_tail.to_lowercase(),
        ),
    )
    .unwrap();
    let meta = dir.join("meta.json");
    std::fs::write(
        &meta,
        r#"{"country":"fr","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let weights = [3.0f32, 2.0, 3.0, 2.0, 2.0, 0.0, 0.0, 0.0, 6.0, 0.0];
    let mut rank = b"GPRK".to_vec();
    rank.push(weights.len() as u8);
    rank.extend_from_slice(&0.0f32.to_le_bytes());
    for weight in weights {
        rank.extend_from_slice(&weight.to_le_bytes());
    }
    let rank_path = dir.join("rank.bin");
    std::fs::write(&rank_path, rank).unwrap();
    let sheet = dir.join("sheet.bin");
    let built = Command::new(BIN)
        .args([
            "build",
            csv.to_str().unwrap(),
            sheet.to_str().unwrap(),
            "--meta",
            meta.to_str().unwrap(),
            "--rank",
            rank_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    sheet
}

fn guard_drop_assert_saved_tail_prefix(hits: &[serde_json::Value]) {
    assert_eq!(hits.len(), 1, "{hits:?}");
    let hit = &hits[0];
    assert_eq!(hit["street"], "Place Gregoire de Tours", "{hit}");
    assert_eq!(hit["precision"], "street", "{hit}");
    assert_eq!(hit["commune"], "Tours", "{hit}");
    assert_eq!(hit["postcode"], "37000", "{hit}");
    let flags = hit["flags"].as_array().unwrap();
    for flag in [
        "street_fuzzy",
        "commune_exact",
        "pc_exact",
        "dropped_prefix",
    ] {
        assert!(flags.iter().any(|f| f == flag), "missing {flag}: {hit}");
    }
    assert!(!flags.iter().any(|f| f == "dropped_suffix"), "{hit}");
}

#[test]
fn guard_drop_tail_uses_first_locality_candidate_k2() {
    let sheet = guard_drop_tail_choice_sheet("guard-drop-tail-first-locality-k2", false);
    // This is the first shortened query. The exact house outranks the nearby
    // house with exact locality; any() must not borrow the runner-up's flags.
    let direct = guard_drop_query_k2(&sheet, "10 rue Victor Hugo 37000 Tours");
    assert_eq!(direct.len(), 2, "the near miss needs the second candidate");
    assert_eq!(direct[0]["street"], "Rue Victor Hugo Tours");
    assert_eq!(direct[0]["commune"], "Courtry");
    assert_eq!(direct[0]["precision"], "house");
    assert_eq!(direct[0]["housenumber"], "10");
    assert_eq!(direct[1]["street"], "Rue Victor Hugo");
    assert_eq!(direct[1]["commune"], "Tours");
    assert_eq!(direct[1]["precision"], "near");
    assert_eq!(direct[1]["housenumber"], "11");
    assert!(direct[0]["score"].as_f64().unwrap() > direct[1]["score"].as_f64().unwrap());
    let has = |i: usize, flag: &str| {
        direct[i]["flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == flag)
    };
    assert!(has(0, "street_exact") && has(0, "pc_dept"), "{direct:?}");
    for flag in ["commune_exact", "pc_exact"] {
        assert!(!has(0, flag), "first candidate has {flag}: {direct:?}");
        assert!(has(1, flag), "second candidate lacks {flag}: {direct:?}");
    }
    for i in 0..2 {
        assert!(
            !has(i, "dropped_prefix") && !has(i, "dropped_suffix"),
            "{direct:?}"
        );
    }
    let hits = guard_drop_query_k2(
        &sheet,
        "10 rue Victor Hugo, 37000 Tours, France, Tours, France",
    );
    guard_drop_assert_saved_tail_prefix(&hits);
}

#[test]
fn guard_drop_tail_stops_at_first_accepted_tail() {
    let sheet = guard_drop_tail_choice_sheet("guard-drop-tail-first-accepted", true);
    // Removing one token accepts a foreign house; removing two would recover
    // an exact local house. The saved prefix must win at the first acceptance.
    let first = guard_drop_query_k2(&sheet, "10 rue Victor Hugo 37000 Tours Tours");
    let later = guard_drop_query_k2(&sheet, "10 rue Victor Hugo 37000 Tours");
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(later.len(), 1, "{later:?}");
    assert_eq!(first[0]["street"], "Rue Victor Hugo Tours Tours");
    assert_eq!(first[0]["commune"], "Courtry");
    assert_eq!(later[0]["street"], "Rue Victor Hugo");
    assert_eq!(later[0]["commune"], "Tours");
    let weak = first[0]["flags"].as_array().unwrap();
    let exact = later[0]["flags"].as_array().unwrap();
    assert!(weak.iter().any(|f| f == "pc_dept"), "{first:?}");
    for flag in ["commune_exact", "pc_exact"] {
        assert!(!weak.iter().any(|f| f == flag), "{first:?}");
        assert!(exact.iter().any(|f| f == flag), "{later:?}");
    }
    for hit in [&first[0], &later[0]] {
        assert_eq!(hit["precision"], "house", "{hit}");
        assert_eq!(hit["housenumber"], "10", "{hit}");
        let flags = hit["flags"].as_array().unwrap();
        assert!(flags.iter().any(|f| f == "street_exact"), "{hit}");
        assert!(
            !flags
                .iter()
                .any(|f| f == "dropped_prefix" || f == "dropped_suffix"),
            "{hit}"
        );
    }
    let hits = guard_drop_query_k2(&sheet, "10 rue Victor Hugo 37000 Tours Tours Tours");
    guard_drop_assert_saved_tail_prefix(&hits);
}

#[test]
fn exact_original_blocks_competing_article_house() {
    let dir =
        std::env::temp_dir().join(format!("gridpin-es-article-compete-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("rows.csv");
    std::fs::write(&csv, "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\ncalle de ejemplo,001,ciudad,28001,7,,-4.7,41.4,calle de ejemplo,Ciudad\ncalle ejemplo,001,ciudad,28001,7,,-3.7,40.4,calle ejemplo,Ciudad\n").unwrap();
    let rules = dir.join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(rules.join("street_types_latin.tsv"), "calle\n").unwrap();
    let manifest = dir.join("manifest.json");
    std::fs::write(
        &manifest,
        r#"{"country":"es","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let bin = dir.join("test.bin");
    builder::build(&csv, &bin, None, None, Some(&rules), None, Some(&manifest)).unwrap();
    let idx = Index::open(&bin).unwrap();
    let literal = idx.query("Calle Ejemplo, 7, Ciudad", 1);
    let article = idx.query("Calle de Ejemplo, 7, Ciudad", 1);
    assert_eq!(literal[0].street, "calle ejemplo");
    assert_eq!(article[0].street, "calle de ejemplo");
    assert_ne!(literal[0].lat, article[0].lat);
    assert_ne!(literal[0].lon, article[0].lon);
}
