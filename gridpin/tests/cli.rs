//! CLI integration tests: drive the built `gridpin` binary end to end. These cover
//! the batch paths (windowed streaming, malformed exit code) that unit tests cannot
//! reach — the coverage-gap map flagged them as never exercised.
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
