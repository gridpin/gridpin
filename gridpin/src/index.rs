//! Index format v7 ("GPC0"): header + table of contents + sections, all little-endian.
//!
//! Sections:
//!   1 communes_fst      — FST: commune name (normalized) → (start<<16 | count) into postings
//!   2 communes_meta     — 16 B per commune: insee[5] + pad[3] + name_off u32 + reserved u32
//!   3 commune_postings  — u32 commune_id array (for same-named communes)
//!   4 streets_fst       — FST: "street 0x1F insee" → street_id
//!   5 streets_meta      — 36 B per street: lat_c i32, lon_c i32, commune_id u32,
//!                         postcode u32 (numeric, used for ranking), name_off u32, house_off u64,
//!                         house_count u32, pc_disp_off u32 (offset of the full display
//!                         postcode string in names, e.g. NL "1012XJ"; 0 = none → derive from
//!                         numeric; u32::MAX = no street-level value and, in v7, sparse house PCs)
//!   6 house_blocks      — per street: varint house numbers (first absolute, then deltas),
//!                         rep_id varint, Δlat/Δlon zigzag-varint from the street center (1e-7°).
//!                         House-postcode streets begin with a sorted local postcode dictionary;
//!                         their house entries have one additional local-postcode-id varint.
//!   7 names             — display-name blob: u8 length + UTF-8 bytes
//!   8 reps              — house-number suffix dictionary (bis/ter/…): u32 count, then u8 len + bytes

use anyhow::{bail, Result};

pub const MAGIC: &[u8; 4] = b"GPC0";
pub const VERSION: u8 = 7; // v7: sparse per-house postcodes on postcode-ambiguous streets
pub const MIN_READ_VERSION: u8 = 6; // migration reader: v6 keeps the legacy four-varint house grammar
pub const N_SECTIONS: usize = 17;

pub const SEC_COMMUNES_FST: usize = 1;
pub const SEC_COMMUNES_META: usize = 2;
pub const SEC_COMMUNE_POSTINGS: usize = 3;
pub const SEC_STREETS_FST: usize = 4;
pub const SEC_STREETS_META: usize = 5;
pub const SEC_HOUSE_BLOCKS: usize = 6;
pub const SEC_NAMES: usize = 7;
pub const SEC_REPS: usize = 8; // v1: u32 count, u32 id
pub const SEC_CELLS: usize = 9; // 0.01° grid: (cell u32, start u32, count u32)* + u32 street postings
pub const SEC_PARSER: usize = 10; // optional: parser model (dim u32, classes u8, intercept f32*, coef f32*)
pub const SEC_RANK: usize = 11; // optional: ranking weights (n u8, bias f32, w f32*)
pub const SEC_WORDS: usize = 12; // FST: street word (≥3 chars, not a type word) → u64 offset into postings
pub const SEC_WORD_POSTINGS: usize = 13; // per word: varint(count) + delta-varint street_id (sorted)
pub const SEC_COMMUNE_COORDS: usize = 14; // per commune: lat_c i32, lon_c i32 (centroid = city point)
pub const SEC_RULES: usize = 15; // rules-in-data: u32 n + (class u8, klen u16, key, vlen u16, val)*; see rules.rs
pub const SEC_MARK: usize = 16; // optional: per-copy watermark section (GPMK…); older engines ignore it
pub const SEC_META: usize = 17; // v6: provenance + identity — flat string map, see encode_meta/decode_meta

/// 0.01° (~1.1 km) grid cell: latitude and longitude are quantized
pub fn cell_of(lat: f64, lon: f64) -> u32 {
    let la = (((lat + 90.0) / 0.01).floor() as i64).clamp(0, 17999) as u32;
    let lo = (((lon + 180.0) / 0.01).floor() as i64).clamp(0, 35999) as u32;
    la * 36000 + lo
}

pub const STREET_META_SIZE: usize = 36;
pub const COMMUNE_META_SIZE: usize = 16;
pub const KEY_SEP: u8 = 0x1F;
/// pc_disp_off sentinel: this street has more than one effective house-postcode value (missing is
/// a value when mixed with known), so no house-accurate street-level value exists. In v7 this also
/// marks the conditional local-postcode dictionary + fifth varint grammar in SEC_HOUSE_BLOCKS.
/// Street-level results still emit EMPTY rather than a neighbour's postcode.
/// u32::MAX is never a real names offset (that would need a ~4 GB names blob).
pub const PC_DISP_AMBIGUOUS: u32 = u32::MAX;

pub fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

pub fn unzigzag(u: u64) -> i64 {
    ((u >> 1) as i64) ^ -((u & 1) as i64)
}

pub fn write_varint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            buf.push(b);
            break;
        }
        buf.push(b | 0x80);
    }
}

pub fn read_varint(buf: &[u8], pos: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    // A corrupt sheet can end mid-varint (continuation bit set at the section boundary):
    // stop instead of indexing past the slice and panicking the host process
    // (Python/DuckDB) — the earlier per-call guards checked
    // only the first byte. Valid files never run off the end, so their result is
    // unchanged; shift is capped against a runaway on garbage.
    while let Some(&b) = buf.get(*pos) {
        *pos += 1;
        v |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 || shift >= 63 {
            break;
        }
        shift += 7;
    }
    v
}

/// STRICT varint used by `Index::open`'s section-13 validator. Unlike the
/// lenient `read_varint` (which returns its accumulator on EOF/overflow so the host never panics),
/// this REJECTS a malformed encoding by returning `None`, so a corrupt sheet is refused at open:
///  - EOF mid-varint (continuation bit set at the buffer end);
///  - more than 10 bytes (a u64 needs at most ceil(64/7) = 10);
///  - OVERFLOW of the payload past bit 63. `checked_shl` only rejects a shift COUNT >= 64, NOT the
///    loss of high bits: `2u64.checked_shl(63) == Some(0)`, so `81 80 80 80 80 80 80 80 80 02`
///    silently decoded to 0. Bounding `payload <= u64::MAX >> shift` before the shift forces the
///    10th byte's payload (shift 63) to be only 0 or 1 and rejects everything wider.
pub fn strict_varint(buf: &[u8], p: &mut usize) -> Option<u64> {
    let mut out: u64 = 0;
    let mut shift = 0u32;
    for nbyte in 0..10 {
        let b = *buf.get(*p)?; // EOF mid-varint: the continuation promised more bytes
        *p += 1;
        let payload = u64::from(b & 0x7f);
        if shift >= 64 || payload > (u64::MAX >> shift) {
            return None; // high bits would be lost -> not a valid u64
        }
        out |= payload << shift; // 7 disjoint bits per group -> OR == checked add
        if b & 0x80 == 0 {
            return Some(out);
        }
        if nbyte == 9 {
            return None; // 10 bytes with a continuation bit: not a u64
        }
        shift += 7;
    }
    None
}

/// Header: MAGIC(4) + ver u8 + nsec u8, then nsec × (id u8, off u64, len u64).
pub fn header_size() -> usize {
    4 + 1 + 1 + N_SECTIONS * (1 + 8 + 8)
}

pub fn write_header(out: &mut Vec<u8>, sections: &[(u8, u64, u64)]) {
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(sections.len() as u8);
    for (id, off, len) in sections {
        out.push(*id);
        out.extend_from_slice(&off.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
    }
}

/// Returns (off, len) by section id; index 0 is unused.
///
/// Every section is checked to lie inside the file: a truncated download or a corrupt
/// file must fail here with a clear error, never slice out of bounds further down.
/// Every section that must have a TOC ENTRY (may be empty): all of 1-9, 12-14. Deleting a TOC
/// entry must fail the open, even for a legally-empty section like word_postings — a MISSING
/// entry silently disables its feature (fuzzy search), a DIFFERENT failure from a present-but-empty
/// one. Alias kept for the old public name.
pub const REQUIRED_PRESENT: [usize; 12] = [
    SEC_COMMUNES_FST,
    SEC_COMMUNES_META,
    SEC_COMMUNE_POSTINGS,
    SEC_STREETS_FST,
    SEC_STREETS_META,
    SEC_HOUSE_BLOCKS,
    SEC_NAMES,
    SEC_REPS,
    SEC_CELLS,
    SEC_WORDS,
    SEC_WORD_POSTINGS,
    SEC_COMMUNE_COORDS,
];

/// Sections that must be PRESENT AND NON-EMPTY: REQUIRED_PRESENT minus SEC_WORD_POSTINGS, which is
/// legally EMPTY when no street word is indexable (every word < 3 chars, e.g. "yu li"). The
/// words-FST↔postings consistency (empty postings ⟺ empty words FST) is enforced at open in
/// query.rs, where the FST is parsed. Alias `REQUIRED_SECTIONS` kept for external callers/tests.
pub const REQUIRED_SECTIONS: [usize; 11] = [
    SEC_COMMUNES_FST,
    SEC_COMMUNES_META,
    SEC_COMMUNE_POSTINGS,
    SEC_STREETS_FST,
    SEC_STREETS_META,
    SEC_HOUSE_BLOCKS,
    SEC_NAMES,
    SEC_REPS,
    SEC_CELLS,
    SEC_WORDS,
    SEC_COMMUNE_COORDS,
];

/// Strict TOC schema check beyond bounds: known ids only, no duplicate ids, no
/// overlapping non-empty byte ranges, and (when `require_sections`) every REQUIRED_PRESENT section
/// has a TOC entry AND every REQUIRED_SECTIONS section is non-empty — `len = 0` means "absent" per
/// the FORMAT. Bounds (off+len ≤ total) are already validated by the callers.
fn validate_toc_schema(entries: &[(u8, u64, u64)], require_sections: bool) -> Result<()> {
    let mut seen = [false; N_SECTIONS + 1];
    let mut nonempty = [false; N_SECTIONS + 1];
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    for &(id, off, len) in entries {
        let id = id as usize;
        if id == 0 || id > N_SECTIONS {
            bail!("index TOC has unknown section id {id} (1..={N_SECTIONS}) — file is corrupt");
        }
        if seen[id] {
            bail!("index TOC declares section {id} twice — file is corrupt");
        }
        seen[id] = true;
        if len > 0 {
            nonempty[id] = true;
            ranges.push((off, off + len)); // off+len already validated in-bounds by the caller
        }
    }
    ranges.sort_unstable();
    for w in ranges.windows(2) {
        if w[0].1 > w[1].0 {
            bail!("index sections overlap — file is corrupt");
        }
    }
    if require_sections {
        for &r in &REQUIRED_PRESENT {
            if !seen[r] {
                bail!("index required section {r} has no TOC entry — file is corrupt or truncated");
            }
        }
        for &r in &REQUIRED_SECTIONS {
            if !nonempty[r] {
                bail!(
                    "index required section {r} is missing or empty — file is corrupt or truncated"
                );
            }
        }
    }
    Ok(())
}

pub fn parse_sections(data: &[u8]) -> Result<[(u64, u64); N_SECTIONS + 1]> {
    if data.len() < header_size() || &data[0..4] != MAGIC {
        bail!("not a GPC0 index file");
    }
    if !matches!(data[4], MIN_READ_VERSION | VERSION) {
        bail!(
            "index format version {} not supported (reader accepts v{MIN_READ_VERSION}..=v{VERSION}) — \
             rebuild the sheet with this engine",
            data[4]
        );
    }
    let nsec = data[5] as usize;
    if nsec > N_SECTIONS {
        bail!("index header declares {nsec} sections (max {N_SECTIONS}) — file is corrupt");
    }
    let total = data.len() as u64;
    let mut secs = [(0u64, 0u64); N_SECTIONS + 1];
    let mut entries: Vec<(u8, u64, u64)> = Vec::with_capacity(nsec);
    let mut p = 6;
    for _ in 0..nsec {
        let id = data[p] as usize;
        let off = u64::from_le_bytes(data[p + 1..p + 9].try_into().unwrap());
        let len = u64::from_le_bytes(data[p + 9..p + 17].try_into().unwrap());
        if off.checked_add(len).is_none_or(|end| end > total) {
            bail!(
                "index section {id} spans bytes {off}..{} but the file is {total} bytes — \
                 the download is truncated or the file is corrupt",
                off.saturating_add(len)
            );
        }
        entries.push((data[p], off, len));
        if (1..=N_SECTIONS).contains(&id) {
            secs[id] = (off, len);
        }
        p += 17;
    }
    validate_toc_schema(&entries, true)?; // strict v7 schema: known/unique/non-overlapping/required
    Ok(secs)
}

/// Like parse_sections, but accepts the given versions and returns the raw section list. ONLY for
/// `gridpin repack` of a same-format v7 sheet (for example, to replace SEC_META): normal readers
/// must go through strict parse_sections. v5/v6 must be rebuilt because v7 changes house_blocks.
pub fn parse_sections_for_repack(data: &[u8]) -> Result<Vec<(u8, u64, u64)>> {
    if data.len() < 6 || &data[0..4] != MAGIC {
        bail!("not a GPC0 index file");
    }
    if data[4] != VERSION {
        if matches!(data[4], 5 | 6) {
            bail!(
                "cannot repack format version {} to v{VERSION}: v7 changes the house_blocks \
                 grammar, so this sheet must be rebuilt from source",
                data[4]
            );
        }
        bail!("cannot repack format version {}", data[4]);
    }
    let nsec = data[5] as usize;
    if nsec > N_SECTIONS || data.len() < 6 + nsec * 17 {
        bail!("index header declares {nsec} sections — file is corrupt");
    }
    let total = data.len() as u64;
    let mut out = Vec::with_capacity(nsec);
    let mut p = 6;
    for _ in 0..nsec {
        let id = data[p];
        let off = u64::from_le_bytes(data[p + 1..p + 9].try_into().unwrap());
        let len = u64::from_le_bytes(data[p + 9..p + 17].try_into().unwrap());
        if off.checked_add(len).is_none_or(|end| end > total) {
            bail!("section {id} out of bounds — file is corrupt");
        }
        out.push((id, off, len));
        p += 17;
    }
    // structural schema only (known-id/unique/non-overlapping); NOT required-sections, since an
    // older version being repacked may predate a section
    validate_toc_schema(&out, false)?;
    Ok(out)
}

/// SEC_META encoding: u32 n, then n × (klen u16, key UTF-8, vlen u32, value UTF-8).
/// Pairs are sorted by key by the writer, so identical inputs give identical bytes.
pub fn encode_meta(pairs: &[(String, String)]) -> Vec<u8> {
    let mut sorted: Vec<&(String, String)> = pairs.iter().collect();
    sorted.sort();
    let mut out = Vec::new();
    out.extend_from_slice(&(sorted.len() as u32).to_le_bytes());
    for (k, v) in sorted {
        out.extend_from_slice(&(k.len() as u16).to_le_bytes());
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(&(v.len() as u32).to_le_bytes());
        out.extend_from_slice(v.as_bytes());
    }
    out
}

/// Bounds-safe SEC_META decoder: None on any malformed input (a corrupt section
/// must degrade to "no provenance", never panic the host process).
pub fn decode_meta(buf: &[u8]) -> Option<Vec<(String, String)>> {
    let n = u32::from_le_bytes(buf.get(0..4)?.try_into().ok()?) as usize;
    if n > 1024 {
        return None;
    }
    let mut p = 4usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let klen = u16::from_le_bytes(buf.get(p..p + 2)?.try_into().ok()?) as usize;
        p += 2;
        let key = std::str::from_utf8(buf.get(p..p + klen)?).ok()?;
        p += klen;
        let vlen = u32::from_le_bytes(buf.get(p..p + 4)?.try_into().ok()?) as usize;
        p += 4;
        let val = std::str::from_utf8(buf.get(p..p + vlen)?).ok()?;
        p += vlen;
        out.push((key.to_string(), val.to_string()));
    }
    Some(out)
}

pub fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

pub fn read_i32(buf: &[u8], off: usize) -> i32 {
    i32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

pub fn read_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A header for `n` sections, each declared at (off, len).
    fn header(nsec: u8, entries: &[(u8, u64, u64)]) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(MAGIC);
        h.push(VERSION);
        h.push(nsec);
        for (id, off, len) in entries {
            h.push(*id);
            h.extend_from_slice(&off.to_le_bytes());
            h.extend_from_slice(&len.to_le_bytes());
        }
        h.resize(header_size(), 0);
        h
    }

    /// A truncated download must fail with an error, never slice out of bounds.
    #[test]
    fn truncated_file_is_rejected() {
        let mut data = header(1, &[(1, header_size() as u64, 4096)]);
        data.resize(header_size() + 10, 0); // section claims 4096 bytes, only 10 present
        let err = parse_sections(&data).unwrap_err().to_string();
        assert!(
            err.contains("truncated or the file is corrupt"),
            "got: {err}"
        );
    }

    /// A section offset+length that overflows u64 must not wrap around.
    #[test]
    fn overflowing_section_is_rejected() {
        let data = header(1, &[(1, u64::MAX - 1, 100)]);
        assert!(parse_sections(&data).is_err());
    }

    /// A header may not declare more sections than the format allows.
    #[test]
    fn absurd_section_count_is_rejected() {
        let mut data = header(0, &[]);
        data[5] = 255;
        let err = parse_sections(&data).unwrap_err().to_string();
        assert!(err.contains("corrupt"), "got: {err}");
    }

    #[test]
    fn meta_roundtrips_and_is_deterministic() {
        // SEC_META must survive encode->decode unchanged, and identical input must
        // give identical bytes regardless of insertion order (deterministic builds)
        let a = vec![
            ("country".to_string(), "fr".to_string()),
            ("layer".to_string(), "addresses".to_string()),
            ("license".to_string(), "Licence Ouverte 2.0".to_string()),
        ];
        let mut b = a.clone();
        b.reverse(); // different order in
        assert_eq!(
            encode_meta(&a),
            encode_meta(&b),
            "byte-identical regardless of order"
        );
        let mut got = decode_meta(&encode_meta(&a)).expect("decodes");
        got.sort();
        let mut want = a.clone();
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn decode_meta_rejects_garbage_and_bombs() {
        assert!(decode_meta(&[]).is_none()); // too short for the count
                                             // claims 5 pairs but the section ends immediately -> None, never a panic/overread
        assert!(decode_meta(&5u32.to_le_bytes()).is_none());
        // an absurd pair count is refused
        let mut bomb = (100_000u32).to_le_bytes().to_vec();
        bomb.extend_from_slice(&[0u8; 8]);
        assert!(decode_meta(&bomb).is_none());
    }

    #[test]
    fn read_varint_does_not_panic_past_the_slice() {
        // a section that ends mid-varint (continuation bit set at the boundary) must
        // stop, not index past the slice and panic the host
        let mut p = 0usize;
        assert_eq!(read_varint(&[0x80], &mut p), 0); // lone continuation byte -> 0, no panic
        let mut p = 0usize;
        let _ = read_varint(&[], &mut p); // empty slice -> no panic
        let mut p = 0usize;
        assert_eq!(read_varint(&[0x2a], &mut p), 42); // valid single-byte still works
    }

    #[test]
    fn strict_varint_rejects_eof_overlong_and_10th_byte_overflow() {
        // valid encodings round-trip
        let mut p = 0;
        assert_eq!(strict_varint(&[0x2a], &mut p), Some(42));
        let mut p = 0;
        assert_eq!(strict_varint(&[0x80, 0x01], &mut p), Some(128));
        // u64::MAX is a legal 10-byte varint: nine 0xff then 0x01 (10th payload = 1)
        let mut max = Vec::new();
        write_varint(&mut max, u64::MAX);
        assert_eq!(max.len(), 10);
        let mut p = 0;
        assert_eq!(strict_varint(&max, &mut p), Some(u64::MAX));
        // EOF mid-varint: a lone continuation byte is REJECTED (lenient read_varint returned 0)
        let mut p = 0;
        assert_eq!(strict_varint(&[0x80], &mut p), None);
        // empty
        let mut p = 0;
        assert_eq!(strict_varint(&[], &mut p), None);
        // 11 bytes (continuation through the 10th) -> not a u64
        let mut p = 0;
        assert_eq!(strict_varint(&[0x80; 11], &mut p), None);
        // The adversarial mutant: nine 0x80 (payload 0) then 0x02 at shift 63. `checked_shl` accepted
        // it as Some(0); the payload bound rejects it because 2 > (u64::MAX >> 63 == 1).
        let mutant = [0x81, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02];
        let mut p = 0;
        assert_eq!(
            strict_varint(&mutant, &mut p),
            None,
            "10th-byte overflow must be rejected"
        );
        // the ONLY valid 10th-byte payloads are 0 and 1
        let ok_one = [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01];
        let mut p = 0;
        assert_eq!(strict_varint(&ok_one, &mut p), Some(1u64 << 63));
    }

    #[test]
    fn a_well_formed_header_parses() {
        let hs = header_size() as u64;
        // a well-formed v7 header carries ALL required-present sections (12), each with a TOC entry;
        // word_postings (13) may be EMPTY, the rest non-empty. 1 byte each except
        // section 13, which is len 0.
        let entries: Vec<(u8, u64, u64)> = REQUIRED_PRESENT
            .iter()
            .enumerate()
            .map(|(i, &id)| {
                let len = if id == SEC_WORD_POSTINGS { 0 } else { 1 };
                (id as u8, hs + i as u64, len)
            })
            .collect();
        let nbytes = entries
            .iter()
            .filter(|(id, _, _)| *id as usize != SEC_WORD_POSTINGS)
            .count();
        let mut data = header(entries.len() as u8, &entries);
        data.resize(header_size() + nbytes + 1, 0); // section bytes (+1 slack for the empty postings offset)
        let secs = parse_sections(&data).expect("valid header");
        assert_eq!(secs[SEC_COMMUNES_FST], (hs, 1));
        assert_eq!(secs[SEC_NAMES].1, 1);
        assert_eq!(secs[SEC_WORD_POSTINGS].1, 0, "word_postings may be empty");
    }

    #[test]
    fn toc_schema_rejects_unknown_dup_overlap_and_missing_required() {
        let hs = header_size() as u64;
        // unknown section id
        assert!(parse_sections(&header(1, &[(200, hs, 0)])).is_err());
        // duplicate id
        assert!(parse_sections(&header(2, &[(1, hs, 0), (1, hs, 0)])).is_err());
        // missing a required section (only 1 and 4 present, no 12)
        assert!(parse_sections(&header(2, &[(1, hs, 0), (4, hs, 0)])).is_err());
        // overlapping non-empty ranges: two sections both claim [hs, hs+8)
        let with_bytes = {
            let mut d = header(3, &[(1, hs, 8), (4, hs, 8), (12, hs, 0)]);
            d.extend_from_slice(&[0u8; 8]);
            d
        };
        assert!(
            parse_sections(&with_bytes).is_err(),
            "overlapping sections must be rejected"
        );
    }

    #[test]
    fn v5_and_v6_repack_are_rejected_because_house_grammar_changed() {
        for old_version in [5u8, 6u8] {
            let data = [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], old_version, 0];
            let error = parse_sections_for_repack(&data)
                .expect_err("old house grammar must never be relabeled as v7")
                .to_string();
            assert!(error.contains("must be rebuilt from source"), "{error}");
        }
    }

    #[test]
    fn v6_remains_readable_but_cannot_be_repacked_as_v7() {
        let hs = header_size() as u64;
        let entries: Vec<(u8, u64, u64)> = REQUIRED_PRESENT
            .iter()
            .enumerate()
            .map(|(i, &id)| {
                let len = if id == SEC_WORD_POSTINGS { 0 } else { 1 };
                (id as u8, hs + i as u64, len)
            })
            .collect();
        let nbytes = entries.iter().filter(|(_, _, len)| *len > 0).count();
        let mut data = header(entries.len() as u8, &entries);
        data[4] = 6;
        data.resize(header_size() + nbytes + 1, 0);
        assert!(
            parse_sections(&data).is_ok(),
            "v6 query compatibility is deliberate"
        );
        assert!(parse_sections_for_repack(&data).is_err());
    }
}
