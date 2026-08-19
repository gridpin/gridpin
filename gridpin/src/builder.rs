//! Single-pass index builder from a pre-sorted CSV.
//! Input must be sorted by (nom_voie_norm, code_insee, numero, rep), which matches
//! the lexicographic order of the FST keys `street 0x1F insee`.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use fst::MapBuilder;

use crate::index::*;

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}
const DEFAULT_MAX_UNCOMPRESSED: u64 = 32 << 30; // 32 GiB — far above any real country sheet
const DEFAULT_MAX_RATIO: u64 = 100; // decompressed/compressed cap for .gz input

/// Streaming guard against a decompression bomb: caps total decompressed bytes and,
/// for .gz input, the decompressed/compressed ratio — so a tiny crafted .gz can't drive unbounded
/// memory/CPU in an automated external-data build. Both caps are overridable via env for a
/// legitimately larger sheet.
struct LimitReader<R> {
    inner: R,
    read: u64,
    max_bytes: u64,
    compressed_len: u64, // 0 disables the ratio check (plain, non-gz input)
    max_ratio: u64,
}
impl<R: Read> LimitReader<R> {
    fn new(inner: R, compressed_len: u64) -> Self {
        Self {
            inner,
            read: 0,
            max_bytes: env_u64("GRIDPIN_MAX_UNCOMPRESSED_BYTES", DEFAULT_MAX_UNCOMPRESSED),
            compressed_len,
            max_ratio: env_u64("GRIDPIN_MAX_RATIO", DEFAULT_MAX_RATIO),
        }
    }
}
impl<R: Read> Read for LimitReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        if self.read > self.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "decompressed input exceeds {} byte cap (possible decompression bomb); \
                 raise GRIDPIN_MAX_UNCOMPRESSED_BYTES for a legitimately larger sheet",
                    self.max_bytes
                ),
            ));
        }
        if self.compressed_len > 0 && self.read > self.compressed_len.saturating_mul(self.max_ratio)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "decompression ratio exceeds {}x of the {}-byte input (possible bomb); \
                 raise GRIDPIN_MAX_RATIO if intended",
                    self.max_ratio, self.compressed_len
                ),
            ));
        }
        Ok(n)
    }
}

struct House {
    numero: u32,
    rep_id: u32,
    lat: f64,
    lon: f64,
    postcode: String,
}

#[derive(Default)]
struct Counts {
    rows: u64,
    streets: u64,
    communes: u64,
    empty_streets: u64, // dropped: every row had unparseable coordinates
    bad_coords: u64,    // dropped: non-finite or out-of-range lat/lon
    bad_number: u64,    // dropped: a non-empty house number that is not an integer
}

/// True if the string looks like a postcode: starts with a digit, ≤8 chars,
/// alphanumeric/space only. Rejects malformed `code_postal_display` values seen in
/// some sources (phone numbers like "+7 8352 428902", commas) that would otherwise
/// leak into output. NL "1012XJ"/"1012 XJ" and FR "75002" pass.
fn plausible_pc(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 8
        && s.as_bytes()[0].is_ascii_digit()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ')
}

/// Append one u8-length-prefixed string to SEC_NAMES. The names section has no room for an entry
/// longer than 255 bytes, so reject instead of silently truncating it to another value.
fn push_name(names: &mut Vec<u8>, s: &str) -> Result<u32> {
    let bytes = s.as_bytes();
    if bytes.len() > 255 {
        bail!(
            "display name is {} bytes — the names section encodes length in one u8 (max 255): {:?}",
            bytes.len(),
            s.chars().take(40).collect::<String>()
        );
    }
    let off = names.len() as u32;
    names.push(bytes.len() as u8);
    names.extend_from_slice(bytes);
    Ok(off)
}

/// Encode one street's house block. Most streets retain the legacy v6 four-varint-per-house grammar.
/// Only a street with more than one EFFECTIVE postcode value (where missing is a value when mixed
/// with known) gets v7's sparse header and fifth varint:
///
///   varint(nonempty postcode count), count * u32 SEC_NAMES offsets,
///   then every house's four old varints + local postcode id (0 missing, 1..=count).
///
/// The dictionary is lexicographically sorted, making both ids and bytes deterministic. An
/// all-missing street and a single-valued known street stay on the old grammar and pay zero bytes.
fn write_house_block(
    out: &mut Vec<u8>,
    names: &mut Vec<u8>,
    houses: &[House],
    lat_c: i32,
    lon_c: i32,
    voie: &str,
    insee: &str,
) -> Result<bool> {
    let effective: BTreeSet<&str> = houses.iter().map(|h| h.postcode.as_str()).collect();
    let has_known = effective.iter().any(|p| !p.is_empty());
    let house_accurate = has_known && effective.len() > 1;
    let local_postcodes: Vec<&str> = if house_accurate {
        effective.into_iter().filter(|p| !p.is_empty()).collect()
    } else {
        Vec::new()
    };

    if house_accurate {
        write_varint(out, local_postcodes.len() as u64);
        for postcode in &local_postcodes {
            out.extend_from_slice(&push_name(names, postcode)?.to_le_bytes());
        }
    }

    let mut prev = 0u32;
    for (i, h) in houses.iter().enumerate() {
        // The delta encoding assumes the input is sorted by numero within a street. An unsorted
        // export would wrap in release builds and write garbage house numbers — fail loudly.
        if i > 0 && h.numero < prev {
            bail!(
                "input not sorted by numero within street {:?} (insee {:?}): {} after {}",
                voie,
                insee,
                h.numero,
                prev
            );
        }
        let delta = if i == 0 {
            h.numero as u64
        } else {
            (h.numero - prev) as u64
        };
        prev = h.numero;
        write_varint(out, delta);
        write_varint(out, h.rep_id as u64);
        let hla = (h.lat * 1e7).round() as i64;
        let hlo = (h.lon * 1e7).round() as i64;
        write_varint(out, zigzag(hla - lat_c as i64));
        write_varint(out, zigzag(hlo - lon_c as i64));
        if house_accurate {
            let postcode_id = if h.postcode.is_empty() {
                0
            } else {
                // local_postcodes is sorted and every non-empty effective value is present.
                local_postcodes
                    .binary_search(&h.postcode.as_str())
                    .expect("house postcode is in its local dictionary")
                    + 1
            };
            write_varint(out, postcode_id as u64);
        }
    }
    Ok(house_accurate)
}

/// A UNIQUE hidden temp path `.<name>.tmp.<pid>.<seq>` next to the output: unique
/// per process+call so it never collides with a concurrent writer and never truncates a
/// pre-existing file that happens to sit at a fixed `<out>.tmp` name (e.g. a repack input).
pub fn unique_tmp_path(out_path: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let base = out_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".into());
    out_path.with_file_name(format!(
        ".{base}.tmp.{}.{}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Atomic replace of `out_path` by `tmp`. `std::fs::rename` maps to
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` on Windows, so there is no pre-delete window; on
/// failure the temp is cleaned up.
/// The publishing rename is then made crash-durable by fsyncing the parent directory here, so
/// EVERY caller (write_atomic AND the repack path in main.rs) is durable, not just write_atomic
///.
pub fn finalize_replace(tmp: &Path, out_path: &Path) -> std::io::Result<()> {
    match std::fs::rename(tmp, out_path) {
        Ok(()) => sync_parent_dir(out_path),
        Err(e) => {
            let _ = std::fs::remove_file(tmp);
            Err(e)
        }
    }
}

/// Best-effort fsync of the directory that CONTAINS `path`. Fsyncing the file persists its
/// DATA, but the directory ENTRY that the publishing rename created can still be lost on a crash
/// until the directory itself is fsynced. On non-Unix platforms this is a NO-OP: directory fsync
/// is not exposed, so crash/power-loss durability there is NOT claimed — the
/// process-kill guarantee proven by the SIGKILL matrix test is POSIX-only.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = parent.unwrap_or_else(|| Path::new("."));
    std::fs::File::open(dir)?.sync_all()
}
#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Write a file atomically: contents go to a UNIQUE hidden sibling temp opened `create_new`
/// (never truncates an existing file — safe against an input named like the temp and against
/// concurrent writers), fsync, then atomic rename into place, then fsync the parent directory so
/// the rename is durable. A graceful error removes the temp; a hard kill leaves only a
/// uniquely-named hidden temp, never a truncated final file.
///
/// PROVEN SCOPE: process-kill safety is exercised by the SIGKILL
/// matrix test (`kill_during_build_never_publishes_a_corrupt_sheet`, sampled timings) on POSIX.
/// Windows durability rests on MoveFileExW semantics (not fault-tested here); power-loss beyond
/// fsync guarantees and exhaustive kill timing are NOT claimed.
pub fn write_atomic(out_path: &Path, chunks: &[&[u8]]) -> std::io::Result<()> {
    let tmp = unique_tmp_path(out_path);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        for c in chunks {
            f.write_all(c)?;
        }
        f.flush()?;
        f.sync_all()
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    finalize_replace(&tmp, out_path) // fsyncs the parent dir itself
}

/// Like `write_atomic`, but VALIDATES the temp file BEFORE it replaces the final (adversarial
/// ). A self-check that runs AFTER the atomic rename has already destroyed the previous
/// good sheet on a rejected rebuild — publish was no longer atomic for the consumer. Here `validate`
/// runs on the hidden TEMP; on failure the temp is removed and `out_path` (the last-known-good
/// sheet, if any) is left byte-for-byte untouched. Only a passing sheet ever reaches the final name.
pub fn write_atomic_validated(
    out_path: &Path,
    chunks: &[&[u8]],
    validate: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let tmp = unique_tmp_path(out_path);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        for c in chunks {
            f.write_all(c)?;
        }
        f.flush()?;
        f.sync_all()
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing temp for {out_path:?}"));
    }
    if let Err(e) = validate(&tmp) {
        let _ = std::fs::remove_file(&tmp); // out_path (old good sheet) untouched
        return Err(e);
    }
    finalize_replace(&tmp, out_path).with_context(|| format!("publishing {out_path:?}"))
}

/// Streaming BLAKE2b-256 of a file, hex. Streams so a multi-GB input (france.csv.gz) is
/// content-hashed without being buffered whole. Hashes the file bytes AS-IS (compressed if `.gz`),
/// which is exactly the source artifact identity we want to record.
fn file_hash_hex(path: &Path) -> Result<String> {
    use blake2::digest::{Update, VariableOutput};
    use std::io::Read;
    let mut f = std::io::BufReader::new(
        File::open(path).with_context(|| format!("cannot open input {path:?} to hash"))?,
    );
    let mut hasher = blake2::Blake2bVar::new(32).expect("blake2b-32");
    let mut buf = [0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let mut out = [0u8; 32];
    hasher
        .finalize_variable(&mut out)
        .expect("blake2b-32 output");
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}

/// Read a build manifest (JSON object) into SEC_META pairs.
/// Scalars become strings; arrays/objects are embedded as compact JSON. The two
/// identity keys are validated here: `country` (lowercased) and `layer`
/// (addresses|poi) — the engine refuses mismatched address/POI pairs by them.
pub fn meta_from_manifest(path: &Path) -> Result<Vec<(String, String)>> {
    let raw: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(path).with_context(|| format!("cannot read manifest {path:?}"))?,
    )
    .with_context(|| format!("manifest {path:?} is not valid JSON"))?;
    let obj = raw.as_object().context("manifest must be a JSON object")?;
    let mut pairs = Vec::new();
    for (k, v) in obj {
        let val = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        pairs.push((k.clone(), val));
    }
    let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let country = get("country")
        .context("manifest is missing `country`")?
        .to_lowercase();
    let layer = get("layer").context("manifest is missing `layer` (addresses|poi)")?;
    if !matches!(layer.as_str(), "addresses" | "poi") {
        bail!("manifest `layer` must be `addresses` or `poi`, got {layer:?}");
    }
    pairs.retain(|(k, _)| k != "country");
    pairs.push(("country".to_string(), country));
    // v6 provenance is a TYPED, mandatory contract: license + source_release must
    // be present and non-empty (country/layer already validated above). The build then stamps its
    // own schema + version so a sheet's provenance identifies exactly how it was produced.
    for key in ["license", "source_release"] {
        let present = pairs.iter().any(|(k, v)| k == key && !v.trim().is_empty());
        if !present {
            bail!("manifest is missing required key `{key}` (v6 provenance)");
        }
    }
    for k in [
        "meta_schema",
        "builder_version",
        "builder_target",
        "builder_git",
    ] {
        pairs.retain(|(kk, _)| kk != k);
    }
    pairs.push(("meta_schema".to_string(), "1".to_string()));
    pairs.push((
        "builder_version".to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
    ));
    // record the toolchain target AND the git commit that produced the sheet, so provenance
    // identifies the build host/arch and the exact source, not just the crate version. builder_git
    // is baked by build.rs (`git rev-parse`); outside a checkout it is "unknown".
    pairs.push((
        "builder_target".to_string(),
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    ));
    pairs.push((
        "builder_git".to_string(),
        option_env!("GRIDPIN_GIT_SHA")
            .unwrap_or("unknown")
            .to_string(),
    ));
    // The decoder drops a section with >1024 pairs or oversized fields as "no
    // provenance" — the writer must never be able to produce such a file, or the
    // address/POI pair check silently degrades to a warning.
    if pairs.len() > 1024 {
        bail!(
            "manifest has {} keys — SEC_META allows at most 1024",
            pairs.len()
        );
    }
    for (k, v) in &pairs {
        if k.len() > u16::MAX as usize {
            // preview by CHARS, not bytes: `&k[..32]` panics when byte 32 splits a
            // multi-byte char — a crafted manifest key could crash the build
            let preview: String = k.chars().take(32).collect();
            bail!("manifest key {preview:?}... is longer than 65535 bytes");
        }
        if v.len() > u32::MAX as usize {
            bail!("manifest value for {k:?} is longer than 4 GiB");
        }
    }
    Ok(pairs)
}

// street_id is bumped inside the flush_street! macro; its final bump after the last
// street flushes is never read (clippy) — the increment is load-bearing for every other
// flush, so silence just this benign tail assignment.
#[allow(unused_assignments)]
pub fn build(
    input: &Path,
    out_path: &Path,
    model_path: Option<&Path>,
    rank_path: Option<&Path>,
    rules_dir: Option<&Path>,
    mark: Option<&str>,
    meta_path: Option<&Path>,
) -> Result<()> {
    // v6: every distributed sheet carries provenance + identity; building without
    // a manifest is allowed for tests/lab but shouts about it.
    let meta_sec: Vec<u8> = match meta_path {
        Some(p) => {
            let mut pairs = meta_from_manifest(p)?;
            // stamp the input's content hash so a sheet's provenance is verifiable end to
            // end (which exact input produced it), next to license/source_release/builder_version.
            pairs.retain(|(k, _)| k != "input_blake2b256");
            pairs.push(("input_blake2b256".to_string(), file_hash_hex(input)?));
            // Re-check the count AFTER this stamp (adversarial finding): meta_from_manifest caps at
            // 1024, but the stamp above adds one more, so a 1024-pair manifest would emit 1025 —
            // which decode_meta rejects (n>1024), silently dropping ALL provenance + country/layer
            // identity. The writer must never exceed the decoder's cap.
            if pairs.len() > 1024 {
                bail!(
                    "manifest yields {} SEC_META pairs after provenance stamping — the decoder caps at 1024",
                    pairs.len()
                );
            }
            encode_meta(&pairs)
        }
        // A release build must carry provenance/identity: GRIDPIN_REQUIRE_META=1 (set by the
        // Makefile release targets / CI) turns the missing-meta warning into a hard error so a
        // sheet can never ship without it. Tests/lab builds leave it unset.
        None if std::env::var("GRIDPIN_REQUIRE_META").is_ok() => bail!(
            "refusing to build a release sheet without --meta (provenance/identity required); \
             pass --meta <manifest.json>"
        ),
        None => {
            eprintln!("warning: building WITHOUT --meta — the sheet will carry no provenance/country identity");
            Vec::new()
        }
    };
    let t0 = std::time::Instant::now();
    let file = File::open(input).with_context(|| format!("cannot open {input:?}"))?;
    let is_gz = input.extension().is_some_and(|e| e == "gz");
    // ratio guard only for .gz (the compressed length is meaningful there); plain input -> 0
    let compressed_len = if is_gz {
        file.metadata().map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    let reader: Box<dyn std::io::Read> = if is_gz {
        Box::new(LimitReader::new(
            flate2::read::GzDecoder::new(file),
            compressed_len,
        ))
    } else {
        Box::new(LimitReader::new(file, 0))
    };
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(std::io::BufReader::with_capacity(1 << 20, reader));

    // resolve columns by header name
    let headers = rdr.headers()?.clone();
    let col = |name: &str| -> Result<usize> {
        headers
            .iter()
            .position(|h| h == name)
            .with_context(|| format!("CSV is missing column {name}"))
    };
    let c_voie_n = col("nom_voie_norm")?;
    let c_insee = col("code_insee")?;
    let c_commune_n = col("nom_commune_norm")?;
    let c_cp = col("code_postal")?;
    let c_numero = col("numero")?;
    let c_rep = col("rep")?;
    let c_lon = col("lon")?;
    let c_lat = col("lat")?;
    let c_voie = col("nom_voie")?;
    let c_commune = col("nom_commune")?;
    // optional column: name of the encompassing city (e.g. Beograd → its settlements)
    let c_prov: Option<usize> = headers.iter().position(|h| h == "provincia_norm");
    // full postcode string (NL "1012XJ": 4 digits + 2 letters); falls back to the numeric
    // code_postal when absent. Preserves letters and leading zeros in output.
    let c_cp_disp: Option<usize> = headers.iter().position(|h| h == "code_postal_display");

    // section accumulators
    let mut streets_fst = MapBuilder::memory();
    let mut streets_meta: Vec<u8> = Vec::new();
    let mut house_blocks: Vec<u8> = Vec::new();
    let mut names: Vec<u8> = Vec::new();
    let mut communes_meta: Vec<u8> = Vec::new();
    // commune significance = its address count (a population proxy: capital ≫ village).
    // Stored in the reserved u32 of commune_meta (no format bump) for ranking tie-breaks.
    let mut commune_house_count: Vec<u32> = Vec::new();
    // address-weighted commune centroid: the "city" point for city-only queries
    let mut commune_lat_sum: Vec<f64> = Vec::new();
    let mut commune_lon_sum: Vec<f64> = Vec::new();

    let mut commune_ids: HashMap<String, u32> = HashMap::new(); // insee -> id
    let mut commune_names: Vec<(String, u32)> = Vec::new(); // (normalized name, commune_id)
    let mut reps: Vec<String> = Vec::new();
    let mut rep_ids: HashMap<String, u32> = HashMap::new();
    let mut cells: Vec<(u32, u32)> = Vec::new(); // (cell, street) — for reverse geocoding
                                                 // inverted word index: street word (normalized, ≥3 chars) → street_ids containing it
    let mut word_post: HashMap<String, Vec<u32>> = HashMap::new();

    // current-street state
    let mut cur_key: Option<(String, String)> = None; // (voie_norm, insee)
    let mut cur_houses: Vec<House> = Vec::new();
    let mut cur_display = String::new();
    let mut cur_commune_id = 0u32;
    let mut cur_cps: HashMap<String, u32> = HashMap::new(); // street postcode string -> house count
    let mut street_id = 0u64;
    let mut n = Counts::default();

    macro_rules! flush_street {
        () => {
            if let Some((voie, insee)) = cur_key.take() {
                // A street whose every row had unparseable coordinates has no houses:
                // writing it would put a ghost street at (0,0) — NaN/0 divides to 0 —
                // polluting reverse geocoding near the null island.
                if cur_houses.is_empty() {
                    cur_cps.clear();
                    n.empty_streets += 1;
                    let _ = (&voie, &insee);
                } else {
                // street centroid
                let (mut sla, mut slo) = (0f64, 0f64);
                for h in &cur_houses {
                    sla += h.lat;
                    slo += h.lon;
                }
                let cnt = cur_houses.len() as f64;
                let lat_c = (sla / cnt * 1e7).round() as i32;
                let lon_c = (slo / cnt * 1e7).round() as i32;
                cells.push((cell_of(sla / cnt, slo / cnt), street_id as u32));
                // most frequent NON-empty street postcode; empty wins only when no
                // non-empty postcode exists at all.
                let cp_disp: String = cur_cps
                    .iter()
                    .filter(|(p, _)| !p.is_empty())
                    // ties on house count go to the lexicographically SMALLEST postcode:
                    // HashMap iteration order would otherwise make builds non-reproducible
                    .max_by_key(|(p, c)| (**c, std::cmp::Reverse((*p).clone())))
                    .map(|(p, _)| p.clone())
                    .unwrap_or_default();
                // numeric prefix for ranking (pc_exact/pc_dept): "75002"→75002, "1012XJ"→1012
                let postcode: u32 = cp_disp
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0);

                // House block. Only a street with multiple effective postcode values uses v7's
                // sparse local dictionary + fifth postcode-id varint; every other street keeps
                // the old four-varint grammar byte-for-byte.
                let house_off = house_blocks.len() as u64;
                let house_accurate = write_house_block(
                    &mut house_blocks,
                    &mut names,
                    &cur_houses,
                    lat_c,
                    lon_c,
                    &voie,
                    &insee,
                )?;

                // street metadata (36 bytes)
                let name_off = push_name(&mut names, &cur_display)?;
                // full postcode string in names (offset 0 is taken by the first commune name, so 0 = none).
                // A house-accurate street has >1 effective value (including missing-vs-known): no
                // single street-level value is truthful. The AMBIGUOUS sentinel both keeps strict
                // street/city results empty and selects the v7 sparse house-block grammar.
                let pc_disp_off: u32 = if house_accurate {
                    PC_DISP_AMBIGUOUS
                } else if cp_disp.is_empty() {
                    0
                } else {
                    push_name(&mut names, &cp_disp)?
                };
                streets_meta.extend_from_slice(&lat_c.to_le_bytes());
                streets_meta.extend_from_slice(&lon_c.to_le_bytes());
                streets_meta.extend_from_slice(&cur_commune_id.to_le_bytes());
                streets_meta.extend_from_slice(&postcode.to_le_bytes());
                streets_meta.extend_from_slice(&name_off.to_le_bytes());
                streets_meta.extend_from_slice(&house_off.to_le_bytes());
                streets_meta.extend_from_slice(&(cur_houses.len() as u32).to_le_bytes());
                streets_meta.extend_from_slice(&pc_disp_off.to_le_bytes());

                // FST key
                let mut key = Vec::with_capacity(voie.len() + 1 + insee.len());
                key.extend_from_slice(voie.as_bytes());
                key.push(KEY_SEP);
                key.extend_from_slice(insee.as_bytes());
                streets_fst.insert(&key, street_id)?;

                // inverted index: significant words of this street → its street_id.
                // Each word's phonetic key is also indexed under the "~" prefix, so
                // cross-script spellings of one name (e.g. Cyrillic vs Latin Qodiriy) match.
                for w in voie.split(|c: char| !c.is_alphanumeric()) {
                    if w.chars().count() >= 3 {
                        word_post.entry(w.to_string()).or_default().push(street_id as u32);
                        let pk = crate::norm::phonetic_key(w);
                        if pk.chars().count() >= 3 && pk != w {
                            word_post.entry(format!("~{pk}")).or_default().push(street_id as u32);
                        }
                    }
                }

                street_id += 1;
                n.streets += 1;
                if let Some(cnt) = commune_house_count.get_mut(cur_commune_id as usize) {
                    *cnt = cnt.saturating_add(cur_houses.len() as u32);
                }
                let ci = cur_commune_id as usize;
                if ci < commune_lat_sum.len() {
                    commune_lat_sum[ci] += lat_c as f64 * cur_houses.len() as f64;
                    commune_lon_sum[ci] += lon_c as f64 * cur_houses.len() as f64;
                }
                cur_houses.clear();
                cur_cps.clear();
                }
            }
        };
    }

    for rec in rdr.records() {
        let rec = rec?;
        let voie_n = rec.get(c_voie_n).unwrap_or("");
        let insee = rec.get(c_insee).unwrap_or("");
        if voie_n.is_empty() || insee.is_empty() {
            continue;
        }
        // Validate coordinates BEFORE touching any commune/street state: a
        // row that is dropped for bad coordinates must not create or name a commune. Range
        // check too — `is_finite` alone let lat/lon like 999 through, which
        // then saturated to a garbage fixed-point value.
        let lat: f64 = rec.get(c_lat).unwrap_or("").parse().unwrap_or(f64::NAN);
        let lon: f64 = rec.get(c_lon).unwrap_or("").parse().unwrap_or(f64::NAN);
        if !(lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0) {
            n.bad_coords += 1;
            continue;
        }
        // A house number that is present but not an integer ("abc") is malformed data —
        // dropped, never silently coerced to 0. An empty field is a legitimate
        // street-level record and stays 0.
        let num_raw = rec.get(c_numero).unwrap_or("");
        let numero: u32 = if num_raw.is_empty() {
            0
        } else {
            match num_raw.parse() {
                Ok(v) => v,
                Err(_) => {
                    n.bad_number += 1;
                    continue;
                }
            }
        };
        let key_changed = match &cur_key {
            Some((v, i)) => v != voie_n || i != insee,
            None => true,
        };
        if key_changed {
            flush_street!();
            // new street; resolve its commune
            let insee_s = insee.to_string();
            cur_commune_id = match commune_ids.get(&insee_s) {
                Some(id) => *id,
                None => {
                    let id = commune_ids.len() as u32;
                    commune_ids.insert(insee_s.clone(), id);
                    let cname = rec.get(c_commune).unwrap_or("");
                    let cname_n = rec.get(c_commune_n).unwrap_or("");
                    let name_off = push_name(&mut names, cname)?;
                    let mut insee_b = [0u8; 8];
                    let ib = insee_s.as_bytes();
                    // The FST street keys carry the FULL insee, but this fixed meta field
                    // holds 8 bytes: a longer code would be truncated silently and the
                    // "street + city" lookup would rebuild a key that never matches. No
                    // current country needs more; a future one must change the format,
                    // not lose data quietly.
                    if ib.len() > 8 {
                        bail!(
                            "code_insee {insee_s:?} is {} bytes — the format stores 8",
                            ib.len()
                        );
                    }
                    insee_b[..ib.len()].copy_from_slice(ib);
                    communes_meta.extend_from_slice(&insee_b);
                    communes_meta.extend_from_slice(&name_off.to_le_bytes());
                    communes_meta.extend_from_slice(&0u32.to_le_bytes()); // significance — patched after the pass
                    commune_house_count.push(0);
                    commune_lat_sum.push(0.0);
                    commune_lon_sum.push(0.0);
                    commune_names.push((cname_n.to_string(), id));
                    // aliases: the encompassing city name (possibly in several scripts,
                    // packed with '|') also resolves to this settlement
                    if let Some(cp) = c_prov {
                        for prov in rec.get(cp).unwrap_or("").split('|') {
                            if !prov.is_empty() && prov != cname_n {
                                commune_names.push((prov.to_string(), id));
                            }
                        }
                    }
                    n.communes += 1;
                    id
                }
            };
            cur_key = Some((voie_n.to_string(), insee.to_string()));
            cur_display = rec.get(c_voie).unwrap_or("").to_string();
        }

        let rep_s = rec.get(c_rep).unwrap_or("");
        let rep_id = if rep_s.is_empty() {
            0u32
        } else {
            match rep_ids.get(rep_s) {
                Some(id) => *id,
                None => {
                    let id = (reps.len() + 1) as u32;
                    reps.push(rep_s.to_string());
                    rep_ids.insert(rep_s.to_string(), id);
                    id
                }
            }
        };
        // aggregation key: the full display postcode when plausible, else the numeric
        // code_postal; "0"/empty → empty. plausible_pc filters malformed display values.
        let cp_disp_s: &str = c_cp_disp.and_then(|c| rec.get(c)).unwrap_or("");
        let cp_num: &str = match rec.get(c_cp).unwrap_or("") {
            "0" => "",
            s => s,
        };
        let cp_key: &str = if plausible_pc(cp_disp_s) {
            cp_disp_s
        } else {
            cp_num
        };
        if let Some(c) = cur_cps.get_mut(cp_key) {
            *c += 1;
        } else {
            cur_cps.insert(cp_key.to_string(), 1);
        }
        cur_houses.push(House {
            numero,
            rep_id,
            lat,
            lon,
            postcode: cp_key.to_string(),
        });
        n.rows += 1;
    }
    flush_street!();

    // patch significance (address count) into each commune's reserved u32
    for (id, &cnt) in commune_house_count.iter().enumerate() {
        let off = id * COMMUNE_META_SIZE + 12;
        communes_meta[off..off + 4].copy_from_slice(&cnt.to_le_bytes());
    }

    // commune centroids (lat_c i32, lon_c i32): the "city" point for city-only queries
    let mut commune_coords: Vec<u8> = Vec::with_capacity(commune_house_count.len() * 8);
    for id in 0..commune_house_count.len() {
        let cnt = commune_house_count[id].max(1) as f64;
        let lat_c = (commune_lat_sum[id] / cnt).round() as i32;
        let lon_c = (commune_lon_sum[id] / cnt).round() as i32;
        commune_coords.extend_from_slice(&lat_c.to_le_bytes());
        commune_coords.extend_from_slice(&lon_c.to_le_bytes());
    }

    // streets FST
    let streets_fst_bytes = streets_fst.into_inner()?;

    // inverted street-word index sections (subset-of-words search)
    const WORD_POST_CAP: usize = 16384; // caps overly common words (street types)
    let mut words_sorted: Vec<(String, Vec<u32>)> = word_post.into_iter().collect();
    words_sorted.sort_by(|a, b| a.0.cmp(&b.0)); // FST requires bytewise lexicographic key order
    let mut words_fst = MapBuilder::memory();
    let mut word_postings: Vec<u8> = Vec::new();
    for (w, mut ids) in words_sorted {
        ids.sort_unstable();
        ids.dedup();
        if ids.len() > WORD_POST_CAP {
            ids.truncate(WORD_POST_CAP);
        }
        let off = word_postings.len() as u64;
        write_varint(&mut word_postings, ids.len() as u64);
        let mut prev = 0u32;
        for id in ids {
            write_varint(&mut word_postings, (id - prev) as u64);
            prev = id;
        }
        words_fst.insert(w.as_bytes(), off)?;
    }
    let words_fst_bytes = words_fst.into_inner()?;

    // communes FST + postings (same-named communes)
    commune_names.sort();
    let mut communes_fst = MapBuilder::memory();
    let mut postings: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < commune_names.len() {
        let mut j = i + 1;
        while j < commune_names.len() && commune_names[j].0 == commune_names[i].0 {
            j += 1;
        }
        let start = (postings.len() / 4) as u64;
        for cn in &commune_names[i..j] {
            postings.extend_from_slice(&cn.1.to_le_bytes());
        }
        let count = (j - i) as u64;
        if !commune_names[i].0.is_empty() {
            communes_fst.insert(commune_names[i].0.as_bytes(), (start << 16) | count)?;
        }
        i = j;
    }
    let communes_fst_bytes = communes_fst.into_inner()?;

    // rep dictionary (v1: u32)
    let mut reps_sec: Vec<u8> = Vec::new();
    reps_sec.extend_from_slice(&(reps.len() as u32).to_le_bytes());
    for r in &reps {
        let b = r.as_bytes();
        // one u8 length prefix: reject a >255-byte suffix rather than silently truncate
        if b.len() > 255 {
            bail!("rep/house-number suffix {r:?} is {} bytes — the reps section encodes length in one u8 (max 255)", b.len());
        }
        reps_sec.push(b.len() as u8);
        reps_sec.extend_from_slice(b);
    }

    // reverse-geocoding cells: directory (cell,start,count) + street postings
    cells.sort_unstable();
    let mut cells_dir: Vec<u8> = Vec::new();
    let mut cells_post: Vec<u8> = Vec::new();
    {
        let mut i = 0;
        while i < cells.len() {
            let mut j = i;
            let start = (cells_post.len() / 4) as u32;
            while j < cells.len() && cells[j].0 == cells[i].0 {
                cells_post.extend_from_slice(&cells[j].1.to_le_bytes());
                j += 1;
            }
            cells_dir.extend_from_slice(&cells[i].0.to_le_bytes());
            cells_dir.extend_from_slice(&start.to_le_bytes());
            cells_dir.extend_from_slice(&((j - i) as u32).to_le_bytes());
            i = j;
        }
    }
    let mut cells_sec: Vec<u8> = Vec::new();
    cells_sec.extend_from_slice(&((cells_dir.len() / 12) as u32).to_le_bytes());
    cells_sec.extend_from_slice(&cells_dir);
    cells_sec.extend_from_slice(&cells_post);

    // optional sections: parser model and ranking weights. VALIDATE them at build time:
    // both are parsed to Option at open time, so a corrupt file was previously embedded silently and
    // then dropped to `None` on load — a sheet that quietly lost its trained parser/ranking with no
    // error anywhere. Parse here so a bad `--parser`/`--rank` fails the BUILD instead.
    let model_sec: Vec<u8> = match model_path {
        Some(p) => {
            let bytes =
                std::fs::read(p).with_context(|| format!("cannot read parser model {p:?}"))?;
            if !crate::ml::Parser::section_is_valid(&bytes) {
                bail!("parser model {p:?} is not a valid SEC_PARSER section — it would be silently dropped at open time");
            }
            bytes
        }
        None => Vec::new(),
    };
    let rank_sec: Vec<u8> = match rank_path {
        Some(p) => {
            let bytes =
                std::fs::read(p).with_context(|| format!("cannot read ranking weights {p:?}"))?;
            if !crate::query::rank_section_is_valid(&bytes) {
                bail!("ranking weights {p:?} are not a valid SEC_RANK section — they would be silently dropped at open time");
            }
            bytes
        }
        None => Vec::new(),
    };
    // rules-in-data: rule tables are embedded in the file (SEC_RULES); with no rules
    // directory the section is empty and the engine uses built-in defaults (rules.rs)
    let rules_sec: Vec<u8> = match rules_dir {
        Some(d) => {
            // `mut` is needed under the `watermark` feature (marking rewrites this vector in
            // place); clippy --fix stripped it while checking without the feature, breaking
            // that build.
            #[allow(unused_mut)]
            let mut entries = crate::rules::entries_from_tsv_dir(d)
                .with_context(|| format!("cannot read rules from {d:?}"))?;
            eprintln!("rules-in-data: {} entries from {:?}", entries.len(), d);
            #[cfg(feature = "watermark")]
            if let Some(m) = mark {
                // Marking is applied here; what it does lives in the private `mark` module.
                // Entries are sorted first so the result is a deterministic function of the
                // mark alone. Semantics are unchanged: rules install in canonical order.
                entries.sort();
                crate::mark::apply_to_rule_entries(&mut entries, m);
            }
            crate::rules::serialize_entries(&entries)
        }
        None => Vec::new(),
    };

    // WATERMARK (private build only). Without --mark the output is byte-identical, so a sheet
    // built by the public engine is indistinguishable from an unmarked private build. The public
    // build has no watermark feature, so --mark is a no-op there. Construction lives in the
    // private `mark` module and is deliberately not described here.
    #[cfg(feature = "watermark")]
    let mark_sec: Vec<u8> = match mark {
        Some(m) => {
            names.extend_from_slice(&crate::mark::tail_bytes(m));
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            eprintln!("watermark: \"{m}\" applied");
            crate::mark::build_section(m, ts)
        }
        None => Vec::new(),
    };
    #[cfg(not(feature = "watermark"))]
    let mark_sec: Vec<u8> = {
        if mark.is_some() {
            eprintln!("note: --mark ignored (this build has no watermark support)");
        }
        Vec::new()
    };

    // file layout
    let sections: Vec<(u8, &Vec<u8>)> = vec![
        (SEC_COMMUNES_FST as u8, &communes_fst_bytes),
        (SEC_COMMUNES_META as u8, &communes_meta),
        (SEC_COMMUNE_POSTINGS as u8, &postings),
        (SEC_STREETS_FST as u8, &streets_fst_bytes),
        (SEC_STREETS_META as u8, &streets_meta),
        (SEC_HOUSE_BLOCKS as u8, &house_blocks),
        (SEC_NAMES as u8, &names),
        (SEC_REPS as u8, &reps_sec),
        (SEC_CELLS as u8, &cells_sec),
        (SEC_PARSER as u8, &model_sec),
        (SEC_RANK as u8, &rank_sec),
        (SEC_WORDS as u8, &words_fst_bytes),
        (SEC_WORD_POSTINGS as u8, &word_postings),
        (SEC_COMMUNE_COORDS as u8, &commune_coords),
        (SEC_RULES as u8, &rules_sec),
        (SEC_MARK as u8, &mark_sec),
        (SEC_META as u8, &meta_sec),
    ];
    let mut toc: Vec<(u8, u64, u64)> = Vec::new();
    let mut off = header_size() as u64;
    for (id, data) in &sections {
        toc.push((*id, off, data.len() as u64));
        off += data.len() as u64;
    }
    let mut header = Vec::new();
    write_header(&mut header, &toc);

    // Atomic publish with a pre-replace SELF-CHECK: the bytes go to a
    // hidden temp, the ACTUAL reader (Index::open — its per-street invariants included) validates
    // the TEMP, and only a passing sheet is atomically renamed into place. A rejected rebuild leaves
    // the previous good sheet byte-for-byte intact; an interrupted build leaves only a hidden temp.
    // builder and reader cannot drift (the reader IS the gate) and publish stays atomic.
    let chunks: Vec<&[u8]> = std::iter::once(header.as_slice())
        .chain(sections.iter().map(|(_, d)| d.as_slice()))
        .collect();
    write_atomic_validated(out_path, &chunks, |tmp| {
        crate::query::Index::open(tmp).map(|_| ()).map_err(|e| {
            anyhow::anyhow!(
                "build produced a sheet the reader rejects ({e}) — degenerate input? (0 usable rows / all rows dropped)"
            )
        })
    })?;

    // summary report
    let total = off as f64 / 1e6;
    eprintln!(
        "== build finished in {:.1} s ==",
        t0.elapsed().as_secs_f64()
    );
    eprintln!(
        "rows: {} · streets: {} · communes: {} · rep dictionary: {}",
        n.rows,
        n.streets,
        n.communes,
        reps.len()
    );
    if n.empty_streets > 0 {
        eprintln!(
            "dropped {} street(s) whose every row had unparseable coordinates",
            n.empty_streets
        );
    }
    let mb = |b: &Vec<u8>| b.len() as f64 / 1e6;
    eprintln!("sections, MB:");
    eprintln!("  communes_fst      {:8.1}", mb(&communes_fst_bytes));
    eprintln!("  communes_meta     {:8.1}", mb(&communes_meta));
    eprintln!("  commune_postings  {:8.1}", mb(&postings));
    eprintln!("  streets_fst       {:8.1}", mb(&streets_fst_bytes));
    eprintln!("  streets_meta      {:8.1}", mb(&streets_meta));
    eprintln!("  house_blocks      {:8.1}", mb(&house_blocks));
    eprintln!("  names             {:8.1}", mb(&names));
    eprintln!("  reps              {:8.3}", mb(&reps_sec));
    eprintln!("  cells             {:8.1}", mb(&cells_sec));
    eprintln!("  parser_model      {:8.1}", mb(&model_sec));
    eprintln!("  rank              {:8.3}", mb(&rank_sec));
    eprintln!("TOTAL: {total:.1} MB -> {out_path:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the tests that mutate the PROCESS-GLOBAL `GRIDPIN_MAX_UNCOMPRESSED_BYTES` env var
    /// (the decompression-bomb test) against the large under-cap build test, so the tiny cap set by
    /// one never leaks into the other's concurrent build.
    static CAP_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

    // Every build_csv call gets a GLOBALLY-unique temp file via an atomic counter: keying
    // by content (or length) let two parallel tests with the same input race on one file —
    // one truncating it while another mmaps it (flaky failures under `cargo test`).
    fn tag_of(_rows: &str) -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    fn build_csv(rows: &str) -> Result<std::path::PathBuf> {
        let dir = std::env::temp_dir().join(format!("gridpin-builder-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let tag = tag_of(rows);
        let src = dir.join(format!("in-{tag}.csv"));
        let out = dir.join(format!("out-{tag}.bin"));
        std::fs::write(&src, format!("{HDR}{rows}"))?;
        build(&src, &out, None, None, None, None, None)?;
        Ok(out)
    }

    fn test_house(numero: u32, postcode: &str) -> House {
        House {
            numero,
            rep_id: 0,
            lat: 0.0,
            lon: 0.0,
            postcode: postcode.to_string(),
        }
    }

    #[test]
    fn v7_sparse_house_postcode_block_is_exact_and_deterministic() {
        // First appearance is 1012AB, but dictionary ids/bytes are lexicographic: AA then AB.
        // names starts with a legal occupied offset 0, so the dictionary offsets are 2 and 9.
        let houses = [test_house(10, "1012AB"), test_house(12, "1012AA")];
        let mut names = vec![1, b'x'];
        let mut block = Vec::new();
        assert!(write_house_block(&mut block, &mut names, &houses, 0, 0, "street", "001").unwrap());
        assert_eq!(
            names,
            [&[1, b'x'][..], &[6], b"1012AA", &[6], b"1012AB"].concat()
        );
        assert_eq!(
            block,
            vec![
                2, // local non-empty postcode count
                2, 0, 0, 0, // names offset: 1012AA
                9, 0, 0, 0, // names offset: 1012AB
                10, 0, 0, 0, 2, // house 10, local id 2 (AB)
                2, 0, 0, 0, 1, // house 12 delta 2, local id 1 (AA)
            ]
        );

        // Repeating the construction yields byte-identical names and block bytes.
        let mut names2 = vec![1, b'x'];
        let mut block2 = Vec::new();
        write_house_block(&mut block2, &mut names2, &houses, 0, 0, "street", "001").unwrap();
        assert_eq!(names2, names);
        assert_eq!(block2, block);
    }

    #[test]
    fn v7_missing_plus_known_uses_sentinel_grammar_but_uniform_values_do_not() {
        let mut names = vec![1, b'x'];
        let mut block = Vec::new();
        assert!(write_house_block(
            &mut block,
            &mut names,
            &[test_house(1, ""), test_house(2, "1012AA")],
            0,
            0,
            "street",
            "001",
        )
        .unwrap());
        assert_eq!(
            block,
            vec![
                1, // one non-empty dictionary value
                2, 0, 0, 0, // names offset
                1, 0, 0, 0, 0, // missing house id 0
                1, 0, 0, 0, 1, // known house id 1
            ]
        );

        for postcode in ["", "1012AA"] {
            let mut uniform_names = vec![1, b'x'];
            let mut uniform_block = Vec::new();
            assert!(!write_house_block(
                &mut uniform_block,
                &mut uniform_names,
                &[test_house(1, postcode), test_house(2, postcode)],
                0,
                0,
                "street",
                "001",
            )
            .unwrap());
            assert_eq!(
                uniform_block,
                vec![1, 0, 0, 0, 1, 0, 0, 0],
                "all-missing and one-known-value streets keep the old four-varint grammar"
            );
            assert_eq!(uniform_names, vec![1, b'x']);
        }
    }

    #[test]
    fn v7_missing_plus_known_build_sets_sentinel_and_house_ids() {
        let out = build_csv(
            "pc mix,001,ville,,1,,1.0,1.0,Pc Mix,Ville\n\
             pc mix,001,ville,1012AA,2,,1.1,1.1,Pc Mix,Ville\n",
        )
        .expect("mixed missing/known postcode sheet builds and self-checks");
        let data = std::fs::read(out).unwrap();
        let sections = parse_sections(&data).unwrap();
        let (streets_off, streets_len) = sections[SEC_STREETS_META];
        assert_eq!(streets_len, STREET_META_SIZE as u64);
        let street = &data[streets_off as usize..(streets_off + streets_len) as usize];
        assert_eq!(read_u32(street, 32), PC_DISP_AMBIGUOUS);

        let house_off = read_u64(street, 20) as usize;
        let (blocks_off, blocks_len) = sections[SEC_HOUSE_BLOCKS];
        let blocks = &data[blocks_off as usize..(blocks_off + blocks_len) as usize];
        let mut p = house_off;
        assert_eq!(strict_varint(blocks, &mut p), Some(1));
        p += 4; // one little-endian u32 SEC_NAMES offset
        let mut ids = Vec::new();
        for _ in 0..2 {
            for _ in 0..4 {
                strict_varint(blocks, &mut p).expect("four legacy house fields");
            }
            ids.push(strict_varint(blocks, &mut p).expect("v7 local postcode id"));
        }
        assert_eq!(ids, vec![0, 1]);
    }

    #[test]
    fn a_rejected_rebuild_preserves_the_last_known_good_sheet() {
        // adversarial : build() used to REPLACE the final then validate + remove on
        // failure, so a bad rebuild to a path holding a good sheet DESTROYED it. The temp is now
        // validated BEFORE replace, so a rejected rebuild leaves the old sheet byte-for-byte intact.
        let good = build_csv("zzq good,001,ville,10000,1,,7.42,43.73,Zzq Good,Ville\n")
            .expect("good build");
        let dir = good.parent().unwrap();
        let target = dir.join("lastknowngood.bin");
        std::fs::copy(&good, &target).unwrap();
        let before = std::fs::read(&target).unwrap();
        assert!(
            crate::query::Index::open(&target).is_ok(),
            "precondition: the target opens"
        );
        // a degenerate (0-row) rebuild to the SAME path must fail WITHOUT touching the good file
        let dir2 = std::env::temp_dir().join(format!("gridpin-h08r6-{}", std::process::id()));
        std::fs::create_dir_all(&dir2).unwrap();
        let empty = dir2.join("empty.csv");
        std::fs::write(&empty, HDR).unwrap();
        let err = build(&empty, &target, None, None, None, None, None)
            .expect_err("a degenerate rebuild must fail");
        assert!(
            err.to_string().contains("reader rejects") || err.to_string().contains("degenerate"),
            "clear self-check error, got: {err}"
        );
        let after = std::fs::read(&target).unwrap();
        assert_eq!(
            before, after,
            "the last-known-good sheet is byte-for-byte unchanged after a rejected rebuild"
        );
        assert!(
            crate::query::Index::open(&target).is_ok(),
            "and it still opens"
        );
    }

    #[test]
    fn empty_display_name_is_legal_not_bricked() {
        // an EMPTY display name (nom_voie blank, nom_voie_norm set) is
        // LEGAL — name() degrades to "". An earlier "name must be non-empty" invariant bricked
        // such a sheet: build exited 0 but open() rejected it. It must build, open AND answer.
        let out = build_csv("zzq place,001,ville,10000,1,,7.42,43.73,,Ville\n")
            .expect("a blank display name is legal and must build+self-check");
        let idx = crate::query::Index::open(&out).expect("and it opens");
        assert!(
            !idx.query("zzq place 1 ville", 1).is_empty(),
            "and it answers"
        );
    }

    #[test]
    fn mid_sheet_zeroed_record_is_rejected_not_only_sampled() {
        // an earlier check sampled every 65536th street, so a zeroed hole
        // at an UNSAMPLED index opened and answered 0,0. Now ALL records are checked. Build many
        // streets, zero one in the middle, confirm open() rejects.
        let mut rows = String::new();
        for i in 0..400 {
            // zero-padded so lexicographic order == numeric (the builder needs sorted FST keys)
            rows.push_str(&format!(
                "zzqstreet{i:03},001,ville,10000,1,,7.42,43.73,ZzqStreet{i:03},Ville\n"
            ));
        }
        let out = build_csv(&rows).expect("build");
        let mut data = std::fs::read(&out).unwrap();
        // locate streets_meta and zero record 200's bytes (unsampled by the old every-65536th scan)
        let nsec = data[5] as usize;
        let (mut off, mut len) = (0usize, 0usize);
        for i in 0..nsec {
            let p = 6 + i * 17;
            if data[p] as usize == crate::index::SEC_STREETS_META {
                off = u64::from_le_bytes(data[p + 1..p + 9].try_into().unwrap()) as usize;
                len = u64::from_le_bytes(data[p + 9..p + 17].try_into().unwrap()) as usize;
            }
        }
        let rec = 200 * crate::index::STREET_META_SIZE;
        assert!(rec < len, "record 200 exists");
        for b in &mut data[off + rec..off + rec + crate::index::STREET_META_SIZE] {
            *b = 0;
        }
        let mutant = out.parent().unwrap().join("mut-mid-zeroed.bin");
        std::fs::write(&mutant, &data).unwrap();
        assert!(
            crate::query::Index::open(&mutant).is_err(),
            "a zeroed mid-sheet record must be rejected (all records checked, not sampled)"
        );
    }

    #[test]
    fn deleting_the_word_postings_toc_entry_is_rejected() {
        // adversarial : word_postings (13) may be legally EMPTY, but DELETING its TOC
        // entry (a truncated/tampered sheet) left the sheet "valid" while silently killing fuzzy
        // search. It must be REQUIRED_PRESENT; and a NON-empty words FST with empty postings is
        // corrupt. Build a real sheet, drop the section-13 TOC entry, assert open fails.
        let out = build_csv("zzq bazaar,001,ville,10000,1,,7.42,43.73,Zzq Bazaar,Ville\n")
            .expect("build");
        let clean = std::fs::read(&out).unwrap();
        let nsec = clean[5] as usize;
        // rebuild the header WITHOUT the section-13 (word_postings) entry
        let mut data = Vec::new();
        data.extend_from_slice(&clean[0..5]);
        data.push((nsec - 1) as u8); // one fewer section
        let mut body_start = 6 + nsec * 17;
        for i in 0..nsec {
            let p = 6 + i * 17;
            if clean[p] as usize != crate::index::SEC_WORD_POSTINGS {
                data.extend_from_slice(&clean[p..p + 17]);
            }
        }
        // note: offsets in the surviving TOC still point into the original body, which we keep
        let _ = &mut body_start;
        data.extend_from_slice(&clean[6 + nsec * 17..]);
        let dir = out.parent().unwrap();
        let mutant = dir.join("mut-no-postings-toc.bin");
        std::fs::write(&mutant, &data).unwrap();
        assert!(
            crate::query::Index::open(&mutant).is_err(),
            "a sheet missing the word_postings TOC entry must be rejected, not silently lose fuzzy"
        );
    }

    #[test]
    fn zeroed_word_postings_payload_is_rejected_at_open() {
        // adversarial : zeroing EVERY byte of the word_postings PAYLOAD (TOC and length
        // untouched) used to pass open — only presence was checked — and silently killed fuzzy
        // search. The content check decodes every word's list: a zeroed payload fails at the first
        // word (count = 0).
        let out = build_csv("zzq bazaar,001,ville,10000,1,,7.42,43.73,Zzq Bazaar,Ville\n")
            .expect("build");
        let mut data = std::fs::read(&out).unwrap();
        let nsec = data[5] as usize;
        let mut zeroed = false;
        for i in 0..nsec {
            let p = 6 + i * 17;
            if data[p] as usize == crate::index::SEC_WORD_POSTINGS {
                let off = u64::from_le_bytes(data[p + 1..p + 9].try_into().unwrap()) as usize;
                let len = u64::from_le_bytes(data[p + 9..p + 17].try_into().unwrap()) as usize;
                assert!(len > 0, "precondition: this sheet has real postings");
                data[off..off + len].fill(0); // payload only — TOC entry and length stay intact
                zeroed = true;
            }
        }
        assert!(zeroed, "precondition: found the section to zero");
        let dir = out.parent().unwrap();
        let mutant = dir.join("mut-zeroed-postings.bin");
        std::fs::write(&mutant, &data).unwrap();
        let err = crate::query::Index::open(&mutant);
        assert!(
            err.is_err(),
            "a zeroed word_postings payload must be rejected at open, not silently lose fuzzy"
        );
        // and a corrupted street id past the table is caught too (partial corruption)
        let mut data2 = std::fs::read(&out).unwrap();
        for i in 0..nsec {
            let p = 6 + i * 17;
            if data2[p] as usize == crate::index::SEC_WORD_POSTINGS {
                let off = u64::from_le_bytes(data2[p + 1..p + 9].try_into().unwrap()) as usize;
                data2[off] = 0x7f; // count=127 with a 2-byte section -> truncated mid-list
            }
        }
        let mutant2 = dir.join("mut-truncated-postings.bin");
        std::fs::write(&mutant2, &data2).unwrap();
        assert!(
            crate::query::Index::open(&mutant2).is_err(),
            "an oversized count/truncated list must be rejected at open"
        );
    }

    #[test]
    fn continuation_varint_at_eof_in_word_postings_is_rejected() {
        // adversarial : the runtime read_varint returns the ACCUMULATED value at EOF, so
        // replacing the last delta byte with 0x80 (continuation bit, nothing after) decoded as a
        // clean 0 — street id 0 is legal, and the mutant sheet opened with silently-wrong fuzzy.
        // The open-time validator now uses a STRICT decoder: EOF mid-varint = corrupt.
        let out = build_csv("zzq bazaar,001,ville,10000,1,,7.42,43.73,Zzq Bazaar,Ville\n")
            .expect("build");
        let mut data = std::fs::read(&out).unwrap();
        let nsec = data[5] as usize;
        let mut mutated = false;
        for i in 0..nsec {
            let p = 6 + i * 17;
            if data[p] as usize == crate::index::SEC_WORD_POSTINGS {
                let off = u64::from_le_bytes(data[p + 1..p + 9].try_into().unwrap()) as usize;
                let len = u64::from_le_bytes(data[p + 9..p + 17].try_into().unwrap()) as usize;
                assert!(len >= 2, "precondition: real postings");
                data[off + len - 1] = 0x80; // last byte promises continuation, then the section ends
                mutated = true;
            }
        }
        assert!(mutated, "precondition: found the section");
        let dir = out.parent().unwrap();
        let mutant = dir.join("mut-eof-varint.bin");
        std::fs::write(&mutant, &data).unwrap();
        assert!(
            crate::query::Index::open(&mutant).is_err(),
            "a continuation varint at EOF must be rejected at open, not decoded as 0"
        );
    }

    #[test]
    fn short_word_street_builds_and_answers_no_false_reject() {
        // every street word < 3 chars ("yu li") leaves word_postings
        // legally EMPTY — requiring it non-empty rejected a sheet the builder produces. The sheet
        // must build, open AND answer.
        let out = build_csv("yu li,001,ville,10000,1,,7.42,43.73,Yu Li,Ville\n").expect("build");
        let idx = crate::query::Index::open(&out).expect("a legal short-word sheet must open");
        assert!(!idx.query("yu li 1 ville", 1).is_empty(), "and it answers");
    }

    #[test]
    fn degenerate_input_fails_at_build_not_at_open() {
        // a zero-row CSV (or all rows dropped) used to build with
        // exit 0 and produce a sheet the reader then rejects. The builder self-checks its output
        // against the reader's validation, so this must fail AT BUILD with a clear error.
        let err = build_csv("").expect_err("an empty input must not produce a sheet");
        let msg = err.to_string();
        assert!(
            msg.contains("reader rejects") || msg.contains("rows") || msg.contains("empty"),
            "clear degenerate-input error, got: {msg}"
        );
    }

    #[test]
    fn zeroed_section_content_and_swapped_ids_are_rejected_at_open() {
        //(the deeper class): TOC intact, CONTENT corrupt. Zeroing the
        // streets_meta bytes (a sparse hole) or swapping two section ids used to open fine and
        // answer a silent 0,0 / NUL garbage. The structural invariants must reject both.
        let out = build_csv("zzq bazaar,001,ville,10000,1,,7.42,43.73,Zzq Bazaar,Ville\n")
            .expect("build");
        let clean = std::fs::read(&out).unwrap();
        let dir = out.parent().unwrap();
        let nsec = clean[5] as usize;
        let find = |data: &[u8], id: u8| -> (usize, u64, u64) {
            for i in 0..nsec {
                let p = 6 + i * 17;
                if data[p] == id {
                    let off = u64::from_le_bytes(data[p + 1..p + 9].try_into().unwrap());
                    let len = u64::from_le_bytes(data[p + 9..p + 17].try_into().unwrap());
                    return (p, off, len);
                }
            }
            panic!("section {id} not in TOC");
        };
        // (a) zero the streets_meta CONTENT, TOC untouched
        let mut zeroed = clean.clone();
        let (_, off, len) = find(&zeroed, crate::index::SEC_STREETS_META as u8);
        zeroed[off as usize..(off + len) as usize].fill(0);
        let mz = dir.join("mut-zeroed-content.bin");
        std::fs::write(&mz, &zeroed).unwrap();
        assert!(
            crate::query::Index::open(&mz).is_err(),
            "zeroed streets_meta content must be rejected, not answer 0,0"
        );
        // (b) swap the TOC ids of house_blocks (6) and names (7)
        let mut swapped = clean.clone();
        let (p6, _, _) = find(&swapped, crate::index::SEC_HOUSE_BLOCKS as u8);
        let (p7, _, _) = find(&swapped, crate::index::SEC_NAMES as u8);
        swapped[p6] = crate::index::SEC_NAMES as u8;
        swapped[p7] = crate::index::SEC_HOUSE_BLOCKS as u8;
        let ms = dir.join("mut-swapped-ids.bin");
        std::fs::write(&ms, &swapped).unwrap();
        assert!(
            crate::query::Index::open(&ms).is_err(),
            "swapped section ids must be rejected, not answer NUL garbage"
        );
    }

    #[test]
    fn every_required_section_zeroed_out_is_rejected_at_open() {
        // repro: an empty REQUIRED section (e.g. streets_meta) used to open fine and answer
        // queries with a silent 0,0 point — only 3 of the 12 FORMAT-required sections were checked,
        // and only for PRESENCE (a len=0 entry passed). Mutate a real sheet's TOC: for each of the
        // twelve required ids, zero its length; open must fail with a clear error, never "answer".
        let out = build_csv("zzq bazaar,001,ville,10000,1,,7.42,43.73,Zzq Bazaar,Ville\n")
            .expect("build must succeed");
        let clean = std::fs::read(&out).unwrap();
        // control: the unmutated sheet opens and answers
        assert!(
            !crate::query::Index::open(&out)
                .unwrap()
                .query("zzq bazaar 1 ville", 1)
                .is_empty(),
            "control sheet must answer"
        );
        let dir = out.parent().unwrap();
        let nsec = clean[5] as usize;
        for &req in &crate::index::REQUIRED_SECTIONS {
            let mut data = clean.clone();
            let mut found = false;
            for i in 0..nsec {
                let p = 6 + i * 17;
                if data[p] as usize == req {
                    data[p + 9..p + 17].copy_from_slice(&0u64.to_le_bytes()); // len = 0 (absent)
                    found = true;
                    break;
                }
            }
            assert!(
                found,
                "section {req} present in the TOC of a freshly built sheet"
            );
            let mutant = dir.join(format!("mutant-{req}.bin"));
            std::fs::write(&mutant, &data).unwrap();
            let err = crate::query::Index::open(&mutant)
                .err()
                .unwrap_or_else(|| panic!("zeroed required section {req} must fail open"))
                .to_string();
            assert!(
                err.contains("missing or empty"),
                "section {req}: clear corruption error, got: {err}"
            );
        }
    }

    #[test]
    fn street_with_only_bad_coordinates_is_dropped_not_ghosted() {
        // one street: every row has unparseable coordinates; a second street is valid.
        // The bad street used to become a ghost at (0,0) with cell 0.
        let out = build_csv(
            "bad st,001,ville,10000,1,,x,y,Bad St,Ville\n\
             bad st,001,ville,10000,2,,,,Bad St,Ville\n\
             good st,001,ville,10000,1,,7.42,43.73,Good St,Ville\n",
        )
        .expect("build must succeed");
        let idx = crate::query::Index::open(&out).expect("open");
        assert!(!idx.query("good st 1 ville", 1).is_empty());
        assert!(
            idx.query("bad st 1 ville", 1).is_empty(),
            "ghost street must not exist"
        );
    }

    #[test]
    fn unsorted_house_numbers_fail_loudly() {
        // delta encoding assumes sorted numero: unsorted input must be a build ERROR,
        // not silently wrapped garbage
        let err = build_csv(
            "rue a,001,ville,10000,5,,7.42,43.73,Rue A,Ville\n\
             rue a,001,ville,10000,3,,7.42,43.73,Rue A,Ville\n",
        )
        .expect_err("must fail");
        assert!(err.to_string().contains("not sorted"), "{err}");
    }

    #[test]
    fn open_rejects_a_corrupt_toc_without_aborting() {
        // a sheet whose TOC carries an unknown/relabeled section id must fail to
        // open with a clean Err (the strict TOC schema), never a silent open of a broken sheet.
        let out = build_csv("rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n").expect("build");
        let mut bytes = std::fs::read(&out).unwrap();
        // data[6] is the id byte of the first TOC entry; relabel it to an unknown id
        bytes[6] = 200;
        let corrupt = out.with_extension("badtoc.bin");
        std::fs::write(&corrupt, &bytes).unwrap();
        let r = crate::query::Index::open(&corrupt);
        assert!(
            r.is_err(),
            "a corrupt TOC must return Err from open, not open a broken sheet"
        );
    }

    #[test]
    fn gz_decompression_bomb_is_rejected_streaming() {
        // a .gz that expands past the cap must be an ERROR, streamed — not buffered
        // into GBs of RSS first. A tiny cap (env) trips on a modest but over-cap decompressed CSV.
        use std::io::Write as _;
        let dir = std::env::temp_dir().join(format!("gridpin-bomb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("bomb.csv.gz");
        let mut enc = flate2::write::GzEncoder::new(
            std::fs::File::create(&src).unwrap(),
            flate2::Compression::best(),
        );
        enc.write_all(b"nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n").unwrap();
        for _ in 0..1000 {
            enc.write_all(b"rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n")
                .unwrap();
        }
        enc.finish().unwrap();
        let out = dir.join("bomb.bin");
        // GRIDPIN_MAX_UNCOMPRESSED_BYTES is PROCESS-GLOBAL, so setting it here would flake any
        // concurrent build test that reads it (the large under-cap test builds ~960KB). Serialize
        // the two env-mutating cap tests on CAP_ENV_LOCK so they never overlap.
        let _cap_guard = CAP_ENV_LOCK.lock().unwrap();
        std::env::set_var("GRIDPIN_MAX_UNCOMPRESSED_BYTES", "4096");
        let res = build(&src, &out, None, None, None, None, None);
        std::env::remove_var("GRIDPIN_MAX_UNCOMPRESSED_BYTES");
        let err = res.expect_err("an over-cap decompressed stream must fail the build");
        assert!(
            err.to_string().contains("decompress") || err.to_string().contains("cap"),
            "{err}"
        );
    }

    #[test]
    fn a_large_under_cap_gz_input_still_builds() {
        // the streaming guard rejects a bomb (gz_decompression_bomb_is_rejected_streaming);
        // a genuinely large but UNDER-cap .gz must still build — the cap must not fire on legit data.
        use std::io::Write as _;
        // hold CAP_ENV_LOCK so the bomb test cannot have GRIDPIN_MAX_UNCOMPRESSED_BYTES=4096 set
        // during THIS ~960KB build (process-global env var would otherwise flake this test).
        let _cap_guard = CAP_ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("gridpin-m10-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("big.csv.gz");
        let mut enc = flate2::write::GzEncoder::new(
            std::fs::File::create(&src).unwrap(),
            flate2::Compression::fast(),
        );
        enc.write_all(HDR.as_bytes()).unwrap();
        // identical rows (like the bomb test) so the FST keys stay ordered; the point is a large
        // DECOMPRESSED stream that passes UNDER the cap, not key cardinality.
        for n in 1..=20_000 {
            writeln!(enc, "rue a,001,ville,10000,{n},,7.42,43.73,Rue A,Ville").unwrap();
        }
        enc.finish().unwrap();
        let out = dir.join("big.bin");
        build(&src, &out, None, None, None, None, None).expect("a large under-cap gz builds fine");
        let idx = crate::query::Index::open(&out).unwrap();
        assert!(
            !idx.query("rue a 1 ville", 1).is_empty(),
            "the built sheet answers"
        );
    }

    #[test]
    fn cascade_precedence_matrix_and_structured_freeform_parity() {
        // the DOCUMENTED precedence matrix (README.en.md), row by row, and the parity
        // guarantee — free-form query_cascade and structured query_structured_cascade agree on the
        // winner and the poi_layer flag for the same inputs.
        use crate::query::{query_cascade, query_structured_cascade};
        let addr_p = build_csv_meta(
            "rue de la paix,001,ville,10000,1,,7.42,43.73,Rue de la Paix,Ville\n",
            "fr",
            "addresses",
        );
        let poi_p = build_csv_meta(
            "zzqcafe gridpin,001,ville,10000,1,,7.43,43.74,ZzqCafe Gridpin,Ville\n",
            "fr",
            "poi",
        );
        let addr = crate::query::Index::open_address(&addr_p).unwrap();
        let poi = crate::query::Index::open_poi(&poi_p).unwrap();
        let flagged =
            |hs: &[crate::query::Hit]| hs.first().is_some_and(|h| h.flags.contains(&"poi_layer"));

        // ROW 1 — confident address answer: POI never overrides; parity on winner + flag
        let ff = query_cascade(&addr, Some(&poi), "rue de la paix 1 ville", 3);
        let st = query_structured_cascade(
            &addr,
            Some(&poi),
            "rue de la paix",
            Some("1"),
            "ville",
            None,
            3,
        );
        assert!(
            !ff.is_empty() && !st.is_empty(),
            "confident address answers on both paths"
        );
        assert!(
            !flagged(&ff) && !flagged(&st),
            "a confident address is never overridden by POI"
        );
        assert_eq!(ff[0].street, st[0].street, "parity: same winner street");

        // ROW 2/4 — address empty, POI answers: POI wins + flagged, on BOTH paths
        let ff = query_cascade(&addr, Some(&poi), "zzqcafe gridpin ville", 3);
        let st =
            query_structured_cascade(&addr, Some(&poi), "zzqcafe gridpin", None, "ville", None, 3);
        assert!(
            flagged(&ff) && flagged(&st),
            "both paths escalate to the POI layer"
        );
        assert_eq!(ff[0].street, st[0].street, "parity: same POI winner");
        // a house number is ADDRESS-ONLY: it must not break the name-based POI escalation
        let stn = query_structured_cascade(
            &addr,
            Some(&poi),
            "zzqcafe gridpin",
            Some("99"),
            "ville",
            None,
            3,
        );
        assert!(
            flagged(&stn),
            "a structured number does not break POI escalation"
        );

        // ROW 3 — tie: identical content in both layers, weak (fuzzy) query -> the address is kept
        // (escalate_to_poi requires the POI to be STRICTLY more confident). Identical rows give
        // identical confidence by construction, so this is deterministic.
        let addr_tie = build_csv_meta(
            "zzq bazaar,001,ville,10000,1,,7.42,43.73,Zzq Bazaar,Ville\n",
            "fr",
            "addresses",
        );
        let poi_tie = build_csv_meta(
            "zzq bazaar,001,ville,10000,1,,7.42,43.73,Zzq Bazaar,Ville\n",
            "fr",
            "poi",
        );
        let a2 = crate::query::Index::open_address(&addr_tie).unwrap();
        let p2 = crate::query::Index::open_poi(&poi_tie).unwrap();
        let ff = query_cascade(&a2, Some(&p2), "zzq bazar ville", 3); // typo -> fuzzy/weak
        if !ff.is_empty() {
            assert!(
                !flagged(&ff),
                "an equal-confidence POI must not displace the address (tie keeps address)"
            );
        }
        let st = query_structured_cascade(&a2, Some(&p2), "zzq bazar", None, "ville", None, 3);
        if !st.is_empty() {
            assert!(
                !flagged(&st),
                "tie keeps the address on the structured path too"
            );
        }

        // ROW 5 — empty everywhere: no POI loaded + nonsense -> empty on both paths
        let ff = query_cascade(&addr, None, "wwqx never exists 123", 3);
        let st = query_structured_cascade(&addr, None, "wwqx never exists", None, "nulle", None, 3);
        assert!(
            ff.is_empty() || ff[0].precision == "city",
            "free-form nonsense yields nothing house-like"
        );
        assert!(
            st.is_empty() || st[0].precision == "city",
            "structured nonsense yields nothing house-like"
        );
    }

    #[test]
    fn structured_number_reaches_the_poi_escalation_like_freeform() {
        // repro ("Studio 54"): a number is often part of a place NAME. The
        // structured path used to DROP it before querying the POI layer, so free-form
        // "studio 54 ville" found Studio 54 while structured(street=studio, number=54) found a
        // DIFFERENT place. Parity: both paths must agree on the winner.
        use crate::query::{query_cascade, query_structured_cascade};
        let addr_p = build_csv_meta(
            "rue de la paix,001,ville,10000,1,,7.42,43.73,Rue de la Paix,Ville\n",
            "fr",
            "addresses",
        );
        let poi_p = build_csv_meta(
            "studio 54,001,ville,10000,1,,7.43,43.74,Studio 54,Ville\n\
             studio 55,001,ville,10000,1,,7.44,43.75,Studio 55,Ville\n",
            "fr",
            "poi",
        );
        let addr = crate::query::Index::open_address(&addr_p).unwrap();
        let poi = crate::query::Index::open_poi(&poi_p).unwrap();
        let ff = query_cascade(&addr, Some(&poi), "studio 54 ville", 3);
        let st =
            query_structured_cascade(&addr, Some(&poi), "studio", Some("54"), "ville", None, 3);
        assert!(
            !ff.is_empty() && !st.is_empty(),
            "both paths escalate to the POI layer"
        );
        assert_eq!(ff[0].street, "Studio 54", "free-form finds the named place");
        assert_eq!(
            ff[0].street, st[0].street,
            "PARITY: the structured number reaches the POI query — same winner as free-form"
        );
        assert!(
            st[0].flags.contains(&"poi_layer"),
            "the structured answer is the POI answer"
        );
    }

    #[test]
    fn structured_freeform_parity_holds_for_nl_postcode_forms() {
        // with city joined BEFORE postcode the two paths ranked
        // different winners for order-sensitive (NL-form) postcodes. The POI escalation now
        // builds the IDENTICAL token string a free-form user types (street number postcode
        // city), so the winners must agree for any postcode form.
        use crate::query::{query_cascade, query_structured_cascade};
        let addr_p = build_csv_meta(
            "rue de la paix,001,ville,10000,1,,7.42,43.73,Rue de la Paix,Ville\n",
            "fr",
            "addresses",
        );
        let poi_p = build_csv_meta(
            "studio 54,001,ville,10000,1,,7.43,43.74,Studio 54,Ville\n\
             studio 55,001,ville,10000,1,,7.44,43.75,Studio 55,Ville\n",
            "fr",
            "poi",
        );
        let addr = crate::query::Index::open_address(&addr_p).unwrap();
        let poi = crate::query::Index::open_poi(&poi_p).unwrap();
        for pc in ["1012", "1012 nz"] {
            let ff = query_cascade(&addr, Some(&poi), &format!("studio 54 {pc} ville"), 3);
            let st = query_structured_cascade(
                &addr,
                Some(&poi),
                "studio",
                Some("54"),
                "ville",
                Some(pc),
                3,
            );
            assert_eq!(
                ff.first().map(|h| h.street.clone()),
                st.first().map(|h| h.street.clone()),
                "parity must hold for postcode form {pc:?} (including both-empty)"
            );
        }
    }

    #[test]
    fn structured_query_consults_the_poi_cascade() {
        // a structured query for a NAMED PLACE (not a street) escalates to the POI layer,
        // exactly like free-form. The address index knows only a street; the POI layer knows the name.
        let addr_p = build_csv_meta(
            "rue de la paix,001,ville,10000,1,,7.42,43.73,Rue de la Paix,Ville\n",
            "fr",
            "addresses",
        );
        let poi_p = build_csv_meta(
            "zzqcafe gridpin,001,ville,10000,1,,7.43,43.74,ZzqCafe Gridpin,Ville\n",
            "fr",
            "poi",
        );
        let addr = crate::query::Index::open_address(&addr_p).unwrap();
        let poi = crate::query::Index::open_poi(&poi_p).unwrap();
        // structured query for the POI name -> escalates to POI, flagged poi_layer
        let hits = crate::query::query_structured_cascade(
            &addr,
            Some(&poi),
            "zzqcafe gridpin",
            None,
            "ville",
            None,
            3,
        );
        assert!(!hits.is_empty(), "a structured POI query returns a result");
        assert!(
            hits[0].flags.contains(&"poi_layer"),
            "answered by the POI layer: {:?}",
            hits[0].flags
        );
        // with NO POI layer the same structured query never carries the poi_layer flag
        let none = crate::query::query_structured_cascade(
            &addr,
            None,
            "zzqcafe gridpin",
            None,
            "ville",
            None,
            3,
        );
        assert!(
            none.iter().all(|h| !h.flags.contains(&"poi_layer")),
            "no POI layer -> no poi_layer answer"
        );
    }

    #[test]
    fn structured_input_contract_is_bounded_and_honors_k() {
        // lock the structured-input contract — k is honored, oversized fields are bounded
        //, a street with no city still resolves gracefully, and a full query resolves.
        let out = build_csv_meta(
            "rue de la paix,001,ville,10000,1,,7.42,43.73,Rue de la Paix,Ville\n",
            "fr",
            "addresses",
        );
        let idx = crate::query::Index::open(&out).unwrap();
        // a street WITHOUT a city resolves gracefully (no panic) and never exceeds k
        let no_city = idx.query_structured("rue de la paix", Some("1"), "", None, 3);
        assert!(
            no_city.len() <= 3,
            "k is honored for a city-less structured query"
        );
        // an oversized field must not amplify CPU (bound_query cap)
        let big = "x ".repeat(5000);
        let t0 = std::time::Instant::now();
        let huge = idx.query_structured(&big, None, &big, None, 3);
        assert!(
            t0.elapsed().as_secs() < 2,
            "an oversized structured field must not amplify CPU"
        );
        assert!(
            huge.len() <= 3,
            "k is honored even for a garbage oversized query"
        );
        // a complete, valid structured query still resolves
        assert!(
            !idx.query_structured("rue de la paix", Some("1"), "ville", Some("10000"), 3)
                .is_empty(),
            "a valid structured query resolves"
        );
    }

    #[test]
    fn manifest_requires_license_and_source_release_and_stamps_version() {
        // v6 provenance is mandatory + typed. license and source_release are
        // required; the build stamps its own meta_schema + builder_version.
        let dir = std::env::temp_dir().join(format!("gridpin-m05-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, json: &str| {
            let p = dir.join(name);
            std::fs::write(&p, json).unwrap();
            p
        };
        let e = meta_from_manifest(&write(
            "nolic.json",
            r#"{"country":"fr","layer":"addresses","source_release":"x"}"#,
        ))
        .unwrap_err();
        assert!(e.to_string().contains("license"), "{e}");
        let e = meta_from_manifest(&write(
            "norel.json",
            r#"{"country":"fr","layer":"addresses","license":"x"}"#,
        ))
        .unwrap_err();
        assert!(e.to_string().contains("source_release"), "{e}");
        let pairs = meta_from_manifest(&write(
            "ok.json",
            r#"{"country":"fr","layer":"addresses","license":"x","source_release":"y"}"#,
        ))
        .unwrap();
        assert!(
            pairs.iter().any(|(k, v)| k == "meta_schema" && v == "1"),
            "schema stamped"
        );
        assert!(
            pairs.iter().any(|(k, _)| k == "builder_version"),
            "builder version stamped"
        );
    }

    #[test]
    fn overlong_display_name_is_rejected_not_truncated() {
        // the names section uses a u8 length prefix; a >255-byte name must be a
        // build ERROR, never silently stored as a shorter, different value.
        let long = "a".repeat(256);
        let err = build_csv(&format!(
            "rue a,001,ville,10000,1,,7.42,43.73,{long},Ville\n"
        ))
        .expect_err("a 256-byte name must fail the build");
        assert!(err.to_string().contains("max 255"), "{err}");
    }

    #[test]
    fn huge_query_is_bounded_and_fast() {
        // a multi-megabyte query must be bounded BEFORE normalization, not drive
        // seconds of CPU / hundreds of MB of RSS. bound_query truncates at a char boundary.
        use crate::query::{bound_query, MAX_QUERY_BYTES};
        let big = "a".repeat(5_000_000);
        assert_eq!(bound_query(&big).len(), MAX_QUERY_BYTES, "input is capped");
        assert!(
            bound_query("rue a 1 ville").len() < MAX_QUERY_BYTES,
            "a normal query is untouched"
        );
        // char-boundary safety: a multibyte string truncates without panicking
        let cyr = "я".repeat(2000); // 2 bytes each -> 4000 bytes > cap
        let bounded = bound_query(&cyr);
        assert!(bounded.len() <= MAX_QUERY_BYTES && cyr.starts_with(bounded));
        // functional: the engine answers a giant query quickly, returning bounded results
        let out = build_csv("rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n").expect("build");
        let idx = crate::query::Index::open(&out).unwrap();
        let t0 = std::time::Instant::now();
        let hits = idx.query(&big, 5);
        assert!(
            t0.elapsed().as_secs() < 2,
            "a huge query must not amplify CPU"
        );
        assert!(hits.len() <= 5);
    }

    #[test]
    fn validate_lat_lon_rejects_nonfinite_and_out_of_range() {
        use crate::query::validate_lat_lon;
        for (la, lo) in [(48.86, 2.33), (-90.0, -180.0), (90.0, 180.0)] {
            assert!(
                validate_lat_lon(la, lo).is_ok(),
                "({la},{lo}) is a valid point"
            );
        }
        for (la, lo) in [
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
            (f64::NEG_INFINITY, 0.0),
            (91.0, 0.0),
            (0.0, 181.0),
            (-90.001, 0.0),
        ] {
            assert!(
                validate_lat_lon(la, lo).is_err(),
                "({la},{lo}) must be rejected"
            );
        }
    }

    fn build_csv_meta(rows: &str, country: &str, layer: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gridpin-builder-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tag = format!("{country}-{layer}-{}", tag_of(rows));
        let src = dir.join(format!("in-{tag}.csv"));
        let out = dir.join(format!("out-{tag}.bin"));
        let man = dir.join(format!("man-{tag}.json"));
        std::fs::write(&src, format!("{HDR}{rows}")).unwrap();
        std::fs::write(
            &man,
            format!(
                "{{\"country\": \"{country}\", \"layer\": \"{layer}\", \"license\": \"test\", \"source_release\": \"test\"}}"
            ),
        )
        .unwrap();
        build(&src, &out, None, None, None, None, Some(&man)).unwrap();
        out
    }

    #[test]
    fn check_pair_refuses_mismatched_country_and_layer() {
        // v6 identity: a POI layer from another country, or the wrong
        // layer kind, must be rejected before any query runs
        let fr = crate::query::Index::open(&build_csv_meta(
            "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n",
            "fr",
            "addresses",
        ))
        .unwrap();
        let it = crate::query::Index::open(&build_csv_meta(
            "via a,002,roma,00100,1,,12.5,41.9,Via A,Roma\n",
            "it",
            "addresses",
        ))
        .unwrap();
        let frpoi = crate::query::Index::open(&build_csv_meta(
            "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n",
            "fr",
            "poi",
        ))
        .unwrap();
        // wrong country: FR sheet + IT "poi" would be a mismatch; use it as a poi over fr
        assert!(
            crate::query::check_pair(&fr, &it).is_err(),
            "IT layer over FR must fail"
        );
        assert!(
            crate::query::check_pair(&fr, &frpoi).is_ok(),
            "FR + FR-poi is a valid pair"
        );
        // passing an address sheet where a POI layer is expected is wrong
        assert!(crate::query::check_pair(&fr, &it).is_err());
    }

    #[test]
    fn open_address_refuses_a_poi_sheet_as_the_primary_index() {
        // a POI-only sheet opened as the MAIN index would answer address
        // queries with places. open_address must reject layer=poi, while address sheets and
        // pre-v6 sheets (no layer meta) open normally.
        let poi = build_csv_meta(
            "cafe,001,ville,10000,1,,7.42,43.73,Cafe,Ville\n",
            "fr",
            "poi",
        );
        assert!(
            crate::query::Index::open_address(&poi).is_err(),
            "a POI sheet must not open as the primary address index"
        );
        let addr = build_csv_meta(
            "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n",
            "fr",
            "addresses",
        );
        assert!(
            crate::query::Index::open_address(&addr).is_ok(),
            "an address sheet opens fine"
        );
        // pre-v6 / no-meta sheet (built without --meta) has no layer key: must still open
        let bare = build_csv("rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n").expect("build");
        assert!(
            crate::query::Index::open_address(&bare).is_ok(),
            "a no-meta sheet opens fine"
        );
    }

    #[test]
    fn open_poi_refuses_an_address_sheet_as_a_poi_layer() {
        // gridpin_load_poi used the permissive open, so an ADDRESS sheet loaded as a
        // POI layer was accepted. open_poi is the symmetric guard to open_address.
        let addr = build_csv_meta(
            "rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n",
            "fr",
            "addresses",
        );
        assert!(
            crate::query::Index::open_poi(&addr).is_err(),
            "an address sheet must not open as a POI layer"
        );
        let poi = build_csv_meta(
            "cafe,001,ville,10000,1,,7.42,43.73,Cafe,Ville\n",
            "fr",
            "poi",
        );
        assert!(
            crate::query::Index::open_poi(&poi).is_ok(),
            "a real POI layer opens fine"
        );
        // a no-meta lab sheet has no layer key: still allowed, matching open_address's leniency
        let bare = build_csv("rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n").expect("build");
        assert!(
            crate::query::Index::open_poi(&bare).is_ok(),
            "a no-meta sheet opens fine"
        );
    }

    #[test]
    fn every_public_reverse_entry_is_strict_on_bad_coords() {
        // the frozen DoD is the invariant INSIDE the public API — BOTH public entries
        // (`reverse` and `try_reverse`) error on NaN/Inf/out-of-range, never a silent empty vec.
        // The lenient behaviour survives only as the crate-private defense-in-depth layer.
        let out = build_csv("rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n").expect("build");
        let idx = crate::query::Index::open(&out).unwrap();
        for (la, lo) in [
            (f64::NAN, 7.42),
            (43.73, f64::INFINITY),
            (91.0, 0.0),
            (0.0, 181.0),
        ] {
            assert!(
                idx.try_reverse(la, lo, 5).is_err(),
                "try_reverse must error on ({la},{lo})"
            );
            assert!(
                idx.reverse(la, lo, 5).is_err(),
                "public reverse is STRICT too on ({la},{lo})"
            );
            // the crate-private lenient layer still shields a hypothetical validation bypass
            assert!(
                idx.reverse_lenient(la, lo, 5).is_empty(),
                "the private lenient layer yields nothing on ({la},{lo})"
            );
        }
        assert!(
            idx.try_reverse(43.73, 7.42, 5).is_ok(),
            "a valid coord is Ok"
        );
        assert!(
            !idx.reverse(43.73, 7.42, 5).unwrap().is_empty(),
            "a valid coord near the data still answers"
        );
    }

    #[test]
    fn build_validates_parser_and_rank_sections() {
        let dir = std::env::temp_dir().join(format!("gridpin-m01-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.csv");
        std::fs::write(
            &src,
            format!("{HDR}rue de test,001,ville,10000,1,,7.42,43.73,Rue de Test,Ville\n"),
        )
        .unwrap();
        let parser = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../ml/parser_v0.bin"));
        let rank = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../ml/rank_v0.bin"));
        // the committed models build cleanly AND the capability survives into the sheet
        let good = dir.join("good.bin");
        build(&src, &good, Some(parser), Some(rank), None, None, None)
            .expect("committed models build");
        let idx = crate::query::Index::open(&good).unwrap();
        assert!(
            idx.has_parser(),
            "the trained parser survives into the sheet"
        );
        assert!(
            idx.has_rank(),
            "the trained ranking survives into the sheet"
        );
        // a CORRUPT rank file now fails the BUILD (was silently embedded then dropped to None on open)
        let bad_rank = dir.join("bad_rank.bin");
        std::fs::write(&bad_rank, b"not a GPRK section at all").unwrap();
        let out = dir.join("bad.bin");
        let err = build(&src, &out, None, Some(&bad_rank), None, None, None).unwrap_err();
        assert!(
            err.to_string().contains("SEC_RANK"),
            "a corrupt --rank must fail the build: {err}"
        );
    }

    #[test]
    fn build_rejects_a_manifest_that_overflows_the_meta_cap() {
        // adversarial: input_blake2b256 is stamped AFTER meta_from_manifest's 1024 guard,
        // so a 1024-pair manifest would emit 1025 pairs — which decode_meta drops entirely (n>1024),
        // silently losing ALL provenance + country/layer identity. The build must REJECT it.
        let dir = std::env::temp_dir().join(format!("gridpin-meta1025-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.csv");
        std::fs::write(
            &src,
            format!("{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n"),
        )
        .unwrap();
        // country/layer/license/source_release + 1016 filler = 1020 user keys; +4 stamped = 1024;
        // + input_blake2b256 = 1025 -> must be rejected
        let mut m = String::from(
            "{\"country\":\"fr\",\"layer\":\"addresses\",\"license\":\"t\",\"source_release\":\"t\"",
        );
        for i in 0..1016 {
            m.push_str(&format!(",\"k{i}\":\"v\""));
        }
        m.push('}');
        let man = dir.join("big.json");
        std::fs::write(&man, &m).unwrap();
        let out = dir.join("out.bin");
        let err = build(&src, &out, None, None, None, None, Some(&man))
            .expect_err("a 1025-pair meta must be rejected");
        assert!(
            err.to_string().contains("1024"),
            "must cite the 1024 cap: {err}"
        );
    }

    #[test]
    fn reverse_survives_a_corrupt_cell_count_without_oob_panic() {
        // adversarial: a tampered SEC_CELLS `count` must not drive an out-of-bounds read that PANICS
        // reverse(). Build a sheet, corrupt the first cell entry's count to u32::MAX, reverse.
        let out = build_csv("rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n").expect("build");
        let mut bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[0..4], b"GPC0");
        let nsec = bytes[5] as usize;
        let mut cells_off = None;
        let mut p = 6;
        for _ in 0..nsec {
            let id = bytes[p];
            let off = u64::from_le_bytes(bytes[p + 1..p + 9].try_into().unwrap()) as usize;
            if id == crate::index::SEC_CELLS as u8 {
                cells_off = Some(off);
            }
            p += 17;
        }
        let cells_off = cells_off.expect("SEC_CELLS present");
        // section = [n_dir u32][entries: (cell u32, start u32, count u32)*]; first entry's count is
        // at cells_off + 4 (n_dir) + 8 (cell, start)
        let count_off = cells_off + 4 + 8;
        bytes[count_off..count_off + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let bad = out.with_extension("corruptcell.bin");
        std::fs::write(&bad, &bytes).unwrap();
        let idx = crate::query::Index::open(&bad).expect("opens (section bounds still valid)");
        // must NOT panic (the bounds guard returns no streets for the corrupt cell)
        let _ = idx.reverse(43.73, 7.42, 3); // strict Result: corrupt data must Err/empty, not panic
    }

    #[test]
    fn open_rejects_a_present_but_corrupt_parser_section() {
        // strict OPEN: a non-empty but unparseable SEC_PARSER must FAIL the open (corrupt/
        // tampered sheet), not silently drop to None. Build with the real parser, then corrupt its
        // magic (GPML -> XPML) and re-open.
        let dir = std::env::temp_dir().join(format!("gridpin-m01open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.csv");
        std::fs::write(
            &src,
            format!("{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n"),
        )
        .unwrap();
        let parser = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../ml/parser_v0.bin"));
        let good = dir.join("good.bin");
        build(&src, &good, Some(parser), None, None, None, None).unwrap();
        assert!(
            crate::query::Index::open(&good).unwrap().has_parser(),
            "sanity: opens with a parser"
        );
        let mut bytes = std::fs::read(&good).unwrap();
        let pos = bytes
            .windows(4)
            .position(|w| w == b"GPML")
            .expect("parser magic present");
        bytes[pos] = b'X'; // corrupt the SEC_PARSER magic
        let bad = dir.join("bad.bin");
        std::fs::write(&bad, &bytes).unwrap();
        let err = crate::query::Index::open(&bad)
            .err()
            .expect("corrupt parser must fail open")
            .to_string();
        assert!(
            err.contains("SEC_PARSER"),
            "a corrupt parser section must fail open: {err}"
        );
    }

    #[test]
    fn build_stamps_input_hash_and_target_provenance() {
        // a built sheet records the content hash of its input + the build target, so
        // provenance pins exactly which input produced it. Deterministic: same input -> same hash.
        let dir = std::env::temp_dir().join(format!("gridpin-m05prov-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.csv");
        std::fs::write(
            &src,
            format!("{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n"),
        )
        .unwrap();
        let man = dir.join("man.json");
        std::fs::write(
            &man,
            r#"{"country":"fr","layer":"addresses","license":"test","source_release":"test"}"#,
        )
        .unwrap();
        let out = dir.join("out.bin");
        build(&src, &out, None, None, None, None, Some(&man)).unwrap();
        let idx = crate::query::Index::open(&out).unwrap();
        let get = |k: &str| {
            idx.meta()
                .iter()
                .find(|(kk, _)| kk == k)
                .map(|(_, v)| v.clone())
        };
        let expect = file_hash_hex(&src).unwrap();
        assert_eq!(
            get("input_blake2b256").as_deref(),
            Some(expect.as_str()),
            "input hash recorded"
        );
        assert!(
            get("builder_target").is_some_and(|t| t.contains('-')),
            "build target recorded"
        );
        assert!(get("builder_version").is_some(), "builder version recorded");
        assert!(
            get("builder_git").is_some(),
            "git commit provenance recorded"
        );
        // rebuild the same input -> identical input hash (deterministic provenance)
        let out2 = dir.join("out2.bin");
        build(&src, &out2, None, None, None, None, Some(&man)).unwrap();
        let idx2 = crate::query::Index::open(&out2).unwrap();
        let h2 = idx2
            .meta()
            .iter()
            .find(|(k, _)| k == "input_blake2b256")
            .map(|(_, v)| v.clone());
        assert_eq!(
            h2.as_deref(),
            Some(expect.as_str()),
            "same input -> same hash"
        );
    }

    #[test]
    fn finalize_replace_publishes_durably_for_repack_and_batch_paths() {
        // repack (main.rs) and batch-dump publish via finalize_replace / (now) the shared
        // path, which must fsync the parent dir just like write_atomic — not a raw rename. Exercise
        // the finalize path into a nested dir and confirm it publishes + round-trips.
        let dir =
            std::env::temp_dir().join(format!("gridpin-m03fin-{}/nested", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join(".t.tmp");
        let out = dir.join("published.bin");
        std::fs::write(&tmp, b"repack-bytes").unwrap();
        finalize_replace(&tmp, &out).expect("finalize publishes durably");
        assert_eq!(std::fs::read(&out).unwrap(), b"repack-bytes");
        assert!(!tmp.exists(), "the temp was renamed away");
    }

    #[test]
    fn write_atomic_publishes_into_a_subdir_and_overwrites() {
        // write_atomic now fsyncs the parent directory after the rename so the
        // publish is crash-durable. This exercises that path (nested dir) and correctness; the
        // durability itself is by construction (the dir fsync cannot be observed from a unit test).
        let dir =
            std::env::temp_dir().join(format!("gridpin-m03-{}/nested/deep", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("f.bin");
        write_atomic(&out, &[b"hello ", b"world"]).expect("first write");
        assert_eq!(std::fs::read(&out).unwrap(), b"hello world");
        write_atomic(&out, &[b"second"]).expect("rename-replace over an existing file");
        assert_eq!(std::fs::read(&out).unwrap(), b"second");
    }

    #[test]
    fn bad_coordinates_and_house_numbers_are_dropped_not_coerced() {
        // out-of-range coords (lat 999) were accepted and saturated to garbage;
        // a non-numeric house number ("abc") became 0. Both rows must be dropped instead.
        let out = build_csv(
            "rue ok,001,ville,10000,5,,7.42,43.73,Rue OK,Ville\n\
             rue bad,002,ville,10000,7,,999,999,Rue Bad,Ville\n\
             rue num,003,ville,10000,abc,,7.42,43.73,Rue Num,Ville\n",
        )
        .expect("build");
        let idx = crate::query::Index::open(&out).unwrap();
        // the out-of-range street resolves to nothing house-precise (row dropped)
        let bad = idx.query("rue bad 7 ville", 1);
        assert!(
            bad.first().is_none_or(|h| h.precision != "house"),
            "row with lat/lon 999 must be dropped, not returned as a house"
        );
        // the alphabetic number is dropped: no house 0 synthesized
        let num = idx.query("rue num ville", 1);
        assert!(
            num.first().is_none_or(|h| h.precision != "house"),
            "row with house number 'abc' must be dropped, not coerced to 0"
        );
        // the good row still resolves
        assert_eq!(idx.query("rue ok 5 ville", 1)[0].precision, "house");
    }

    #[test]
    fn dropped_first_row_does_not_poison_commune_name() {
        // the commune was created before coordinate validation, so a dropped
        // bad-coordinate first row named the commune "Wrong". The good row must win.
        let out = build_csv(
            "rue a,001,wrong,10000,1,,nan,nan,Rue A,Wrong\n\
             rue a,001,town,10000,3,,7.42,43.73,Rue A,Town\n",
        )
        .expect("build");
        let idx = crate::query::Index::open(&out).unwrap();
        assert_eq!(
            idx.query("rue a 3 town", 1)[0].commune,
            "Town",
            "commune must be named from the accepted row, not the dropped one"
        );
    }

    #[test]
    fn k_zero_returns_no_hits_on_any_path() {
        // k=0 returned one city-only hit through the bare-place-name shortcut.
        let out = build_csv("rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n").expect("build");
        let idx = crate::query::Index::open(&out).unwrap();
        assert!(
            idx.query("ville", 0).is_empty(),
            "city-only path must honor k=0"
        );
        assert!(
            idx.query("rue a 1 ville", 0).is_empty(),
            "house path must honor k=0"
        );
        assert!(idx
            .query_structured("rue a", Some("1"), "ville", None, 0)
            .is_empty());
    }

    #[test]
    fn structured_postcode_keeps_all_six_digits() {
        // the 5-digit cap truncated a 6-digit postcode (200456 -> 20045), so it
        // stopped matching. Two houses on same-named streets in different postcodes; the
        // 6-digit postcode must pick the matching one.
        let out = build_csv(
            "centralnaya,001,tashkent,200456,10,,69.24,41.31,Centralnaya,Tashkent\n\
             centralnaya,002,tashkent,100123,10,,69.30,41.35,Centralnaya,Tashkent\n",
        )
        .expect("build");
        let idx = crate::query::Index::open(&out).unwrap();
        let hit = idx.query_structured("centralnaya", Some("10"), "tashkent", Some("200456"), 1);
        assert!(!hit.is_empty());
        // the 6-digit postcode must steer to the 200456 house (lon ~69.24), not 100123
        assert!(
            (hit[0].0.lon - 69.24).abs() < 0.02,
            "6-digit postcode must not be truncated to 5 (got lon {})",
            hit[0].0.lon
        );
    }

    #[test]
    fn mixed_postcode_street_is_house_accurate_but_street_level_stays_empty() {
        // v7 resolves postcode accuracy at the correct level: an exact house may return its own,
        // while the mixed street as a whole still emits EMPTY rather than a neighbour's value.
        let dir = std::env::temp_dir().join(format!("gridpin-h10-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("h10.csv");
        // needs the code_postal_display column (build reads headers dynamically)
        std::fs::write(
            &src,
            "nom_voie_norm,code_insee,nom_commune_norm,code_postal,code_postal_display,numero,rep,lon,lat,nom_voie,nom_commune\n\
             damrak,001,amsterdam,1012,1012AA,1,,4.90,52.37,Damrak,Amsterdam\n\
             damrak,001,amsterdam,1012,1012AB,3,,4.90,52.37,Damrak,Amsterdam\n",
        )
        .unwrap();
        let out = dir.join("h10.bin");
        build(&src, &out, None, None, None, None, None).expect("build");
        let idx = crate::query::Index::open(&out).unwrap();
        let house = idx.query("damrak 1 amsterdam", 1);
        assert!(!house.is_empty(), "the house still resolves");
        assert_eq!(house[0].postcode, "1012AA");
        let street = idx.query("damrak amsterdam", 1);
        assert!(!street.is_empty(), "the street still resolves");
        assert_eq!(
            street[0].postcode, "",
            "a mixed-postcode street emits no postcode, not a neighbour's"
        );
    }

    #[test]
    fn poi_cascade_consulted_only_when_address_is_weak() {
        // query_cascade contract (untested before): a CONFIDENT address answer skips the
        // POI layer entirely; a weak/empty address answer falls through to POI and the
        // winning POI hit is flagged `poi_layer`. Two address-shaped sheets stand in for
        // (addresses, POI) — the cascade only calls .query() on each.
        let addr = crate::query::Index::open(
            &build_csv("rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n").expect("build addr"),
        )
        .unwrap();
        let poi = crate::query::Index::open(
            &build_csv("cafe central,001,ville,10000,1,,7.43,43.74,Cafe Central,Ville\n")
                .expect("build poi"),
        )
        .unwrap();

        // (1) exact house in the address sheet -> confident -> POI NOT consulted
        let strong = crate::query::query_cascade(&addr, Some(&poi), "rue a 1 ville", 1);
        assert!(!strong.is_empty());
        assert!(
            !strong[0].flags.contains(&"poi_layer"),
            "confident address must skip POI"
        );

        // (2) a name only the POI sheet knows -> address weak/empty -> POI answers, flagged
        let via_poi = crate::query::query_cascade(&addr, Some(&poi), "cafe central ville", 1);
        assert!(
            !via_poi.is_empty(),
            "POI should answer when the address layer cannot"
        );
        assert!(
            via_poi[0].flags.contains(&"poi_layer"),
            "a POI-sourced hit must carry poi_layer"
        );

        // (3) without the POI layer, the same query gets no POI rescue
        let no_poi = crate::query::query_cascade(&addr, None, "cafe central ville", 1);
        assert!(no_poi
            .first()
            .is_none_or(|h| !h.flags.contains(&"poi_layer")));
    }

    #[test]
    fn corrupt_sheet_stays_recoverable_never_ub_or_abort() {
        // The "corrupt sheet crashes the host" class is easy to regress, so lock it here.
        // Contract we CAN guarantee and lock here: no corruption causes memory-unsafety
        // or an un-catchable abort — every failure is a cleanly UNWINDING panic (or a
        // clean Err), which the embedding boundaries catch (DuckDB `no_panic`, pyo3 wraps
        // pymethods). Our own parsers (varint/postings/meta/street_meta) are guarded and
        // never panic; a byte-flip inside an FST section can still panic INSIDE the `fst`
        // crate, but it unwinds and is caught — the host survives. This test fails loudly
        // if any variant causes an abort/segfault (process dies) — e.g. a future
        // panic="abort" release profile would break host protection.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // silence the expected caught panics
                                                // input MUST be sorted by street name (FST key order): avenue < rue
        let out = build_csv(
            "avenue des champs,001,paris,75008,10,,2.307,48.872,Avenue des Champs,Paris\n\
             rue de la paix,001,paris,75002,1,,2.331,48.869,Rue de la Paix,Paris\n\
             rue de la paix,001,paris,75002,3,bis,2.332,48.870,Rue de la Paix,Paris\n",
        )
        .expect("build");
        let good = std::fs::read(&out).unwrap();
        let dir = out.parent().unwrap();

        let mut variants: Vec<Vec<u8>> = Vec::new();
        // truncations at many lengths (past the header, so parse_sections may still pass)
        for cut in [
            300usize,
            500,
            700,
            good.len() / 2,
            good.len() - 1,
            good.len() - 20,
        ] {
            if cut < good.len() {
                variants.push(good[..cut].to_vec());
            }
        }
        // single-byte flips scattered through the body (offsets, counts, varints)
        for off in (300..good.len()).step_by(37) {
            let mut v = good.clone();
            v[off] ^= 0xFF;
            variants.push(v);
        }

        for (i, bytes) in variants.iter().enumerate() {
            let bad = dir.join(format!("corrupt-{i}.bin"));
            std::fs::write(&bad, bytes).unwrap();
            let path = bad.clone();
            let r = std::panic::catch_unwind(move || {
                if let Ok(idx) = crate::query::Index::open(&path) {
                    // if it opened, exercise the read paths that touch in-section offsets
                    let _ = idx.query("rue de la paix 1 paris", 3);
                    let _ = idx.reverse(48.869, 2.331, 3); // Result ignored: probing for panics
                }
            });
            // r.is_ok() -> no panic; r.is_err() -> a panic was CLEANLY caught (host safe).
            // Either is acceptable; an abort/segfault would instead kill this test process.
            let _ = (r, i);
        }
        std::panic::set_hook(prev_hook);
    }

    #[test]
    fn reverse_precision_bands_and_distinct_k() {
        // reverse precision: <=50m house, 50-250m near, >250m approximate.
        // Two houses on one street ~334m apart (0.003 deg lat); query at the first.
        let out = build_csv(
            "rue a,001,ville,10000,1,,7.4200,43.7300,Rue A,Ville\n\
             rue a,001,ville,10000,3,,7.4200,43.7330,Rue A,Ville\n",
        )
        .expect("build");
        let idx = crate::query::Index::open(&out).unwrap();
        let hits = idx.reverse(43.7300, 7.4200, 2).unwrap();
        assert_eq!(hits.len(), 2, "two distinct houses");
        assert_eq!(hits[0].precision, "house", "0 m from house 1");
        assert_eq!(
            hits[1].precision, "approximate",
            "~334 m from house 3 (>250m band)"
        );
        assert!(
            hits[0].confidence > hits[1].confidence,
            "confidence decays with distance"
        );
    }

    #[test]
    fn reverse_dedups_true_duplicates_still_returns_k_distinct() {
        // Two source rows for the SAME (street, number, no-suffix) at slightly different
        // coords are true duplicates: the nearest survives, and -k still yields k DISTINCT
        // addresses.
        let out = build_csv(
            "rue a,001,ville,10000,5,,7.4200,43.7300,Rue A,Ville\n\
             rue a,001,ville,10000,5,,7.4201,43.7301,Rue A,Ville\n\
             rue a,001,ville,10000,7,,7.4202,43.7302,Rue A,Ville\n",
        )
        .expect("build");
        let idx = crate::query::Index::open(&out).unwrap();
        let hits = idx.reverse(43.7300, 7.4200, 2).unwrap();
        let nums: Vec<_> = hits
            .iter()
            .map(|h| h.housenumber.clone().unwrap())
            .collect();
        assert_eq!(
            nums.len(),
            nums.iter().collect::<std::collections::HashSet<_>>().len(),
            "no duplicate address in the reverse result: {nums:?}"
        );
    }

    #[test]
    fn query_structured_resolves_prebound_fields() {
        // the structured path (batch with pre-parsed street/number/city) must resolve a
        // house without any field-boundary guessing — untested before.
        let out =
            build_csv("rue de la paix,001,paris,75002,1,,2.331,48.869,Rue de la Paix,Paris\n")
                .expect("build");
        let idx = crate::query::Index::open(&out).unwrap();
        let hits = idx.query_structured("rue de la paix", Some("1"), "paris", Some("75002"), 1);
        assert!(!hits.is_empty(), "structured query must resolve the house");
        assert_eq!(hits[0].0.precision, "house");
    }

    #[test]
    fn reverse_contract_keeps_suffixed_houses_distinct() {
        // 12 and 12A are different addresses: the reverse dedup must not
        // collapse them, the street field must be the pure name, and the number must
        // ship in its own housenumber field.
        let out = build_csv(
            "rue a,001,ville,10000,12,,7.4200,43.7300,Rue A,Ville\n\
             rue a,001,ville,10000,12,a,7.4201,43.7301,Rue A,Ville\n",
        )
        .expect("build must succeed");
        let idx = crate::query::Index::open(&out).expect("open");
        let hits = idx.reverse(43.7300, 7.4200, 3).unwrap();
        assert_eq!(hits.len(), 2, "12 and 12A must both survive dedup");
        assert!(
            hits.iter().all(|h| h.street == "Rue A"),
            "street must be the pure name"
        );
        let nums: Vec<_> = hits
            .iter()
            .map(|h| h.housenumber.clone().unwrap())
            .collect();
        assert!(
            nums.contains(&"12".to_string()) && nums.contains(&"12a".to_string()),
            "{nums:?}"
        );
    }

    #[test]
    fn overlong_insee_fails_loudly_instead_of_truncating() {
        // an insee longer than the 8-byte meta field would silently break
        // street+city lookups for that commune
        let err = build_csv("rue a,123456789,ville,10000,1,,7.42,43.73,Rue A,Ville\n")
            .expect_err("must fail");
        assert!(err.to_string().contains("8"), "{err}");
    }
}
