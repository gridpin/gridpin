use std::io::{BufRead, BufWriter, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use gridpin::{builder, query};

fn parse_near(raw: &str) -> Result<(f64, f64), String> {
    let Some((lat_raw, lon_raw)) = raw.split_once(',') else {
        return Err("expected LAT,LON with exactly one comma".to_string());
    };
    if lat_raw.is_empty() || lon_raw.is_empty() || lon_raw.contains(',') {
        return Err("expected LAT,LON with exactly one comma".to_string());
    }
    let lat = lat_raw
        .parse::<f64>()
        .map_err(|_| "latitude must be a number".to_string())?;
    let lon = lon_raw
        .parse::<f64>()
        .map_err(|_| "longitude must be a number".to_string())?;
    query::validate_query_near(lat, lon)?;
    Ok((lat, lon))
}

/// gridpin — offline geocoder: country-in-a-file data sheets, forward and reverse.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show or verify the watermark of an index file
    #[cfg(feature = "watermark")]
    Mark {
        /// index file (.bin)
        index: PathBuf,
        /// claimed mark to verify
        #[arg(long)]
        verify: Option<String>,
    },
    /// Stamp a finished index file with a watermark (no rebuild)
    #[cfg(feature = "watermark")]
    #[command(hide = true)]
    Stamp {
        /// input index file (.bin)
        input: PathBuf,
        /// output index file (may equal the input to stamp in place)
        output: PathBuf,
        /// mark string to embed
        #[arg(long)]
        mark: String,
    },
    /// Build an index from a sorted CSV stream (prep/export_build.py)
    Build {
        input: PathBuf,
        out: PathBuf,
        /// optional parser model (ml/parser_v0.bin)
        #[arg(long)]
        model: Option<PathBuf>,
        /// optional ranking weights (ml/rank_v0.bin)
        #[arg(long)]
        rank: Option<PathBuf>,
        /// optional rules-in-data directory (TSV files)
        #[arg(long)]
        rules: Option<PathBuf>,
        /// optional watermark string to embed in the index (no-op without the watermark feature)
        // hidden from --help in public builds: advertising a do-nothing flag reads
        // as a broken promise; still accepted for script compat
        #[arg(long, hide = !cfg!(feature = "watermark"))]
        mark: Option<String>,
        /// build manifest (JSON): provenance + country/layer identity -> SEC_META (v6+).
        /// Distributed sheets MUST carry one; omitting it prints a loud warning.
        #[arg(long)]
        meta: Option<PathBuf>,
    },
    /// Reverse geocoding: coordinates -> nearest indexed addresses (approximate; see precision/distance_m)
    // allow_negative_numbers: otherwise clap treats a western longitude like "-1.5536"
    // as a flag, making addresses west of 0° unreachable for reverse
    #[command(allow_negative_numbers = true)]
    Reverse {
        index: PathBuf,
        lat: f64,
        lon: f64,
        #[arg(short, default_value_t = 3)]
        k: usize,
    },
    /// Single forward query
    Query {
        index: PathBuf,
        q: String,
        #[arg(short, default_value_t = 10)]
        k: usize,
        /// optional WGS84 focus; widens cityless homonyms and breaks equal-quality ties
        #[arg(
            long,
            value_name = "LAT,LON",
            value_parser = parse_near,
            allow_hyphen_values = true
        )]
        near: Option<(f64, f64)>,
        /// optional POI layer (second .bin): cascades when the address result is weak
        #[arg(long)]
        poi: Option<PathBuf>,
    },
    /// Batch mode: JSONL {"q": ...} in -> JSONL {"results": [...]} out, order preserved
    Batch {
        index: PathBuf,
        input: PathBuf,
        output: PathBuf,
        #[arg(short, default_value_t = 10)]
        k: usize,
        /// dump candidate features (rank training)
        #[arg(long, default_value_t = false)]
        dump: bool,
        /// optional POI layer (second .bin): cascades when the address result is weak
        #[arg(long)]
        poi: Option<PathBuf>,
    },
    /// Print the provenance/identity record of a sheet (SEC_META, v6+)
    Meta {
        index: PathBuf,
        /// machine-readable JSON object (safe against values with embedded newlines)
        #[arg(long)]
        json: bool,
    },
    /// Replace SEC_META in a current-format sheet without rebuilding data sections.
    /// Older v5/v6 sheets must be rebuilt because v7 changes house_blocks.
    Repack {
        input: PathBuf,
        output: PathBuf,
        /// build manifest (JSON): provenance + country/layer identity
        #[arg(long)]
        meta: PathBuf,
    },
    /// (internal) speed measurement stub
    #[command(hide = true)]
    Bench { index: PathBuf },
}

/// Run a query that reads mmap'd sections, but never let a panic INSIDE the fst crate on a
/// corrupt-but-openable sheet abort the CLI: the Python/DuckDB bindings have their
/// own panic boundary; the CLI query/reverse/batch paths need one too. A caught panic yields the
/// type's default (empty results) so the process exits cleanly instead of aborting the host.
fn panic_safe<T: Default>(f: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_default()
}

fn main() -> anyhow::Result<()> {
    // A corrupt or hostile sheet can drive a deep dependency (e.g. the fst reader) to panic on
    // bytes our own accessors can't pre-validate. For a shipped CLI that must be a controlled
    // exit(1), never a 101/SIGABRT that takes down the host shell or pipeline.
    // The hook is SILENT (not exit) so the inner `panic_safe` boundaries can actually CATCH a
    // query-time panic — e.g. one corrupt line in a batch yields empty and the batch continues,
    // instead of the whole process dying. Uncaught panics hit the top-level catch below -> exit(1).
    std::panic::set_hook(Box::new(|_| {}));
    let cli = Cli::parse();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(cli))) {
        Ok(r) => r,
        Err(_) => {
            eprintln!("gridpin: fatal: corrupt or unsupported input");
            std::process::exit(1);
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.cmd {
        #[cfg(feature = "watermark")]
        Cmd::Mark { index, verify } => gridpin::mark::cli_show(&index, verify.as_deref())?,
        #[cfg(feature = "watermark")]
        Cmd::Stamp {
            input,
            output,
            mark,
        } => gridpin::mark::cli_stamp(&input, &output, &mark)?,
        Cmd::Build {
            input,
            out,
            model,
            rank,
            rules,
            mark,
            meta,
        } => builder::build(
            &input,
            &out,
            model.as_deref(),
            rank.as_deref(),
            rules.as_deref(),
            mark.as_deref(),
            meta.as_deref(),
        )?,
        Cmd::Reverse { index, lat, lon, k } => {
            // reverse on a POI-only sheet would mark a place as a `house` — refuse it as primary
            // too, consistent with query/batch; POI belongs to the cascade only.
            let idx = query::Index::open_address(&index)?;
            // try_reverse is the single strict entry point: a bad coordinate is an error
            // here just as in py/DuckDB, not a silent empty result.
            let hits = idx
                .try_reverse(lat, lon, k)
                .map_err(|e| anyhow::anyhow!(e))?;
            for h in hits {
                println!("{}", serde_json::to_string(&h)?);
            }
        }
        Cmd::Meta { index, json } => {
            let idx = query::Index::open(&index)?;
            if json {
                // a value can contain arbitrary bytes incl. newlines; JSON escapes them, so a
                // downstream parser cannot be spoofed by an injected `key: value` line.
                let map: std::collections::BTreeMap<&str, &str> = idx
                    .meta()
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                println!("{}", serde_json::to_string(&map)?);
            } else {
                if idx.meta().is_empty() {
                    println!("(no SEC_META: pre-v6 data or built without --meta)");
                }
                for (k, v) in idx.meta() {
                    println!("{k}: {v}");
                }
            }
        }
        Cmd::Repack {
            input,
            output,
            meta,
        } => {
            use gridpin::index;
            let data = std::fs::read(&input)?;
            // Repack is byte-for-byte for all data sections and is therefore safe only within v7.
            // v5/v6 house blocks do not carry v7's conditional per-house postcode ids; relabeling
            // them as v7 would make the reader interpret unrelated bytes as a dictionary.
            let secs = index::parse_sections_for_repack(&data)?;
            let meta_sec = index::encode_meta(&builder::meta_from_manifest(&meta)?);
            // copy every section verbatim, drop any pre-existing SEC_META, append ours
            let mut sections: Vec<(u8, Vec<u8>)> = secs
                .iter()
                .filter(|(id, _, _)| *id as usize != index::SEC_META)
                .map(|(id, off, len)| (*id, data[*off as usize..(*off + *len) as usize].to_vec()))
                .collect();
            sections.push((index::SEC_META as u8, meta_sec));
            let mut toc: Vec<(u8, u64, u64)> = Vec::new();
            // header size follows the ACTUAL entry count: a sheet built before some
            // optional section existed has fewer TOC entries than N_SECTIONS, and
            // assuming the maximum would shift every offset (caught by self-check)
            let mut off = (6 + sections.len() * 17) as u64;
            for (id, sec) in &sections {
                toc.push((*id, off, sec.len() as u64));
                off += sec.len() as u64;
            }
            let mut header = Vec::new();
            index::write_header(&mut header, &toc);
            // Self-check BEFORE publishing: assembling the bytes in
            // memory lets us structurally parse them and read back the identity we
            // just wrote; only a verified result reaches the target path. Publishing
            // first once destroyed a source sheet when the check failed after rename.
            let mut bytes = header;
            for (_, s) in &sections {
                bytes.extend_from_slice(s);
            }
            let secs_check = index::parse_sections(&bytes)?;
            let (moff, mlen) = secs_check[index::SEC_META];
            let meta_ok = index::decode_meta(&bytes[moff as usize..(moff + mlen) as usize])
                .is_some_and(|m| !m.is_empty());
            anyhow::ensure!(
                meta_ok,
                "repack self-check failed: SEC_META unreadable — output not written"
            );
            // Stronger self-check: actually OPEN + query the produced bytes before
            // publishing, so a structurally-valid-but-semantically-corrupt sheet (e.g. a byte-flip
            // inside an FST) is caught here — not handed to a buyer as a sheet that won't open.
            let tmp = builder::unique_tmp_path(&output);
            {
                // fsync the file DATA before the self-check + durable rename (adversarial finding):
                // repack used std::fs::write (no fsync), so finalize_replace made only the directory
                // entry durable — a crash after rename could publish a final-named sheet with
                // unflushed/zeroed data blocks. Match write_atomic / the batch path.
                use std::io::Write as _;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&tmp)?;
                f.write_all(&bytes)?;
                f.sync_all()?;
            }
            let verified =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<()> {
                    let idx = gridpin::query::Index::open(&tmp)?; // full open = semantic validation
                    let _ = idx.query("a", 1); // exercise the FST/postings read paths
                    let _ = idx.reverse(0.0, 0.0, 1);
                    Ok(())
                }));
            if !matches!(verified, Ok(Ok(()))) {
                let _ = std::fs::remove_file(&tmp);
                anyhow::bail!("repack self-check failed: produced sheet did not open/query — output not written");
            }
            builder::finalize_replace(&tmp, &output)?;
            println!(
                "repacked {} -> {} (v{}, {} sections + meta)",
                input.display(),
                output.display(),
                index::VERSION,
                sections.len()
            );
        }
        Cmd::Query {
            index,
            q,
            k,
            near,
            poi,
        } => {
            let idx = query::Index::open_address(&index)?;
            let poi_idx = poi.map(|p| query::Index::open(&p)).transpose()?;
            if let Some(pi) = &poi_idx {
                query::check_pair(&idx, pi).map_err(|e| anyhow::anyhow!(e))?;
            }
            let hits = match near {
                None => panic_safe(|| query::query_cascade(&idx, poi_idx.as_ref(), &q, k)),
                Some((lat, lon)) => panic_safe(|| {
                    query::query_cascade_near(&idx, poi_idx.as_ref(), &q, k, lat, lon)
                        .unwrap_or_default()
                }),
            };
            for h in hits {
                println!("{}", serde_json::to_string(&h)?);
            }
        }
        Cmd::Batch {
            index,
            input,
            output,
            k,
            dump,
            poi,
        } => {
            use rayon::prelude::*;
            let t0 = std::time::Instant::now();
            let idx = query::Index::open_address(&index)?;
            let poi_idx = poi.map(|p| query::Index::open(&p)).transpose()?;
            if let Some(pi) = &poi_idx {
                query::check_pair(&idx, pi).map_err(|e| anyhow::anyhow!(e))?;
            }
            let open_s = t0.elapsed().as_secs_f64();
            // gentle pool: cores minus two (override with GRIDPIN_THREADS; clamped to >=1
            // so GRIDPIN_THREADS=0 does not fail the build)
            let n_threads = std::env::var("GRIDPIN_THREADS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .map(|v| v.max(1))
                .unwrap_or_else(|| {
                    std::thread::available_parallelism()
                        .map(|c| c.get().saturating_sub(2).max(1))
                        .unwrap_or(2)
                });
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(n_threads)
                .build()?;
            let mut inp = std::io::BufReader::new(std::fs::File::open(&input)?).lines();
            // input == output would destroy the input. canonicalize catches
            // symlinks and the same literal path; but two HARDLINKS have different paths yet
            // the SAME inode, so also compare file identity (dev+ino) on unix.
            if let (Ok(a), Ok(b)) = (
                std::fs::canonicalize(&input),
                std::fs::canonicalize(&output),
            ) {
                if a == b {
                    anyhow::bail!(
                        "batch: input and output are the same file — this would destroy the input"
                    );
                }
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if let (Ok(mi), Ok(mo)) = (std::fs::metadata(&input), std::fs::metadata(&output)) {
                    if mi.dev() == mo.dev() && mi.ino() == mo.ino() {
                        anyhow::bail!(
                            "batch: input and output are the same file (hardlink/alias) — refusing to overwrite the input"
                        );
                    }
                }
            }
            // Stream into a sibling temp, then atomically rename on full success (
            // ): a read/decode failure mid-stream leaves the PREVIOUS output intact
            // instead of a truncated partial, and writing a fresh inode keeps a hardlinked
            // input untouched. The temp name is UNIQUE (pid + counter) and opened with
            // create_new, so it can never collide with — and truncate — an input that happens
            // to be named like the temp: create_new
            // fails on an existing path instead of destroying it. One shared unique-temp writer
            // with the build/repack path.
            let tmp_out = gridpin::builder::unique_tmp_path(&output);
            let mut out = BufWriter::new(
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&tmp_out)?,
            );
            // Remove the temp on ANY early return (a mid-stream `?` error) so a failed batch never
            // leaves a ~MB partial temp behind. Disarmed only after a clean publish.
            struct TempCleanup {
                path: Option<std::path::PathBuf>,
            }
            impl TempCleanup {
                fn disarm(&mut self) {
                    self.path = None;
                }
                fn cleanup(&mut self) {
                    if let Some(p) = self.path.take() {
                        let _ = std::fs::remove_file(p);
                    }
                }
            }
            impl Drop for TempCleanup {
                fn drop(&mut self) {
                    self.cleanup();
                }
            }
            let mut tmp_guard = TempCleanup {
                path: Some(tmp_out.clone()),
            };
            let mut nq = 0u64;
            let tq = std::time::Instant::now();
            // input is JSONL: {"q": "..."} per line. Lines without a usable "q" would
            // silently yield empty results, so they are counted and reported.
            let malformed = std::sync::atomic::AtomicU64::new(0);
            // Windowed streaming: holding the whole input AND the whole
            // output in memory used to OOM on tens of millions of rows. A window keeps
            // par_iter's order guarantee within itself, and windows are written in
            // order, so the output is byte-identical to the old all-in-memory path.
            const WINDOW: usize = 65_536;
            loop {
                let mut lines: Vec<String> = Vec::with_capacity(WINDOW);
                for l in inp.by_ref() {
                    // Push EVERY input line — do NOT silently drop a blank one: that
                    // broke input↔output cardinality (N lines in, fewer out). A blank line has no
                    // usable "q", so it is counted as malformed below (fail-closed: no publish),
                    // while a well-formed JSONL (one object per line) never contains a blank line.
                    lines.push(l?);
                    if lines.len() == WINDOW {
                        break;
                    }
                }
                if lines.is_empty() {
                    break;
                }
                nq += lines.len() as u64;
                let results: Vec<String> = pool.install(|| {
                lines
                    .par_iter()
                    .map(|line| {
                        let v: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
                        let q = v.get("q").and_then(|x| x.as_str()).unwrap_or("").trim();
                        // field as string OR number ("housenumber":5 / postcode:75001); a blank or
                        // whitespace-only field is treated as ABSENT so it can't hijack a valid `q`
                        // and return an empty structured result.
                        let fld = |key: &str| -> Option<String> {
                            v.get(key)
                                .and_then(|x| x.as_str().map(String::from).or_else(|| x.as_u64().map(|n| n.to_string())))
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                        };
                        // Dispatch: dump > structured (non-blank "street", address-index only) >
                        // free-form ("q", full address+POI cascade). A line is usable if it has a
                        // free-form "q" OR a structured "street"; only genuinely empty lines are malformed.
                        if q.is_empty() && fld("street").is_none() && !dump {
                            malformed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        // each per-line query is panic-guarded: a corrupt-but-openable
                        // sheet must not abort the whole batch — a panicking line yields empty.
                        if dump {
                            let cands: Vec<serde_json::Value> = panic_safe(|| idx.query_feats(q, k))
                                .into_iter()
                                .map(|(h, f)| serde_json::json!({"f": f, "lat": h.lat, "lon": h.lon}))
                                .collect();
                            serde_json::json!({ "cands": cands }).to_string()
                        } else if let Some(street) = fld("street") {
                            // structured input: pre-parsed fields, no field-boundary guessing. Uses
                            // the SAME POI cascade as free-form, so a structured POI query
                            // is not silently unanswerable just because the caller pre-split fields.
                            let city = fld("city").unwrap_or_default();
                            let number = fld("housenumber");
                            let postcode = fld("postcode");
                            let hits = panic_safe(|| {
                                query::query_structured_cascade(
                                    &idx,
                                    poi_idx.as_ref(),
                                    &street,
                                    number.as_deref(),
                                    &city,
                                    postcode.as_deref(),
                                    k,
                                )
                            });
                            serde_json::json!({ "results": hits }).to_string()
                        } else {
                            let hits = panic_safe(|| query::query_cascade(&idx, poi_idx.as_ref(), q, k));
                            serde_json::json!({ "results": hits }).to_string()
                        }
                    })
                    .collect()
            });
                for r in &results {
                    out.write_all(r.as_bytes())?;
                    out.write_all(b"\n")?;
                }
            }
            out.flush()?; // Drop cleans the temp on an error return
            out.into_inner()?.sync_all()?; // ditto
            let dt = tq.elapsed().as_secs_f64();
            eprintln!(
                "open {:.2}s · {} queries in {:.2}s · {:.0} req/s",
                open_s,
                nq,
                dt,
                nq as f64 / dt
            );
            let bad = malformed.load(std::sync::atomic::Ordering::Relaxed);
            if bad > 0 {
                // Semantic FAILURE: never publish, keep the previous output intact.
                // exit() skips Drop, so remove the temp explicitly here.
                tmp_guard.cleanup();
                eprintln!(
                    "warning: {bad} of {nq} input lines had no usable \"q\" field and returned \
                     nothing — batch input is JSONL, one {{\"q\": \"...\"}} object per line; \
                     output NOT written"
                );
                std::process::exit(1);
            }
            // success: atomic publish + parent-dir fsync so the rename is crash-durable, same as the
            // build/repack path.
            gridpin::builder::finalize_replace(&tmp_out, &output)?; // on Err, Drop cleans the temp
            tmp_guard.disarm(); // published; do not remove the renamed file
        }
        Cmd::Bench { index } => {
            println!("bench: {index:?} (not implemented)");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{panic_safe, Cli, Cmd};
    use clap::Parser;

    #[test]
    fn panic_safe_returns_default_on_panic_never_aborts() {
        // the CLI query/reverse/batch paths wrap the fst read in panic_safe, so a
        // corrupt-but-openable sheet that panics INSIDE the fst crate yields empty results and a
        // clean exit instead of aborting the process (which has no other panic boundary).
        assert_eq!(
            panic_safe(|| vec![1, 2, 3]),
            vec![1, 2, 3],
            "the happy path is untouched"
        );
        let caught: Vec<i32> = panic_safe(|| panic!("simulated fst traversal panic"));
        assert!(
            caught.is_empty(),
            "a panic is caught and yields the default, not an abort"
        );
    }

    #[test]
    fn query_near_parser_accepts_south_and_west_coordinates() {
        for (raw, expected) in [("-33.9,-70.7", (-33.9, -70.7)), ("48.8,-1.5", (48.8, -1.5))] {
            let cli =
                Cli::try_parse_from(["gridpin", "query", "sheet.bin", "markt", "--near", raw])
                    .expect("valid signed LAT,LON must parse");
            let Cmd::Query { near, .. } = cli.cmd else {
                panic!("query command expected");
            };
            assert_eq!(near, Some(expected));
        }
    }

    #[test]
    fn query_near_parser_rejects_malformed_or_non_wgs84_values() {
        for raw in [
            "48.8",
            "48.8,1.5,2",
            ",1.5",
            "48.8,",
            "nan,1",
            "48,inf",
            "91,1",
            "48,181",
        ] {
            assert!(
                Cli::try_parse_from(["gridpin", "query", "sheet.bin", "markt", "--near", raw])
                    .is_err(),
                "{raw} must be rejected"
            );
        }
    }
}
