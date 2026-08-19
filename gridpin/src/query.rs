//! Index reading (mmap) and the query path: hypothesis-based parsing of the input
//! (digit groups as house-number candidates; compound suffixes via a dictionary),
//! exact lookup + prefix lookup + typo tolerance (Levenshtein automaton).

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use fst::automaton::Levenshtein;
use fst::{Automaton, IntoStreamer, Map, Streamer};
use memmap2::Mmap;
use serde::Serialize;

use crate::index::*;
use crate::norm::normalize;

/// Owns a file mapping so it is freed on Drop: opening a sheet no longer permanently
/// leaks the mmap, so repeated open/close (Python/DuckDB churn) reclaims memory. The Index holds it
/// in an `Arc`, so a test can assert the Index dropped its reference deterministically.
struct Mapping(Mmap);
impl std::ops::Deref for Mapping {
    type Target = Mmap;
    fn deref(&self) -> &Mmap {
        &self.0
    }
}
impl Mapping {
    fn new(m: Mmap) -> Self {
        Mapping(m)
    }
}

/// Colloquial commune names -> official NORMALIZED forms (as stored in the index after
/// normalization: apostrophe/hyphen -> space). Users write "Den Haag" while the index only
/// stores "s gravenhage"; without the alias such queries go empty or fuzzy-match a house
/// tens of km away. Curated list of major cities; keys and values are already normalized.
fn commune_alias(name: &str) -> Option<&'static str> {
    // the table ships with the data (SEC_RULES, see rules.rs); defaults in rules::defaults
    crate::rules::rules().commune_alias(name)
}

// POI-layer cascade (opt-in secondary places index).
// The address index is ALWAYS queried first; the POI layer is consulted ONLY when the
// address top-1 is weak, and its answer is taken ONLY if its own top-1 is more
// confident — an exact house match is never overridden.

/// Is the address top-1 weak (i.e. may the POI layer be consulted)?
///
/// An exact house match is never weak, whatever its calibrated confidence: a distant
/// homonym elsewhere in the country lowers confidence without making the match itself
/// any less exact, and the POI layer must never override an exact address.
pub fn hit_is_weak(h: &Hit) -> bool {
    if h.precision == "house" && h.flags.contains(&"street_exact") {
        return false;
    }
    h.precision == "city"
        || h.confidence < 0.30
        || (h.flags.contains(&"street_fuzzy") && h.confidence < 0.60)
}

/// Query with an optional POI layer. Without `poi` the behavior is identical to
/// `addr.query()` (the cascade is strictly opt-in). POI answers carry a "poi_layer" flag.
/// Refuse mismatched address/POI pairs: a French POI layer loaded
/// over the Italian sheet would silently answer Italian queries with French
/// cafes. Applied at LOAD time in every binding — never on the hot query path.
/// Sheets without identity (lab builds, no --meta) only produce a warning:
/// refusing them would break legitimate local workflows.
pub fn check_pair(addr: &Index, poi: &Index) -> std::result::Result<(), String> {
    match (addr.country(), poi.country()) {
        (Some(a), Some(p)) if a != p => {
            return Err(format!(
                "POI layer is for country {p:?} but the address sheet is for {a:?} — wrong file pair"
            ));
        }
        (None, _) | (_, None) => {
            eprintln!(
                "warning: sheet without country identity (pre-v6 or no --meta) — pair not verified"
            );
        }
        _ => {}
    }
    if let Some(l) = addr.layer() {
        if l != "addresses" {
            return Err(format!("expected an address sheet, got layer {l:?}"));
        }
    }
    if let Some(l) = poi.layer() {
        if l != "poi" {
            return Err(format!("expected a POI layer as --poi, got layer {l:?}"));
        }
    }
    Ok(())
}

/// An address query is short; bound the raw input BEFORE any normalization/transliteration so a
/// multi-megabyte string can't drive CPU/RAM amplification. A real query is far
/// under this; anything longer is truncated at a char boundary, then the 32-token cap applies.
pub const MAX_QUERY_BYTES: usize = 1024;

pub fn bound_query(raw: &str) -> &str {
    if raw.len() <= MAX_QUERY_BYTES {
        return raw;
    }
    let mut end = MAX_QUERY_BYTES;
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }
    &raw[..end]
}

/// Result-count ceiling. A caller (CLI/Python/DuckDB) that asks for k = usize::MAX must not be able
/// to drive Vec::with_capacity / sort cost without bound. No real query wants > 100.
pub const MAX_K: usize = 100;

/// Clamp the requested result count. k = 0 keeps its "zero results" contract; anything above the
/// ceiling is capped, so an interface can never turn a single call into an unbounded allocation.
pub fn bound_k(k: usize) -> usize {
    k.min(MAX_K)
}

/// A valid WGS84 point: finite and within [-90,90] × [-180,180]. The public reverse boundary must
/// REJECT bad input rather than silently probe a garbage grid cell and return an empty "success"
/// — the same predicate the builder uses to drop bad rows.
pub fn validate_lat_lon(lat: f64, lon: f64) -> std::result::Result<(), String> {
    if lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0 {
        Ok(())
    } else {
        Err(format!(
            "reverse: coordinates out of range or non-finite (lat={lat}, lon={lon}); \
             expected finite lat in [-90,90], lon in [-180,180]"
        ))
    }
}

/// A valid WGS84 focus for forward geocoding. Kept separate from the reverse error text so a
/// public `query_near` caller never receives a misleading `reverse:` diagnostic.
pub fn validate_query_near(lat: f64, lon: f64) -> std::result::Result<(), String> {
    if lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0 {
        Ok(())
    } else {
        Err(format!(
            "query --near: coordinates out of range or non-finite (lat={lat}, lon={lon}); \
             expected finite lat in [-90,90], lon in [-180,180]"
        ))
    }
}

fn plausible_house_postcode(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 8
        && value.as_bytes()[0].is_ascii_digit()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == ' ')
}

pub fn query_cascade(addr: &Index, poi: Option<&Index>, q: &str, k: usize) -> Vec<Hit> {
    let k = bound_k(k); // cap result count at the public boundary
    let q = bound_query(q); // and the input length, before either index normalizes it
    let hits = addr.query(q, k);
    escalate_to_poi(poi, hits, q, k)
}

/// Forward cascade with an explicit location hint. The address candidate set is widened with
/// streets from the existing spatial grid, while POI escalation keeps the same precedence as the
/// ordinary cascade. Coordinates are a strict public boundary, like reverse geocoding.
pub fn query_cascade_near(
    addr: &Index,
    poi: Option<&Index>,
    q: &str,
    k: usize,
    lat: f64,
    lon: f64,
) -> std::result::Result<Vec<Hit>, String> {
    let k = bound_k(k);
    let q = bound_query(q);
    let hits = addr.query_near(q, k, lat, lon)?;
    Ok(escalate_to_poi(poi, hits, q, k))
}

/// Structured-input variant of the cascade: the same POI
/// precedence as free-form, so a structured query for a POI (a named place, not a street) is not
/// silently unanswerable just because the caller pre-split the fields. The address index is tried
/// first; if its answer is weak (or empty) the POI layer is consulted with EVERY provided token
/// joined in a CANONICAL order (street number postcode city).
///
/// PARITY SCOPE (honest boundary): parity with free-form holds for ADDRESS resolution and for any
/// POI whose tokens appear in that canonical order. It does NOT hold when the caller's field
/// assignment REORDERS the tokens of an order-sensitive POI NAME: free-form "54 studio ville" ranks
/// "54 Studio", but structured {street:"studio", number:"54"} joins to "studio 54 ville" and can
/// rank "Studio 54" — same confidence, different place. Making POI matching order-insensitive (or
/// scoring supported permutations with a deterministic merge) is the fuller fix; until then the
/// public promise is deliberately narrowed to the canonical-order case, not "any input".
pub fn query_structured_cascade(
    addr: &Index,
    poi: Option<&Index>,
    street: &str,
    number: Option<&str>,
    city: &str,
    postcode: Option<&str>,
    k: usize,
) -> Vec<Hit> {
    // Bound EVERY structured field AND k at the public boundary: only street/city
    // were capped downstream, so a multi-MB `number`/`postcode`, or the free-form `poi_q` join of
    // the RAW fields, drove normalization/allocation before any cap. Bounding each field here caps
    // the join too (≤ 4 × MAX_QUERY_BYTES), so the structured path amplifies no more than free-form.
    let k = bound_k(k);
    let street = bound_query(street);
    let number = number.map(bound_query);
    let city = bound_query(city);
    let postcode = postcode.map(bound_query);
    let hits: Vec<Hit> = addr
        .query_structured(street, number, city, postcode, k)
        .into_iter()
        .map(|(h, _feats)| h)
        .collect();
    // CANONICAL-ORDER join: the POI escalation sees EVERY token the caller gave, in
    // the canonical street-number-postcode-city order ("Damstraat 1 1012JS Amsterdam"). This gives
    // parity for address resolution and canonical-order POIs; an order-sensitive POI name the caller
    // split across fields can still diverge (see the parity-scope note above). An earlier fix joined city
    // before postcode, which broke
    // parity for order-sensitive postcode forms (NL): the two paths ranked different winners.
    // With the identical token string, parity holds BY CONSTRUCTION for any input.
    let poi_q = [street, number.unwrap_or(""), postcode.unwrap_or(""), city]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    escalate_to_poi(poi, hits, &poi_q, k)
}

/// Shared cascade tail: if `hits` from the address index is weak/empty and a POI layer exists,
/// query it with `poi_q` and prefer the POI answer when it is more confident.
fn escalate_to_poi(poi: Option<&Index>, hits: Vec<Hit>, poi_q: &str, k: usize) -> Vec<Hit> {
    let Some(p) = poi else { return hits };
    if !hits.first().is_none_or(hit_is_weak) {
        return hits; // address answer is confident — skip the POI layer entirely
    }
    if poi_q.is_empty() {
        return hits;
    }
    let mut ph = p.query(poi_q, k);
    let better = match (ph.first(), hits.first()) {
        (Some(pt), Some(at)) => pt.confidence > at.confidence,
        (Some(_), None) => true,
        _ => false,
    };
    if better {
        for h in &mut ph {
            h.flags.push("poi_layer");
        }
        return ph;
    }
    hits
}

pub struct StreetMeta {
    pub lat_c: i32,
    pub lon_c: i32,
    pub commune_id: u32,
    pub postcode: u32,
    pub name_off: u32,
    pub house_off: u64,
    pub house_count: u32,
    /// offset of the full postcode string in names (NL "1012XJ"); 0 = absent -> print numeric postcode
    pub postcode_disp_off: u32,
}

#[derive(Serialize)]
pub struct Hit {
    pub lat: f64,
    pub lon: f64,
    pub precision: &'static str,
    pub score: f32,
    /// calibrated confidence 0..1 (unlike the raw score it is comparable across queries,
    /// suitable for threshold-based garbage cutoff).
    pub confidence: f32,
    pub street: String,
    /// matched house number with its suffix ("27", "12bis"). For forward answers this
    /// is the address actually used for the returned point: a near snap therefore reports
    /// its stored neighbour rather than echoing the requested number. Street-only answers
    /// leave it absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub housenumber: Option<String>,
    pub commune: String,
    pub postcode: String,
    /// match flags for output (explainability): street_exact/street_fuzzy,
    /// commune_exact/commune_prefix, house_rep, pc_exact/pc_dept, ml.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<&'static str>,
    /// administrative region (Who's on First, reverse point-in-polygon).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
}

/// WOF administrative region: name + bbox + rings (lat,lon x1e7) for ray-casting PIP.
struct AdminRegion {
    name: String,
    min_lat: i32,
    min_lon: i32,
    max_lat: i32,
    max_lon: i32,
    rings: Vec<Vec<(i32, i32)>>,
}

/// Load the sibling "<stem>_admin.bin" (WOF region polygons) if present. Format: "WOFA" +
/// u32 n; per region: u8 len + name, bbox i32 x4, u16 n_rings; per ring: u16 n_pts + (i32,i32) x n.
/// The admin sidecar is loaded by DERIVING its name from the sheet (`<stem>_admin.bin`). A release
/// rename that moves the sheet but not the sidecar would silently drop regions, so
/// distinguish the cases and WARN when a mismatched sibling exists.
enum AdminSidecar {
    Loaded(Vec<AdminRegion>),
    MissingWithSibling { expected: String, found: String },
    MissingClean,
    NotWofa(String),
}

fn admin_sidecar(index_path: &Path) -> AdminSidecar {
    let stem = index_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let expected = format!("{stem}_admin.bin");
    // country prefix (leading ISO-2) so an UNRELATED sidecar (e.g. uz_admin.bin next to rs.bin)
    // never triggers a false rename warning — only a same-country orphan does.
    let country = |n: &str| -> String { n.chars().take(2).collect::<String>().to_lowercase() };
    let sheet_cc = country(stem);
    let p = index_path.with_file_name(&expected);
    match std::fs::read(&p) {
        Ok(d) if d.len() > 8 && &d[..4] == b"WOFA" => AdminSidecar::Loaded(parse_admin_body(&d)),
        Ok(_) => AdminSidecar::NotWofa(expected),
        Err(_) => {
            // scan for a mismatched sibling *_admin.bin OF THE SAME COUNTRY (a rename that forgot it)
            if let Some(dir) = index_path.parent() {
                if let Ok(rd) = std::fs::read_dir(dir) {
                    for e in rd.flatten() {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if name != expected
                            && name.ends_with("_admin.bin")
                            && country(&name) == sheet_cc
                        {
                            let mut buf = [0u8; 4];
                            let is_wofa = std::fs::File::open(e.path())
                                .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
                                .is_ok()
                                && &buf == b"WOFA";
                            if is_wofa {
                                return AdminSidecar::MissingWithSibling {
                                    expected,
                                    found: name,
                                };
                            }
                        }
                    }
                }
            }
            AdminSidecar::MissingClean
        }
    }
}

fn load_admin(index_path: &Path) -> Vec<AdminRegion> {
    match admin_sidecar(index_path) {
        AdminSidecar::Loaded(v) => v,
        AdminSidecar::MissingWithSibling { expected, found } => {
            eprintln!(
                "warning: {index_path:?} expected admin sidecar {expected} but it is missing; found \
                 {found} next to it — regions disabled. Rename the sidecar to match the sheet (release rename?)."
            );
            Vec::new()
        }
        AdminSidecar::NotWofa(name) => {
            eprintln!("warning: {name} next to {index_path:?} is not a WOFA admin sidecar — regions disabled");
            Vec::new()
        }
        AdminSidecar::MissingClean => Vec::new(),
    }
}

/// Parse a validated (magic-checked) WOFA sidecar body into regions. A truncated or corrupt one
/// degrades to what parsed cleanly, never a panic — every read is bounds-checked.
fn parse_admin_body(data: &[u8]) -> Vec<AdminRegion> {
    let mut o = 4usize;
    let rd_u32 = |d: &[u8], o: usize| u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
    let rd_i32 = |d: &[u8], o: usize| i32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
    let rd_u16 = |d: &[u8], o: usize| u16::from_le_bytes([d[o], d[o + 1]]) as usize;
    let n = rd_u32(data, o) as usize;
    o += 4;
    let mut out = Vec::new();
    for _ in 0..n {
        if o >= data.len() {
            break;
        }
        let nl = data[o] as usize;
        o += 1;
        if o + nl + 18 > data.len() {
            break; // name + bbox + ring count do not fit
        }
        let name = String::from_utf8_lossy(&data[o..o + nl]).into_owned();
        o += nl;
        let (min_lat, min_lon, max_lat, max_lon) = (
            rd_i32(data, o),
            rd_i32(data, o + 4),
            rd_i32(data, o + 8),
            rd_i32(data, o + 12),
        );
        o += 16;
        let nr = rd_u16(data, o);
        o += 2;
        let mut rings = Vec::new();
        let mut truncated = false;
        for _ in 0..nr {
            if o + 2 > data.len() {
                truncated = true;
                break;
            }
            let np = rd_u16(data, o);
            o += 2;
            if o + np * 8 > data.len() {
                truncated = true;
                break;
            }
            let mut ring = Vec::with_capacity(np);
            for _ in 0..np {
                ring.push((rd_i32(data, o), rd_i32(data, o + 4)));
                o += 8;
            }
            rings.push(ring);
        }
        if truncated {
            break;
        }
        out.push(AdminRegion {
            name,
            min_lat,
            min_lon,
            max_lat,
            max_lon,
            rings,
        });
    }
    out
}

/// Is the point (lat,lon x1e7) inside the region? Even-odd ray casting over all rings (holes/multipolygons).
fn point_in_rings(la: i32, lo: i32, rings: &[Vec<(i32, i32)>]) -> bool {
    let mut inside = false;
    for ring in rings {
        let n = ring.len();
        if n < 3 {
            continue;
        }
        let mut j = n - 1;
        for i in 0..n {
            let (yi, xi) = ring[i];
            let (yj, xj) = ring[j];
            if (yi > la) != (yj > la) {
                let xint = xi as i64
                    + (la as i64 - yi as i64) * (xj as i64 - xi as i64) / (yj as i64 - yi as i64);
                if (lo as i64) < xint {
                    inside = !inside;
                }
            }
            j = i;
        }
    }
    inside
}

/// Calibrated confidence 0..1 = P(result within 150 m). Logistic model with empirically
/// fitted coefficients; predicted probabilities track observed hit rates.
fn confidence_score(precision: &str, f: &Feats, _name_sim: i32) -> f32 {
    let mut z = -2.748f32; // bias
    z += match precision {
        "house" => 0.965,
        "interp" => 0.158,
        "near" => -0.412,
        "street" => -0.222,
        _ => -0.506, // city
    };
    if f.street_exact {
        z += 0.899;
    }
    if f.street_fuzzy {
        z -= 0.410;
    }
    if f.commune_exact {
        z += 2.360;
    }
    if f.commune_prefix {
        z += 1.240;
    }
    if f.house_exact_rep {
        z += 1.280;
    }
    if f.pc_exact {
        z += 0.738;
    }
    if f.pc_dept {
        z -= 0.515;
    }
    if f.from_ml {
        z -= 0.270;
    }
    let c = 1.0 / (1.0 + (-z).exp());
    (c * 100.0).round() / 100.0
}

/// Match flags for output (explainability): why this answer, what to filter out.
fn match_flags(f: &Feats) -> Vec<&'static str> {
    let mut v = Vec::new();
    if f.street_exact {
        v.push("street_exact");
    }
    if f.street_fuzzy {
        v.push("street_fuzzy");
    }
    if f.commune_exact {
        v.push("commune_exact");
    } else if f.commune_prefix {
        v.push("commune_prefix");
    }
    if f.house_exact_rep {
        v.push("house_rep");
    }
    if f.pc_exact {
        v.push("pc_exact");
    } else if f.pc_dept {
        v.push("pc_dept");
    }
    if f.from_ml {
        v.push("ml");
    }
    v
}

/// Candidate features (order is fixed: SEC_RANK weights are indexed by position).
pub const N_FEATS: usize = 10;

#[derive(Clone, Copy, Default)]
pub struct Feats {
    street_exact: bool,
    street_fuzzy: bool,
    commune_exact: bool,
    commune_prefix: bool,
    pc_exact: bool,
    pc_dept: bool,
    from_ml: bool,
    house_found: bool,
    house_exact_rep: bool,
    numero_present: bool,
}

impl Feats {
    fn merge(&mut self, o: Feats) {
        self.street_exact |= o.street_exact;
        self.street_fuzzy |= o.street_fuzzy;
        self.commune_exact |= o.commune_exact;
        self.commune_prefix |= o.commune_prefix;
        self.pc_exact |= o.pc_exact;
        self.pc_dept |= o.pc_dept;
        self.from_ml |= o.from_ml;
    }

    pub fn to_vec(&self) -> [f32; N_FEATS] {
        let b = |x: bool| if x { 1.0 } else { 0.0 };
        [
            b(self.street_exact),
            b(self.street_fuzzy),
            b(self.commune_exact),
            b(self.commune_prefix),
            b(self.pc_exact),
            b(self.pc_dept),
            b(self.from_ml),
            b(self.house_found),
            b(self.house_exact_rep),
            b(self.numero_present),
        ]
    }

    fn from_vec(v: &[f32; N_FEATS]) -> Self {
        let b = |i: usize| v[i] > 0.5;
        Self {
            street_exact: b(0),
            street_fuzzy: b(1),
            commune_exact: b(2),
            commune_prefix: b(3),
            pc_exact: b(4),
            pc_dept: b(5),
            from_ml: b(6),
            house_found: b(7),
            house_exact_rep: b(8),
            numero_present: b(9),
        }
    }

    /// Hand-tuned baseline score (used for hypothesis selection and when no trained weights are present).
    fn legacy(&self) -> i32 {
        let mut s = 0;
        if self.street_exact {
            s += 3;
        }
        if self.street_fuzzy && !self.street_exact {
            s += 2;
        }
        if self.commune_exact {
            s += 3;
        }
        if self.commune_prefix && !self.commune_exact {
            s += 2;
        }
        if self.pc_exact {
            s += 2;
        }
        if self.house_exact_rep {
            s += 2;
        } else if self.house_found {
            s += 1;
        }
        s
    }
}

/// Trained ranking weights (SEC_RANK section: 'GPRK' + n u8 + bias f32 + w f32 x n).
struct Rank {
    bias: f32,
    w: Vec<f32>,
}

impl Rank {
    fn from_section(data: &[u8]) -> Option<Rank> {
        if data.len() < 9 || &data[0..4] != b"GPRK" {
            return None;
        }
        let n = data[4] as usize;
        // n MUST equal the fixed feature count AND the section length must be EXACT: the old
        // `< 9 + n*4` accepted a mutant that shrank n (10 -> 1) while keeping
        // all weight bytes — score() then zips only the first n weights against the N_FEATS feature
        // vector, silently changing every score/confidence with no error. Reject a wrong n or any
        // short/over-long weight table.
        if n != N_FEATS || data.len() != 9 + n * 4 {
            return None;
        }
        let bias = f32::from_le_bytes(data[5..9].try_into().ok()?);
        let w: Vec<f32> = (0..n)
            .map(|i| f32::from_le_bytes(data[9 + i * 4..13 + i * 4].try_into().unwrap()))
            .collect();
        // reject a non-finite bias/weight: a NaN here propagates to every score and surfaces as
        // `score:null` (CLI/DuckDB) / `nan` (Python) — the model must be finite.
        if !bias.is_finite() || w.iter().any(|x| !x.is_finite()) {
            return None;
        }
        Some(Rank { bias, w })
    }

    fn score(&self, f: &Feats) -> f32 {
        let v = f.to_vec();
        let mut s = self.bias;
        for (wi, vi) in self.w.iter().zip(&v) {
            s += wi * vi;
        }
        s
    }
}

/// Build-time validation hook: does this byte slice parse as a SEC_RANK section? The
/// builder calls it so a corrupt `--rank` file fails the BUILD, instead of being embedded and then
/// silently dropped to `None` at open time (a sheet that quietly lost its trained ranking).
pub(crate) fn rank_section_is_valid(bytes: &[u8]) -> bool {
    Rank::from_section(bytes).is_some()
}

/// Parse hypothesis: house number, suffix, remainder (street+commune) as token indices.
struct Hyp {
    numero: Option<u32>,
    rep: u32,
    rest_idx: Vec<usize>,
    from_ml: bool,
}

/// Per-query focus context. The nearby street ids are collected once and reused across parser
/// retries; this keeps every retry on the same deterministic candidate union.
struct QueryFocus {
    lat: f64,
    lon: f64,
    streets: Vec<u32>,
}

/// Expansion of first-word street abbreviations (only adds a variant).
const ABBREV: &[(&str, &str)] = &[
    ("r", "rue"),
    ("av", "avenue"),
    ("avn", "avenue"),
    ("bd", "boulevard"),
    ("bld", "boulevard"),
    ("blvd", "boulevard"),
    ("pl", "place"),
    ("imp", "impasse"),
    ("chem", "chemin"),
    ("all", "allee"),
    ("sq", "square"),
    ("rte", "route"),
    ("crs", "cours"),
    ("fbg", "faubourg"),
    ("st", "saint"),
    ("ste", "sainte"),
    ("ln", "laan"),
    ("str", "straat"),
    ("v", "via"),
    ("vle", "viale"),
    ("pza", "piazza"),
    ("cso", "corso"),
    ("vic", "vicolo"),
    ("ул", "улица"),
    ("пр", "проспект"),
    ("просп", "проспект"),
    ("пер", "переулок"),
    ("наб", "набережная"),
    ("ш", "шоссе"),
    ("бул", "бульвар"),
    ("пл", "площадь"),
    ("кв", "квартал"),
    ("мкр", "микрорайон"),
    ("мкрн", "микрорайон"),
];

/// Is the word a street TYPE (not a distinguishing name)? Covers all scripts.
fn is_street_type_word(w: &str) -> bool {
    crate::rules::rules().is_street_type(w)
}

/// A LEADING designator that can be stripped from the string start as a place type/prefix:
/// street types in any script plus housing-estate/microdistrict prefixes (and their
/// transliterations). The bare name then resolves on its own, whereas a leading type shifts
/// the name away from the string start so prefix Levenshtein / inverted-index lookups fail
/// to match. Distinguishing names are never stripped.
fn is_affix_word(w: &str) -> bool {
    crate::rules::rules().is_affix(w)
}

/// Normalized, order-independent street-name key so the same name in different word
/// orders ("X street" vs "street X") counts as one name.
fn street_key(s: &str) -> String {
    let mut t: Vec<String> = normalize(s)
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect();
    t.sort();
    t.join(" ")
}

fn expand_first(phrase: &str) -> Option<String> {
    let (first, tail) = phrase.split_once(' ')?;
    let full = ABBREV.iter().find(|(a, _)| *a == first)?.1;
    Some(format!("{full} {tail}"))
}

/// Expansion of a LAST-word abbreviation: in Russian-style addresses the street type
/// trails the name (abbreviated "prospekt"/"ulitsa" after the street name).
fn expand_last(phrase: &str) -> Option<String> {
    let (head, last) = phrase.rsplit_once(' ')?;
    let full = ABBREV.iter().find(|(a, _)| *a == last)?.1;
    Some(format!("{head} {full}"))
}

/// Rotate the type word of a Cyrillic street name — BOTH directions, because source data
/// is inconsistent: the type may trail the name or lead it (genitive names), and queries
/// use either order. Produces the variant with the type word moved to the other end.
fn rotate_type_first(phrase: &str) -> Option<String> {
    if let Some((first, tail)) = phrase.split_once(' ') {
        if crate::rules::rules().types_cyr.iter().any(|t| t == first) {
            return Some(format!("{tail} {first}"));
        }
    }
    if let Some((head, last)) = phrase.rsplit_once(' ') {
        if crate::rules::rules().types_cyr.iter().any(|t| t == last) {
            return Some(format!("{last} {head}"));
        }
    }
    None
}

/// Queries often omit the street type ("13 de la Paix") while the registry has it
/// ("rue de la Paix"). For a BARE name (no type word) generate typed variants for exact
/// key lookup. Cyrillic: type position is inconsistent, so pad both front and back;
/// Latin scripts: the type always leads, so pad the front only.
fn type_padded_variants(phrase: &str) -> Vec<String> {
    let cyr = crate::norm::has_cyrillic(phrase);
    let r = crate::rules::rules();
    let types: &[String] = if cyr { &r.types_cyr } else { &r.types_latin };
    // already has a type word — leave as is (canonical queries stay fast)
    if phrase.split(' ').any(|w| types.iter().any(|t| t == w)) {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(types.len() * 2);
    for t in types {
        out.push(format!("{t} {phrase}")); // type in front (all languages)
        if cyr {
            out.push(format!("{phrase} {t}")); // trailing type — Cyrillic only
        }
    }
    out
}

/// Short form of a secondary house-number designator (Cyrillic korpus/stroenie/vladenie
/// markers), else None; the standalone "house" marker word is handled separately (dropped).
fn unit_designator(tok: &str) -> Option<&'static str> {
    match tok {
        "корпус" | "корп" | "копр" | "корпуса" | "кор" | "к" => Some("к"),
        // the data stores the "stroenie" suffix as a single letter ("6 s3"), not "str"
        "строение" | "стр" | "строения" | "с" => Some("с"),
        "владение" | "влад" | "вл" => Some("вл"),
        _ => None,
    }
}

/// Typed noise pair: the word is dropped ONLY with a matching argument —
/// "porte gauche" yes, "Rue Porte Pinte" no (blind stop words would kill real streets).
fn noise_pair_ok(word: &str, arg: &str, prev_digitish: bool) -> bool {
    let digit = !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit());
    let single = arg.chars().count() == 1 && arg.chars().all(|c| c.is_alphanumeric());
    let roman = matches!(arg, "i" | "ii" | "iii" | "iv" | "v" | "vi");
    match word {
        "кв" | "квартира" | "оф" | "офис" | "пом" | "помещение" | "подъезд" | "эт" | "этаж"
        | "комната" | "ком" | "int" | "interno" | "lokal" | "stan" | "sprat" | "xonadon"
        | "kvartira" | "piano" => digit || roman,
        "sc" | "scala" | "gebouw" => single,
        "porte" => matches!(arg, "gauche" | "droite") || digit,
        "appartement" | "appt" | "apt" | "app" | "bat" | "batiment" => digit || single,
        "etage" | "hoog" | "verdieping" => prev_digitish || digit,
        _ => false,
    }
}

/// Single noise word — administrative markers (NOT street words).
/// "sh"/"shahri" is an Uzbek city marker ("Toshkent sh."): the city name itself stays.
/// "gorod" is a mid-string city marker: the name after it stays as well.
fn is_noise_word(tok: &str) -> bool {
    crate::rules::rules().noise.contains(tok)
}

/// Region/district marker (e.g. "Chilonzor tumani"): drop the marker AND the name
/// before it (the commune appears later in the string).
fn is_region_marker(tok: &str) -> bool {
    crate::rules::rules().region_markers.contains(tok)
}
/// Noise word appearing after the house number.
fn is_noise_word_after(tok: &str) -> bool {
    crate::rules::rules().noise_after.contains(tok)
}

/// Trailing country names — stripped from the end of the string.
fn is_country_word(tok: &str) -> bool {
    crate::rules::rules().countries_tail.contains(tok)
}
/// Spelled-out numerals -> digits (French date streets: "Douze Mai" -> "12 Mai",
/// "Quatorze Juillet" -> "14 Juillet"). Compounds ("dix sept") must precede simple ones.
const NUM_WORDS: &[(&str, &str)] = &[
    ("dix sept", "17"),
    ("dix huit", "18"),
    ("dix neuf", "19"),
    ("vingt cinq", "25"),
    ("premier", "1er"),
    ("une", "1"),
    ("un", "1"),
    ("deux", "2"),
    ("trois", "3"),
    ("quatre", "4"),
    ("cinq", "5"),
    ("six", "6"),
    ("sept", "7"),
    ("huit", "8"),
    ("neuf", "9"),
    ("dix", "10"),
    ("onze", "11"),
    ("douze", "12"),
    ("treize", "13"),
    ("quatorze", "14"),
    ("quinze", "15"),
    ("seize", "16"),
    ("vingt", "20"),
    ("trente", "30"),
];

/// Replace spelled-out numerals with digits; None if nothing changed.
fn num_words_to_digits(phrase: &str) -> Option<String> {
    let mut s = format!(" {phrase} ");
    let mut changed = false;
    for (w, d) in NUM_WORDS {
        let pat = format!(" {w} ");
        if s.contains(&pat) {
            s = s.replace(&pat, &format!(" {d} "));
            changed = true;
        }
    }
    if changed {
        Some(s.trim().to_string())
    } else {
        None
    }
}

/// Serbian genitive street names: the registry stores "Kneza Mihaila" (genitive) while
/// people write the nominative "Knez Mihailova". Title -> +a (knez -> kneza), possessive
/// "-ova/-eva" -> "-a" (Mihailova -> Mihaila). Extra variant, Latin script only.
fn serbian_genitive_variant(phrase: &str) -> Option<String> {
    let words: Vec<&str> = phrase.split(' ').collect();
    if words.len() < 2 {
        return None;
    }
    let mut changed = false;
    let out: Vec<String> = words
        .iter()
        .map(|w| match *w {
            // Serbian titles in street names + Russian transliterated forms (a Russian
            // speaker writes the Serbian street in their own genitive: knjaza -> kneza,
            // korolja -> kralja)
            "knez" | "knjaz" | "knjaza" | "knjazja" => {
                changed = true;
                "kneza".into()
            }
            "kralj" | "korol" | "korolja" | "korolj" => {
                changed = true;
                "kralja".into()
            }
            "car" | "carja" => {
                changed = true;
                "cara".into()
            }
            "vojvoda" | "voevody" | "voevoda" => {
                changed = true;
                "vojvode".into()
            }
            s if (s.ends_with("ova") || s.ends_with("eva")) && s.chars().count() > 4 => {
                changed = true;
                format!("{}a", &s[..s.len() - 3])
            }
            s => s.to_string(),
        })
        .collect();
    if changed {
        Some(out.join(" "))
    } else {
        None
    }
}

/// Levenshtein distance (for the name-similarity ranking tie-breaker).
fn lev(a: &[char], b: &[char]) -> usize {
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

/// Folds spelled-out secondary designators into compact suffix tokens ("korpus 3" -> "k3",
/// "stroenie 1" -> "str1", the bare "house" word is dropped): Cyrillic addresses spell
/// these out while the data stores them as part of the suffix ("32 k3"). Merges the
/// designator and its number into one token BEFORE parsing.
fn fold_units(q: &str) -> String {
    let toks: Vec<&str> = q.split(' ').filter(|t| !t.is_empty()).collect();
    let n0 = toks.len();
    // position of the first digit group: floor/apartment noise lives AFTER the number,
    // while before it "porte"/"piano"/"gauche" are parts of real street names
    let first_digit = toks
        .iter()
        .position(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or(n0);
    let mut out: Vec<String> = Vec::with_capacity(n0);
    let mut i = 0;
    while i < n0 {
        let t = toks[i];
        if t == "дом"
            || t == "uy"
            || t == "д" && i + 1 < n0 && toks[i + 1].bytes().all(|b| b.is_ascii_digit())
        {
            i += 1; // bare house-marker word; the adjacent number is the house number
            continue;
        }
        // "korpus N" BEFORE the first digit group is just a number marker (block numbers
        // are stored in the index as the numero). AFTER the house number it is left alone:
        // "12 k 1" is the suffix (rep) form and takes its own path.
        if i < first_digit
            && matches!(t, "к" | "корп" | "корпус" | "korpus")
            && i + 1 < n0
            && toks[i + 1]
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_digit())
        {
            i += 1;
            continue;
        }
        if t == "тел" || t == "tel" || t == "phone" || t.starts_with('+') {
            break; // phone numbers always trail; cut to the end
        }
        // CEDEX tail (French corporate mail): in "Lyon Cedex 03" the "cedex" token and the
        // office number are not part of the address (BAN has no "cedex" entries). Drop both.
        if t == "cedex" {
            i += 1;
            if i < n0 && toks[i].len() <= 2 && toks[i].bytes().all(|b| b.is_ascii_digit()) {
                i += 1;
            }
            continue;
        }
        if t == "chez" && i == 0 {
            i += 1; // "Chez M. Durand, ..." — only at the START (Rue de Chez Guillot is a street!)
            continue;
        }
        if is_noise_word(t) {
            i += 1;
            continue;
        }
        // region/district marker (e.g. "Chilonzor tumani"): drop the marker AND the
        // region name before it — the commune appears later in the string
        if is_region_marker(t) {
            out.pop();
            i += 1;
            continue;
        }
        // country word MID-STRING ("torcy france 77200") is dropped, EXCEPT as part of a
        // street name ("Rue de France", "Via Italia" — country after a linker, plus the
        // explicit Italian `via/corso italia` forms). Keep the new exception deliberately narrow:
        // treating every country noun after every multilingual street type as a name changed
        // established FR/NL/RU parsing behaviour.
        if crate::rules::rules().countries_mid.contains(t)
            && !matches!(
                out.last().map(|s| s.as_str()),
                Some("de" | "du" | "des" | "di" | "della" | "del" | "da")
            )
            && !matches!(
                (out.last().map(String::as_str), t),
                (Some("via" | "corso"), "italia")
            )
        {
            i += 1;
            continue;
        }
        // Roman-numeral floor right after a number: "73 ii" (from "73/II") — drop it
        if i > first_digit
            && matches!(t, "i" | "ii" | "iii" | "iv" | "v" | "vi")
            && out
                .last()
                .is_some_and(|p| p.bytes().all(|b| b.is_ascii_digit()))
        {
            i += 1;
            continue;
        }
        if i > first_digit && is_noise_word_after(t) {
            i += 1;
            continue;
        }
        // typed pairs: apartment/floor markers with an argument ("int 5", "sc b", "porte gauche", "2eme etage")
        {
            let arg = if i + 1 < n0 { toks[i + 1] } else { "" };
            let prev_digitish = out
                .last()
                .is_some_and(|p| p.bytes().next().is_some_and(|b| b.is_ascii_digit()));
            if noise_pair_ok(t, arg, prev_digitish) {
                if matches!(t, "etage" | "hoog" | "verdieping")
                    && prev_digitish
                    && !arg.bytes().all(|b| b.is_ascii_digit())
                {
                    out.pop(); // "2eme etage" / "3e verdieping" — drop the ordinal too
                    i += 1;
                } else {
                    i += if !arg.is_empty() { 2 } else { 1 };
                }
                continue;
            }
        }
        if let Some(short) = unit_designator(t) {
            // fold the single-letter designator only when a number follows
            // (otherwise it may be an initial in a street name)
            if i + 1 < n0
                && toks[i + 1].bytes().all(|b| b.is_ascii_digit())
                && !toks[i + 1].is_empty()
            {
                out.push(format!("{short}{}", toks[i + 1]));
                i += 2;
                continue;
            }
        }
        // "litera X" -> the letter becomes the suffix
        if (t == "литера" || t == "лит") && i + 1 < n0 && toks[i + 1].chars().count() == 1
        {
            out.push(toks[i + 1].to_string());
            i += 2;
            continue;
        }
        out.push(t.to_string());
        i += 1;
    }
    // trailing countries ("..., France", "..., Italia")
    while out.len() > 2 && is_country_word(out.last().unwrap()) {
        out.pop();
    }
    out.join(" ")
}

/// Expand two-token abbreviations in the prepared string.
fn expand_two_token(q: &str) -> String {
    let mut s = q.to_string();
    for (ab, full) in &crate::rules::rules().abbrev2 {
        s = s.replace(&format!("{ab} "), &format!("{full} "));
    }
    s
}

/// French arrondissements: human spellings -> BAN canon, STRICTLY BY CONTEXT (hundreds of
/// streets have "Neme" in the name itself — "rue du 87eme" — so a global rewrite would corrupt them):
///  - "lyon 3eme" -> "lyon 3e" (otherwise the compound-token hypothesis reads "3eme" as
///    house number 3); also applies before "arrondissement";
///  - Roman numerals after a city or before "arrondissement": "paris xi"/"paris ive" -> "paris 11e"/"paris 4e";
///  - order "1er arrondissement paris" -> "paris 1er arrondissement" (exact BAN commune name);
///  - the same order with the formal preposition, "1er arrondissement de paris".
fn fr_arrondissement_rewrite(q: &str) -> Option<String> {
    if !(q.contains("paris") || q.contains("lyon") || q.contains("marseille")) {
        return None;
    }
    const ROMANS: [&str; 20] = [
        "i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x", "xi", "xii", "xiii", "xiv",
        "xv", "xvi", "xvii", "xviii", "xix", "xx",
    ];
    let is_city = |t: &str| crate::rules::rules().fr_ord_cities.contains(t);
    let mut toks: Vec<String> = q
        .split(' ')
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    let mut changed = false;
    for i in 0..toks.len() {
        let prev_city = i > 0 && is_city(&toks[i - 1]);
        let next_arr = toks
            .get(i + 1)
            .is_some_and(|t| t.starts_with("arrondissement"));
        if !prev_city && !next_arr {
            continue;
        }
        let t = toks[i].clone();
        // "3eme" -> "3e", and every first-ordinal spelling -> BAN's canonical "1er"
        // (accented forms are already normalized to "3eme").
        if let Some(num) = t
            .strip_suffix("eme")
            .or_else(|| t.strip_suffix("er"))
            .or_else(|| t.strip_suffix('e'))
        {
            if !num.is_empty() && num.len() <= 2 && num.bytes().all(|b| b.is_ascii_digit()) {
                let canonical = if num == "1" {
                    "1er".to_string()
                } else {
                    format!("{num}e")
                };
                if toks[i] != canonical {
                    toks[i] = canonical;
                    changed = true;
                }
                continue;
            }
        }
        // Roman numerals (with optional e/er/eme): "xi" -> "11e", "ive" -> "4e", "ier" -> "1er"
        let core = t
            .strip_suffix("eme")
            .or_else(|| t.strip_suffix("er"))
            .or_else(|| t.strip_suffix('e'))
            .unwrap_or(&t);
        if let Some(pos) = ROMANS.iter().position(|r| *r == core) {
            let n = pos + 1;
            toks[i] = if n == 1 {
                "1er".to_string()
            } else {
                format!("{n}e")
            };
            changed = true;
        }
    }
    let reorder_city_first =
        toks.len() >= 4 && is_city(&toks[0]) && fr_ordinal_number(&toks[1]).is_some();
    // "1er arrondissement paris" / "1er arrondissement de paris"
    // -> "paris 1er arrondissement".
    let mut i = 0;
    while i + 2 < toks.len() {
        let ord_core = toks[i].trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let city_match = if is_city(&toks[i + 2]) {
            Some((i + 2, false))
        } else if toks.get(i + 2).is_some_and(|t| t == "de")
            && toks.get(i + 3).is_some_and(|t| is_city(t))
        {
            Some((i + 3, true))
        } else {
            None
        };
        if toks[i + 1].starts_with("arrondissement")
            && !ord_core.is_empty()
            && ord_core.len() <= 2
            && ord_core.bytes().all(|b| b.is_ascii_digit())
            && toks[i].len() > ord_core.len()
        {
            if let Some((city_pos, had_de)) = city_match {
                let city = toks.remove(city_pos);
                if had_de {
                    toks.remove(i + 2);
                }
                toks.insert(i, city);
                changed = true;
                i += 3;
                continue;
            }
        }
        i += 1;
    }
    // City-first full address: "Paris 15e Rue du Hameau 37" (with an optional
    // "arrondissement" label) -> street/house first, canonical commune last. Area-only
    // "Paris 15e" is intentionally left alone for the city resolver.
    if reorder_city_first {
        let city = toks.remove(0);
        let ordinal = toks.remove(0);
        if toks
            .first()
            .is_some_and(|token| token.starts_with("arrondissement"))
        {
            toks.remove(0);
        }
        toks.push(city);
        toks.push(ordinal);
        toks.push("arrondissement".to_string());
        changed = true;
    }
    if changed {
        Some(toks.join(" "))
    } else {
        None
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FrPostcodeArea {
    Match(String),
    Conflict,
}

fn fr_ordinal_number(token: &str) -> Option<usize> {
    let digits = token
        .strip_suffix("eme")
        .or_else(|| token.strip_suffix("er"))
        .or_else(|| token.strip_suffix('e'))?;
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Recognize the postcode-only administrative-area forms that cannot go through the normal
/// address parser (it otherwise interprets the postcode as a house number). This is deliberately
/// restricted to the three French cities whose postal codes encode an arrondissement.
///
/// A contradictory city/ordinal is rejected instead of silently returning the wrong district.
/// Queries with street-like tokens are left to the normal parser.
fn fr_arrondissement_postcode_area(q: &str) -> Option<FrPostcodeArea> {
    let toks: Vec<&str> = q.split(' ').filter(|t| !t.is_empty()).collect();
    let postcodes: Vec<(usize, &str)> = toks
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            (t.len() == 5 && t.bytes().all(|b| b.is_ascii_digit())).then_some((i, *t))
        })
        .collect();
    if postcodes.len() != 1 {
        return None;
    }
    let (postcode_pos, postcode) = postcodes[0];
    let postcode_num: usize = postcode.parse().ok()?;
    let (city, ordinal) = match postcode_num {
        75_001..=75_020 => ("paris", postcode_num - 75_000),
        75_116 => ("paris", 16),
        69_001..=69_009 => ("lyon", postcode_num - 69_000),
        13_001..=13_016 => ("marseille", postcode_num - 13_000),
        _ => return None,
    };
    let rest: Vec<&str> = toks
        .iter()
        .enumerate()
        .filter_map(|(i, t)| (i != postcode_pos).then_some(*t))
        .collect();
    let canonical_ordinal = if ordinal == 1 {
        "1er".to_string()
    } else {
        format!("{ordinal}e")
    };
    let canonical = format!("{city} {canonical_ordinal} arrondissement");

    let matches_area_form = |area: &[&str]| {
        let without_label: Vec<&str> = area
            .iter()
            .copied()
            .filter(|t| !t.starts_with("arrondissement"))
            .collect();
        match without_label.as_slice() {
            [] => true,
            [one] if *one == city => true,
            [one] => fr_ordinal_number(one) == Some(ordinal),
            [first, second] if *first == city => fr_ordinal_number(second) == Some(ordinal),
            [first, second] if *second == city => fr_ordinal_number(first) == Some(ordinal),
            _ => false,
        }
    };
    let is_area_token = |t: &&str| {
        matches!(*t, "paris" | "lyon" | "marseille")
            || t.starts_with("arrondissement")
            || fr_ordinal_number(t).is_some()
    };
    let suffix = &toks[postcode_pos + 1..];
    if !suffix.is_empty() && suffix.iter().all(is_area_token) && !matches_area_form(suffix) {
        return Some(FrPostcodeArea::Conflict);
    }
    let mut area_start = postcode_pos;
    while area_start > 0 && is_area_token(&toks[area_start - 1]) {
        area_start -= 1;
    }
    let prefix_area = &toks[area_start..postcode_pos];
    if prefix_area.len() >= 2 && !matches_area_form(prefix_area) {
        return Some(FrPostcodeArea::Conflict);
    }
    let matches = matches_area_form(&rest);
    if matches {
        return Some(FrPostcodeArea::Match(canonical));
    }

    if !rest.is_empty() && rest.iter().all(is_area_token) {
        Some(FrPostcodeArea::Conflict)
    } else {
        None
    }
}

/// Whether the normalized query contains an explicit adjacent French city + arrondissement.
/// The pair may be before or after the street phrase; fallbacks must not drop it and return a
/// house from another district.
fn has_explicit_fr_arrondissement(q: &str) -> bool {
    let toks: Vec<&str> = q.split(' ').filter(|t| !t.is_empty()).collect();
    let is_city = |t: &str| matches!(t, "paris" | "lyon" | "marseille");
    toks.windows(2)
        .any(|pair| is_city(pair[0]) && fr_ordinal_number(pair[1]).is_some())
}

/// Canonical arrondissement explicitly constrained by a French query.
///
/// This is a postcondition for full-address parsing, not another parser: an encoded Paris,
/// Lyon or Marseille postcode, or an explicit trailing `city ordinal`, must be reflected by
/// the returned commune. Otherwise a perfectly matching street in another arrondissement
/// can win and silently discard the user's administrative constraint.
fn fr_arrondissement_constraint(q: &str) -> Option<FrPostcodeArea> {
    let toks: Vec<&str> = q.split(' ').filter(|t| !t.is_empty()).collect();
    let from_parts = |city: &str, ordinal: usize| -> Option<String> {
        let max = match city {
            "paris" => 20,
            "lyon" => 9,
            "marseille" => 16,
            _ => return None,
        };
        if !(1..=max).contains(&ordinal) {
            return None;
        }
        let ordinal = if ordinal == 1 {
            "1er".to_string()
        } else {
            format!("{ordinal}e")
        };
        Some(format!("{city} {ordinal} arrondissement"))
    };

    let mut signals = Vec::new();
    for token in &toks {
        if token.len() != 5 || !token.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(postcode) = token.parse::<usize>() else {
            continue;
        };
        let area = match postcode {
            75_001..=75_020 => from_parts("paris", postcode - 75_000),
            75_116 => from_parts("paris", 16),
            69_001..=69_009 => from_parts("lyon", postcode - 69_000),
            13_001..=13_016 => from_parts("marseille", postcode - 13_000),
            _ => None,
        };
        if let Some(area) = area {
            signals.push(area);
        }
    }

    if has_explicit_fr_arrondissement(q) {
        for pair in toks.windows(2) {
            if let Some(area) = fr_ordinal_number(pair[1]).and_then(|n| from_parts(pair[0], n)) {
                signals.push(area);
            }
        }
    }
    let first = signals.first()?.clone();
    if signals.iter().skip(1).any(|area| area != &first) {
        Some(FrPostcodeArea::Conflict)
    } else {
        Some(FrPostcodeArea::Match(first))
    }
}

/// Street-phrase variant with the LAST TWO words swapped: the Italian registry stores
/// "corso matteotti giacomo" (surname-first) while people write "corso giacomo matteotti",
/// so the exact key would miss and fuzzy matching would drift to a wrong street.
fn swap_last_two_variant(phrase: &str) -> Option<String> {
    let w: Vec<&str> = phrase.split(' ').filter(|t| !t.is_empty()).collect();
    if w.len() < 3 {
        return None;
    }
    let mut v = w.clone();
    let n = v.len();
    v.swap(n - 2, n - 1);
    Some(v.join(" "))
}

/// Phrase variant UNGLUING "letter(s)+digits" tokens ("c5" -> "c 5"): preprocessing glues
/// hyphenated block codes ("c-5" -> "c5") while the index stores them spaced ("c 5"), so
/// the exact key would be unreachable. Unglues short fused tokens.
fn unglue_variant(phrase: &str) -> Option<String> {
    let mut changed = false;
    let out: Vec<String> = phrase
        .split(' ')
        .filter(|t| !t.is_empty())
        .map(|t| {
            let alpha: String = t.chars().take_while(|c| c.is_alphabetic()).collect();
            let rest = &t[alpha.len()..];
            if !alpha.is_empty()
                && alpha.chars().count() <= 2
                && !rest.is_empty()
                && rest.len() <= 4
                && rest.bytes().all(|b| b.is_ascii_digit())
            {
                changed = true;
                format!("{alpha} {rest}")
            } else {
                t.to_string()
            }
        })
        .collect();
    if changed {
        Some(out.join(" "))
    } else {
        None
    }
}

/// Phone-number run: in "Anna Visser 06 81 22 64 90" digit pairs would parse as house
/// numbers and the surname could match a street, yielding a confident bogus hit. A run of
/// >=4 consecutive short digit tokens never occurs in a real address (house+postcode+block
/// > are <=3 and separated by words) — cut the run; the remainder honestly yields empty/low.
fn strip_phone_runs(q: &str) -> Option<String> {
    let toks: Vec<&str> = q.split(' ').filter(|t| !t.is_empty()).collect();
    let is_short_num = |t: &str| t.len() <= 4 && t.bytes().all(|b| b.is_ascii_digit());
    let mut keep = vec![true; toks.len()];
    let mut changed = false;
    let mut i = 0;
    while i < toks.len() {
        if is_short_num(toks[i]) {
            let mut j = i;
            while j < toks.len() && is_short_num(toks[j]) {
                j += 1;
            }
            if j - i >= 4 {
                keep[i..j].fill(false);
                changed = true;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    if !changed {
        return None;
    }
    Some(
        toks.iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(t, _)| *t)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

pub struct Index {
    // Owned backing memory: the mapping and this file's rules are freed when the
    // Index drops. Every `&'static` field below borrows from `_mmap`; they never escape the Index,
    // so the lie is sound. Declared first, but field drop order is irrelevant — the borrowing
    // fields' Drops never dereference the bytes.
    _mmap: Arc<Mapping>,
    _rules_owned: Option<Box<crate::rules::Rules>>,
    /// Header version controls the conditional house-block grammar. v6 remains readable during
    /// migration but never carries v7's local postcode dictionary or fifth house varint.
    format_version: u8,
    communes_fst: Map<&'static [u8]>,
    streets_fst: Map<&'static [u8]>,
    communes_meta: &'static [u8],
    postings: &'static [u8],
    streets_meta: &'static [u8],
    houses: &'static [u8],
    names: &'static [u8],
    cells_dir: &'static [u8],
    cells_post: &'static [u8],
    words_fst: Map<&'static [u8]>,
    word_postings: &'static [u8],
    commune_coords: &'static [u8],
    rep_lookup: HashMap<String, u32>,
    /// Suffix by on-disk rep id. Index 0 is the empty suffix, so decoding a house's rep is
    /// O(1); `rep_lookup` remains the separate query-text -> id parser dictionary.
    rep_suffixes: Vec<String>,
    parser: Option<crate::ml::Parser>,
    rank: Option<Rank>,
    /// This file's own rule tables (SEC_RULES), or the built-in defaults. Made current for
    /// the thread while a query runs, so two files built from different rule versions —
    /// a current sheet and an older one — never answer with each other's rules.
    rules: &'static crate::rules::Rules,
    /// Centroid of the most prominent commune in the index (the de-facto capital) — a weak
    /// anchor for tie-breaking homonyms when the query names no city.
    top_anchor: Option<(f64, f64)>,
    /// WOF administrative-region polygons for reverse PIP — from the sibling _admin.bin.
    admin: Vec<AdminRegion>,
    /// SEC_META (v6): provenance + identity pairs; empty on sheets built without --meta.
    meta: Vec<(String, String)>,
}

impl Index {
    /// Open a sheet meant to be the PRIMARY address index, refusing a POI layer.
    /// A POI-only sheet loaded as the main index would answer address queries with places; POI
    /// sheets must be loaded via the cascade (`--poi` / `poi=` / `gridpin_load_poi`), never as
    /// the primary. Address sheets and pre-v6 sheets (no `layer` meta) are accepted.
    pub fn open_address(path: &Path) -> Result<Index> {
        let idx = Index::open(path)?;
        // Uniform layer policy: accept ONLY an `addresses` layer or a layer-less lab/pre-v6
        // sheet. The old check rejected exactly `poi` but let an UNKNOWN/malformed layer (e.g.
        // `bad_layer`) open as the main index. Symmetric to open_poi's whitelist.
        if matches!(idx.layer(), Some(l) if l != "addresses") {
            anyhow::bail!(
                "{path:?} has layer {:?}, not an address sheet — load an `addresses` layer as the \
                 main index (a POI layer goes via --poi / gridpin_load_poi)",
                idx.layer().unwrap_or("")
            );
        }
        Ok(idx)
    }

    /// Open a sheet as a POI layer, refusing the wrong layer: the symmetric guard to
    /// `open_address`. Loading an ADDRESS sheet as a POI layer used to be accepted (the POI loader
    /// used the permissive `open`), so an address index could be attached where a POI was meant. A
    /// layer-less lab build is still allowed, matching `open_address`'s leniency.
    pub fn open_poi(path: &Path) -> Result<Index> {
        let idx = Index::open(path)?;
        if matches!(idx.layer(), Some(l) if l != "poi") {
            anyhow::bail!(
                "{path:?} is a '{}' sheet, not a POI layer — load the address sheet via the main \
                 index, not as a POI layer",
                idx.layer().unwrap_or("")
            );
        }
        Ok(idx)
    }

    pub fn open(path: &Path) -> Result<Index> {
        let file = File::open(path).with_context(|| format!("cannot open {path:?}"))?;
        // The mapping is OWNED by the returned Index and freed on Drop — no more
        // permanent leak per open. SAFETY: `mmap` moves into the Index and lives as long as it; the
        // mapped bytes sit at a fixed OS address that does not move with the Arc/Index, so the
        // `&'static` derived below is valid for the Index's life and never exposed past it. On any
        // early return, the local/moved `mmap` drops -> munmap, so a FAILED open frees too.
        let mmap = Arc::new(Mapping::new(unsafe { Mmap::map(&file)? }));
        let data: &'static [u8] = unsafe { std::mem::transmute::<&[u8], &'static [u8]>(&mmap[..]) };
        // No panic must escape the read API: a semantic-corrupt sheet that passes
        // bounds + TOC checks can still panic inside the fst crate during construction. The CLI
        // has no other panic boundary, so catch it here and return a clean error instead.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            Self::open_mapped(mmap, data, path)
        }))
        .map_err(|_| anyhow::anyhow!("{path:?}: corrupt index (panic while reading sections)"))?
    }

    fn open_mapped(mmap: Arc<Mapping>, data: &'static [u8], path: &Path) -> Result<Index> {
        let secs = parse_sections(data)?;
        let format_version = data[4];
        let sl = |i: usize| -> &'static [u8] {
            let (off, len) = secs[i];
            &data[off as usize..(off + len) as usize]
        };
        // reps and cells are read from raw section bytes; a truncated or empty section must
        // degrade gracefully (empty dict / no cells), never index out of bounds.
        let reps_raw = sl(SEC_REPS);
        let mut rep_lookup = HashMap::new();
        let mut rep_suffixes = vec![String::new()]; // rep id 0 = no suffix
        if reps_raw.len() >= 4 {
            let cnt = read_u32(reps_raw, 0) as usize;
            let mut p = 4usize;
            for id in 1..=cnt {
                if p >= reps_raw.len() {
                    break;
                }
                let l = reps_raw[p] as usize;
                p += 1;
                if p + l > reps_raw.len() {
                    break;
                }
                let s = std::str::from_utf8(&reps_raw[p..p + l])
                    .unwrap_or("")
                    .to_string();
                p += l;
                rep_lookup.insert(s.clone(), id as u32);
                rep_suffixes.push(s);
            }
        }
        let cells_raw = sl(SEC_CELLS);
        let (cells_dir, cells_post): (&[u8], &[u8]) = if cells_raw.len() >= 4 {
            let n_dir = read_u32(cells_raw, 0) as usize;
            let body = &cells_raw[4..];
            if n_dir.saturating_mul(12) <= body.len() {
                body.split_at(n_dir * 12)
            } else {
                (&[], &[]) // corrupt cells directory — treat as no reverse-geo cells
            }
        } else {
            (&[], &[])
        };
        // Strict open: an ABSENT optional section is fine (None), but a section that is
        // PRESENT (non-empty) yet unparseable means the sheet is corrupt/tampered — fail the open
        // instead of silently dropping the trained parser/ranking to None (a sheet that quietly
        // lost a capability with no error). The build already validates these, so this only fires
        // on a damaged/tampered file.
        let parser_raw = sl(SEC_PARSER);
        let parser = if parser_raw.is_empty() {
            None
        } else {
            Some(
                crate::ml::Parser::from_section(parser_raw).with_context(|| {
                    format!(
                        "{path:?}: SEC_PARSER is present but malformed — corrupt/tampered sheet"
                    )
                })?,
            )
        };
        let rank_raw = sl(SEC_RANK);
        let rank = if rank_raw.is_empty() {
            None
        } else {
            Some(Rank::from_section(rank_raw).with_context(|| {
                format!("{path:?}: SEC_RANK is present but malformed — corrupt/tampered sheet")
            })?)
        };
        // RULES-IN-DATA: this file's own tables, needed before their first use below (the capital
        // anchor already reads rules().capitals). OWNED so it frees on Drop.
        // SAFETY: same invariant as `mmap` — the box moves into the Index and outlives every use.
        let rules_owned: Option<Box<crate::rules::Rules>> =
            crate::rules::from_section_owned(sl(SEC_RULES));
        let rules: &'static crate::rules::Rules = match &rules_owned {
            Some(b) => unsafe {
                std::mem::transmute::<&crate::rules::Rules, &'static crate::rules::Rules>(&**b)
            },
            None => crate::rules::defaults_static(),
        };
        let _rules_scope = crate::rules::scope(rules);
        // CAPITAL ANCHOR. Capitals are fragmented in the data (e.g. Paris = 20
        // arrondissements), so the single most-address-rich commune may be the wrong
        // anchor. Two steps: (1) fallback = the single most prominent commune; (2) on top,
        // a curated capital list (same approach as commune_alias): the FST key group
        // "name" and "name ..." summed by prominence; the capital wins unless it is
        // tiny (<1/20 of the fallback).
        let cmeta = sl(SEC_COMMUNES_META);
        let ccoord = sl(SEC_COMMUNE_COORDS);
        let ncom = cmeta.len() / COMMUNE_META_SIZE;
        let (mut top_id, mut top_prom) = (0usize, 0u32);
        for id in 0..ncom {
            let prom = read_u32(cmeta, id * COMMUNE_META_SIZE + 12);
            if prom > top_prom {
                top_prom = prom;
                top_id = id;
            }
        }
        let mut top_anchor = if top_prom > 0 && (top_id * 8 + 8) <= ccoord.len() {
            let o = top_id * 8;
            let (la, lo) = (
                read_i32(ccoord, o) as f64 / 1e7,
                read_i32(ccoord, o + 4) as f64 / 1e7,
            );
            if la != 0.0 || lo != 0.0 {
                Some((la, lo))
            } else {
                None
            }
        } else {
            None
        };
        {
            let communes_fst_ref = Map::new(sl(SEC_COMMUNES_FST))?;
            let postings_ref = sl(SEC_COMMUNE_POSTINGS);
            let (mut best_sum, mut best_c) = (0u64, (0f64, 0f64));
            for cap in crate::rules::rules().capitals.iter().map(|s| s.as_str()) {
                let lo_key = cap.as_bytes().to_vec();
                let mut hi_key = cap.as_bytes().to_vec();
                hi_key.push(0xFF);
                let mut stream = communes_fst_ref
                    .range()
                    .ge(&lo_key)
                    .lt(&hi_key)
                    .into_stream();
                let (mut sum, mut wla, mut wlo) = (0u64, 0f64, 0f64);
                while let Some((key, v)) = stream.next() {
                    // only the exact name or "name + space" ("paris 1er..."), not "parisot"
                    if key.len() > cap.len() && key[cap.len()] != b' ' {
                        continue;
                    }
                    let start = (v >> 16) as usize;
                    let count = (v & 0xFFFF) as usize;
                    // start/count come from an FST value: on a corrupt-but-parseable file
                    // they can point past the postings section — reading would panic at
                    // open time and take the host process with it
                    if (start + count) * 4 > postings_ref.len() {
                        continue;
                    }
                    for i in 0..count {
                        let cid = read_u32(postings_ref, (start + i) * 4) as usize;
                        if cid * COMMUNE_META_SIZE + 16 > cmeta.len() || cid * 8 + 8 > ccoord.len()
                        {
                            continue;
                        }
                        let prom = read_u32(cmeta, cid * COMMUNE_META_SIZE + 12) as u64;
                        let la = read_i32(ccoord, cid * 8) as f64 / 1e7;
                        let lo = read_i32(ccoord, cid * 8 + 4) as f64 / 1e7;
                        if la == 0.0 && lo == 0.0 {
                            continue;
                        }
                        sum += prom;
                        wla += la * prom as f64;
                        wlo += lo * prom as f64;
                    }
                }
                if sum > best_sum {
                    best_sum = sum;
                    best_c = (wla / sum as f64, wlo / sum as f64);
                }
            }
            if best_sum > 0 && best_sum >= (top_prom as u64) / 20 {
                top_anchor = Some(best_c);
            }
        }
        // Content-shape invariants: a section whose TOC entry is intact but
        // whose CONTENT is zeroed (a sparse hole from an interrupted copy) or points into the wrong
        // section (swapped ids) used to open fine and answer a silent 0,0. Check record sizes divide
        // and EVERY street record satisfies the builder's own invariants — offsets land inside their
        // sections, commune id is in range, a street has >=1 house (the builder emits none with 0).
        // ALL records, not a sample;
        // this is bounded arithmetic per record, no allocation. Full BYTE integrity (tampering) is
        // still the release sha256 manifest's job — this catches structural corruption. NOTE: an
        // EMPTY display name is LEGAL (name() degrades to ""), so name_off may point at a 0-length
        // entry — only its BOUNDS are checked, never non-emptiness (that false invariant bricked a
        // legal sheet,).
        {
            let sm = sl(SEC_STREETS_META);
            let cm = sl(SEC_COMMUNES_META);
            let cc = sl(SEC_COMMUNE_COORDS);
            let names_b = sl(SEC_NAMES);
            let houses_b = sl(SEC_HOUSE_BLOCKS);
            if sm.len() % crate::index::STREET_META_SIZE != 0 {
                anyhow::bail!(
                    "{path:?}: streets_meta length is not a whole number of records — corrupt"
                );
            }
            if cm.len() % crate::index::COMMUNE_META_SIZE != 0 {
                anyhow::bail!(
                    "{path:?}: communes_meta length is not a whole number of records — corrupt"
                );
            }
            let ncommunes = cm.len() / crate::index::COMMUNE_META_SIZE;
            if cc.len() != ncommunes * 8 {
                anyhow::bail!(
                    "{path:?}: commune_coords does not match the commune count — corrupt"
                );
            }
            let nstreets = sm.len() / crate::index::STREET_META_SIZE;
            for i in 0..nstreets {
                let o = i * crate::index::STREET_META_SIZE;
                let commune_id = read_u32(sm, o + 8) as usize;
                let name_off = read_u32(sm, o + 16) as usize;
                let house_off = read_u64(sm, o + 20) as usize;
                let house_count = read_u32(sm, o + 28);
                let postcode_disp_off = read_u32(sm, o + 32);
                let house_end = if i + 1 < nstreets {
                    usize::try_from(read_u64(sm, (i + 1) * crate::index::STREET_META_SIZE + 20))
                        .ok()
                } else {
                    Some(houses_b.len())
                };
                let bad = commune_id >= ncommunes
                    || house_count == 0
                    || house_off >= houses_b.len()
                    || house_end.is_none_or(|end| house_off >= end || end > houses_b.len())
                    || name_off >= names_b.len()
                    || name_off + 1 + names_b[name_off] as usize > names_b.len();
                if bad {
                    anyhow::bail!(
                        "{path:?}: street record {i} violates structural invariants — corrupt (zeroed or misdirected section content)"
                    );
                }
                // v7 sparse house-postcode prefix. Validate the bounded dictionary header for
                // every encoded street at open, without scanning every house record (which would
                // make opening a country sheet proportional to tens of millions of addresses).
                if format_version >= 7 && postcode_disp_off == PC_DISP_AMBIGUOUS {
                    let house_end = house_end.expect("validated above");
                    let bounded_houses = &houses_b[..house_end];
                    let mut p = house_off;
                    let Some(count) = strict_varint(bounded_houses, &mut p) else {
                        anyhow::bail!(
                            "{path:?}: street record {i} has a malformed house-postcode dictionary count — corrupt"
                        );
                    };
                    if count == 0 || count > u64::from(house_count) {
                        anyhow::bail!(
                            "{path:?}: street record {i} has an invalid house-postcode dictionary size — corrupt"
                        );
                    }
                    let Some(table_len) = usize::try_from(count)
                        .ok()
                        .and_then(|value| value.checked_mul(4))
                    else {
                        anyhow::bail!(
                            "{path:?}: street record {i} house-postcode dictionary overflows — corrupt"
                        );
                    };
                    let Some(table_end) = p.checked_add(table_len) else {
                        anyhow::bail!(
                            "{path:?}: street record {i} house-postcode dictionary overflows — corrupt"
                        );
                    };
                    if table_end > house_end {
                        anyhow::bail!(
                            "{path:?}: street record {i} house-postcode dictionary is truncated — corrupt"
                        );
                    }
                    let mut previous_postcode: Option<&str> = None;
                    for entry in (p..table_end).step_by(4) {
                        let postcode_off = read_u32(houses_b, entry) as usize;
                        if postcode_off >= names_b.len()
                            || postcode_off + 1 + names_b[postcode_off] as usize > names_b.len()
                        {
                            anyhow::bail!(
                                "{path:?}: street record {i} house-postcode dictionary points outside names — corrupt"
                            );
                        }
                        let postcode_len = names_b[postcode_off] as usize;
                        let postcode_bytes =
                            &names_b[postcode_off + 1..postcode_off + 1 + postcode_len];
                        let Ok(postcode) = std::str::from_utf8(postcode_bytes) else {
                            anyhow::bail!(
                                "{path:?}: street record {i} house-postcode dictionary is not UTF-8 — corrupt"
                            );
                        };
                        if !plausible_house_postcode(postcode)
                            || previous_postcode.is_some_and(|previous| previous >= postcode)
                        {
                            anyhow::bail!(
                                "{path:?}: street record {i} house-postcode dictionary is empty, implausible, duplicated, or unsorted — corrupt"
                            );
                        }
                        previous_postcode = Some(postcode);
                    }
                }
            }
        }
        // Hoist the fallible Map::new calls so a failure drops the `mmap`/`rules_owned` LOCALS
        // (munmap + free) before any move into the struct — failed opens never leak.
        let communes_fst = Map::new(sl(SEC_COMMUNES_FST))?;
        let streets_fst = Map::new(sl(SEC_STREETS_FST))?;
        let words_fst = Map::new(sl(SEC_WORDS))?;
        // word_postings (13) may be legally EMPTY only when the words FST has NO keys (a sheet whose
        // every street word is < 3 chars). A non-empty words FST with empty postings means the
        // postings section was truncated/deleted — fuzzy search would silently return nothing
        //. Reject rather than degrade silently.
        if !words_fst.is_empty() && sl(SEC_WORD_POSTINGS).is_empty() {
            anyhow::bail!(
                "{path:?}: word_postings is empty but the words FST has {} keys — corrupt (fuzzy search would silently fail)",
                words_fst.len()
            );
        }
        // CONTENT check of section 13: a PRESENT but zeroed/corrupted payload
        // used to pass open (only presence was verified) and silently kill fuzzy search. Decode
        // EVERY word's postings list against the builder's invariants: a word is in the FST only
        // BECAUSE it has ids (count >= 1, <= the builder cap), ids are strictly increasing
        // (sorted + deduped deltas) and each stays under nstreets. A zeroed payload fails at the
        // first word (count = 0); a partially-corrupted one breaks bounds/monotonicity.
        {
            let wp = sl(SEC_WORD_POSTINGS);
            let sm = sl(SEC_STREETS_META);
            let nstreets = (sm.len() / crate::index::STREET_META_SIZE) as u64;
            // STRICT varint validator for section 13: unlike the lenient
            // read_varint, `strict_varint` (index.rs) returns None on EOF-mid-varint, >10 bytes, OR
            // payload overflow past bit 63 — so a corrupt word_postings section is refused at open.
            let mut stream = words_fst.stream();
            while let Some((word, off)) = stream.next() {
                let mut p = off as usize;
                let fail = |why: &str| -> anyhow::Error {
                    anyhow::anyhow!(
                        "{path:?}: word_postings entry for {:?} {why} — corrupt (fuzzy search would silently fail)",
                        String::from_utf8_lossy(word)
                    )
                };
                if p >= wp.len() {
                    return Err(fail("points past the section"));
                }
                let n = match strict_varint(wp, &mut p) {
                    Some(n) => n,
                    None => return Err(fail("has a malformed/unterminated street count varint")),
                };
                if n == 0 || n > 16384 {
                    return Err(fail("has a zero/oversized street count"));
                }
                let mut prev: u64 = 0;
                for i in 0..n {
                    let delta = match strict_varint(wp, &mut p) {
                        Some(d) => d,
                        None => return Err(fail("is truncated mid-list (unterminated varint)")),
                    };
                    if i > 0 && delta == 0 {
                        return Err(fail("repeats a street id (deltas must be positive)"));
                    }
                    prev = match prev.checked_add(delta) {
                        Some(v) => v,
                        None => return Err(fail("overflows the street id accumulator")),
                    };
                    if prev >= nstreets {
                        return Err(fail("references a street id past the street table"));
                    }
                }
            }
        }
        Ok(Index {
            _mmap: mmap,
            _rules_owned: rules_owned,
            format_version,
            communes_fst,
            streets_fst,
            communes_meta: sl(SEC_COMMUNES_META),
            postings: sl(SEC_COMMUNE_POSTINGS),
            streets_meta: sl(SEC_STREETS_META),
            houses: sl(SEC_HOUSE_BLOCKS),
            names: sl(SEC_NAMES),
            cells_dir,
            cells_post,
            words_fst,
            word_postings: sl(SEC_WORD_POSTINGS),
            commune_coords: sl(SEC_COMMUNE_COORDS),
            rep_lookup,
            rep_suffixes,
            parser,
            rank,
            rules,
            top_anchor,
            admin: load_admin(path),
            meta: decode_meta(sl(SEC_META)).unwrap_or_default(),
        })
    }

    /// Provenance/identity pairs from SEC_META (empty on pre-v6-style sheets).
    pub fn meta(&self) -> &[(String, String)] {
        &self.meta
    }

    /// Whether this sheet carries a usable trained parser / ranking: the build now
    /// validates these sections, so a `true` here means the capability survived into the file.
    pub fn has_parser(&self) -> bool {
        self.parser.is_some()
    }

    pub fn has_rank(&self) -> bool {
        self.rank.is_some()
    }

    fn meta_get(&self, key: &str) -> Option<&str> {
        self.meta
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// ISO country code carried by the sheet (v6 identity).
    pub fn country(&self) -> Option<&str> {
        self.meta_get("country")
    }

    /// Layer kind carried by the sheet: "addresses" or "poi" (v6 identity).
    pub fn layer(&self) -> Option<&str> {
        self.meta_get("layer")
    }

    /// Administrative region of a point (lat,lon) via WOF polygons — bbox filter + ray-cast PIP.
    fn admin_at(&self, lat: f64, lon: f64) -> Option<String> {
        let (la, lo) = ((lat * 1e7) as i32, (lon * 1e7) as i32);
        for r in &self.admin {
            if la < r.min_lat || la > r.max_lat || lo < r.min_lon || lo > r.max_lon {
                continue;
            }
            if point_in_rings(la, lo, &r.rings) {
                return Some(r.name.clone());
            }
        }
        None
    }

    fn name(&self, off: u32) -> &str {
        // Bounds-safe: a corrupt sheet may carry a name_off past the names blob. Direct
        // indexing panicked with "index out of bounds" and aborted the host;
        // an out-of-range offset now degrades to an empty name.
        let off = off as usize;
        let Some(&l) = self.names.get(off) else {
            return "";
        };
        self.names
            .get(off + 1..off + 1 + l as usize)
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("")
    }

    fn street_meta(&self, id: u32) -> StreetMeta {
        let b = self.streets_meta;
        let o = id as usize * STREET_META_SIZE;
        // street ids come from FST/postings values (file data): out-of-section ids on
        // a corrupt sheet must degrade to an empty record, not panic the host
        if o + STREET_META_SIZE > b.len() {
            return StreetMeta {
                lat_c: 0,
                lon_c: 0,
                commune_id: 0,
                postcode: 0,
                name_off: 0,
                house_off: 0,
                house_count: 0,
                postcode_disp_off: 0,
            };
        }
        StreetMeta {
            lat_c: read_i32(b, o),
            lon_c: read_i32(b, o + 4),
            commune_id: read_u32(b, o + 8),
            postcode: read_u32(b, o + 12),
            name_off: read_u32(b, o + 16),
            house_off: read_u64(b, o + 20),
            house_count: read_u32(b, o + 28),
            postcode_disp_off: read_u32(b, o + 32),
        }
    }

    /// Postcode for OUTPUT: the full string (NL "1012XJ", FR "75002") from the names table,
    /// else the zero-padded numeric form, else empty (sources without postcodes).
    fn postcode_out(&self, m: &StreetMeta) -> String {
        // a street spanning >1 postcode has no house-accurate street value — emit empty, not a
        // neighbour's postcode. name() is bounds-safe, so the sentinel never indexes.
        if m.postcode_disp_off == PC_DISP_AMBIGUOUS {
            return String::new();
        }
        if m.postcode_disp_off != 0 {
            let d = self.name(m.postcode_disp_off);
            if !d.is_empty() {
                return d.to_string();
            }
        }
        if m.postcode == 0 {
            String::new()
        } else {
            format!("{:05}", m.postcode)
        }
    }

    /// Decode the optional sparse house-postcode prefix of a v7 ambiguous street block.
    /// Returns `(first_house_byte, dictionary_byte, dictionary_count)`. Dictionary entries are
    /// fixed-width little-endian offsets into `SEC_NAMES`; each house then carries a local varint
    /// id (0 = source row had no postcode, 1..=count = dictionary entry).
    ///
    /// The routine is deliberately bounds-safe: house blocks are external file data, and a
    /// truncated dictionary must degrade to no candidate rather than panic the host process.
    fn house_block_layout(
        &self,
        street_id: u32,
        m: &StreetMeta,
    ) -> Option<(usize, usize, u32, usize)> {
        let mut pos = usize::try_from(m.house_off).ok()?;
        let nstreets = self.streets_meta.len() / STREET_META_SIZE;
        let street_index = usize::try_from(street_id).ok()?;
        if street_index >= nstreets {
            return None;
        }
        let house_end = if street_index + 1 < nstreets {
            usize::try_from(read_u64(
                self.streets_meta,
                (street_index + 1) * STREET_META_SIZE + 20,
            ))
            .ok()?
        } else {
            self.houses.len()
        };
        if pos >= house_end || house_end > self.houses.len() {
            return None;
        }
        if self.format_version < 7 || m.postcode_disp_off != PC_DISP_AMBIGUOUS {
            return Some((pos, 0, 0, house_end));
        }
        let count = u32::try_from(strict_varint(&self.houses[..house_end], &mut pos)?).ok()?;
        // An encoded street always has at least one known postcode; it cannot have more
        // distinct known values than address rows.
        if count == 0 || count > m.house_count {
            return None;
        }
        let dictionary_byte = pos;
        let dictionary_len = usize::try_from(count).ok()?.checked_mul(4)?;
        pos = pos.checked_add(dictionary_len)?;
        if pos > house_end {
            return None;
        }
        Some((pos, dictionary_byte, count, house_end))
    }

    fn house_postcode_offset(&self, dictionary_byte: usize, count: u32, id: u32) -> u32 {
        if id == 0 || id > count {
            return 0;
        }
        let Some(entry) = usize::try_from(id - 1)
            .ok()
            .and_then(|index| index.checked_mul(4))
            .and_then(|delta| dictionary_byte.checked_add(delta))
        else {
            return 0;
        };
        if entry + 4 > self.houses.len() {
            return 0;
        }
        read_u32(self.houses, entry)
    }

    /// Output postcode for the concrete represented address. Unambiguous streets retain their
    /// compact street-level value. On a sparse house-accurate street, only the selected house's
    /// own dictionary value is allowed; a missing/corrupt value stays empty and never falls back
    /// to a neighbour or to the street majority.
    fn postcode_for_house(&self, m: &StreetMeta, house_postcode_off: u32) -> String {
        if m.postcode_disp_off != PC_DISP_AMBIGUOUS {
            return self.postcode_out(m);
        }
        if house_postcode_off == 0 {
            return String::new();
        }
        self.name(house_postcode_off).to_string()
    }

    fn commune_insee(&self, id: u32) -> &str {
        let o = id as usize * COMMUNE_META_SIZE;
        let raw = &self.communes_meta[o..o + 8];
        let end = raw.iter().position(|&c| c == 0).unwrap_or(8);
        std::str::from_utf8(&raw[..end]).unwrap_or("")
    }

    fn commune_name(&self, id: u32) -> &str {
        let o = id as usize * COMMUNE_META_SIZE;
        self.name(read_u32(self.communes_meta, o + 8))
    }

    /// Commune prominence = its address count (a population proxy): ranking tie-break when
    /// score and name similarity are equal — a capital beats a village, a city center beats
    /// a suburb. Indexes without the field store 0, which disables the tie-break.
    fn commune_prominence(&self, id: u32) -> u32 {
        read_u32(self.communes_meta, id as usize * COMMUNE_META_SIZE + 12)
    }

    /// Commune centroid (the "city point") — for city-only queries.
    fn commune_coord(&self, id: u32) -> (f64, f64) {
        let o = id as usize * 8;
        if o + 8 > self.commune_coords.len() {
            return (0.0, 0.0);
        }
        (
            read_i32(self.commune_coords, o) as f64 / 1e7,
            read_i32(self.commune_coords, o + 4) as f64 / 1e7,
        )
    }

    /// Universal resolution of a bare place name (city/district/estate/arrondissement) -> point.
    /// (1) Exact commune name -> its centroid (among homonyms, the most prominent one).
    /// (2) Else the prefix commune group: "Lyon" -> "Lyon 1er...", i.e. an umbrella of
    ///     sub-units -> prominence-weighted mean centroid.
    /// Country-agnostic: the data itself decides whether a district is a commune or a
    /// prefix umbrella. Returns (lat, lon, name, prominence). Prominence = the commune's
    /// address count (for an umbrella, the group sum): a large umbrella city scores high,
    /// a specific settlement low. This is the anchor weight for the place fallback:
    /// among resolved places the anchor is the most prominent one.
    fn resolve_place(&self, name: &str) -> Option<(f64, f64, String, u32)> {
        if name.is_empty() {
            return None;
        }
        // (1) Exact commune name. Large cities may be split into same-named fragments by
        // geo-cell splitting; taking a single fragment's centroid would shift the "city
        // center". Homonyms within 40 km of the most prominent one are fragments of ONE
        // city: take their weighted centroid and summed prominence. Distant true homonyms
        // (hundreds of km apart) stay out of the group.
        let ids = self.communes_by_name(name);
        if let Some(&top) = ids.iter().max_by_key(|&&id| self.commune_prominence(id)) {
            let (tla, tlo) = self.commune_coord(top);
            if tla != 0.0 || tlo != 0.0 {
                // merge ONLY same-named entries (same display name as the most prominent):
                // communes_by_name may also return villages under a city umbrella, and
                // merging those would pull the anchor toward the villages. City fragments
                // share the display name and merge; distinct places do not.
                let top_name = self.commune_name(top);
                let (mut sla, mut slo, mut sw) = (0.0f64, 0.0f64, 0.0f64);
                for &id in &ids {
                    let (la, lo) = self.commune_coord(id);
                    if (la == 0.0 && lo == 0.0)
                        || Self::dist_km(tla, tlo, la, lo) > 40.0
                        || self.commune_name(id) != top_name
                    {
                        continue;
                    }
                    let w = self.commune_prominence(id).max(1) as f64;
                    sla += la * w;
                    slo += lo * w;
                    sw += w;
                }
                return Some((sla / sw, slo / sw, top_name.to_string(), sw as u32));
            }
        }
        // (2) prefix commune group (+SPACE so "lyon " does not match "lyonne")
        let arr = self.communes_by_prefix(&format!("{name} "));
        let (mut sla, mut slo, mut sw) = (0.0f64, 0.0f64, 0.0f64);
        for id in &arr {
            let (la, lo) = self.commune_coord(*id);
            if la == 0.0 && lo == 0.0 {
                continue;
            }
            let w = self.commune_prominence(*id).max(1) as f64;
            sla += la * w;
            slo += lo * w;
            sw += w;
        }
        if sw > 0.0 {
            return Some((sla / sw, slo / sw, name.to_string(), sw as u32));
        }
        None
    }

    /// resolve_place + transliteration both ways (mixed scripts): a Cyrillic query finds
    /// Latin data (both Serbian Gaj and English digraphs) and vice versa.
    fn resolve_place_translit(&self, phrase: &str) -> Option<(f64, f64, String, u32)> {
        let try_t = |f: fn(&str) -> String| -> Option<(f64, f64, String, u32)> {
            let t = normalize(&f(phrase));
            if t != phrase {
                self.resolve_place(&t)
            } else {
                None
            }
        };
        self.resolve_place(phrase)
            .or_else(|| try_t(crate::norm::translit_cyr_lat))
            .or_else(|| try_t(crate::norm::translit_cyr_lat_en))
            .or_else(|| try_t(crate::norm::translit_lat_cyr))
            .filter(|&(la, lo, _, _)| la != 0.0 || lo != 0.0)
    }

    /// Distance between points in km (haversine).
    fn dist_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
        let r = 6371.0_f64;
        let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
        let dp = (lat2 - lat1).to_radians();
        let dl = (lon2 - lon1).to_radians();
        let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
        2.0 * r * a.sqrt().asin()
    }

    /// Resolve an explicit trailing geographic qualifier for a genuinely distant homonym:
    /// `... Castro Bergamo`, `... Samone Trento`, `... San Teodoro Messina`.
    ///
    /// The qualifier must itself resolve as a place, the immediately preceding commune name
    /// must have centroids over 80 km apart, and the chosen homonym must be within 120 km of
    /// the qualifier with at least a 25 km advantage over every other remote cluster. The
    /// address query then runs without the qualifier and retains only hits assigned to the
    /// selected 40 km homonym cluster. If that cluster has no matching address, return empty
    /// rather than a confident address in a different province.
    fn trailing_homonym_qualifier_retry(
        &self,
        q: &str,
        k: usize,
        focus: Option<&QueryFocus>,
    ) -> Option<Vec<(Hit, [f32; N_FEATS])>> {
        let toks: Vec<&str> = q.split(' ').filter(|t| !t.is_empty()).collect();
        if toks.len() < 4 {
            return None;
        }
        let max_qualifier = 3.min(toks.len().saturating_sub(3));
        for qualifier_len in (1..=max_qualifier).rev() {
            let qualifier_start = toks.len() - qualifier_len;
            let qualifier = toks[qualifier_start..].join(" ");
            let Some((anchor_lat, anchor_lon, _, _)) = self.resolve_place_translit(&qualifier)
            else {
                continue;
            };
            let max_commune = 4.min(qualifier_start.saturating_sub(2));
            for commune_len in (1..=max_commune).rev() {
                let commune_start = qualifier_start - commune_len;
                let commune = toks[commune_start..qualifier_start].join(" ");
                let ids = self.communes_by_name(&commune);
                if ids.len() < 2 {
                    continue;
                }
                // `Oriolo Romano` is a complete commune, not homonym `Oriolo` qualified by
                // place `Romano`. Preserve every exact full-commune interpretation.
                let full_commune = toks[commune_start..].join(" ");
                if !self.communes_by_name(&full_commune).is_empty() {
                    return None;
                }
                // Exact-name postings are file-controlled and may contain u16::MAX ids. Bound
                // the quadratic distance check on adversarial/lab sheets; real IT max is 9.
                if ids.len() > 40 {
                    return Some(Vec::new());
                }
                let coords: Vec<(u32, f64, f64)> = ids
                    .iter()
                    .filter_map(|&id| {
                        let (lat, lon) = self.commune_coord(id);
                        (lat != 0.0 || lon != 0.0).then_some((id, lat, lon))
                    })
                    .collect();
                let remote_homonyms = coords.iter().enumerate().any(|(i, (_, alat, alon))| {
                    coords[i + 1..]
                        .iter()
                        .any(|(_, blat, blon)| Self::dist_km(*alat, *alon, *blat, *blon) > 80.0)
                });
                if !remote_homonyms {
                    continue;
                }

                let Some((selected_id, selected_lat, selected_lon, selected_distance)) = coords
                    .iter()
                    .map(|&(id, lat, lon)| {
                        (
                            id,
                            lat,
                            lon,
                            Self::dist_km(anchor_lat, anchor_lon, lat, lon),
                        )
                    })
                    .min_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal))
                else {
                    continue;
                };
                if selected_distance > 120.0 {
                    return Some(Vec::new());
                }
                let selected_ids: Vec<u32> = coords
                    .iter()
                    .filter_map(|&(id, lat, lon)| {
                        (Self::dist_km(selected_lat, selected_lon, lat, lon) <= 40.0).then_some(id)
                    })
                    .collect();
                debug_assert!(selected_ids.contains(&selected_id));
                let second_distance = coords
                    .iter()
                    .filter(|(id, _, _)| !selected_ids.contains(id))
                    .map(|&(_, lat, lon)| Self::dist_km(anchor_lat, anchor_lon, lat, lon))
                    .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                if second_distance.is_none_or(|distance| distance - selected_distance < 25.0) {
                    return Some(Vec::new());
                }
                let base = toks[..qualifier_start].join(" ");
                let mut hits = self.query_feats_prepared(&base, k.max(10), focus);
                hits.retain(|(hit, _)| {
                    if normalize(&hit.commune) != commune {
                        return false;
                    }
                    coords
                        .iter()
                        .min_by(|(_, alat, alon), (_, blat, blon)| {
                            Self::dist_km(*alat, *alon, hit.lat, hit.lon)
                                .partial_cmp(&Self::dist_km(*blat, *blon, hit.lat, hit.lon))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .is_some_and(|(id, _, _)| selected_ids.contains(id))
                });
                for (hit, features) in &mut hits {
                    // The base query may have been capped as `ambiguous_far` before the explicit
                    // qualifier selected one remote cluster. Recompute the original calibrated
                    // confidence from its features and remove that now-resolved warning.
                    let feats = Feats::from_vec(features);
                    hit.confidence = confidence_score(hit.precision, &feats, 0);
                    if hit.score < 0.0 {
                        hit.confidence = hit.confidence.min(0.4);
                    }
                    hit.flags.retain(|flag| *flag != "ambiguous_far");
                    hit.flags.push("geo_qualifier");
                }
                hits.truncate(k);
                return Some(hits);
            }
        }
        None
    }

    /// Wrap a city point into a single city-precision Hit.
    fn city_hit(lat: f64, lon: f64, commune: String) -> Vec<(Hit, [f32; N_FEATS])> {
        vec![(
            Hit {
                lat,
                lon,
                precision: "city",
                score: 0.0,
                confidence: 0.05, // calibrated: city-level hits rarely fall within 150 m
                street: String::new(),
                housenumber: None,
                commune,
                postcode: String::new(),
                flags: Vec::new(),
                region: None,
                distance_m: None,
            },
            [0.0; N_FEATS],
        )]
    }

    /// Commune ids by normalized name (all homonyms).
    fn communes_by_name_raw(&self, name: &str) -> Vec<u32> {
        match self.communes_fst.get(name.as_bytes()) {
            None => Vec::new(),
            Some(v) => {
                let start = (v >> 16) as usize;
                let count = (v & 0xFFFF) as usize;
                // FST values come from the file: on a corrupt-but-parseable sheet they
                // can point past the postings section — a panic here would take the
                // host process (Python/DuckDB) down with it
                if (start + count) * 4 > self.postings.len() {
                    return Vec::new();
                }
                (0..count)
                    .map(|i| read_u32(self.postings, (start + i) * 4))
                    .collect()
            }
        }
    }

    fn communes_by_name(&self, name: &str) -> Vec<u32> {
        let name = commune_alias(name).unwrap_or(name);
        let ids = self.communes_by_name_raw(name);
        if !ids.is_empty() {
            return ids;
        }
        // ARTICLE ELISION (FR): users commonly drop "L'/La/Le/Les" ("Ile Rousse" for
        // "L'Île-Rousse") or glue the article onto the name ("Lhay").
        for art in ["l", "la", "le", "les"] {
            let ids = self.communes_by_name_raw(&format!("{art} {name}"));
            if !ids.is_empty() {
                return ids;
            }
        }
        if let Some(rest) = name.strip_prefix('l') {
            if rest.chars().next().is_some_and(|c| c.is_alphabetic()) {
                let ids = self.communes_by_name_raw(&format!("l {rest}"));
                if !ids.is_empty() {
                    return ids;
                }
            }
        }
        // PLACE TYPE WORDS (e.g. Uzbek block markers): "name N kvartal" != commune name
        // "name N" — strip district type words and retry (bridges kvartal/mavze synonyms).
        let stripped: Vec<&str> = name
            .split(' ')
            .filter(|w| !crate::rules::rules().place_type_strip.contains(*w))
            .collect();
        if stripped.len() < name.split(' ').count() && !stripped.is_empty() {
            return self.communes_by_name_raw(&stripped.join(" "));
        }
        Vec::new()
    }

    /// Communes by name prefix ("paris" -> all arrondissements) — fallback path.
    fn communes_by_prefix(&self, name: &str) -> Vec<u32> {
        let name = commune_alias(name).unwrap_or(name);
        let out = self.communes_by_prefix_raw(name);
        if !out.is_empty() {
            return out;
        }
        for art in ["l", "la", "le", "les"] {
            let out = self.communes_by_prefix_raw(&format!("{art} {name}"));
            if !out.is_empty() {
                return out;
            }
        }
        Vec::new()
    }

    fn communes_by_prefix_raw(&self, name: &str) -> Vec<u32> {
        let lo = name.as_bytes().to_vec();
        let mut hi = name.as_bytes().to_vec();
        hi.push(0xFF);
        let mut out = Vec::new();
        let mut stream = self.communes_fst.range().ge(&lo).lt(&hi).into_stream();
        while let Some((_, v)) = stream.next() {
            let start = (v >> 16) as usize;
            let count = (v & 0xFFFF) as usize;
            if (start + count) * 4 > self.postings.len() {
                continue; // corrupt posting span: skip this name, never panic
            }
            for i in 0..count {
                out.push(read_u32(self.postings, (start + i) * 4));
                if out.len() >= 40 {
                    return out;
                }
            }
        }
        out
    }

    /// House lookup within a street block. Returns
    /// `(lat, lon, kind, number, rep, postcode_name_off)`, kind:
    ///   3 = interpolated between tight neighbors ("interp"),
    ///   2 = exact house (number AND rep matched),
    ///   1 = number matched, any rep,
    ///   0 = number absent -> NEAREST-by-number neighbor on the same street (snapping to a
    ///       house instead of the street center: missing numbers with present neighbors are
    ///       common, and the neighbor is tens of meters off instead of hundreds or km).
    /// Houses are sorted ascending (delta coding) -> lower/upper neighbor.
    /// The final two elements identify the address represented by the returned point. A
    /// near snap carries the stored neighbour's number and suffix; interpolation has no
    /// stored house, so it carries the requested number and suffix. The number is also used
    /// as a snap tie-breaker: on a split street, a near-snap to a distant number of another
    /// fragment loses to a candidate whose number is closer to the requested one.
    fn find_house(
        &self,
        street_id: u32,
        m: &StreetMeta,
        numero: u32,
        rep: u32,
    ) -> Option<(f64, f64, u8, u32, u32, u32)> {
        let (mut pos, postcode_dictionary, postcode_count, house_end) =
            self.house_block_layout(street_id, m)?;
        let bounded_houses = &self.houses[..house_end];
        let mut cur = 0u32;
        let mut num_only: Option<(f64, f64, u32, u32)> = None;
        let mut lower: Option<(u32, u32, f64, f64, u32)> = None; // last house below requested
        let mut upper: Option<(u32, u32, f64, f64, u32)> = None; // first house above requested
        for i in 0..m.house_count {
            let d = u32::try_from(strict_varint(bounded_houses, &mut pos)?).ok()?;
            cur = if i == 0 { d } else { cur.checked_add(d)? };
            let rid = u32::try_from(strict_varint(bounded_houses, &mut pos)?).ok()?;
            let dlat = unzigzag(strict_varint(bounded_houses, &mut pos)?);
            let dlon = unzigzag(strict_varint(bounded_houses, &mut pos)?);
            let postcode_off =
                if self.format_version >= 7 && m.postcode_disp_off == PC_DISP_AMBIGUOUS {
                    let id = u32::try_from(strict_varint(bounded_houses, &mut pos)?).ok()?;
                    self.house_postcode_offset(postcode_dictionary, postcode_count, id)
                } else {
                    0
                };
            let lat_e7 = (m.lat_c as i64).checked_add(dlat)?;
            let lon_e7 = (m.lon_c as i64).checked_add(dlon)?;
            if !(-900_000_000..=900_000_000).contains(&lat_e7)
                || !(-1_800_000_000..=1_800_000_000).contains(&lon_e7)
            {
                return None;
            }
            let lat = lat_e7 as f64 / 1e7;
            let lon = lon_e7 as f64 / 1e7;
            if cur == numero {
                if rid == rep {
                    return Some((lat, lon, 2, cur, rid, postcode_off));
                }
                if num_only.is_none() {
                    num_only = Some((lat, lon, rid, postcode_off));
                }
            } else if cur < numero {
                lower = Some((cur, rid, lat, lon, postcode_off));
            } else {
                // houses are sorted: the first exceeding one is the upper neighbor; stop
                upper = Some((cur, rid, lat, lon, postcode_off));
                break;
            }
        }
        if let Some((la, lo, rid, postcode_off)) = num_only {
            return Some((la, lo, 1, numero, rid, postcode_off));
        }
        // No exact number. If neighbors on BOTH sides are TIGHT (small number gap AND small
        // distance — dense linear numbering) -> interpolate by number fraction (47 between
        // 45 and 49 -> 0.5 of the segment), kind=3 "interp". In sparse data the bracket can
        // be wide (5 and 200) and interpolation overshoots, so a wide/distant bracket falls
        // back to the nearest neighbor (kind=0 "near"). Tightness threshold empirically tuned.
        if let (Some((ln, lrid, la, lo, lpc)), Some((un, urid, ua, uo, upc))) = (lower, upper) {
            let tight = un - ln <= 12 && Self::dist_km(la, lo, ua, uo) < 0.3;
            if tight && un > ln {
                let frac = (numero - ln) as f64 / (un - ln) as f64;
                // Interpolation does not represent either neighbor. Its postcode is known only
                // when both bracketing source addresses agree on one non-empty value.
                let postcode_off = if lpc != 0 && lpc == upc { lpc } else { 0 };
                return Some((
                    la + frac * (ua - la),
                    lo + frac * (uo - lo),
                    3,
                    numero,
                    rep,
                    postcode_off,
                ));
            }
            // wide bracket — nearest-by-number neighbor
            return Some(if numero - ln <= un - numero {
                (la, lo, 0, ln, lrid, lpc)
            } else {
                (ua, uo, 0, un, urid, upc)
            });
        }
        // only one neighbor on the same street
        match (lower, upper) {
            (Some((n, rid, la, lo, postcode_off)), None)
            | (None, Some((n, rid, la, lo, postcode_off))) => {
                Some((la, lo, 0, n, rid, postcode_off))
            }
            _ => None,
        }
    }

    fn house_number(&self, number: u32, rep: u32) -> String {
        let suffix = self
            .rep_suffixes
            .get(rep as usize)
            .map_or("", String::as_str);
        format!("{number}{suffix}")
    }

    /// Suffixes safe for greedy parsing (the rep dictionary contains junk like
    /// "rue"/"route" — digit-free words must not be consumed greedily).
    fn is_safe_rep(t: &str) -> bool {
        matches!(t, "bis" | "ter" | "quater" | "quinquies" | "sexies")
            || (t.chars().count() == 1 && t.chars().all(|c| c.is_alphabetic()))
    }

    fn add_cand(cand: &mut HashMap<u32, Feats>, sid: u32, f: Feats) {
        cand.entry(sid).or_default().merge(f);
    }

    /// Candidate collection: scanning the "street | commune" boundary from the end.
    fn collect_candidates(
        &self,
        rest: &[&str],
        postcode: Option<u32>,
        from_ml: bool,
    ) -> HashMap<u32, Feats> {
        let mut cand: HashMap<u32, Feats> = HashMap::new();
        if rest.is_empty() {
            return cand;
        }
        // French commune names can run to 8 words ("Saint-Remy-en-Bouzemont-...")
        let max_c = rest.len().saturating_sub(1).min(9);
        for c in 0..=max_c {
            let street_phrase = rest[..rest.len() - c].join(" ");
            // phrase variants: as is + expanded abbreviations + rotated type word
            let mut phrases = vec![street_phrase];
            if let Some(exp) = expand_first(&phrases[0]) {
                phrases.push(exp);
            }
            if let Some(exp) = expand_last(&phrases[0]) {
                phrases.push(exp); // trailing type abbreviation
            }
            if let Some(nd) = num_words_to_digits(&phrases[0]) {
                phrases.push(nd); // spelled-out numerals -> digits ("Douze Mai" -> "12 Mai")
            }
            if !crate::norm::has_cyrillic(&phrases[0]) {
                if let Some(gen) = serbian_genitive_variant(&phrases[0]) {
                    phrases.push(gen); // Serbian genitive ("Knez Mihailova" -> "Kneza Mihaila")
                }
            }
            for i in 0..phrases.len().min(2) {
                if let Some(rot) = rotate_type_first(&phrases[i]) {
                    phrases.push(rot);
                }
            }
            if c == 0 {
                // the whole string is the street; prefix search across all communes
                for phrase in &phrases {
                    let mut lo = phrase.clone().into_bytes();
                    lo.push(KEY_SEP);
                    let mut hi = phrase.clone().into_bytes();
                    hi.push(KEY_SEP + 1);
                    let mut stream = self.streets_fst.range().ge(&lo).lt(&hi).into_stream();
                    let mut taken = 0;
                    while let Some((_, v)) = stream.next() {
                        let sid = v as u32;
                        let m = self.street_meta(sid);
                        let mut f = Feats {
                            street_exact: true,
                            from_ml,
                            ..Default::default()
                        };
                        if let Some(pc) = postcode {
                            // postcode==0 = "absent from the data" — do not filter these,
                            // or a query WITH a postcode would go empty where the same
                            // query without one succeeds
                            if m.postcode != 0 && m.postcode / 1000 != pc / 1000 {
                                continue; // different part of the country — skip
                            }
                            if m.postcode != 0 {
                                f.pc_dept = true;
                                f.pc_exact = m.postcode == pc;
                            }
                        }
                        Self::add_cand(&mut cand, sid, f);
                        taken += 1;
                        if taken >= 300 {
                            break;
                        }
                    }
                }
            } else {
                let commune_phrase = rest[rest.len() - c..].join(" ");
                let mut exact_commune = true;
                let mut cids = self.communes_by_name(&commune_phrase);
                if cids.is_empty() {
                    cids = self.communes_by_prefix(&commune_phrase);
                    exact_commune = false;
                }
                // bare name without a street type: pad with type-word variants
                let mut ph = phrases.clone();
                ph.extend(type_padded_variants(&phrases[0]));
                // last-two-words swap and unglue variants — cheap extra keys (one FST read each)
                for extra in phrases
                    .iter()
                    .filter_map(|f| swap_last_two_variant(f))
                    .chain(phrases.iter().filter_map(|f| unglue_variant(f)))
                    .collect::<Vec<_>>()
                {
                    if !ph.contains(&extra) {
                        ph.push(extra);
                    }
                }
                for cid in cids {
                    let insee = self.commune_insee(cid);
                    for phrase in &ph {
                        let mut key = phrase.clone().into_bytes();
                        key.push(KEY_SEP);
                        key.extend_from_slice(insee.as_bytes());
                        if let Some(v) = self.streets_fst.get(&key) {
                            let sid = v as u32;
                            let m = self.street_meta(sid);
                            let f = Feats {
                                street_exact: true,
                                commune_exact: exact_commune,
                                commune_prefix: !exact_commune,
                                pc_exact: postcode
                                    .is_some_and(|pc| m.postcode != 0 && m.postcode == pc),
                                pc_dept: postcode.is_some_and(|pc| {
                                    m.postcode != 0 && m.postcode / 1000 == pc / 1000
                                }),
                                from_ml,
                                ..Default::default()
                            };
                            Self::add_cand(&mut cand, sid, f);
                        }
                    }
                }
            }
        }
        cand
    }

    /// street_ids whose normalized name contains the word (inverted index).
    fn word_streets(&self, word: &str) -> Vec<u32> {
        match self.words_fst.get(word.as_bytes()) {
            None => Vec::new(),
            Some(off) => {
                let mut p = off as usize;
                // offset and each varint must stay inside the section: FST values are
                // file data, and running off the end would panic the host
                if p >= self.word_postings.len() {
                    return Vec::new();
                }
                let n = read_varint(self.word_postings, &mut p) as usize;
                let mut ids = Vec::with_capacity(n.min(1 << 20));
                let mut prev = 0u32;
                for _ in 0..n {
                    if p >= self.word_postings.len() {
                        break; // truncated postings: return what decoded cleanly
                    }
                    prev += read_varint(self.word_postings, &mut p) as u32;
                    ids.push(prev);
                }
                ids
            }
        }
    }

    /// "Street-word subset" fallback pass: when exact search is empty and the query holds
    /// only PART of the street's words (suffix/middle) — e.g. "amir temur" -> "Amir Temur
    /// shoh". Takes the significant street words from the query, pulls street_ids for each
    /// via the inverted index, intersects them and filters by commune. Only with a known
    /// commune (c>0) — country-wide it is far too diffuse.
    fn collect_subset(&self, rest: &[&str], postcode: Option<u32>) -> HashMap<u32, Feats> {
        let mut cand: HashMap<u32, Feats> = HashMap::new();
        if rest.len() < 2 {
            return cand;
        }
        let max_c = rest.len().saturating_sub(1).min(9);
        for c in 1..=max_c {
            let commune_phrase = rest[rest.len() - c..].join(" ");
            let mut exact_commune = true;
            let mut cids = self.communes_by_name(&commune_phrase);
            if cids.is_empty() {
                cids = self.communes_by_prefix(&commune_phrase);
                exact_commune = false;
            }
            if cids.is_empty() {
                continue;
            }
            let cidset: std::collections::HashSet<u32> = cids.into_iter().collect();

            let (n_sets, _nontype, acc) = match self.subset_intersect(&rest[..rest.len() - c]) {
                Some(v) => v,
                None => continue, // no significant words / matched only via a street type
            };
            // commune filter
            let mut sids: Vec<u32> = acc
                .into_iter()
                .filter(|&sid| cidset.contains(&self.street_meta(sid).commune_id))
                .collect();
            // Hallucination guard: a SINGLE word matching MANY streets in the commune is a
            // type/frequent word or a fragment of one (normalization splits "ko'chasi" ->
            // "ko chasi", and "chasi" matches every street), not a distinguishing name. A
            // real name matches only a handful. In that case a confident house answer is
            // not allowed.
            const SINGLE_MAX: usize = 8;
            if n_sets == 1 && sids.len() > SINGLE_MAX {
                continue;
            }
            sids.sort_unstable();
            sids.truncate(200);
            for sid in sids {
                let m = self.street_meta(sid);
                let f = Feats {
                    street_fuzzy: true, // word subset is not an exact string match
                    commune_exact: exact_commune,
                    commune_prefix: !exact_commune,
                    pc_exact: postcode.is_some_and(|pc| m.postcode != 0 && m.postcode == pc),
                    pc_dept: postcode
                        .is_some_and(|pc| m.postcode != 0 && m.postcode / 1000 == pc / 1000),
                    ..Default::default()
                };
                Self::add_cand(&mut cand, sid, f);
            }
        }
        cand
    }

    /// Intersect street_ids over the phrase's significant words (>=3 chars, not a type)
    /// via the inverted index. Returns (set count, non-type set count, intersection); None
    /// if no significant word has postings or only street-type words matched. Words at the
    /// postings cap (types) are skipped; the rarest word seeds the result, and a word that
    /// would zero the intersection is skipped (a variant spelling must not kill the answer).
    fn subset_intersect(
        &self,
        toks: &[&str],
    ) -> Option<(usize, usize, std::collections::HashSet<u32>)> {
        const CAP: usize = 16384;
        let mut sets: Vec<Vec<u32>> = Vec::new();
        let mut nontype = 0usize;
        for tok in toks {
            let is_sep = |ch: char| {
                !ch.is_alphanumeric()
                    && !matches!(
                        ch,
                        '\'' | '\u{2019}' | '\u{2018}' | '\u{02BB}' | '\u{02BC}' | '`'
                    )
            };
            for w0 in tok.split(is_sep) {
                let w = normalize(w0);
                if w.chars().count() < 3 {
                    continue;
                }
                let w = w.as_str();
                let mut tried = vec![w.to_string()];
                let cl = normalize(&crate::norm::translit_cyr_lat(w));
                if cl != w && cl.chars().count() >= 3 {
                    tried.push(cl);
                }
                let lc = normalize(&crate::norm::translit_lat_cyr(w));
                if lc != w && lc.chars().count() >= 3 {
                    tried.push(lc);
                }
                let is_type = tried.iter().any(|t| is_street_type_word(t));
                // phonetic key: cross-language spelling variants (e.g. Kadyri ~ Qodiriy)
                // share a "~"-prefixed key in the inverted index. Extra key, after the type check.
                let pk = crate::norm::phonetic_key(w);
                if pk.chars().count() >= 3 {
                    tried.push(format!("~{pk}"));
                }
                let mut best: Option<Vec<u32>> = None;
                for t in &tried {
                    let ids = self.word_streets(t);
                    if ids.is_empty() || ids.len() >= CAP {
                        continue;
                    }
                    if best.as_ref().is_none_or(|b| ids.len() < b.len()) {
                        best = Some(ids);
                    }
                }
                if let Some(ids) = best {
                    if !is_type {
                        nontype += 1;
                    }
                    sets.push(ids);
                }
            }
        }
        if sets.is_empty() || nontype == 0 {
            return None;
        }
        sets.sort_by_key(|s| s.len());
        let mut acc: std::collections::HashSet<u32> = sets[0].iter().copied().collect();
        for s in &sets[1..] {
            let other: std::collections::HashSet<u32> = s.iter().copied().collect();
            let inter: std::collections::HashSet<u32> = acc.intersection(&other).copied().collect();
            if !inter.is_empty() {
                acc = inter;
            }
        }
        Some((sets.len(), nontype, acc))
    }

    /// Single-edit variants over CHARACTERS (works for any script — byte-level edits would
    /// break multi-byte scripts): deletions, adjacent transpositions, and optionally
    /// full-alphabet replacements and insertions.
    fn edit1_variants(phrase: &str, with_repl: bool) -> Vec<String> {
        let chars: Vec<char> = phrase.chars().collect();
        let n = chars.len();
        let mut out: Vec<String> = Vec::new();
        // deletions
        for i in 0..n {
            if chars[i] == ' ' {
                continue;
            }
            let mut v = String::with_capacity(phrase.len());
            v.extend(chars[..i].iter());
            v.extend(chars[i + 1..].iter());
            out.push(v);
        }
        // adjacent transpositions
        for i in 0..n.saturating_sub(1) {
            if chars[i] == ' ' || chars[i + 1] == ' ' || chars[i] == chars[i + 1] {
                continue;
            }
            let mut v = chars.clone();
            v.swap(i, i + 1);
            out.push(v.into_iter().collect());
        }
        if with_repl && n <= 26 {
            // full alphabet of the phrase's script: replacements AND insertions
            // (a replacement typo is fixed by replacement, a deletion by insertion);
            // too expensive on long phrases — capped at 26 characters
            let cyr = chars.iter().any(|c| ('а'..='я').contains(c) || *c == 'ё');
            let alphabet: &str = if cyr {
                "абвгдеёжзийклмнопрстуфхцчшщъыьэюя"
            } else {
                "abcdefghijklmnopqrstuvwxyz"
            };
            // replacements
            for i in 0..n {
                if chars[i] == ' ' {
                    continue;
                }
                for a in alphabet.chars() {
                    if a == chars[i] {
                        continue;
                    }
                    let mut v = chars.clone();
                    v[i] = a;
                    out.push(v.into_iter().collect());
                }
            }
            // insertions (including the position after the last character)
            for i in 0..=n {
                if i > 0 && i < n && chars[i - 1] == ' ' && chars[i] == ' ' {
                    continue;
                }
                for a in alphabet.chars() {
                    let mut v: Vec<char> = Vec::with_capacity(n + 1);
                    v.extend_from_slice(&chars[..i]);
                    v.push(a);
                    v.extend_from_slice(&chars[i..]);
                    out.push(v.into_iter().collect());
                }
            }
        }
        out
    }

    /// Fuzzy candidate collection: single-edit variants via exact keys
    /// + a Levenshtein automaton (1 edit for 5-11 chars, 2 for 12+).
    fn collect_fuzzy(
        &self,
        rest: &[&str],
        postcode: Option<u32>,
        from_ml: bool,
    ) -> HashMap<u32, Feats> {
        let mut cand: HashMap<u32, Feats> = HashMap::new();
        if rest.is_empty() {
            return cand;
        }
        let max_c = rest.len().saturating_sub(1).min(9);
        for c in 0..=max_c {
            let street_phrase = rest[..rest.len() - c].join(" ");
            let dist = match street_phrase.chars().count() {
                0..=4 => continue,
                5..=11 => 1,
                _ => 2,
            };
            // allowed communes, if the boundary carved them out
            let mut exact_commune = true;
            let allowed: Option<Vec<String>> = if c > 0 {
                let phrase = rest[rest.len() - c..].join(" ");
                let mut ids = self.communes_by_name(&phrase);
                if ids.is_empty() {
                    ids = self.communes_by_prefix(&phrase);
                    exact_commune = false;
                }
                if ids.is_empty() {
                    continue;
                }
                Some(
                    ids.iter()
                        .map(|&id| self.commune_insee(id).to_string())
                        .collect(),
                )
            } else {
                None
            };
            let mk_feats = |m: &StreetMeta| Feats {
                street_fuzzy: true,
                commune_exact: allowed.is_some() && exact_commune,
                commune_prefix: allowed.is_some() && !exact_commune,
                pc_exact: postcode.is_some_and(|pc| m.postcode != 0 && m.postcode == pc),
                pc_dept: postcode
                    .is_some_and(|pc| m.postcode != 0 && m.postcode / 1000 == pc / 1000),
                from_ml,
                ..Default::default()
            };

            // 1) single-edit variants — cheap, via exact keys; replacements
            // only when communes are known (few keys to try)
            for var in Self::edit1_variants(&street_phrase, allowed.is_some()) {
                match &allowed {
                    Some(allow) => {
                        for insee in allow {
                            let mut key = var.clone().into_bytes();
                            key.push(KEY_SEP);
                            key.extend_from_slice(insee.as_bytes());
                            if let Some(v) = self.streets_fst.get(&key) {
                                let sid = v as u32;
                                let m = self.street_meta(sid);
                                Self::add_cand(&mut cand, sid, mk_feats(&m));
                            }
                        }
                    }
                    None => {
                        let mut lo = var.clone().into_bytes();
                        lo.push(KEY_SEP);
                        let mut hi = var.clone().into_bytes();
                        hi.push(KEY_SEP + 1);
                        let mut stream = self.streets_fst.range().ge(&lo).lt(&hi).into_stream();
                        let mut taken = 0;
                        while let Some((_, v)) = stream.next() {
                            let sid = v as u32;
                            let m = self.street_meta(sid);
                            if let Some(pc) = postcode {
                                // postcode==0 in data is not a filter (see collect_candidates)
                                if m.postcode != 0 && m.postcode / 1000 != pc / 1000 {
                                    continue;
                                }
                            }
                            Self::add_cand(&mut cand, sid, mk_feats(&m));
                            taken += 1;
                            if taken >= 50 {
                                break;
                            }
                        }
                    }
                }
            }

            // 2) Levenshtein automaton — extra recall for pure ASCII (on Unicode the
            // automaton blows up in states and silently gives up)
            if !cand.is_empty() {
                if c > 0 {
                    break;
                }
                continue;
            }
            if !street_phrase.is_ascii() {
                continue;
            }
            // Content-word gate: a phrase-level automaton matches "rue de la gare" ->
            // "rue de la gagnerie..." through function words — a confident house on a
            // DISSIMILAR street. Require at least one content word of the query (not a
            // type/article, >=3 chars) to match a candidate name word within 1 edit.
            // Declared BEFORE the `lev` automaton binding below, which shadows the
            // distance function of the same name.
            let q_content: Vec<Vec<char>> = street_phrase
                .split(' ')
                .filter(|w| w.chars().count() >= 3 && !is_affix_word(w))
                .map(|w| w.chars().collect())
                .collect();
            let word_covered = |kname: &str| -> bool {
                kname.split(' ').any(|kw| {
                    let kc: Vec<char> = kw.chars().collect();
                    q_content.iter().any(|qc| lev(&kc, qc) <= 1)
                })
            };
            let lev = match Levenshtein::new(&street_phrase, dist) {
                Ok(l) => l,
                Err(_) => continue,
            };
            let mut stream = self.streets_fst.search(lev.starts_with()).into_stream();
            let mut taken = 0;
            while let Some((key, v)) = stream.next() {
                let sep = match key.iter().position(|&b| b == KEY_SEP) {
                    Some(p) => p,
                    None => continue,
                };
                if !q_content.is_empty()
                    && !word_covered(std::str::from_utf8(&key[..sep]).unwrap_or(""))
                {
                    continue;
                }
                let insee = std::str::from_utf8(&key[sep + 1..]).unwrap_or("");
                if let Some(ref allow) = allowed {
                    if !allow.iter().any(|a| a == insee) {
                        continue;
                    }
                }
                let sid = v as u32;
                let m = self.street_meta(sid);
                if let Some(pc) = postcode {
                    // postcode==0 in data is not a filter (see collect_candidates)
                    if allowed.is_none() && m.postcode != 0 && m.postcode / 1000 != pc / 1000 {
                        continue;
                    }
                }
                Self::add_cand(&mut cand, sid, mk_feats(&m));
                taken += 1;
                if taken >= 400 {
                    break;
                }
            }
            if !cand.is_empty() && c > 0 {
                break; // found with a known commune — good enough
            }
        }
        cand
    }

    /// Hypothesis from digit token g0; greedy = consume the suffix too (up to 3 short
    /// tokens glued without spaces, longest dictionary match wins).
    fn build_hyp(&self, toks: &[&str], used: &[bool], g0: usize, greedy: bool) -> Hyp {
        let numero: Option<u32> = toks[g0].parse().ok();
        let mut consumed = vec![g0];
        let mut rep: u32 = 0;
        if greedy {
            let mut s = String::new();
            let mut extra: Vec<usize> = Vec::new();
            let mut best: Option<(u32, usize)> = None;
            let mut j = g0 + 1;
            while j < toks.len() && !used[j] && extra.len() < 3 {
                let t = toks[j];
                let ok = !t.is_empty()
                    && t.chars().count() <= 4
                    && t.chars().all(|c| c.is_alphanumeric())
                    && (t.chars().any(|c| c.is_ascii_digit()) || Self::is_safe_rep(t));
                if !ok {
                    break;
                }
                s.push_str(t);
                extra.push(j);
                if let Some(&rid) = self.rep_lookup.get(&s) {
                    best = Some((rid, extra.len()));
                }
                j += 1;
            }
            if let Some((rid, cnt)) = best {
                rep = rid;
                consumed.extend_from_slice(&extra[..cnt]);
            }
        }
        let rest_idx = (0..toks.len())
            .filter(|i| !used[*i] && !consumed.contains(i))
            .collect();
        Hyp {
            numero,
            rep,
            rest_idx,
            from_ml: false,
        }
    }

    /// Hypothesis from the parsing model: token labels -> ready segmentation
    /// (street and commune taken from labels, in street-then-commune order).
    fn ml_hyp(&self, toks: &[&str], used: &[bool]) -> Option<Hyp> {
        let parser = self.parser.as_ref()?;
        let labels = parser.label(toks);
        let mut street_idx: Vec<usize> = Vec::new();
        let mut city_idx: Vec<usize> = Vec::new();
        let mut numero: Option<u32> = None;
        let mut rep_parts: Vec<&str> = Vec::new();
        for i in 0..toks.len() {
            if used[i] {
                continue; // postcode already consumed
            }
            match labels[i] {
                crate::ml::L_STREET => street_idx.push(i),
                crate::ml::L_CITY => city_idx.push(i),
                crate::ml::L_NUM => {
                    let d = toks[i].bytes().take_while(|b| b.is_ascii_digit()).count();
                    if numero.is_none() && d >= 1 {
                        numero = toks[i][..d].parse().ok();
                        if d < toks[i].len() {
                            rep_parts.push(&toks[i][d..]); // fused "12a"
                        }
                    } else {
                        // no leading digits (model misfired on an unfamiliar language)
                        // or a number already found — the token is more likely street
                        street_idx.push(i);
                    }
                }
                crate::ml::L_REP => rep_parts.push(toks[i]),
                crate::ml::L_PC => {} // postcode already parsed by the heuristic
                _ => street_idx.push(i),
            }
        }
        if street_idx.is_empty() {
            return None;
        }
        let rep = if rep_parts.is_empty() {
            0
        } else {
            *self.rep_lookup.get(&rep_parts.concat()).unwrap_or(&0)
        };
        let mut rest_idx = street_idx;
        rest_idx.extend(city_idx);
        Some(Hyp {
            numero,
            rep,
            rest_idx,
            from_ml: true,
        })
    }

    /// City aliases — ONLY if the target city exists in this index
    /// (otherwise "2805 BG Gouda" would turn into "... beograd gouda").
    fn expand_city_aliases(&self, q: &str) -> String {
        let mut s = q.to_string();
        for (alias, full) in &crate::rules::rules().city_alias {
            let inside = format!(" {alias} ");
            let tail = format!(" {alias}");
            if !(s == *alias || s.ends_with(&tail) || s.contains(&inside)) {
                continue;
            }
            if self.communes_fst.get(full.as_bytes()).is_none() {
                continue;
            }
            if s == *alias {
                s = (*full).to_string();
            } else if let Some(rest) = s.strip_suffix(&tail) {
                s = format!("{rest} {full}");
            } else {
                s = s.replace(&inside, &format!(" {full} "));
            }
        }
        s
    }

    pub fn query_feats(&self, raw: &str, k: usize) -> Vec<(Hit, [f32; N_FEATS])> {
        if k == 0 {
            return Vec::new(); // k=0 asks for zero results; the city-only fallback used to ignore it
        }
        let k = bound_k(k); // cap result count before it drives allocation/sort
        let raw = bound_query(raw); // cap work before normalization
        let _rules = crate::rules::scope(self.rules); // this file's tables, not another file's
        let mut hits = self.query_feats_d(raw, k, 0, None);
        Self::monotone_confidence(&mut hits);
        hits
    }

    fn query_feats_near(
        &self,
        raw: &str,
        k: usize,
        lat: f64,
        lon: f64,
    ) -> std::result::Result<Vec<(Hit, [f32; N_FEATS])>, String> {
        validate_query_near(lat, lon)?;
        if k == 0 {
            return Ok(Vec::new());
        }
        let k = bound_k(k);
        let raw = bound_query(raw);
        let _rules = crate::rules::scope(self.rules);
        let focus = QueryFocus {
            lat,
            lon,
            streets: self.streets_around(lat, lon),
        };
        let mut hits = self.query_feats_d(raw, k, 0, Some(&focus));
        Self::monotone_confidence(&mut hits);
        Ok(hits)
    }

    /// Confidence is MONOTONE by rank: a lower-ranked answer cannot look "more confident"
    /// than the one above it. The margin cutoff (and similar mechanics) lowers ONLY the
    /// top-1 (ambiguous_far); without this clamp a consumer would see the contradiction
    /// "top-1 0.2, top-2 0.6" and distrust the order. Ranking is untouched — only the
    /// visible confidence is leveled down the list.
    fn monotone_confidence(hits: &mut [(Hit, [f32; N_FEATS])]) {
        let mut cap = f32::MAX;
        for (h, _) in hits.iter_mut() {
            if h.confidence > cap {
                h.confidence = cap;
            }
            cap = h.confidence;
        }
    }

    fn query_feats_d(
        &self,
        raw: &str,
        k: usize,
        depth: u8,
        focus: Option<&QueryFocus>,
    ) -> Vec<(Hit, [f32; N_FEATS])> {
        let q = self.expand_city_aliases(&expand_two_token(&fold_units(
            &crate::norm::fold_homoglyphs(&normalize(raw)),
        )));
        // French arrondissements: context-gated rewrite of "3eme"/Roman/order forms to the canon
        let q = fr_arrondissement_rewrite(&q).unwrap_or(q);
        // phone-number runs: cut before parsing, else digit pairs become "houses"
        let q = strip_phone_runs(&q).unwrap_or(q);
        // In Paris/Lyon/Marseille the postcode itself identifies an arrondissement. Handle
        // postcode-only and postcode+area queries before the address parser can mistake the
        // five-digit code for a house number. Contradictory area hints fail closed.
        if self.country() == Some("fr") {
            match fr_arrondissement_postcode_area(&q) {
                Some(FrPostcodeArea::Match(place)) => {
                    return self
                        .resolve_place_translit(&place)
                        .map(|(lat, lon, commune, _)| Self::city_hit(lat, lon, commune))
                        .unwrap_or_default();
                }
                Some(FrPostcodeArea::Conflict) => return Vec::new(),
                None => {}
            }
        }
        // city-only: the whole string (minus digits) EXACTLY matches a commune name -> the
        // city point (precision "city"). Otherwise the engine fuzzes a bare city into a
        // same-named street tens of km away ("Eindhoven" -> "Eindhovenlaan,
        // s-Hertogenbosch"). Among homonyms, the most prominent one wins.
        let city_phrase: String = q
            .split(' ')
            .filter(|t| !t.is_empty() && !t.bytes().all(|b| b.is_ascii_digit()))
            .collect::<Vec<_>>()
            .join(" ");
        // with a DIGIT in the query this shortcut would swallow the house number whenever a
        // street shares its name with a commune. With a digit the street path goes first;
        // the city fallback (d) at the end still provides city-level coverage.
        let has_digit_tok = q
            .split(' ')
            .any(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()));
        if !city_phrase.is_empty() && !has_digit_tok {
            // Universal bare-place-name resolution -> a point (the center). Works for ANY
            // country without lists: city, district, estate, arrondissement — each is
            // either a commune or a prefix commune group. Transliteration both ways: a
            // Cyrillic query finds Latin-script communes and vice versa.
            if let Some((lat, lon, commune, _)) = self.resolve_place_translit(&city_phrase) {
                return Self::city_hit(lat, lon, commune);
            }
        }
        let fr_area_signal = if self.country() == Some("fr") {
            fr_arrondissement_constraint(&q)
        } else {
            None
        };
        if fr_area_signal == Some(FrPostcodeArea::Conflict) {
            return Vec::new();
        }
        let fr_area_constraint = match fr_area_signal {
            Some(FrPostcodeArea::Match(area)) => Some(area),
            _ => None,
        };
        // The unconstrained winner can sit in another arrondissement while a lower-ranked,
        // valid candidate exists in the requested one (`1 rue de rivoli 75001 paris`).
        // Pull a bounded candidate window before enforcing the hard postcondition, then restore
        // the caller's k. Without this, k=1 becomes an accidental false-empty.
        let prepared_k = if fr_area_constraint.is_some() {
            k.max(20)
        } else {
            k
        };
        let mut hits = self.query_feats_prepared(&q, prepared_k, focus);
        if let Some(expected) = fr_area_constraint.as_deref() {
            hits.retain(|(hit, _)| normalize(&hit.commune) == expected);
            hits.truncate(k);
        }
        // A suffix immediately after a distant Italian homonym may be a geographic qualifier,
        // not another street/city token. The ordinary full-query interpretation has precedence:
        // only retry when it could not produce any exact-street + exact-commune candidate. This
        // preserves addresses whose street happens to end in a homonymous place word, such as
        // `Vicolo del Ponte, Macerata`.
        let primary_has_exact_address = hits
            .iter()
            .any(|(_, features)| features[0] > 0.5 && features[2] > 0.5);
        if depth == 0 && self.country() == Some("it") && !primary_has_exact_address {
            if let Some(qualified) = self.trailing_homonym_qualifier_retry(&q, k, focus) {
                return qualified;
            }
        }
        if !hits.is_empty() {
            // "city first + trailing number": see city_first_house_retry
            if depth == 0 && hits[0].0.precision == "street" {
                if let Some(h2) = self.city_first_house_retry(&q, k, focus) {
                    return h2;
                }
            }
            return hits;
        }
        if fr_area_constraint.is_some() {
            return Vec::new();
        }
        // unglue "c5" -> "c 5" on an EMPTY result: preprocessing glues hyphenated codes
        // ("c-5" -> "c5") while the index (normalization splits hyphens) stores "c 5", so
        // the exact key is unreachable ("kiet c-5", "Labzak"). Retry the unglued form.
        if let Some(u) = unglue_variant(&q) {
            let uq = fold_units(&u);
            let h = self.query_feats_prepared(&uq, k, focus);
            if !h.is_empty() {
                // unglued form yielded only a street with a leading city — same refinement
                if h[0].0.precision == "street" {
                    if let Some(h2) = self.city_first_house_retry(&uq, k, focus) {
                        return h2;
                    }
                }
                return h;
            }
        }
        // cross-script fallback passes on an empty result:
        // (a) Cyrillic -> Latin, Serbian Gaj mapping — for Serbian Latin-script data
        if crate::norm::has_cyrillic(&q) {
            let q2 = normalize(&crate::norm::translit_cyr_lat(&q));
            if q2 != q {
                let h = self.query_feats_prepared(&q2, k, focus);
                if !h.is_empty() {
                    return h;
                }
            }
            // (a2) Cyrillic -> Latin with ENGLISH digraphs — for Uzbek Latin-script data
            // ("farobiy ko'chasi"); tried when the Serbian mapping (a) found nothing
            let q2e = normalize(&crate::norm::translit_cyr_lat_en(&q));
            if q2e != q && q2e != normalize(&crate::norm::translit_cyr_lat(&q)) {
                let h = self.query_feats_prepared(&q2e, k, focus);
                if !h.is_empty() {
                    return h;
                }
            }
        }
        // (b) Latin -> Cyrillic (for Cyrillic-script data):
        // "bratsk mira 60" — a Latin query against Cyrillic street names
        if crate::norm::has_latin(&q) {
            let q3 = normalize(&crate::norm::translit_lat_cyr(&q));
            if q3 != q {
                let h = self.query_feats_prepared(&q3, k, focus);
                if !h.is_empty() {
                    return h;
                }
                // transliteration + unglue: "kiet c-5" -> fused Cyrillic form -> spaced form
                // (the commune itself is stored in Cyrillic with a spaced block code)
                if let Some(u) = unglue_variant(&q3) {
                    let h = self.query_feats_prepared(&u, k, focus);
                    if !h.is_empty() {
                        return h;
                    }
                }
            }
        }
        // (b2) a single "j" is ambiguous between two Cyrillic letters. French-style name
        // spellings use "j" for the "zh" sound ("Lejena"). Try the zh-variant (j -> zh)
        // when the primary mapping (j -> y) is empty. Only affects words containing "j".
        if q.contains('j') {
            let q3b = normalize(&crate::norm::translit_lat_cyr(&q.replace('j', "zh")));
            if q3b != q {
                let h = self.query_feats_prepared(&q3b, k, focus);
                if !h.is_empty() {
                    return h;
                }
            }
        }
        // (s) Serbian orthographic digraph dj -> d: normalization folds đ -> d (the index
        // stores "karadordeva", see norm.rs), so bare "d" and the đ form match directly.
        // That leaves the "dj" digraph ("Karadjordjeva", "Djusina") — on an empty primary
        // path try dj -> d. Cheap, and only when "dj" is present.
        if q.contains("dj") {
            let swapped = q.replace("dj", "d");
            if swapped != q {
                let h = self.query_feats_prepared(&swapped, k, focus);
                if !h.is_empty() {
                    return h;
                }
            }
        }
        // (c) junk before the address: drop leading words ("c/o Rossi, Corso Italia 10",
        // "maps via ...", a venue name up front). Accepted on a house match; OR when the
        // dropped prefix is a REAL commune / umbrella city: settlements under an umbrella
        // live under their OWN commune name, and the umbrella prefix merely blocks the
        // commune match — then a street-level result is accepted too. Fires only when the
        // primary (commune-aware) path is ALREADY empty, so homonyms thousands of km away
        // never reach here (their primary path is non-empty).
        let toks: Vec<&str> = q.split(' ').filter(|t| !t.is_empty()).collect();
        // (c0) LEADING TYPE WORD ("street X 12", "estate Y 6"): strip up to 3 affixes
        // (place type/prefix) from the start and run the FULL ladder on the remainder
        // (including transliteration, which the junk drop below lacks). A bare name
        // resolves correctly, while a leading type shifts the name from the string start
        // so prefix matching fails. Affixes only — distinguishing names are never
        // stripped. One level of recursion (depth>0 does not repeat this).
        if depth == 0 {
            let mut d = 0;
            while d + 1 < toks.len() && d < 3 && is_affix_word(toks[d]) {
                d += 1;
            }
            if d > 0 {
                let h = self.query_feats_d(&toks[d..].join(" "), k, depth + 1, focus);
                if !h.is_empty() {
                    return h;
                }
            }
        }
        // up to 8 leading words: a leading VENUE NAME can be long ("Coupole de l'Institut
        // de France, 23 Quai de Conti..." is 6 junk tokens; the apostrophe adds an extra
        // "l" token). The pass is house-gated, so the wide window is safe: accepted only
        // if an EXACT house resolves after the drop.
        let maxd = 8.min(toks.len().saturating_sub(2));
        for drop in 1..=maxd {
            let h = self.query_feats_prepared(&toks[drop..].join(" "), k, focus);
            if h.is_empty() {
                continue;
            }
            // "city as street" guard: after the drop a remainder like "109 amsterdam" gets
            // FUZZY-matched as a street ("Amsterdam" -> "Amsterdamseweg" 90 km away, a
            // false house). Accept a house ONLY from an EXACT street (feats[0]=
            // street_exact), not fuzzy. Real streets behind junk still pass (their street
            // matches exactly), while fuzzy city-as-street is cut.
            // feats: [street_exact, street_fuzzy, ...].
            let house_exact = h
                .iter()
                .any(|(hit, f)| hit.precision == "house" && f[0] > 0.5);
            // a numberless query with leading noise ("Hotel rue de Rivoli Paris") would die
            // entirely under a house-only gate. If a street type (rue/via/...) follows the
            // drop and the match is exact, accept street level too (conf cap/flag below).
            let street_after_type = is_street_type_word(toks[drop])
                && h.iter()
                    .any(|(hit, f)| f[0] > 0.5 && hit.precision != "city");
            let dropped_is_commune = !self.communes_by_name(&toks[..drop].join(" ")).is_empty();
            if house_exact || dropped_is_commune || street_after_type {
                // the drop is not silent: "Jan van Harenstraat" -> "Van Harenstraat" must
                // not come back looking perfect. Flag it and cap confidence; dropping a
                // REAL umbrella commune is semantically clean and is not penalized.
                if !dropped_is_commune {
                    let mut h = h;
                    for (hit, _) in h.iter_mut() {
                        hit.confidence = hit.confidence.min(0.6);
                        hit.flags.push("dropped_prefix");
                    }
                    return h;
                }
                return h;
            }
        }
        // (c2) SYMMETRIC to the front drop: an unrecognized TAIL after the commune
        // ("... Lyon Xyz", marketing suffixes). Drop 1-2 trailing tokens; same gate as the
        // front drop: accept ONLY an exact street (feats[0]=street_exact), never fuzzy —
        // and honestly flag/cap confidence. Digit tails are NOT touched (a trailing house
        // number is a legitimate form; dropping it would swap a house for a street).
        {
            let maxtd = 2.min(toks.len().saturating_sub(2));
            // A full commune at the tail is stronger evidence than an unrecognized suffix.
            // Inspect the whole drop window BEFORE trying a shorter drop: otherwise
            // "Via Falsa San Fratello" first removes only "fratello" and revives a
            // same-named street in an unrelated commune.
            if self.country() == Some("it")
                && (1..=maxtd).any(|tail_len| {
                    !self
                        .communes_by_name(&toks[toks.len() - tail_len..].join(" "))
                        .is_empty()
                })
            {
                return Vec::new();
            }
            for drop in 1..=maxtd {
                if toks[toks.len() - drop..]
                    .iter()
                    .any(|t| t.bytes().all(|b| b.is_ascii_digit()))
                {
                    break;
                }
                let h = self.query_feats_prepared(&toks[..toks.len() - drop].join(" "), k, focus);
                if h.is_empty() {
                    continue;
                }
                let exact_ok = h
                    .iter()
                    .any(|(hit, f)| f[0] > 0.5 && hit.precision != "city");
                if exact_ok {
                    let mut h = h;
                    for (hit, _) in h.iter_mut() {
                        hit.confidence = hit.confidence.min(0.6);
                        hit.flags.push("dropped_suffix");
                    }
                    return h;
                }
            }
        }
        // (d) LAST resort — degrade to a settlement. Everything failed but the string
        // holds a place name: a city/district trailing a listing, OR a settlement with a
        // house number but no streets. Take the last segments (including parenthesized
        // aliases), strip settlement type words and try each as a place name. City level
        // ONLY (resolve_place, no street search) — stray same-named streets never get
        // here; a non-address stays empty. Segments come from raw (q has no commas/
        // brackets left).
        // candidate segments: non-empty and digit-free (filtered BEFORE take, so an
        // umbrella city is not pushed out of the window by an empty bracket segment).
        // a LEADING postcode is trimmed off a segment ("2513 AA Den Haag" -> "Den Haag")
        // so that "postcode + city" still degrades to the city. Digits MID-segment still
        // exclude it (a street+number segment must not become a city).
        fn strip_leading_pc(s: &str) -> &str {
            let t = s.trim_start();
            let d = t.bytes().take_while(|b| b.is_ascii_digit()).count();
            if !(4..=6).contains(&d) {
                return s;
            }
            let mut rest = t[d..].trim_start();
            let letters = rest.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
            if letters == 2 && rest.as_bytes().get(2).is_none_or(|b| *b == b' ') {
                rest = rest[2..].trim_start(); // NL postcode letters ("AA")
            }
            if rest.is_empty() {
                s
            } else {
                rest
            }
        }
        let segs: Vec<&str> = raw
            .split([',', ';', '|', '·', '(', ')', '\n'])
            .map(|s| strip_leading_pc(s.trim()))
            .filter(|s| !s.is_empty() && !s.bytes().any(|b| b.is_ascii_digit()))
            .collect();
        let mut places: Vec<(f64, f64, String, u32)> = Vec::new();
        for seg in segs.iter().rev().take(6) {
            let segn = self
                .expand_city_aliases(&fold_units(&crate::norm::fold_homoglyphs(&normalize(seg))));
            let toks: Vec<&str> = segn.split(' ').filter(|w| !w.is_empty()).collect();
            if toks.is_empty() || toks.len() > 4 {
                continue;
            }
            // forms by decreasing specificity: full -> without junk suffixes -> without the type prefix
            let a: Vec<&str> = toks
                .iter()
                .copied()
                .filter(|w| !crate::rules::rules().place_junk.contains(*w))
                .collect();
            let b: Vec<&str> = a
                .iter()
                .copied()
                .filter(|w| !crate::rules::rules().place_prefix.contains(*w))
                .collect();
            for form in [toks.join(" "), a.join(" "), b.join(" ")] {
                if form.is_empty() {
                    continue;
                }
                if let Some(p) = self.resolve_place_translit(&form) {
                    places.push(p);
                    break; // first form of the segment that resolves
                }
            }
        }
        if let Some(anchor) = places.iter().max_by_key(|p| p.3).cloned() {
            // ANCHOR against false homonyms: among the resolved places the anchor is the
            // most prominent one (an umbrella city). The answer is the MOST specific place
            // (minimal prominence) within 50 km of the anchor; a distant homonym hundreds
            // of km away is filtered out.
            let best = places
                .iter()
                .filter(|p| Self::dist_km(p.0, p.1, anchor.0, anchor.1) <= 50.0)
                .min_by_key(|p| p.3)
                .unwrap_or(&anchor);
            // if the query has a HOUSE NUMBER it is an address and the user expects a
            // HOUSE; a rough settlement/city center kilometers away is a substitution, not
            // help — staying silent is more honest. Place degradation applies only to AREA
            // queries WITHOUT a number. This "has number -> stay silent" guard must NOT
            // choke "postcode + city" ("2513 AA Den Haag"): a 4-6-digit group counts as a
            // postcode, not a house number (house numbers longer than 3 digits are rare,
            // postcodes shorter than 4 do not exist).
            let has_number = q
                .split(' ')
                .any(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) && t.len() <= 3);
            if !has_number {
                return Self::city_hit(best.0, best.1, best.2.clone());
            }
        }
        hits
    }

    /// Build ONE candidate Hit (house/interp/near/street via find_house, score, name
    /// similarity, confidence, flags). Factored out of the main loop for reuse by
    /// structured input. Returns (Hit, features, name similarity, SNAP DELTA, commune
    /// prominence). Snap delta = |requested number - returned number|: on a split street a
    /// near-snap onto another fragment (house 290 for a query of 2205) loses the tie-break
    /// to the fragment with a nearby number; 0 for exact house/interp, MAX for street level.
    fn make_hit(
        &self,
        sid: u32,
        mut f: Feats,
        numero: Option<u32>,
        rep: u32,
        qwords: &[Vec<char>],
    ) -> (Hit, [f32; N_FEATS], i32, u32, u32) {
        let m = self.street_meta(sid);
        f.numero_present = numero.is_some();
        let mut snap_delta = 0u32;
        let (lat, lon, precision, housenumber, house_postcode_off) =
            match numero.and_then(|nm| self.find_house(sid, &m, nm, rep)) {
                Some((la, lo, 3, got, got_rep, postcode_off)) => {
                    // Interpolation returns the requested address, not either bracketing house.
                    (
                        la,
                        lo,
                        "interp",
                        Some(self.house_number(got, got_rep)),
                        postcode_off,
                    )
                }
                Some((la, lo, kind, got, got_rep, postcode_off)) if kind >= 1 => {
                    f.house_found = true;
                    f.house_exact_rep = kind == 2;
                    (
                        la,
                        lo,
                        "house",
                        Some(self.house_number(got, got_rep)),
                        postcode_off,
                    )
                }
                Some((la, lo, _, got, got_rep, postcode_off)) => {
                    snap_delta = numero.map_or(0, |nm| nm.abs_diff(got));
                    // A snap is the neighbour's address; never echo the number that missed.
                    (
                        la,
                        lo,
                        "near",
                        Some(self.house_number(got, got_rep)),
                        postcode_off,
                    )
                }
                None => {
                    if numero.is_some() {
                        snap_delta = u32::MAX; // a street with no neighbor at all is worse than any snap
                    }
                    (
                        m.lat_c as f64 / 1e7,
                        m.lon_c as f64 / 1e7,
                        "street",
                        None,
                        0,
                    )
                }
            };
        let mut score = match &self.rank {
            Some(r) => r.score(&f),
            None => f.legacy() as f32,
        };
        if f.house_found {
            score += 1.0; // a house beats a same-named street in another region
        }
        let name_norm = normalize(self.name(m.name_off));
        let mut name_sim = 0i32;
        for w in name_norm.split(' ').filter(|w| !w.is_empty()) {
            let wc: Vec<char> = w.chars().collect();
            for qw in qwords {
                let maxlen = wc.len().max(qw.len()).max(1);
                let sim = 100 - (lev(&wc, qw) * 100 / maxlen) as i32;
                if sim > name_sim {
                    name_sim = sim;
                }
            }
        }
        // confidence cannot stay high with a NEGATIVE ranking score: garbage like "name +
        // phone" could yield a confident house — the score is the primary quality signal
        let mut confidence = confidence_score(precision, &f, name_sim);
        if score < 0.0 {
            confidence = confidence.min(0.4);
        }
        (
            Hit {
                lat,
                lon,
                precision,
                score,
                confidence,
                street: self.name(m.name_off).to_string(),
                housenumber,
                commune: self.commune_name(m.commune_id).to_string(),
                postcode: self.postcode_for_house(&m, house_postcode_off),
                flags: match_flags(&f),
                region: self.admin_at(lat, lon), // WOF region in forward answers too
                distance_m: None,
            },
            f.to_vec(),
            name_sim,
            snap_delta,
            self.commune_prominence(m.commune_id),
        )
    }

    /// STRUCTURED INPUT: pre-parsed street/number/city/postcode fields bypass the
    /// segmentation heuristics (no "street|commune" boundary guessing). Resolve the commune
    /// from city directly, look the street up within it (exact + type padding + rotation,
    /// fuzzy when empty), find_house for the number. Isolated from free-form parsing, so
    /// the primary path is unaffected.
    pub fn query_structured(
        &self,
        street: &str,
        number: Option<&str>,
        city: &str,
        postcode: Option<&str>,
        k: usize,
    ) -> Vec<(Hit, [f32; N_FEATS])> {
        if k == 0 {
            return Vec::new(); // uniform k=0 contract
        }
        let k = bound_k(k); // cap result count even on a direct call
                            // bound EVERY structured field before normalization: number and
                            // postcode were unbounded, so a multi-MB value amplified inside normalize()/char-scan below.
        let street = bound_query(street);
        let city = bound_query(city);
        let number = number.map(bound_query);
        let postcode = postcode.map(bound_query);
        let _rules = crate::rules::scope(self.rules);
        let sn = self.expand_city_aliases(&fold_units(&crate::norm::fold_homoglyphs(&normalize(
            street,
        ))));
        let cn = self.expand_city_aliases(&crate::norm::fold_homoglyphs(&normalize(city)));
        let pc: Option<u32> = postcode.and_then(|p| {
            let d: String = p.chars().filter(|c| c.is_ascii_digit()).collect();
            // Keep the FULL numeric postcode: the hard-coded 5-digit cap
            // truncated UZ's 6-digit codes (200456 -> 20045) so they never matched the
            // stored value. Cap at 9 digits only to stay within u32.
            if d.len() >= 4 {
                d[..d.len().min(9)].parse().ok()
            } else {
                None
            }
        });
        let (numero, rep): (Option<u32>, u32) = match number {
            Some(n) => {
                let nn = normalize(n);
                let d: String = nn.chars().take_while(|c| c.is_ascii_digit()).collect();
                let rs: String = nn[d.len()..]
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect();
                (d.parse().ok(), *self.rep_lookup.get(&rs).unwrap_or(&0))
            }
            None => (None, 0),
        };
        // communes from the city field (exact -> prefix -> transliteration) — no boundary guessing
        let mut exact_commune = true;
        let mut cids = self.communes_by_name(&cn);
        if cids.is_empty() {
            cids = self.communes_by_prefix(&cn);
            exact_commune = false;
        }
        if cids.is_empty() {
            let ct = normalize(&crate::norm::translit_cyr_lat_en(&cn));
            cids = self.communes_by_name(&ct);
            exact_commune = !cids.is_empty();
            if cids.is_empty() {
                cids = self.communes_by_prefix(&ct);
            }
        }
        if cids.is_empty() || sn.is_empty() {
            return Vec::new();
        }
        // street-name variants: as is + expansions + type padding + rotation (as in collect_candidates)
        let mut phrases = vec![sn.clone()];
        if let Some(e) = expand_first(&sn) {
            phrases.push(e);
        }
        if let Some(e) = expand_last(&sn) {
            phrases.push(e);
        }
        if let Some(r) = rotate_type_first(&sn) {
            phrases.push(r);
        }
        phrases.extend(type_padded_variants(&sn));
        let mut cand: HashMap<u32, Feats> = HashMap::new();
        for cid in &cids {
            let insee = self.commune_insee(*cid);
            for ph in &phrases {
                let mut key = ph.clone().into_bytes();
                key.push(KEY_SEP);
                key.extend_from_slice(insee.as_bytes());
                if let Some(v) = self.streets_fst.get(&key) {
                    let m = self.street_meta(v as u32);
                    Self::add_cand(
                        &mut cand,
                        v as u32,
                        Feats {
                            street_exact: true,
                            commune_exact: exact_commune,
                            commune_prefix: !exact_commune,
                            pc_exact: pc.is_some_and(|p| m.postcode != 0 && m.postcode == p),
                            pc_dept: pc
                                .is_some_and(|p| m.postcode != 0 && m.postcode / 1000 == p / 1000),
                            ..Default::default()
                        },
                    );
                }
            }
        }
        // exact path empty — fuzzy/subset within the same communes (rest = street + city)
        if cand.is_empty() {
            let rest: Vec<&str> = sn
                .split(' ')
                .chain(cn.split(' '))
                .filter(|t| !t.is_empty())
                .collect();
            cand = self.collect_fuzzy(&rest, pc, false);
            if cand.is_empty() {
                cand = self.collect_subset(&rest, pc);
            }
        }
        if cand.is_empty() {
            return Vec::new();
        }
        let qwords: Vec<Vec<char>> = sn
            .split(' ')
            .filter(|t| !t.is_empty())
            .map(|s| s.chars().collect())
            .collect();
        // Candidates arrive from a hash map, whose order varies between processes. Order
        // them by street id first: the sort below is stable, so candidates that tie on
        // every ranking key then resolve identically on every run ("via roma" and "piazza
        // roma" in Verona tie exactly, and the answer must not depend on the hasher seed).
        let mut cand_v: Vec<(u32, Feats)> = cand.into_iter().collect();
        cand_v.sort_by_key(|(sid, _)| *sid);
        let mut hits: Vec<_> = cand_v
            .into_iter()
            .map(|(sid, f)| self.make_hit(sid, f, numero, rep, &qwords))
            .collect();
        hits.sort_by(|a, b| {
            b.0.score
                .partial_cmp(&a.0.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.2.cmp(&a.2))
                .then(a.3.cmp(&b.3))
                .then(b.4.cmp(&a.4))
        });
        hits.truncate(k);
        {
            let mut out: Vec<(Hit, [f32; N_FEATS])> =
                hits.into_iter().map(|(h, f, _, _, _)| (h, f)).collect();
            Self::monotone_confidence(&mut out);
            out
        }
    }

    /// "City first + trailing number": the exact path expects the commune at the TAIL, and
    /// the word fallback yields only a street even though the house exists. If the leading
    /// phrase is a commune, retry the remainder (and its unglued form) and accept ONLY a
    /// top-1 that is an exact house IN THAT SAME commune — homonyms in other cities are
    /// cut by the comparison.
    fn city_first_house_retry(
        &self,
        q: &str,
        k: usize,
        focus: Option<&QueryFocus>,
    ) -> Option<Vec<(Hit, [f32; N_FEATS])>> {
        let t2: Vec<&str> = q.split(' ').filter(|t| !t.is_empty()).collect();
        for lead in [2usize, 1] {
            if t2.len() < lead + 2 {
                continue;
            }
            let city = t2[..lead].join(" ");
            if self.communes_by_name(&city).is_empty() {
                continue;
            }
            let rest = t2[lead..].join(" ");
            let unglued = unglue_variant(&rest).map(|u| fold_units(&u));
            for cand in std::iter::once(rest).chain(unglued) {
                let h2 = self.query_feats_prepared(&cand, k, focus);
                if h2.first().is_some_and(|(hit, f)| {
                    hit.precision == "house" && f[0] > 0.5 && normalize(&hit.commune) == city
                }) {
                    return Some(h2);
                }
            }
        }
        None
    }

    fn query_feats_prepared(
        &self,
        q: &str,
        k: usize,
        focus: Option<&QueryFocus>,
    ) -> Vec<(Hit, [f32; N_FEATS])> {
        // input CAP (token bombs): the cascade is ~O(n^2), so hundreds of repeated tokens
        // could pin a core for seconds. Real addresses are <= ~15 tokens: collapse repeats
        // of a token (max 2 occurrences — "new york new york" survives), overall cap 32.
        let mut toks: Vec<&str> = q.split(' ').filter(|t| !t.is_empty()).collect();
        if toks.len() > 32 {
            let mut cnt: HashMap<&str, u8> = HashMap::new();
            toks.retain(|t| {
                let c = cnt.entry(t).or_insert(0);
                *c += 1;
                *c <= 2
            });
            toks.truncate(32);
        }
        let toks = toks;
        if toks.is_empty() {
            return Vec::new();
        }
        let n = toks.len();

        // 1) postcode — mark consumed tokens
        let mut used = vec![false; n];
        let mut postcode: Option<u32> = None;
        let mut i = 0;
        while i < n {
            let t = toks[i];
            let all_digit = t.bytes().all(|b| b.is_ascii_digit());
            if all_digit && t.len() == 5 {
                // French/Italian postcode: 5 digits
                postcode = Some(t.parse().unwrap_or(0));
                used[i] = true;
            } else if all_digit && t.len() == 6 {
                // Uzbek postcode: 6 digits
                postcode = Some(t.parse().unwrap_or(0));
                used[i] = true;
            } else if all_digit
                && t.len() == 4
                && i + 1 < n
                && toks[i + 1].len() == 2
                && toks[i + 1].bytes().all(|b| b.is_ascii_alphabetic())
            {
                // Dutch postcode as a pair: "1012 nz"
                postcode = Some(t.parse().unwrap_or(0));
                used[i] = true;
                used[i + 1] = true;
                i += 1;
            } else if t.len() == 6
                && t.bytes().take(4).all(|b| b.is_ascii_digit())
                && t.bytes().skip(4).all(|b| b.is_ascii_alphabetic())
            {
                // Dutch postcode fused: "1012nz"
                postcode = Some(t[..4].parse().unwrap_or(0));
                used[i] = true;
            }
            i += 1;
        }

        // 2) digit groups — house-number candidates
        let mut groups: Vec<usize> = Vec::new(); // first index of each group
        let mut i = 0;
        while i < n {
            // Russian ORDINAL street prefix: normalization splits the hyphen ("6-ya" ->
            // "6 ya"). The ordinal digit is PART of the street name, NOT a house number;
            // otherwise the "6" gets stolen as a house and the street is never found
            // (the index stores the split form).
            let ordinal = i + 1 < n
                && matches!(
                    toks[i + 1],
                    "я" | "й" | "е" | "го" | "ой" | "ая" | "ого" | "ье"
                );
            let digit = !used[i]
                && !toks[i].is_empty()
                && toks[i].len() <= 4
                && toks[i].bytes().all(|b| b.is_ascii_digit())
                && !ordinal;
            if digit {
                groups.push(i);
                // skip the rest of the group
                let mut j = i + 1;
                while j < n
                    && !used[j]
                    && toks[j].len() <= 4
                    && toks[j].bytes().all(|b| b.is_ascii_digit())
                {
                    j += 1;
                }
                i = j;
            } else {
                i += 1;
            }
        }

        // 3) hypotheses: each digit group (leading = French order, trailing = Dutch),
        // each with and without greedy suffix consumption; model hypothesis;
        // compound token; numberless
        let mut hyps: Vec<Hyp> = Vec::new();
        // all digit groups (up to 4): the house number can sit mid-string
        // ("Yunusobod 17 mavzesi 13" — the street itself contains a number)
        for &g0 in groups.iter().take(4) {
            hyps.push(self.build_hyp(&toks, &used, g0, true));
            let no_greedy = self.build_hyp(&toks, &used, g0, false);
            if no_greedy.rest_idx != hyps.last().unwrap().rest_idx {
                hyps.push(no_greedy);
            }
        }
        // compound token ("12a", "7a", "599a1") — always an additional hypothesis:
        // standalone digits in the string may be part of the street name
        for ci in 0..n {
            if used[ci] {
                continue;
            }
            let t = toks[ci];
            let d = t.bytes().take_while(|b| b.is_ascii_digit()).count();
            if (1..=4).contains(&d)
                && t.len() > d
                && t[d..].chars().count() <= 4
                && t[d..].chars().all(|c| c.is_alphanumeric())
            {
                let numero = t[..d].parse().ok();
                // keep consuming suffixes after the compound token, but ONLY
                // letter+digit mixes: the rep dictionary contains junk ("rue", "5")
                // and greedy consumption of it would eat half the street
                let mut rep_s = t[d..].to_string();
                let mut j = ci + 1;
                while j < n && !used[j] && {
                    let w = toks[j];
                    w.bytes().any(|b| b.is_ascii_digit())
                        && !w.bytes().all(|b| b.is_ascii_digit())
                        && self.rep_lookup.contains_key(w)
                } {
                    rep_s.push_str(toks[j]);
                    j += 1;
                }
                let rep = *self.rep_lookup.get(&rep_s).unwrap_or(&0);
                let rest_idx = (0..n)
                    .filter(|i| !used[*i] && (*i < ci || *i >= j))
                    .collect();
                hyps.push(Hyp {
                    numero,
                    rep,
                    rest_idx,
                    from_ml: false,
                });
                break;
            }
        }
        if let Some(mh) = self.ml_hyp(&toks, &used) {
            hyps.push(mh); // model hypothesis — after the heuristics (selection is by score)
        }
        // "city first" ("Amsterdam Dapperstraat 325"): a variant of each hypothesis with
        // the first remainder word moved to the end. >=2 (not >=3), otherwise a city +
        // ONE-word street would never get rotated.
        let extra: Vec<Hyp> = hyps
            .iter()
            .filter(|h| h.rest_idx.len() >= 2)
            .map(|h| {
                let mut r = h.rest_idx[1..].to_vec();
                r.push(h.rest_idx[0]);
                Hyp {
                    numero: h.numero,
                    rep: h.rep,
                    rest_idx: r,
                    from_ml: h.from_ml,
                }
            })
            .collect();
        hyps.extend(extra);
        hyps.truncate(14);
        // numberless street (last)
        hyps.push(Hyp {
            numero: None,
            rep: 0,
            rest_idx: (0..n).filter(|i| !used[*i]).collect(),
            from_ml: false,
        });

        // 4) hypothesis selection: exact candidates; best by maximum score
        let rest_of = |h: &Hyp| -> Vec<&str> { h.rest_idx.iter().map(|&ix| toks[ix]).collect() };
        let mut best: Option<(usize, HashMap<u32, Feats>, i32)> = None;
        for (hi, h) in hyps.iter().enumerate() {
            let rest = rest_of(h);
            if rest.is_empty() {
                continue;
            }
            let cand = self.collect_candidates(&rest, postcode, h.from_ml);
            let top = cand.values().map(|f| f.legacy()).max().unwrap_or(i32::MIN);
            let better = match &best {
                None => !cand.is_empty(),
                Some((_, _, bs)) => top > *bs,
            };
            if better {
                let stop = top >= 8;
                best = Some((hi, cand, top));
                if stop {
                    break; // exact street+commune — stop searching
                }
            }
        }

        // 5) typos — only when the exact passes are empty; among hypotheses pick the
        // BEST by score (taking the first non-empty one misleads on streets with
        // numbers in the name); the fuzzy path is expensive — cap the hypothesis count
        if best.is_none() {
            for (hi, h) in hyps.iter().enumerate().take(5) {
                let rest = rest_of(h);
                if rest.is_empty() {
                    continue;
                }
                let cand = self.collect_fuzzy(&rest, postcode, h.from_ml);
                let top = cand.values().map(|f| f.legacy()).max().unwrap_or(i32::MIN);
                let better = match &best {
                    None => !cand.is_empty(),
                    Some((_, _, bs)) => top > *bs,
                };
                if better {
                    best = Some((hi, cand, top));
                }
            }
        }

        // 6) street-word subset — the LAST fallback (after exact and typo passes). Fires
        // only when both are empty: e.g. "amir temur" -> "Amir Temur shoh". Runs last
        // so it never overrides correct answers from the typo path ("Via Roma 1 Roma").
        // Uses the street-word inverted index.
        if best.is_none() && std::env::var_os("GRIDPIN_NO_SUBSET").is_none() {
            for (hi, h) in hyps.iter().enumerate().take(5) {
                let rest = rest_of(h);
                if rest.is_empty() {
                    continue;
                }
                let cand = self.collect_subset(&rest, postcode);
                let top = cand.values().map(|f| f.legacy()).max().unwrap_or(i32::MIN);
                let better = match &best {
                    None => !cand.is_empty(),
                    Some((_, _, bs)) => top > *bs,
                };
                if better {
                    best = Some((hi, cand, top));
                }
            }
        }

        let (numero, rep, cand) = match best {
            Some((hi, cand, _)) => (hyps[hi].numero, hyps[hi].rep, cand),
            None => return Vec::new(),
        };

        // The ordinary FST prefix scan is deliberately capped at 300 rows. With an explicit
        // focus, recover same-name streets from the existing spatial grid that fell beyond that
        // cap. A local street is accepted only when the ordinary search has already established
        // the same normalized street-name key without a commune constraint: the point can widen
        // a cityless homonym set, but cannot invent an unrelated address or override a named city.
        let local_scored = focus
            .map(|point| self.focus_candidates(&cand, postcode, point))
            .unwrap_or_default();

        // 7) houses, features and final sorting (trained weights or hand-tuned scores)
        let mut scored: Vec<(u32, Feats)> = cand.into_iter().collect();
        // HARD postcode disambiguation: with a postcode in the query, ALWAYS drop
        // candidates from a FOREIGN department (pc/1000) that have a known non-zero
        // postcode. p==0 (absent from the data) is kept. Fixes "35000 Rennes" -> a false
        // Rennes-les-Bains (dept 11): if a right-department candidate exists, only it
        // survives; if NONE does, candidates empty out and the outer junk drop retries
        // without the venue prefix and finds the right street.
        if let Some(pc) = postcode {
            let dept = pc / 1000;
            scored.retain(|(sid, _)| {
                let p = self.street_meta(*sid).postcode;
                p == 0 || p / 1000 == dept
            });
        }
        // the pre-ranking truncation must not cut by street_id (= source CSV order), or at
        // equal score only low-id candidates would survive and the top-1 would depend on
        // -k. At equal score the PROMINENT commune survives; id is the last resort.
        let mut scored: Vec<(u32, Feats, u32)> = scored
            .into_iter()
            .map(|(sid, f)| {
                let prom = self.commune_prominence(self.street_meta(sid).commune_id);
                (sid, f, prom)
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.legacy()
                .cmp(&a.1.legacy())
                .then(b.2.cmp(&a.2))
                .then(a.0.cmp(&b.0))
        });
        scored.truncate(k.max(10) * 3);
        let mut scored: Vec<(u32, Feats)> =
            scored.into_iter().map(|(sid, f, _)| (sid, f)).collect();
        if focus.is_some() {
            let mut seen: std::collections::HashSet<u32> =
                scored.iter().map(|(sid, _)| *sid).collect();
            for (sid, features) in local_scored {
                if seen.insert(sid) {
                    scored.push((sid, features));
                }
            }
        }

        // query words (+both transliterations) for the name-similarity tie-breaker: at
        // EQUAL score the candidate whose name is closer to the query ranks higher.
        // Distinguishes fuzzy-match quality, which the score does not capture.
        let qwords: Vec<Vec<char>> = q
            .split(' ')
            .filter(|t| !t.is_empty() && !t.bytes().all(|b| b.is_ascii_digit()))
            .flat_map(|t| {
                let mut v = vec![t.to_string()];
                let cl = normalize(&crate::norm::translit_cyr_lat(t));
                if cl != t && !cl.is_empty() {
                    v.push(cl);
                }
                let lc = normalize(&crate::norm::translit_lat_cyr(t));
                if lc != t && !lc.is_empty() {
                    v.push(lc);
                }
                v
            })
            .map(|s| s.chars().collect())
            .collect();

        let mut hits: Vec<(Hit, [f32; N_FEATS], i32, u32, u32)> = Vec::new();
        for (sid, f) in scored {
            hits.push(self.make_hit(sid, f, numero, rep, &qwords));
        }
        // GEO ANCHOR for homonyms. If the query TAIL names a major city (resolving to a
        // high-prominence center), then among candidates with the SAME street name the one
        // CLOSEST to that center wins. Fixes "Kneza Mihaila ... beograd" -> Stari Grad
        // (central Belgrade) rather than the Nova Pazova homonym 24 km away — works even
        // when the province umbrella in the data is broken. Affects ONLY homonyms (one
        // name across communes); distinct streets and queries without a trailing city are
        // untouched.
        let (anchor, has_named_city): (Option<(f64, f64)>, bool) = {
            let toks: Vec<&str> = q
                .split(' ')
                .filter(|t| t.chars().count() >= 3 && !t.bytes().all(|b| b.is_ascii_digit()))
                .collect();
            let mut best: Option<(f64, f64, u32)> = None;
            for len in 1..=2.min(toks.len()) {
                let phrase = toks[toks.len() - len..].join(" ");
                if let Some((la, lo, _, prom)) = self.resolve_place_translit(&phrase) {
                    if prom >= 2000 && best.is_none_or(|(_, _, bp)| prom > bp) {
                        best = Some((la, lo, prom));
                    }
                }
            }
            // no city named in the query -> weak capital anchor (the most prominent
            // commune). The adist tie-break is continuous and comes AFTER score/name/
            // postcode — it only flips EQUAL-SCORE homonyms toward the capital.
            // House-level hits and explicitly named cities are untouched.
            let named = best.is_some();
            (best.map(|(la, lo, _)| (la, lo)).or(self.top_anchor), named)
        };
        // each candidate's distance to the city anchor (homonym tie-break)
        let adist: Vec<f64> = match anchor {
            Some((alat, alon)) => hits
                .iter()
                .map(|h| Self::dist_km(alat, alon, h.0.lat, h.0.lon))
                .collect(),
            None => vec![0.0; hits.len()],
        };
        // score first; name similarity as tie-break; THEN proximity to the city anchor
        // (same-street homonyms in different cities have EQUAL score and name -> closer
        // to "beograd" wins; house>near cases are unaffected since their scores differ);
        // commune prominence last. The proximity comparison is continuous, so the total
        // order stays intact (a "same street" condition would break it). Handles homonyms
        // where prominence (address count) misleads (an administratively small center).
        let mut hd: Vec<_> = hits.into_iter().zip(adist).collect();
        hd.sort_by(|(a, ad), (b, bd)| {
            b.0.score
                .partial_cmp(&a.0.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.2.cmp(&a.2))
                // POSTCODE PROXIMITY: with a postcode in the query, among equal-score
                // homonyms of the SAME department prefer the candidate whose postcode is
                // CLOSEST to the requested one ("13001" -> Marseille 130xx, not Arles
                // 13200). City postcodes are contiguous; another city in the same
                // department means a distant postcode.
                .then_with(|| match postcode {
                    Some(qpc) => {
                        let pa = a.0.postcode.parse::<u32>().unwrap_or(0) as i64;
                        let pb = b.0.postcode.parse::<u32>().unwrap_or(0) as i64;
                        if pa != 0 && pb != 0 {
                            (pa - qpc as i64).abs().cmp(&(pb - qpc as i64).abs())
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    }
                    None => std::cmp::Ordering::Equal,
                })
                // snap delta: at equal score a house/neighbor whose number is CLOSER to
                // the requested one beats a distant snap onto another fragment of a split
                // street — BEFORE the geo anchor, since on a long street the centroid
                // anchor pulls toward the wrong end
                .then(a.3.cmp(&b.3))
                // EXPLICIT FOCUS: strict distance tie-break only after every existing text/address
                // quality discriminator is equal. No epsilon, score boost, or learned weight.
                .then_with(|| match focus {
                    Some(point) => Self::dist_km(point.lat, point.lon, a.0.lat, a.0.lon)
                        .partial_cmp(&Self::dist_km(point.lat, point.lon, b.0.lat, b.0.lon))
                        .unwrap_or(std::cmp::Ordering::Equal),
                    None => std::cmp::Ordering::Equal,
                })
                .then(ad.partial_cmp(bd).unwrap_or(std::cmp::Ordering::Equal))
                .then(b.4.cmp(&a.4))
        });
        let mut hits: Vec<_> = hd.into_iter().map(|(h, _)| h).collect();
        // CAPITAL PRIOR for cityless homonyms: no city named, the winner is a DISTANT
        // homonym (another region whose house number exists -> house score > the capital's
        // near), yet the SAME street exists near the capital anchor. A messy listing with
        // no city is almost always about the capital. Promote the capital instance over
        // the distant one. Narrow gate: (1) no city named (has_named_city false —
        // otherwise the geo anchor already handled it), (2) the winner is FARTHER than
        // 60 km from the capital, (3) a candidate with the SAME street name lies WITHIN
        // 40 km of it. Houses near the capital (already closest) and non-homonyms are
        // unaffected.
        if focus.is_none() && !has_named_city {
            if let Some((alat, alon)) = anchor {
                let top_far = hits
                    .first()
                    .is_some_and(|t| Self::dist_km(alat, alon, t.0.lat, t.0.lon) > 60.0);
                if top_far {
                    let tkey = street_key(&hits[0].0.street);
                    // the capital candidate must be an EXACT street (f[0]=street_exact),
                    // not fuzzy — else a fuzzy capital homonym would oust a correct
                    // distant exact house.
                    let near_cap = hits.iter().position(|t| {
                        t.1[0] > 0.5
                            && street_key(&t.0.street) == tkey
                            && Self::dist_km(alat, alon, t.0.lat, t.0.lon) < 40.0
                    });
                    if let Some(pos) = near_cap {
                        if pos != 0 {
                            let chosen = hits.remove(pos);
                            hits.insert(0, chosen);
                        }
                    }
                }
            }
        }
        // confidence MARGIN CUTOFF: a small top-1 vs top-2 SCORE gap plus a LARGE
        // geographic spread means a high risk of a distant homonym. Lower the top-1
        // confidence and set a flag — the ANSWER itself is unchanged (a downstream
        // signal: "confident but ambiguous"). Hit rate is unaffected.
        if hits.len() >= 2 {
            let margin = hits[0].0.score - hits[1].0.score;
            let spread = Self::dist_km(hits[0].0.lat, hits[0].0.lon, hits[1].0.lat, hits[1].0.lon);
            if margin < 1.0 && spread > 50.0 {
                hits[0].0.confidence = hits[0].0.confidence.min(0.2);
                hits[0].0.flags.push("ambiguous_far");
            }
        }
        hits.truncate(k);
        hits.into_iter().map(|(h, f, _, _, _)| (h, f)).collect()
    }

    pub fn query(&self, raw: &str, k: usize) -> Vec<Hit> {
        self.query_feats(raw, k)
            .into_iter()
            .map(|(h, _)| h)
            .collect()
    }

    /// Forward geocoding with an explicit WGS84 focus. The point only widens cityless homonym
    /// candidates from the sheet's spatial grid and breaks otherwise-equal ranking ties.
    pub fn query_near(
        &self,
        raw: &str,
        k: usize,
        lat: f64,
        lon: f64,
    ) -> std::result::Result<Vec<Hit>, String> {
        self.query_feats_near(raw, k, lat, lon)
            .map(|hits| hits.into_iter().map(|(hit, _)| hit).collect())
    }

    /// Same-name candidates from the focus grid that the bounded global FST scan did not expose.
    /// Identity is the strict normalized display name, not the order-insensitive homonym key:
    /// `Alpha Beta` must never confer exact-match evidence on `Beta Alpha`. Commune evidence is
    /// intentionally never copied: only cityless global templates are valid.
    fn focus_candidates(
        &self,
        global: &HashMap<u32, Feats>,
        postcode: Option<u32>,
        focus: &QueryFocus,
    ) -> Vec<(u32, Feats)> {
        let mut templates: HashMap<String, Feats> = HashMap::new();
        for (&sid, features) in global {
            if features.commune_exact || features.commune_prefix {
                continue;
            }
            let meta = self.street_meta(sid);
            let template = Feats {
                street_exact: features.street_exact,
                street_fuzzy: features.street_fuzzy,
                from_ml: features.from_ml,
                ..Default::default()
            };
            templates
                .entry(normalize(self.name(meta.name_off)))
                .or_default()
                .merge(template);
        }
        if templates.is_empty() {
            return Vec::new();
        }

        let mut local = Vec::new();
        for &sid in &focus.streets {
            let meta = self.street_meta(sid);
            let Some(template) = templates.get(&normalize(self.name(meta.name_off))) else {
                continue;
            };
            let mut features = *template;
            if let Some(query_postcode) = postcode {
                if meta.postcode != 0 && meta.postcode / 1000 != query_postcode / 1000 {
                    continue;
                }
                if meta.postcode != 0 {
                    features.pc_dept = true;
                    features.pc_exact = meta.postcode == query_postcode;
                }
            }
            local.push((sid, features));
        }
        local
    }

    /// Streets in a grid cell (binary search over the directory).
    fn streets_in_cell(&self, cell: u32, out: &mut Vec<u32>) {
        let n = self.cells_dir.len() / 12;
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let c = read_u32(self.cells_dir, mid * 12);
            if c < cell {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo < n && read_u32(self.cells_dir, lo * 12) == cell {
            let start = read_u32(self.cells_dir, lo * 12 + 4) as usize;
            let count = read_u32(self.cells_dir, lo * 12 + 8) as usize;
            // Bounds-guard the file-derived (start, count) against cells_post (adversarial finding):
            // a corrupt/tampered SEC_CELLS could otherwise drive an out-of-bounds read that PANICS
            // reverse(), where every sibling postings reader degrades to empty. Saturating math so a
            // crafted overflow can't wrap.
            if start.saturating_add(count).saturating_mul(4) > self.cells_post.len() {
                return;
            }
            for i in 0..count {
                out.push(read_u32(self.cells_post, (start + i) * 4));
            }
        }
    }

    /// Street ids in a square of spatial cells around a point, in deterministic id order.
    fn streets_in_square(&self, lat: f64, lon: f64, ring: i64) -> Vec<u32> {
        let base_la = ((lat + 90.0) / 0.01).floor() as i64;
        let base_lo = ((lon + 180.0) / 0.01).floor() as i64;
        let mut streets = Vec::new();
        for dla in -ring..=ring {
            for dlo in -ring..=ring {
                let la = (base_la + dla).clamp(0, 17999) as u32;
                let lo = (base_lo + dlo).clamp(0, 35999) as u32;
                self.streets_in_cell(la * 36000 + lo, &mut streets);
            }
        }
        streets.sort_unstable();
        streets.dedup();
        streets
    }

    /// Forward focus searches the complete allowed 9x9 neighborhood. Unlike reverse, it cannot
    /// stop merely because three unrelated streets appeared in a smaller ring: the wanted homonym
    /// may have its centroid in the next ring.
    fn streets_around(&self, lat: f64, lon: f64) -> Vec<u32> {
        self.streets_in_square(lat, lon, 4)
    }

    /// Approximate reverse geocoding: the nearest houses among streets indexed within ~10 km of the
    /// point. A street is indexed at its centroid cell, so a closer house on a street whose centroid
    /// falls farther away can be missed — this is NOT a guaranteed global nearest.
    /// Each result carries precision/confidence and (for reverse) distance_m so the caller sees how
    /// far the answer actually is.
    /// Reverse geocode with STRICT validation: a NaN/out-of-range coordinate is an
    /// `Err`, not a silent empty result — the single fallible entry point every interface
    /// (CLI/py/DuckDB) routes through, so bad input behaves identically everywhere. `reverse`
    /// is an alias with the SAME strict contract ( no public lenient variant
    /// remains; the lenient layer is crate-private defense-in-depth only).
    pub fn try_reverse(
        &self,
        lat: f64,
        lon: f64,
        k: usize,
    ) -> std::result::Result<Vec<Hit>, String> {
        validate_lat_lon(lat, lon)?;
        // Cap k like every other public entry: reverse passed RAW k through, so
        // `-k usize::MAX` returned every candidate in the rings (15k+ rows on France) while forward
        // was already bounded — the abuse bound must hold on ALL public interfaces.
        Ok(self.reverse_lenient(lat, lon, bound_k(k)))
    }

    /// Reverse geocode. STRICT like every public entry point: invalid input
    /// (NaN/out-of-range) is an error, never a silent empty vec — the lenient behaviour survives
    /// only as the crate-private `reverse_lenient` defense-in-depth layer under the validator.
    pub fn reverse(&self, lat: f64, lon: f64, k: usize) -> std::result::Result<Vec<Hit>, String> {
        self.try_reverse(lat, lon, k)
    }

    /// Reverse geocode, LENIENT (crate-private): invalid input yields an empty vec. Kept as
    /// defense-in-depth under the public strict API — a garbage cell must never be indexed even
    /// if a future caller bypasses validation.
    pub(crate) fn reverse_lenient(&self, lat: f64, lon: f64, k: usize) -> Vec<Hit> {
        let _rules = crate::rules::scope(self.rules);
        if k == 0 {
            return Vec::new(); // "-k 0" must return nothing (the max(1) below is for internal calls)
        }
        let k = bound_k(k); // defense-in-depth: a direct crate-internal caller is bounded too
                            // Defense-in-depth: a NaN or out-of-range coordinate must yield nothing, never
                            // index a garbage cell (a NaN `as i64` cast is 0, silently the south pole). BOTH public
                            // entries (`reverse` = `try_reverse`) are strict Err on bad input; this
                            // lenient empty-vec layer is crate-private only, for a future caller that skips validation.
        if validate_lat_lon(lat, lon).is_err() {
            return Vec::new();
        }
        // widen rings 3x3 -> 5x5 -> 9x9 until we have enough candidates
        let mut streets: Vec<u32> = Vec::new();
        for ring in [1i64, 2, 4] {
            streets = self.streets_in_square(lat, lon, ring);
            if streets.len() >= 3 {
                break;
            }
        }

        // collect ALL houses of the candidate streets; dedup and truncation to k happen
        // AFTER sorting — otherwise collapsed duplicate house numbers occupy slots, dedup
        // trims them, and reverse -k N silently returns FEWER than N. The candidate set
        // is small (~3 streets of the local cell).
        let coslat = lat.to_radians().cos().max(0.01);
        let mut all: Vec<(f64, u32, u32, u32, f64, f64, u32)> = Vec::new(); // + postcode name offset
        for &sid in &streets {
            let m = self.street_meta(sid);
            let Some((mut pos, postcode_dictionary, postcode_count, house_end)) =
                self.house_block_layout(sid, &m)
            else {
                continue; // corrupt house offset/dictionary: skip, never panic
            };
            let bounded_houses = &self.houses[..house_end];
            let mut cur = 0u32;
            for i in 0..m.house_count {
                if pos >= self.houses.len() {
                    break; // truncated house block
                }
                let Some(d) = strict_varint(bounded_houses, &mut pos)
                    .and_then(|value| u32::try_from(value).ok())
                else {
                    break;
                };
                let Some(next_cur) = (if i == 0 { Some(d) } else { cur.checked_add(d) }) else {
                    break;
                };
                cur = next_cur;
                let Some(rid) = strict_varint(bounded_houses, &mut pos)
                    .and_then(|value| u32::try_from(value).ok())
                else {
                    break;
                };
                let Some(dlat_raw) = strict_varint(bounded_houses, &mut pos) else {
                    break;
                };
                let Some(dlon_raw) = strict_varint(bounded_houses, &mut pos) else {
                    break;
                };
                let dlat = unzigzag(dlat_raw);
                let dlon = unzigzag(dlon_raw);
                let postcode_off =
                    if self.format_version >= 7 && m.postcode_disp_off == PC_DISP_AMBIGUOUS {
                        let Some(id) = strict_varint(bounded_houses, &mut pos)
                            .and_then(|value| u32::try_from(value).ok())
                        else {
                            break;
                        };
                        self.house_postcode_offset(postcode_dictionary, postcode_count, id)
                    } else {
                        0
                    };
                let Some(hla_e7) = (m.lat_c as i64).checked_add(dlat) else {
                    break;
                };
                let Some(hlo_e7) = (m.lon_c as i64).checked_add(dlon) else {
                    break;
                };
                if !(-900_000_000..=900_000_000).contains(&hla_e7)
                    || !(-1_800_000_000..=1_800_000_000).contains(&hlo_e7)
                {
                    break;
                }
                let hla = hla_e7 as f64 / 1e7;
                let hlo = hlo_e7 as f64 / 1e7;
                let dy = (hla - lat) * 111_320.0;
                let dx = (hlo - lon) * 111_320.0 * coslat;
                let dist = (dx * dx + dy * dy).sqrt();
                all.push((dist, sid, cur, rid, hla, hlo, postcode_off));
            }
        }
        all.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        // dedup by (street, number, SUFFIX) BEFORE truncating to k: 12, 12A and 12bis
        // are distinct addresses and must not collapse into one; the
        // nearest among true duplicates survives, so -k N still returns N addresses
        let mut seen: std::collections::HashSet<(u32, u32, u32)> = std::collections::HashSet::new();
        all.into_iter()
            .filter(|(_, sid, num, rid, ..)| seen.insert((*sid, *num, *rid)))
            .take(k.max(1))
            .map(|(dist, sid, num, rid, hla, hlo, postcode_off)| {
                let m = self.street_meta(sid);
                // same contract as forward: `street` is the pure street name; the house
                // number ships in its own field
                let street = self.name(m.name_off).to_string();
                let housenumber = Some(self.house_number(num, rid));
                Hit {
                    lat: hla,
                    lon: hlo,
                    // honest precision: "house" only when actually at a house; farther out
                    // "near"/"approximate" — else a house km away would be labeled "house"
                    precision: if dist <= 50.0 {
                        "house"
                    } else if dist <= 250.0 {
                        "near"
                    } else {
                        "approximate"
                    },
                    score: 0.0,
                    // honest confidence: decays with distance to the nearest house
                    confidence: (0.97 - dist / 500.0).clamp(0.1, 0.95) as f32,
                    street,
                    housenumber,
                    commune: self.commune_name(m.commune_id).to_string(),
                    postcode: self.postcode_for_house(&m, postcode_off),
                    flags: Vec::new(),
                    region: self.admin_at(hla, hlo),
                    distance_m: Some((dist * 10.0).round() / 10.0),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forward_housenumber_index(case: &str, rows: &str) -> Index {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "gridpin-forward-housenumber-{case}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("addresses.csv");
        std::fs::write(
            &csv,
            format!(
                "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n{rows}"
            ),
        )
        .unwrap();
        let bin = dir.join("addresses.bin");
        crate::builder::build(&csv, &bin, None, None, None, None, None).unwrap();
        Index::open(&bin).unwrap()
    }

    fn forward_postcode_index(case: &str, rows: &str) -> Index {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "gridpin-forward-postcode-{case}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("addresses.csv");
        std::fs::write(
            &csv,
            format!(
                "nom_voie_norm,code_insee,nom_commune_norm,code_postal,code_postal_display,numero,rep,lon,lat,nom_voie,nom_commune\n{rows}"
            ),
        )
        .unwrap();
        let bin = dir.join("addresses.bin");
        crate::builder::build(&csv, &bin, None, None, None, None, None).unwrap();
        Index::open(&bin).unwrap()
    }

    #[test]
    fn v6_ambiguous_house_block_uses_legacy_grammar_end_to_end() {
        let dir = std::env::temp_dir().join(format!(
            "gridpin-v6-ambiguous-postcode-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("addresses.csv");
        std::fs::write(
            &csv,
            "nom_voie_norm,code_insee,nom_commune_norm,code_postal,code_postal_display,numero,rep,lon,lat,nom_voie,nom_commune\n\
             legacy,001,ville,1012,1012AA,1,,4.9000,52.3700,Legacy,Ville\n\
             legacy,001,ville,1012,1012AA,3,,4.9010,52.3710,Legacy,Ville\n",
        )
        .unwrap();
        let v7 = dir.join("v7.bin");
        crate::builder::build(&csv, &v7, None, None, None, None, None).unwrap();
        let mut bytes = std::fs::read(&v7).unwrap();
        let sections = parse_sections(&bytes).unwrap();
        let street_meta = sections[SEC_STREETS_META].0 as usize;
        bytes[4] = 6;
        bytes[street_meta + 32..street_meta + 36].copy_from_slice(&PC_DISP_AMBIGUOUS.to_le_bytes());
        let v6 = dir.join("v6.bin");
        std::fs::write(&v6, bytes).unwrap();

        let idx = Index::open(&v6).expect("v6 legacy ambiguous grammar must remain readable");
        let exact = idx
            .query_structured("legacy", Some("3"), "ville", None, 1)
            .remove(0)
            .0;
        assert_eq!(exact.precision, "house");
        assert_eq!(exact.housenumber.as_deref(), Some("3"));
        assert_eq!(exact.postcode, "");
        let reverse = idx.reverse(52.3710, 4.9010, 1).unwrap().remove(0);
        assert_eq!(reverse.housenumber.as_deref(), Some("3"));
        assert_eq!(reverse.postcode, "");
        assert!(
            parse_sections_for_repack(&std::fs::read(v6).unwrap()).is_err(),
            "a readable v6 sheet must still never be relabeled by repack"
        );
    }

    #[test]
    fn v7_corrupt_house_data_cannot_cross_into_the_next_street_block() {
        let dir = std::env::temp_dir().join(format!(
            "gridpin-v7-postcode-block-boundary-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("addresses.csv");
        std::fs::write(
            &csv,
            "nom_voie_norm,code_insee,nom_commune_norm,code_postal,code_postal_display,numero,rep,lon,lat,nom_voie,nom_commune\n\
             alpha,001,ville,1012,1012AA,1,,4.9000,52.3700,Alpha,Ville\n\
             alpha,001,ville,1012,1012AB,3,,4.9010,52.3710,Alpha,Ville\n\
             beta,001,ville,1013,1013AA,1,,4.9100,52.3800,Beta,Ville\n",
        )
        .unwrap();
        let good = dir.join("good.bin");
        crate::builder::build(&csv, &good, None, None, None, None, None).unwrap();
        let mut bytes = std::fs::read(&good).unwrap();
        let sections = parse_sections(&bytes).unwrap();
        let streets_meta = sections[SEC_STREETS_META].0 as usize;
        let houses = sections[SEC_HOUSE_BLOCKS].0 as usize;
        let next_house_off = read_u64(&bytes, streets_meta + STREET_META_SIZE + 20) as usize;
        bytes[houses + next_house_off - 1] = 0x80;
        let corrupt = dir.join("corrupt.bin");
        std::fs::write(&corrupt, bytes).unwrap();

        let idx = Index::open(&corrupt).expect("dictionary header itself remains valid");
        let hit = idx
            .query_structured("alpha", Some("3"), "ville", None, 1)
            .remove(0)
            .0;
        assert_ne!(
            hit.precision, "house",
            "an unterminated id must stop at alpha's boundary, not consume beta bytes"
        );
        assert_eq!(hit.postcode, "");
    }

    #[test]
    fn v7_dictionary_cannot_point_at_an_arbitrary_name() {
        let dir = std::env::temp_dir().join(format!(
            "gridpin-v7-postcode-dictionary-name-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("addresses.csv");
        std::fs::write(
            &csv,
            "nom_voie_norm,code_insee,nom_commune_norm,code_postal,code_postal_display,numero,rep,lon,lat,nom_voie,nom_commune\n\
             alpha,001,ville,1012,1012AA,1,,4.9000,52.3700,Alpha,Ville\n\
             alpha,001,ville,1012,1012AB,3,,4.9010,52.3710,Alpha,Ville\n",
        )
        .unwrap();
        let good = dir.join("good.bin");
        crate::builder::build(&csv, &good, None, None, None, None, None).unwrap();
        let mut bytes = std::fs::read(&good).unwrap();
        let sections = parse_sections(&bytes).unwrap();
        let street_meta = sections[SEC_STREETS_META].0 as usize;
        let house_section = sections[SEC_HOUSE_BLOCKS].0 as usize;
        let first_house = read_u64(&bytes, street_meta + 20) as usize;
        let street_name_off = read_u32(&bytes, street_meta + 16);
        // count is one byte for this two-postcode fixture; overwrite dictionary entry 1.
        bytes[house_section + first_house + 1..house_section + first_house + 5]
            .copy_from_slice(&street_name_off.to_le_bytes());
        let corrupt = dir.join("corrupt.bin");
        std::fs::write(&corrupt, bytes).unwrap();
        let error = Index::open(&corrupt)
            .err()
            .expect("a street name must never be accepted as a postcode")
            .to_string();
        assert!(error.contains("house-postcode dictionary"), "{error}");
    }

    #[test]
    fn v7_exact_house_postcodes_are_house_accurate_while_street_stays_empty() {
        let idx = forward_postcode_index(
            "exact-street",
            "damrak,001,amsterdam,1012,1012AA,1,,4.9000,52.3700,Damrak,Amsterdam\n\
             damrak,001,amsterdam,1012,1012AB,3,,4.9010,52.3710,Damrak,Amsterdam\n",
        );
        for (number, expected) in [("1", "1012AA"), ("3", "1012AB")] {
            let hit = idx
                .query_structured("damrak", Some(number), "amsterdam", None, 1)
                .remove(0)
                .0;
            assert_eq!(hit.precision, "house");
            assert_eq!(hit.postcode, expected);
        }
        let street = idx
            .query_structured("damrak", None, "amsterdam", None, 1)
            .remove(0)
            .0;
        assert_eq!(street.precision, "street");
        assert_eq!(
            street.postcode, "",
            "street-level postcode remains empty on a mixed-postcode street"
        );
    }

    #[test]
    fn v7_missing_house_postcode_never_inherits_the_known_neighbor() {
        let idx = forward_postcode_index(
            "missing-known",
            "damrak,001,amsterdam,0,,1,,4.9000,52.3700,Damrak,Amsterdam\n\
             damrak,001,amsterdam,1012,1012AA,3,,4.9010,52.3710,Damrak,Amsterdam\n",
        );
        let missing = idx
            .query_structured("damrak", Some("1"), "amsterdam", None, 1)
            .remove(0)
            .0;
        let known = idx
            .query_structured("damrak", Some("3"), "amsterdam", None, 1)
            .remove(0)
            .0;
        assert_eq!(missing.postcode, "");
        assert_eq!(known.postcode, "1012AA");
    }

    #[test]
    fn v7_near_and_interpolation_postcodes_follow_the_represented_address() {
        let near_idx = forward_postcode_index(
            "near-postcode",
            "damrak,001,amsterdam,1012,1012AA,10,,4.9000,52.3700,Damrak,Amsterdam\n\
             damrak,001,amsterdam,1012,1012AB,30,,4.9100,52.3800,Damrak,Amsterdam\n",
        );
        let near = near_idx
            .query_structured("damrak", Some("17"), "amsterdam", None, 1)
            .remove(0)
            .0;
        assert_eq!(near.precision, "near");
        assert_eq!(near.housenumber.as_deref(), Some("10"));
        assert_eq!(near.postcode, "1012AA");

        let interp_idx = forward_postcode_index(
            "interp-postcode",
            "damrak,001,amsterdam,1012,1012AA,10,,4.9000,52.3700,Damrak,Amsterdam\n\
             damrak,001,amsterdam,1012,1012AA,20,,4.9010,52.3705,Damrak,Amsterdam\n\
             damrak,001,amsterdam,1012,1012AB,30,,4.9020,52.3710,Damrak,Amsterdam\n",
        );
        let same = interp_idx
            .query_structured("damrak", Some("15"), "amsterdam", None, 1)
            .remove(0)
            .0;
        let cross = interp_idx
            .query_structured("damrak", Some("25"), "amsterdam", None, 1)
            .remove(0)
            .0;
        assert_eq!(same.precision, "interp");
        assert_eq!(same.postcode, "1012AA");
        assert_eq!(cross.precision, "interp");
        assert_eq!(cross.postcode, "");
    }

    #[test]
    fn v7_same_number_suffixes_and_reverse_keep_each_house_postcode() {
        let idx = forward_postcode_index(
            "suffix-reverse",
            "damrak,001,amsterdam,1012,1012AA,12,a,4.9000,52.3700,Damrak,Amsterdam\n\
             damrak,001,amsterdam,1012,1012AB,12,b,4.9010,52.3710,Damrak,Amsterdam\n",
        );
        for (number, expected) in [("12a", "1012AA"), ("12b", "1012AB")] {
            let hit = idx
                .query_structured("damrak", Some(number), "amsterdam", None, 1)
                .remove(0)
                .0;
            assert_eq!(hit.housenumber.as_deref(), Some(number));
            assert_eq!(hit.postcode, expected);
        }
        let reverse = idx.reverse(52.3700, 4.9000, 2).unwrap();
        let by_number: std::collections::HashMap<&str, &str> = reverse
            .iter()
            .filter_map(|hit| {
                hit.housenumber
                    .as_deref()
                    .map(|number| (number, hit.postcode.as_str()))
            })
            .collect();
        assert_eq!(by_number.get("12a"), Some(&"1012AA"));
        assert_eq!(by_number.get("12b"), Some(&"1012AB"));
    }

    #[test]
    fn v7_truncated_house_postcode_id_never_panics_or_borrows_a_postcode() {
        let dir = std::env::temp_dir().join(format!(
            "gridpin-v7-truncated-postcode-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("addresses.csv");
        std::fs::write(
            &csv,
            "nom_voie_norm,code_insee,nom_commune_norm,code_postal,code_postal_display,numero,rep,lon,lat,nom_voie,nom_commune\n\
             damrak,001,amsterdam,1012,1012AA,1,,4.9000,52.3700,Damrak,Amsterdam\n\
             damrak,001,amsterdam,1012,1012AB,3,,4.9010,52.3710,Damrak,Amsterdam\n",
        )
        .unwrap();
        let good = dir.join("good.bin");
        crate::builder::build(&csv, &good, None, None, None, None, None).unwrap();
        let mut bytes = std::fs::read(&good).unwrap();
        let sections = parse_sections(&bytes).unwrap();
        let (house_off, house_len) = sections[SEC_HOUSE_BLOCKS];
        // The last byte is the second house's one-byte local postcode id. Turn it into an
        // unterminated varint at the SEC_HOUSE_BLOCKS boundary; the following names section must
        // never be read as continuation bytes.
        bytes[(house_off + house_len - 1) as usize] = 0x80;
        let corrupt = dir.join("corrupt.bin");
        std::fs::write(&corrupt, bytes).unwrap();
        let idx = Index::open(&corrupt).expect("bounded dictionary header remains openable");
        let hits = idx.query_structured("damrak", Some("3"), "amsterdam", None, 1);
        assert!(
            hits.first().is_none_or(|(hit, _)| hit.postcode.is_empty()),
            "a truncated house postcode id must fail closed, never borrow another postcode"
        );
    }

    #[test]
    fn forward_exact_house_forwards_the_stored_number_and_suffix() {
        let idx = forward_housenumber_index(
            "exact",
            "rue test,001,ville,10000,12,a,7.4200,43.7300,Rue Test,Ville\n",
        );
        let hit = idx
            .query_structured("rue test", Some("12a"), "ville", Some("10000"), 1)
            .remove(0)
            .0;
        assert_eq!(hit.precision, "house");
        assert_eq!(hit.housenumber.as_deref(), Some("12a"));

        let street = idx
            .query_structured("rue test", None, "ville", Some("10000"), 1)
            .remove(0)
            .0;
        assert_eq!(street.precision, "street");
        assert_eq!(
            street.housenumber, None,
            "street-only answers have no number"
        );
    }

    #[test]
    fn forward_near_house_forwards_the_snapped_stored_address() {
        let idx = forward_housenumber_index(
            "near",
            "rue test,001,ville,10000,10,a,7.4200,43.7300,Rue Test,Ville\n\
             rue test,001,ville,10000,30,b,7.4210,43.7310,Rue Test,Ville\n",
        );
        let hit = idx
            .query_structured("rue test", Some("17"), "ville", Some("10000"), 1)
            .remove(0)
            .0;
        assert_eq!(hit.precision, "near");
        assert_eq!(hit.housenumber.as_deref(), Some("10a"));
    }

    #[test]
    fn forward_interpolation_forwards_the_requested_address() {
        let idx = forward_housenumber_index(
            "interp",
            "rue test,001,ville,10000,10,a,7.4200,43.7300,Rue Test,Ville\n\
             rue test,001,ville,10000,20,b,7.4210,43.7305,Rue Test,Ville\n\
             rue test,001,ville,10000,30,c,7.4220,43.7310,Rue Test,Ville\n",
        );
        let hit = idx
            .query_structured("rue test", Some("15c"), "ville", Some("10000"), 1)
            .remove(0)
            .0;
        assert_eq!(hit.precision, "interp");
        assert_eq!(hit.housenumber.as_deref(), Some("15c"));
    }

    #[test]
    fn forward_focus_injects_a_same_name_street_beyond_the_global_fst_cap() {
        let mut rows = String::new();
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
                normalize(&commune)
            ));
        }
        let idx = forward_housenumber_index("focus-ring-cap", &rows);
        let ordinary = idx.query("1 markt", 100);
        assert!(
            ordinary.iter().all(|hit| hit.commune != "Wanted"),
            "the fixture must place Wanted beyond the ordinary 300-row prefix cap"
        );

        let focused = idx.query_near("1 markt", 100, 48.5839, 7.7455).unwrap();
        assert_eq!(
            focused.first().map(|hit| hit.commune.as_str()),
            Some("Wanted")
        );
    }

    #[test]
    fn forward_focus_keeps_the_global_fallback_for_an_unrelated_local_grid() {
        let idx = forward_housenumber_index(
            "focus-global-fallback",
            "alpha,001,origin,75000,1,,2.3500,48.8500,Alpha,Origin\n\
             beta,002,focus,67000,1,,7.7455,48.5839,Beta,Focus\n\
             delta,002,focus,67000,1,,7.7460,48.5840,Delta,Focus\n\
             gamma,002,focus,67000,1,,7.7465,48.5841,Gamma,Focus\n",
        );
        let focused = idx.query_near("1 alpha", 3, 48.5839, 7.7455).unwrap();
        assert_eq!(
            focused.first().map(|hit| hit.street.as_str()),
            Some("Alpha")
        );
        assert_eq!(
            focused.first().map(|hit| hit.commune.as_str()),
            Some("Origin")
        );
    }

    #[test]
    fn forward_focus_does_not_turn_reordered_words_into_an_exact_homonym() {
        let idx = forward_housenumber_index(
            "focus-strict-street-identity",
            "alpha beta,001,origin,75000,1,,2.3500,48.8500,Alpha Beta,Origin\n\
             beta alpha,002,focus,67000,1,,7.7455,48.5839,Beta Alpha,Focus\n",
        );
        let ordinary = idx.query("1 alpha beta", 1);
        let focused = idx.query_near("1 alpha beta", 1, 48.5839, 7.7455).unwrap();
        assert_eq!(
            ordinary.first().map(|hit| hit.street.as_str()),
            Some("Alpha Beta")
        );
        assert_eq!(
            focused.first().map(|hit| hit.street.as_str()),
            Some("Alpha Beta")
        );
    }

    #[test]
    fn forward_focus_breaks_only_an_equal_quality_homonym_tie() {
        let mut rows = String::new();
        for number in 1..=10 {
            rows.push_str(&format!(
                "markt,001,far,10000,{number},,2.{number:04},43.0000,Markt,Far\n"
            ));
        }
        rows.push_str("markt,002,near,67000,1,,7.7455,48.5839,Markt,Near\n");
        let idx = forward_housenumber_index("focus-distance-tie", &rows);

        let ordinary = idx.query("1 markt", 2);
        let focused = idx.query_near("1 markt", 2, 48.5839, 7.7455).unwrap();
        assert_eq!(ordinary.len(), 2);
        assert_eq!(focused.len(), 2);
        assert_eq!(
            ordinary[0].commune, "Far",
            "fixture must expose the legacy order"
        );
        assert_eq!(focused[0].commune, "Near");
        assert_eq!(ordinary[0].score, ordinary[1].score);
        assert_eq!(focused[0].score, focused[1].score);

        let ordinary_set: std::collections::BTreeSet<_> =
            ordinary.iter().map(|hit| hit.commune.as_str()).collect();
        let focused_set: std::collections::BTreeSet<_> =
            focused.iter().map(|hit| hit.commune.as_str()).collect();
        assert_eq!(
            ordinary_set, focused_set,
            "focus may reorder but not replace the global set"
        );
    }

    #[test]
    fn missing_admin_sidecar_after_rename_is_detected() {
        // a release rename that moves the sheet but not the sidecar must be caught,
        // not silently drop regions. admin_sidecar derives the expected name and flags a mismatch.
        let dir = std::env::temp_dir().join(format!("gridpin-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // a valid WOFA sidecar named for the ORIGINAL sheet (rs.bin -> rs_admin.bin)
        let mut wofa = b"WOFA".to_vec();
        wofa.extend_from_slice(&0u32.to_le_bytes()); // n = 0 regions
        wofa.extend_from_slice(&[0u8; 4]); // padding so len > 8
        std::fs::write(dir.join("rs_admin.bin"), &wofa).unwrap();
        std::fs::write(dir.join("rs.bin"), b"dummy").unwrap();
        std::fs::write(dir.join("rs-2026.07.gpin"), b"dummy").unwrap();
        // the correctly-named sheet loads its sidecar
        assert!(matches!(
            admin_sidecar(&dir.join("rs.bin")),
            AdminSidecar::Loaded(_)
        ));
        // the RENAMED sheet has no matching sidecar but a SAME-COUNTRY sibling exists -> flagged
        assert!(matches!(
            admin_sidecar(&dir.join("rs-2026.07.gpin")),
            AdminSidecar::MissingWithSibling { .. }
        ));
        // an UNRELATED-country sidecar next to a sheet must NOT false-warn
        std::fs::write(dir.join("uz.bin"), b"dummy").unwrap();
        assert!(matches!(
            admin_sidecar(&dir.join("uz.bin")),
            AdminSidecar::MissingClean
        ));
    }

    #[test]
    fn open_owns_and_drops_its_mapping_no_leak() {
        // an Index must OWN its mapping and free it on Drop, not leak it forever.
        // Deterministic (immune to parallel tests): clone the Arc<Mapping> and watch strong_count.
        let dir = std::env::temp_dir().join(format!("gridpin-h12-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("h12.csv");
        std::fs::write(
            &csv,
            "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n\
             rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n",
        )
        .unwrap();
        let bin = dir.join("h12.bin");
        crate::builder::build(&csv, &bin, None, None, None, None, None).unwrap();

        let idx = Index::open(&bin).unwrap();
        let mapping = std::sync::Arc::clone(&idx._mmap); // our clone + the Index's = 2
        assert_eq!(
            std::sync::Arc::strong_count(&mapping),
            2,
            "the Index owns a live mapping"
        );
        assert!(
            !idx.query("rue a 1 ville", 1).is_empty(),
            "and it still queries"
        );
        drop(idx);
        assert_eq!(
            std::sync::Arc::strong_count(&mapping),
            1,
            "dropping the Index released its mapping -> the mmap is freed (no leak)"
        );

        // a FAILED open must not leak either: a corrupt sheet returns Err and drops its mapping
        let mut bytes = std::fs::read(&bin).unwrap();
        bytes[6] = 200; // corrupt the TOC
        let bad = dir.join("h12-bad.bin");
        std::fs::write(&bad, &bytes).unwrap();
        assert!(
            Index::open(&bad).is_err(),
            "corrupt sheet fails to open (and freed its mapping)"
        );
    }

    #[test]
    fn rank_rejects_wrong_feature_count_or_length() {
        // bypass: a GPRK whose n != N_FEATS, or with short/over-long weight bytes, must be
        // rejected — else score() silently zips only n weights against the N_FEATS vector.
        let build = |n: u8, weights: usize| -> Vec<u8> {
            let mut v = b"GPRK".to_vec();
            v.push(n);
            v.extend_from_slice(&0f32.to_le_bytes()); // bias
            for _ in 0..weights {
                v.extend_from_slice(&0.5f32.to_le_bytes());
            }
            v
        };
        assert!(
            rank_section_is_valid(&build(N_FEATS as u8, N_FEATS)),
            "exact n + length loads"
        );
        assert!(
            !rank_section_is_valid(&build(1, N_FEATS)),
            "shrunk n (mutant) rejected"
        );
        assert!(
            !rank_section_is_valid(&build(N_FEATS as u8, N_FEATS + 3)),
            "trailing bytes rejected"
        );
        assert!(
            !rank_section_is_valid(&build(N_FEATS as u8, N_FEATS - 1)),
            "short weight table rejected"
        );
    }

    #[test]
    fn rank_rejects_nonfinite_bias_and_weights() {
        // a NaN bias/weight would propagate to every score (score:null / nan).
        let gprk = |bias: f32, w0: f32| -> Vec<u8> {
            let mut v = b"GPRK".to_vec();
            v.push(N_FEATS as u8); // n MUST equal N_FEATS
            v.extend_from_slice(&bias.to_le_bytes());
            v.extend_from_slice(&w0.to_le_bytes());
            for _ in 1..N_FEATS {
                v.extend_from_slice(&0f32.to_le_bytes());
            }
            v
        };
        assert!(
            Rank::from_section(&gprk(0.0, 0.5)).is_some(),
            "a finite model loads"
        );
        assert!(
            Rank::from_section(&gprk(f32::NAN, 0.5)).is_none(),
            "NaN bias rejected"
        );
        assert!(
            Rank::from_section(&gprk(0.0, f32::INFINITY)).is_none(),
            "non-finite weight rejected"
        );
    }

    fn hit(precision: &'static str, confidence: f32, flags: Vec<&'static str>) -> Hit {
        Hit {
            lat: 0.0,
            lon: 0.0,
            precision,
            score: 0.0,
            confidence,
            street: String::new(),
            housenumber: None,
            commune: String::new(),
            postcode: String::new(),
            flags,
            region: None,
            distance_m: None,
        }
    }

    /// The POI cascade may only be consulted when the address top-1 is weak. An exact house
    /// match must NEVER be weak, whatever its calibrated confidence — a distant homonym
    /// lowers confidence without making the match less exact (regression guard for the POI
    /// override bug).
    #[test]
    fn input_budget_bounds_k_and_every_structured_field() {
        // k and every field are capped at the public boundary so no interface (CLI/Python/
        // DuckDB) can turn one call into an unbounded allocation / normalization pass.
        assert_eq!(bound_k(usize::MAX), MAX_K, "huge k is capped");
        assert_eq!(bound_k(0), 0, "k=0 keeps the zero-results contract");
        assert_eq!(bound_k(7), 7, "a normal k passes through");
        // a multi-MB field is bounded to MAX_QUERY_BYTES BEFORE normalization
        let huge = "9".repeat(4 * 1024 * 1024);
        assert!(
            bound_query(&huge).len() <= MAX_QUERY_BYTES,
            "number/postcode field is bounded"
        );
        // bounding is at a char boundary (never panics mid-codepoint)
        let huge_utf8 = "é".repeat(2 * 1024 * 1024); // 2 bytes/char
        let b = bound_query(&huge_utf8);
        assert!(b.len() <= MAX_QUERY_BYTES && huge_utf8.starts_with(b));
    }

    #[test]
    fn num_words_to_digits_rewrites_french_numerals() {
        assert_eq!(
            num_words_to_digits("rue du quatre septembre").as_deref(),
            Some("rue du 4 septembre")
        );
        assert_eq!(num_words_to_digits("avenue de la paix"), None); // nothing to rewrite
    }

    #[test]
    fn french_arrondissement_rewrite_accepts_formal_de_only_in_context() {
        assert_eq!(
            fr_arrondissement_rewrite("37 rue du hameau 15e arrondissement de paris").as_deref(),
            Some("37 rue du hameau paris 15e arrondissement")
        );
        assert_eq!(
            fr_arrondissement_rewrite("3e arrondissement de lyon").as_deref(),
            Some("lyon 3e arrondissement")
        );
        assert_eq!(
            fr_arrondissement_rewrite("1eme arrondissement de paris").as_deref(),
            Some("paris 1er arrondissement")
        );
        assert_eq!(
            fr_arrondissement_rewrite("1e arrondissement de marseille").as_deref(),
            Some("marseille 1er arrondissement")
        );
        assert_eq!(
            fr_arrondissement_rewrite("15e arrondissement paris de test").as_deref(),
            Some("paris 15e arrondissement de test"),
            "a de after the direct city form is not the optional preposition"
        );
        assert_eq!(
            fr_arrondissement_rewrite("paris 15e rue du hameau 37").as_deref(),
            Some("rue du hameau 37 paris 15e arrondissement")
        );
        assert_eq!(
            fr_arrondissement_rewrite("paris 14e arrondissement rue du hameau 37").as_deref(),
            Some("rue du hameau 37 paris 14e arrondissement")
        );
        assert_eq!(
            fr_arrondissement_rewrite("rue de paris"),
            None,
            "ordinary de + city text is not an arrondissement rewrite"
        );
        assert_eq!(
            fr_arrondissement_rewrite("15e de paris"),
            None,
            "the arrondissement context word is mandatory"
        );
    }

    #[test]
    fn french_arrondissement_postcode_area_is_narrow_and_fail_closed() {
        assert_eq!(
            fr_arrondissement_postcode_area("75015"),
            Some(FrPostcodeArea::Match(
                "paris 15e arrondissement".to_string()
            ))
        );
        assert_eq!(
            fr_arrondissement_postcode_area("paris 75015"),
            Some(FrPostcodeArea::Match(
                "paris 15e arrondissement".to_string()
            ))
        );
        assert_eq!(
            fr_arrondissement_postcode_area("69003 lyon 3eme arrondissement"),
            Some(FrPostcodeArea::Match("lyon 3e arrondissement".to_string()))
        );
        assert_eq!(
            fr_arrondissement_postcode_area("13001 marseille 1er"),
            Some(FrPostcodeArea::Match(
                "marseille 1er arrondissement".to_string()
            ))
        );
        assert_eq!(
            fr_arrondissement_postcode_area("75116 paris"),
            Some(FrPostcodeArea::Match(
                "paris 16e arrondissement".to_string()
            ))
        );
        assert_eq!(
            fr_arrondissement_postcode_area("75015 paris 14e"),
            Some(FrPostcodeArea::Conflict)
        );
        assert_eq!(
            fr_arrondissement_postcode_area("75015 lyon"),
            Some(FrPostcodeArea::Conflict)
        );
        assert_eq!(
            fr_arrondissement_postcode_area("35000 rennes"),
            None,
            "ordinary French postcodes stay on the generic parser path"
        );
        assert_eq!(
            fr_arrondissement_postcode_area("75015 rue de vaugirard"),
            None,
            "a full street query is not swallowed by the area shortcut"
        );
        assert_eq!(
            fr_arrondissement_postcode_area("37 rue du hameau 75015 lyon"),
            Some(FrPostcodeArea::Conflict),
            "a trailing conflicting city on a full address fails closed"
        );
        assert_eq!(
            fr_arrondissement_postcode_area("37 rue du hameau 75015 paris 14e"),
            Some(FrPostcodeArea::Conflict),
            "a trailing conflicting ordinal on a full address fails closed"
        );
        assert_eq!(
            fr_arrondissement_postcode_area("37 rue de lyon 75015 paris"),
            None,
            "a city word inside the street name is not a conflict"
        );
        assert_eq!(
            fr_arrondissement_postcode_area("37 rue du hameau paris 14e 75015"),
            Some(FrPostcodeArea::Conflict),
            "a conflicting area immediately before the postcode fails closed"
        );
        assert!(has_explicit_fr_arrondissement("37 rue du hameau paris 14e"));
        assert!(has_explicit_fr_arrondissement(
            "37 rue du hameau paris 14e arrondissement"
        ));
        assert!(has_explicit_fr_arrondissement(
            "paris 14e arrondissement 37 rue du hameau"
        ));
        assert!(!has_explicit_fr_arrondissement(
            "37 rue de lyon 75015 paris"
        ));
        let matched = |area: &str| Some(FrPostcodeArea::Match(area.to_string()));
        assert_eq!(
            fr_arrondissement_constraint("1 rue du louvre paris 15e"),
            matched("paris 15e arrondissement")
        );
        assert_eq!(
            fr_arrondissement_constraint("37 rue du hameau 75014 paris"),
            matched("paris 14e arrondissement")
        );
        assert_eq!(
            fr_arrondissement_constraint("150 cours lafayette lyon 3e arrondissement"),
            matched("lyon 3e arrondissement")
        );
        assert_eq!(
            fr_arrondissement_constraint("paris 15e rue du hameau 37"),
            matched("paris 15e arrondissement")
        );
        assert_eq!(
            fr_arrondissement_constraint("1 rue du louvre 75116 paris"),
            matched("paris 16e arrondissement")
        );
        assert_eq!(
            fr_arrondissement_constraint(
                "37 rue du hameau 75015 paris 14e arrondissement batiment a"
            ),
            Some(FrPostcodeArea::Conflict),
            "unrelated trailing tokens must not hide contradictory district signals"
        );
        assert_eq!(
            fr_arrondissement_constraint("37 rue de lyon paris"),
            None,
            "ordinary city words in a street do not create a district constraint"
        );
    }

    #[test]
    fn trailing_geographic_qualifier_selects_only_the_distant_homonym_cluster() {
        let dir =
            std::env::temp_dir().join(format!("gridpin-homonym-qualifier-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("homonyms.csv");
        std::fs::write(
            &csv,
            "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n\
             anchor ancora,AN,ancora,,1,,10.00,45.00,Anchor Ancora,Ancora\n\
             anchor bergamo,BG,bergamo,,1,,9.67,45.70,Anchor Bergamo,Bergamo\n\
             anchor centro,CE,centro,,1,,10.75,45.00,Anchor Centro,Centro\n\
             anchor lecce,LE,lecce,,1,,18.17,40.35,Anchor Lecce,Lecce\n\
             anchor macerata,MA,macerata,,1,,13.45,43.30,Anchor Macerata,Macerata\n\
             anchor romano,RO,romano,,1,,12.50,42.00,Anchor Romano,Romano\n\
             anchor torino,TO,torino,,1,,7.69,45.07,Anchor Torino,Torino\n\
             via intera,OR,oriolo romano,,7,,12.14,42.16,Via Intera,Oriolo Romano\n\
             via marker,O1,oriolo,,1,,10.00,45.00,Via Marker,Oriolo\n\
             via marker,O2,oriolo,,1,,18.00,40.00,Via Marker,Oriolo\n\
             via mezzo,M1,mezzo,,14,,10.00,45.00,Via Mezzo,Mezzo\n\
             via mezzo,M2,mezzo,,14,,11.50,45.00,Via Mezzo,Mezzo\n\
             via ponte,P1,ponte,,14,,10.00,45.00,Via Ponte,Ponte\n\
             via ponte,P2,ponte,,14,,10.63,45.00,Via Ponte,Ponte\n\
             via ponte,P3,ponte,,14,,18.00,40.00,Via Ponte,Ponte\n\
             via solo,CS,castro,,8,,18.43,40.01,Via Solo,Castro\n\
             via test,CB,castro,,14,,10.06,45.80,Via Test,Castro\n\
             via test,CS,castro,,14,,18.43,40.01,Via Test,Castro\n\
             vicolo del ponte,MA,macerata,,8,,13.45,43.30,Vicolo Del Ponte,Macerata\n",
        )
        .unwrap();
        let bin = dir.join("homonyms.bin");
        let manifest = dir.join("it-manifest.json");
        std::fs::write(
            &manifest,
            r#"{"country":"it","layer":"addresses","license":"test","source_release":"test"}"#,
        )
        .unwrap();
        crate::builder::build(&csv, &bin, None, None, None, None, Some(&manifest)).unwrap();
        let idx = Index::open(&bin).unwrap();

        let bergamo = idx.query("via test 14 castro bergamo", 2);
        assert_eq!(bergamo.len(), 1);
        assert!((bergamo[0].lat - 45.80).abs() < 0.001);
        assert!(bergamo[0].flags.contains(&"geo_qualifier"));

        let lecce = idx.query("via test 14 castro lecce", 2);
        assert_eq!(lecce.len(), 1);
        assert!((lecce[0].lat - 40.01).abs() < 0.001);

        assert!(
            idx.query("via solo 8 castro bergamo", 2).is_empty(),
            "a street absent from the selected cluster fails closed"
        );
        assert!(
            idx.query("via test 14 castro torino", 2).is_empty(),
            "a qualifier far from every homonym fails closed"
        );
        let full_commune = idx.query("via intera 7 oriolo romano", 1);
        assert_eq!(full_commune.len(), 1);
        assert_eq!(full_commune[0].commune, "Oriolo Romano");
        assert!(
            idx.query("via mezzo 14 mezzo centro", 2).is_empty(),
            "a qualifier midway between remote homonyms is not enough evidence"
        );
        let coherent_cluster = idx.query("via ponte 14 ponte ancora", 3);
        assert_eq!(
            coherent_cluster.len(),
            1,
            "a same-name municipality outside the selected 40 km cluster is removed"
        );
        assert!((coherent_cluster[0].lon - 10.00).abs() < 0.001);
        let street_tail = idx.query("8 vicolo del ponte macerata", 1);
        assert_eq!(street_tail.len(), 1);
        assert_eq!(street_tail[0].commune, "Macerata");

        let no_meta_bin = dir.join("homonyms-no-meta.bin");
        crate::builder::build(&csv, &no_meta_bin, None, None, None, None, None).unwrap();
        let no_meta_idx = Index::open(&no_meta_bin).unwrap();
        assert!(
            no_meta_idx
                .query("via test 14 castro bergamo", 2)
                .iter()
                .all(|hit| !hit.flags.contains(&"geo_qualifier")),
            "an Italy-specific heuristic must not alter a metadata-less custom sheet"
        );
    }

    #[test]
    fn suffix_fallback_and_country_street_names_preserve_explicit_address_parts() {
        let dir = std::env::temp_dir().join(format!(
            "gridpin-suffix-country-guards-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("guards.csv");
        std::fs::write(
            &csv,
            "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n\
             anchor san fratello,SF,san fratello,,1,,14.59,38.02,Anchor San Fratello,San Fratello\n\
             anchor san giuliano terme,SGT,san giuliano terme,,1,,10.44,43.76,Anchor San Giuliano Terme,San Giuliano Terme\n\
             anchor villasalto,VI,villasalto,,1,,9.50,39.49,Anchor Villasalto,Villasalto\n\
             corso italia,GI,giarre,,123,,15.18,37.73,Corso Italia,Giarre\n\
             via falsa,LA,latina,,12,,12.90,41.47,Via Falsa,Latina\n\
             via italia,SGT,san giuliano terme,,7,,10.45,43.77,Via Italia,San Giuliano Terme\n\
             via roma,RM,roma,,1,,12.50,41.90,Via Roma,Roma\n",
        )
        .unwrap();
        let bin = dir.join("guards.bin");
        let manifest = dir.join("it-manifest.json");
        std::fs::write(
            &manifest,
            r#"{"country":"it","layer":"addresses","license":"test","source_release":"test"}"#,
        )
        .unwrap();
        crate::builder::build(&csv, &bin, None, None, None, None, Some(&manifest)).unwrap();
        let idx = Index::open(&bin).unwrap();

        assert!(
            idx.query("via falsa 12 san fratello", 1).is_empty(),
            "a recognized two-token commune must block the trailing-suffix fallback"
        );
        assert!(
            idx.query("via falsa 12 villasalto", 1).is_empty(),
            "a recognized one-token commune must not be dropped for a house elsewhere"
        );

        let italia = idx.query("via italia 7 san giuliano terme", 1);
        assert_eq!(italia.len(), 1);
        assert_eq!(italia[0].street, "Via Italia");
        assert_eq!(italia[0].commune, "San Giuliano Terme");

        let corso_italia = idx.query("corso italia 123 giarre", 1);
        assert_eq!(corso_italia.len(), 1);
        assert_eq!(corso_italia[0].street, "Corso Italia");
        assert_eq!(corso_italia[0].commune, "Giarre");

        let italia_unit = idx.query("via italia 7 scala b san giuliano terme", 1);
        assert_eq!(italia_unit.len(), 1);
        assert_eq!(italia_unit[0].street, "Via Italia");
        assert_eq!(italia_unit[0].precision, "house");

        let roma = idx.query("via roma roma", 1);
        assert_eq!(roma.len(), 1);
        assert_eq!(roma[0].street, "Via Roma");
        assert_eq!(roma[0].commune, "Roma");

        assert_eq!(
            fold_units("torcy france 77200"),
            "torcy 77200",
            "a country noun after an ordinary locality remains removable"
        );
    }

    #[test]
    fn serbian_genitive_variant_forms() {
        // knez -> kneza (title), possessive -ova -> -a (Mihailova -> Mihaila)
        assert_eq!(
            serbian_genitive_variant("knez mihailova").as_deref(),
            Some("kneza mihaila")
        );
        assert_eq!(serbian_genitive_variant("rue de rivoli"), None);
    }

    #[test]
    fn strip_phone_runs_drops_number_runs_but_keeps_a_house_number() {
        // a run of >=4 short numeric tokens is a phone number, stripped
        assert_eq!(
            strip_phone_runs("rue x 1 2 3 4 paris").as_deref(),
            Some("rue x paris")
        );
        // a lone house number is NOT a phone run and must survive (regression: never eat it)
        assert_eq!(strip_phone_runs("rue x 12 paris"), None);
        assert_eq!(strip_phone_runs("12 rue de la paix"), None);
    }

    #[test]
    fn exact_house_is_never_weak() {
        // even at rock-bottom confidence, an exact house is strong
        assert!(!hit_is_weak(&hit(
            "house",
            0.05,
            vec!["street_exact", "house_rep"]
        )));
        assert!(!hit_is_weak(&hit(
            "house",
            0.20,
            vec!["street_exact", "ambiguous_far"]
        )));
    }

    #[test]
    fn weak_cases_are_still_weak() {
        assert!(
            hit_is_weak(&hit("city", 0.9, vec![])),
            "city precision is weak"
        );
        assert!(
            hit_is_weak(&hit("house", 0.10, vec!["street_fuzzy"])),
            "very low confidence is weak"
        );
        assert!(
            hit_is_weak(&hit("street", 0.50, vec!["street_fuzzy"])),
            "fuzzy street below 0.60 is weak"
        );
    }

    #[test]
    fn confident_answers_are_not_weak() {
        assert!(!hit_is_weak(&hit("street", 0.80, vec!["street_exact"])));
        assert!(
            !hit_is_weak(&hit("house", 0.65, vec!["street_fuzzy", "house_rep"])),
            "fuzzy but confident enough"
        );
    }
}
