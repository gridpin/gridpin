//! Index reading (mmap) and the query path: hypothesis-based parsing of the input
//! (digit groups as house-number candidates; compound suffixes via a dictionary),
//! exact lookup + prefix lookup + typo tolerance (Levenshtein automaton).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use fst::automaton::Levenshtein;
use fst::{Automaton, IntoStreamer, Map, Streamer};
use memmap2::Mmap;
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

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

fn is_safe_house_rep(token: &str) -> bool {
    matches!(token, "bis" | "ter" | "quater" | "quinquies" | "sexies")
        || (token.chars().count() == 1 && token.chars().all(char::is_alphabetic))
}

fn compound_house_parts(token: &str) -> Option<(usize, &str)> {
    let digits = token
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    ((1..=4).contains(&digits)
        && token.len() > digits
        && token[digits..].chars().count() <= 4
        && token[digits..].chars().all(char::is_alphanumeric))
    .then_some((digits, &token[digits..]))
}

fn is_five_digit_postcode(token: &str) -> bool {
    token.len() == 5 && token.bytes().all(|byte| byte.is_ascii_digit())
}

/// Test-observer for the two real house-suffix syntax branches.  It uses
/// the same predicates as `build_hyp` and the compound-token hypothesis; an
/// index-specific rep dictionary still decides whether a particular suffix is
/// present in a built sheet.
#[cfg(test)]
pub(crate) fn de_house_suffix_trace(raw: &str) -> Option<String> {
    let normalized = normalize(raw);
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    for token in &tokens {
        if let Some((digits, suffix)) = compound_house_parts(token) {
            return Some(format!(
                "{token} -> numero={} rep={suffix}",
                &token[..digits]
            ));
        }
    }
    for pair in tokens.windows(2) {
        if (1..=4).contains(&pair[0].len())
            && pair[0].bytes().all(|byte| byte.is_ascii_digit())
            && is_safe_house_rep(pair[1])
        {
            return Some(format!(
                "{} {} -> numero={} rep={}",
                pair[0], pair[1], pair[0], pair[1]
            ));
        }
    }
    None
}

/// Test-observer for the exact five-digit branch used by the free-form
/// parser.  Leading zeroes remain present in this normalized token even though
/// lookup later compares the numeric value and returns the stored display form.
#[cfg(test)]
pub(crate) fn de_five_digit_postcode_trace(raw: &str) -> Option<String> {
    normalize(raw)
        .split_whitespace()
        .find(|token| is_five_digit_postcode(token))
        .map(|token| format!("PLZ token preserved: {token}"))
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
    if f.de_street_type {
        v.push("de_street_type");
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
    /// Explainability-only: a DE compound/split street-type variant supplied
    /// the exact FST key.  It is deliberately not an eleventh ranking feature.
    de_street_type: bool,
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
        self.de_street_type |= o.de_street_type;
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
            de_street_type: false,
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

/// Original DE locality evidence retained across the suffix-recovery retry. The retry
/// deliberately removes the unrecognised tail before candidate lookup; without carrying
/// this evidence forward, otherwise identical street/house homonyms fall back to the
/// global anchor or commune prominence.
#[derive(Clone, Copy)]
struct DeRetainedLocality<'a> {
    /// All normalized tokens after the rightmost explicit five-digit postcode.
    postcode_tail: &'a str,
    /// The exact one- or two-token slice removed by the successful c2 retry.
    dropped_tail: &'a str,
    /// Frozen from the original raw DE input before iterating query variants. House-range
    /// and mixed-fraction spellings keep their established variant-quality winner and may
    /// still use retained locality, but must not enter the independent postal-tail rule.
    postal_tail_eligible: bool,
}

fn de_retained_locality<'a>(
    postcode_tail: Option<&'a str>,
    dropped_tail: &'a str,
    postal_tail_eligible: bool,
) -> Option<DeRetainedLocality<'a>> {
    postcode_tail.map(|postcode_tail| DeRetainedLocality {
        postcode_tail,
        dropped_tail,
        postal_tail_eligible,
    })
}

fn de_commune_core(commune: &str) -> String {
    let normalized = normalize(commune);
    let mut words: Vec<&str> = normalized
        .split(' ')
        .filter(|word| !word.is_empty())
        .collect();
    while words.first() == Some(&"stadt") {
        words.remove(0);
    }
    while words.last() == Some(&"stadt") {
        words.pop();
    }
    words.join(" ")
}

fn de_retained_locality_score(postcode_tail: &str, commune: &str) -> u8 {
    let query = normalize(postcode_tail);
    let candidate = de_commune_core(commune);
    if query.is_empty() || candidate.is_empty() {
        return 0;
    }
    if candidate == query {
        return 2;
    }
    if candidate
        .strip_prefix(&query)
        .is_some_and(|suffix| suffix.starts_with(' '))
    {
        1
    } else {
        0
    }
}

fn de_dropped_tail_reaches_commune(dropped_tail: &str, commune: &str) -> bool {
    let commune = de_commune_core(commune);
    let commune_words: std::collections::HashSet<&str> = commune.split(' ').collect();
    normalize(dropped_tail)
        .split(' ')
        .any(|word| word.chars().count() >= 4 && commune_words.contains(word))
}

fn de_locality_qualifiers_match(query_tail: &str, commune: &str) -> bool {
    let query = normalize(query_tail);
    let candidate = de_commune_core(commune);
    matches!(
        (query.as_str(), candidate.as_str()),
        ("reichenbach vogt", "reichenbach im vogtland")
            | ("sankt wendel", "st wendel")
            | ("homburg saar", "homburg")
            | ("kottmar ot eibau", "eibau")
            | ("st peter ording", "sankt peter ording")
            | ("burg auf fehmarn", "fehmarn")
    )
}

/// Exact, postcode-bound relations between a user-facing locality and the
/// indexed postal locality.  These are deliberately triples rather than two
/// independent allowlists: Berlin's city name must never cross-match every
/// district, and the same spelling in another postcode remains unrelated.
const DE_EXACT_LOCALITY_ALIASES: &[(&str, u32, &str)] = &[
    ("brandenburg an der havel", 14770, "brandenburg"),
    ("brandenburg an der havel", 14776, "brandenburg"),
    ("berlin", 13187, "pankow"),
    ("berlin", 13189, "pankow"),
    ("berlin", 13355, "gesundbrunnen"),
    ("berlin", 10317, "rummelsburg"),
    ("berlin", 13129, "blankenburg"),
    ("berlin", 13159, "blankenfelde"),
    ("berlin", 13127, "franzosisch buchholz"),
    ("berlin", 12165, "steglitz"),
    ("berlin", 12159, "friedenau"),
    ("berlin", 14052, "westend"),
    ("werder a d havel", 14542, "werder"),
    ("petershagen eggersdorf", 15345, "eggersdorf"),
    ("landkirchen", 23769, "fehmarn"),
    ("berlin kaulsdorf", 12621, "kaulsdorf"),
    ("zerkwitz", 3222, "lubbenau"),
    ("schwedt oder", 16303, "schwedt"),
    ("konigstein taunus", 61462, "konigstein im taunus"),
    ("freiburg breisgau", 79104, "freiburg im breisgau"),
    ("freiburg", 79115, "freiburg im breisgau"),
    ("oelsnitz vogtland", 8606, "oelsnitz vogtl"),
    ("frankenberg sachsen", 9669, "frankenberg sa"),
    ("lutherstadt wittenberg", 6886, "wittenberg"),
    ("wittenberg lutherstadt", 6886, "wittenberg"),
    ("weilheim teck", 73235, "weilheim an der teck"),
    ("berlin", 10587, "charlottenburg"),
    ("berlin", 10783, "schoneberg"),
    ("berlin", 13627, "charlottenburg nord"),
    ("berlin", 14059, "charlottenburg"),
    ("berlin", 14195, "lichterfelde"),
];

fn de_exact_locality_alias_matches(query_tail: &str, postcode: u32, commune: &str) -> bool {
    let query = normalize(query_tail);
    let candidate = de_commune_core(commune);
    DE_EXACT_LOCALITY_ALIASES
        .iter()
        .any(|(expected_query, expected_postcode, expected_commune)| {
            query == *expected_query
                && postcode == *expected_postcode
                && candidate == *expected_commune
        })
}

fn de_is_exact_locality_alias_query(query_tail: &str, postcode: u32) -> bool {
    let query = normalize(query_tail);
    DE_EXACT_LOCALITY_ALIASES
        .iter()
        .any(|(expected_query, expected_postcode, _)| {
            query == *expected_query && postcode == *expected_postcode
        })
}

fn de_is_berlin_postal_locality(commune: &str) -> bool {
    matches!(
        de_commune_core(commune).as_str(),
        "berlin"
            | "adlershof"
            | "charlottenburg"
            | "dahlem"
            | "kaulsdorf"
            | "kreuzberg"
            | "marienfelde"
            | "mitte"
            | "neukolln"
            | "niederschoneweide"
            | "nikolassee"
            | "tempelhof"
            | "wilmersdorf"
    )
}

fn de_is_proven_berlin_postcode(postcode: u32) -> bool {
    matches!(
        postcode,
        10115
            | 10117
            | 10178
            | 10589
            | 10715
            | 10717
            | 10969
            | 12043
            | 12101
            | 12277
            | 12439
            | 12489
            | 12621
            | 14129
            | 14195
    )
}

#[cfg(test)]
thread_local! {
    static DE_PREFIX_DROP_GUARD_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DE_ABBREVIATION_GUARD_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DE_POSTCODE_HOUSE_RESCUE_HOUSE_DECODES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DE_POSTCODE_HOUSE_RESCUE_FUZZY_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DE_POSTCODE_HOUSE_RESCUE_SUBSET_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT: std::cell::Cell<usize> = const { std::cell::Cell::new(DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT_DEFAULT) };
    static DE_P4_POSTCODE_BUCKET_SCAN_ROWS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DE_P4_POSTCODE_BUCKET_MATCHING_SIDS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DE_BLANK_POSTCODE_HOUSE_SCAN_ROWS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DE_COUNTRY_VARIANT_PREPARED_SEARCH_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Hard ceiling for the one narrow, post-failure exact-key scan. Common German street names can
/// exceed 4,000 locality postings; ordinary queries retain their 300-row cap, while this path
/// proves uniqueness or fails closed on the first row beyond this bound.
const DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT_DEFAULT: usize = 8192;

fn de_postcode_house_rescue_scan_limit() -> usize {
    #[cfg(test)]
    {
        DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT_DEFAULT
    }
}

fn de_prefix_drop_preserves_postcode_locality(
    hit: &Hit,
    features: &[f32; N_FEATS],
    postcode: u32,
    postcode_tail: &str,
) -> bool {
    #[cfg(test)]
    DE_PREFIX_DROP_GUARD_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
    Feats::from_vec(features).pc_exact
        || Index::postcode_numeric_prefix(&hit.postcode) == Some(postcode)
        || de_retained_locality_score(postcode_tail, &hit.commune) != 0
}

fn de_postcode_context(q: &str) -> Option<(u32, String)> {
    let tokens: Vec<&str> = q.split(' ').filter(|token| !token.is_empty()).collect();
    let position = tokens
        .iter()
        .rposition(|token| is_five_digit_postcode(token))?;
    let postcode = tokens[position].parse().ok()?;
    let tail = tokens[position + 1..].join(" ");
    (!tail.is_empty()).then_some((postcode, tail))
}

/// The exact normalization performed before every prepared lookup. Keeping this
/// in one helper lets the DE variant loop identify equivalent raw/base variants
/// without drifting from `query_feats_d` itself.
fn prepared_query_key(raw: &str) -> String {
    expand_two_token(&fold_units(&crate::norm::fold_homoglyphs(&normalize(raw))))
}

fn de_house_range_separator(address: &str) -> bool {
    address.char_indices().any(|(position, character)| {
        if !matches!(character, '-' | '–' | '—') {
            return false;
        }
        let left_endpoint = address[..position].split_whitespace().next_back();
        let right_endpoint = address[position + character.len_utf8()..]
            .split_whitespace()
            .next();
        left_endpoint.is_some_and(|endpoint| endpoint.chars().any(|ch| ch.is_ascii_digit()))
            && right_endpoint.is_some_and(|endpoint| endpoint.chars().any(|ch| ch.is_ascii_digit()))
    })
}

fn de_capital_prior_candidate_allowed(
    country: Option<&str>,
    sorted_top_features: &[f32; N_FEATS],
    candidate_precision: &str,
) -> bool {
    // A weak cityless capital anchor may resolve equal-quality homonyms, but
    // it must not replace an already exact German house with a snapped
    // neighbour carrying a different number.  Interpolation remains eligible:
    // it can be the only useful address on a sparse street and has an explicit
    // regression sentinel in the tests below.
    !(country == Some("de")
        && candidate_precision == "near"
        && Feats::from_vec(sorted_top_features).house_exact_rep)
}

fn de_comma_postcode_house_rescue_query(raw: &str) -> Option<(String, u32, String)> {
    if !crate::de::postal_tail_eligible(raw) {
        return None;
    }
    // Parenthetical building labels with slash locality qualifiers belong to
    // the audited P4 parser. Letting this generic comma rescue strip them
    // would bypass P4's exact-core and fill-empty admission contract.
    if de_parenthetical_locality_uses_slash_qualifier(raw) {
        return None;
    }
    let mut segments = raw.split(',');
    let address = segments.next()?.trim();
    let locality = segments.next()?.trim();
    let country = segments.next().map(str::trim);
    if address.is_empty() || locality.is_empty() || segments.next().is_some() {
        return None;
    }
    if country.is_some_and(|country| {
        let country = normalize(country);
        !matches!(country.as_str(), "deutschland" | "germany")
    }) {
        return None;
    }
    if address.contains('/') || de_house_range_separator(address) {
        return None;
    }
    let locality = normalize(locality);
    let mut tokens = locality.split_whitespace();
    let postcode_token = tokens.next()?;
    if !is_five_digit_postcode(postcode_token) {
        return None;
    }
    let locality_tail = tokens.collect::<Vec<_>>().join(" ");
    if locality_tail.is_empty() {
        return None;
    }
    let normalized_address = normalize(address);
    if !normalized_address
        .split_whitespace()
        .any(|token| token.bytes().any(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    Some((
        format!("{address} {postcode_token}"),
        postcode_token.parse().ok()?,
        locality_tail,
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeCompactHousePairSpec {
    query: String,
    postcode: u32,
    locality_tail: String,
    effect: crate::de::Effect,
    left: u32,
    right: u32,
}

/// Parse one compact, literal two-endpoint house set immediately before an
/// explicit five-digit postcode.  The strict Wave-A path proves both numbers
/// on one runtime street id; therefore even a small slash pair such as `1/3`
/// is no longer guessed from the left endpoint alone.
fn de_compact_house_pair_spec(raw: &str) -> Option<DeCompactHousePairSpec> {
    let bytes = raw.as_bytes();
    let mut parsed = None;
    for start in 0..bytes.len().saturating_sub(4) {
        if !bytes[start..start + 5].iter().all(u8::is_ascii_digit)
            || start
                .checked_sub(1)
                .is_some_and(|left| bytes[left].is_ascii_digit())
            || bytes.get(start + 5).is_some_and(u8::is_ascii_digit)
        {
            continue;
        }
        let postcode_token = &raw[start..start + 5];
        let locality_raw = raw[start + 5..].trim();
        if locality_raw.is_empty()
            || !locality_raw.chars().any(char::is_alphabetic)
            || locality_raw.chars().any(|character| {
                character.is_ascii_digit() || matches!(character, ',' | ';' | '|' | '\n' | '\r')
            })
        {
            continue;
        }
        let before_postcode = raw[..start].trim_end();
        let address = before_postcode
            .strip_suffix(',')
            .unwrap_or(before_postcode)
            .trim_end();
        if address.is_empty()
            || address
                .chars()
                .any(|character| matches!(character, ',' | ';' | '|' | '\n' | '\r'))
        {
            continue;
        }
        let Some(split) = address.rfind(char::is_whitespace) else {
            continue;
        };
        let street = address[..split].trim_end();
        let pair = address[split..].trim();
        if street.is_empty() || !street.chars().any(char::is_alphabetic) {
            continue;
        }
        let mut separator = None;
        for (position, character) in pair.char_indices() {
            if matches!(character, '-' | '–' | '—' | '/') {
                if separator.is_some() {
                    separator = None;
                    break;
                }
                separator = Some((position, character));
            }
        }
        let Some((position, separator)) = separator else {
            continue;
        };
        let left_raw = &pair[..position];
        let right_raw = &pair[position + separator.len_utf8()..];
        if left_raw.is_empty()
            || right_raw.is_empty()
            || left_raw.len() > 4
            || right_raw.len() > 4
            || !left_raw.bytes().all(|byte| byte.is_ascii_digit())
            || !right_raw.bytes().all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let left: u32 = left_raw.parse().ok()?;
        let right: u32 = right_raw.parse().ok()?;
        if left == 0 || left >= right {
            continue;
        }
        let effect = if separator == '/' {
            crate::de::Effect::HouseSlash
        } else {
            crate::de::Effect::HouseRange
        };
        let locality = normalize(locality_raw);
        if locality.is_empty() {
            continue;
        }
        let candidate = DeCompactHousePairSpec {
            query: format!("{street} {left_raw} {postcode_token}"),
            postcode: postcode_token.parse().ok()?,
            locality_tail: locality,
            effect,
            left,
            right,
        };
        if parsed.replace(candidate).is_some() {
            return None;
        }
    }
    parsed
}

/// Legacy left-endpoint fallback remains deliberately conservative for small
/// slash pairs.  Wave A uses `de_compact_house_pair_spec` directly and admits
/// those pairs only after proving the complete set against the runtime index.
fn de_compact_house_pair_left_rescue_query(
    raw: &str,
) -> Option<(String, u32, String, crate::de::Effect)> {
    let spec = de_compact_house_pair_spec(raw)?;
    if spec.effect == crate::de::Effect::HouseSlash && (spec.left < 10 || spec.right < 10) {
        return None;
    }
    Some((spec.query, spec.postcode, spec.locality_tail, spec.effect))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeStrictSourceStreetTypoSpec {
    normalized_street: String,
    normalized_locality: String,
    house_number: u32,
    postcode: u32,
    postcode_raw: String,
}

/// Product-field normalization used by the frozen Germany diagnostic roster.
/// It intentionally mirrors the offline analyzer instead of the broader search
/// normalizer: NFKC/lowercase, German umlauts to digraphs, `ß` to `ss`, and
/// ASCII alphanumeric words only.  Retrieval may use broader variants, but an
/// admission predicate must compare this single projection on both sides.
fn de_product_normalize_text(raw: &str) -> String {
    let mut normalized = String::with_capacity(raw.len());
    let mut separated = true;
    for character in raw.nfkc().flat_map(char::to_lowercase) {
        let replacement = match character {
            'ä' => Some("ae"),
            'ö' => Some("oe"),
            'ü' => Some("ue"),
            'ß' => Some("ss"),
            _ => None,
        };
        if let Some(replacement) = replacement {
            normalized.push_str(replacement);
            separated = false;
        } else if character.is_ascii_alphanumeric() {
            normalized.push(character);
            separated = false;
        } else if !separated && !normalized.is_empty() {
            normalized.push(' ');
            separated = true;
        }
    }
    normalized.trim_end().to_string()
}

fn de_product_normalize_street(raw: &str) -> String {
    let mut words: Vec<String> = de_product_normalize_text(raw)
        .split_whitespace()
        .map(|word| {
            match word {
                "str" | "strasse" => "strasse",
                "pl" => "platz",
                "al" => "allee",
                "uf" => "ufer",
                "wg" => "weg",
                "g" => "gasse",
                "ch" => "chaussee",
                _ => word,
            }
            .to_string()
        })
        .collect();
    if let Some(last) = words.last_mut() {
        if last != "str" && last.ends_with("str") {
            last.truncate(last.len() - 3);
            last.push_str("strasse");
        }
    }
    words.join(" ")
}

/// The audited c/o syntax contains one fused compound street token.  Split it
/// only at a terminal product street-type word; arbitrary byte splits would
/// turn retrieval hypotheses into product predicate inputs.
fn de_product_normalize_compound_street(raw: &str) -> Option<String> {
    let compact = de_product_normalize_street(raw);
    if compact.is_empty() || compact.contains(' ') || compact.len() > 96 {
        return None;
    }
    for suffix in [
        "chaussee", "strasse", "allee", "gasse", "platz", "ring", "ufer", "weg",
    ] {
        let Some(prefix) = compact.strip_suffix(suffix) else {
            continue;
        };
        if prefix.len() >= 3 {
            return Some(format!("{prefix} {suffix}"));
        }
    }
    None
}

/// Product-visible German street lookup forms for the typed P4 surfaces.
/// These variants are a bounded locator, never admission fields: P4 separately
/// compares one canonical query projection with the canonical display street.
fn de_product_street_forms(raw_street: &str) -> Vec<String> {
    let mut forms = Vec::new();
    let mut seen = HashSet::new();
    for variant in crate::de::query_variants(raw_street) {
        let base = normalize(&variant.query).replace('ß', "ss");
        if base.is_empty()
            || base.len() > 96
            || !base.is_ascii()
            || base.chars().any(|character| character.is_ascii_digit())
        {
            continue;
        }
        if seen.insert(base.clone()) {
            forms.push(base.clone());
        }
        let words: Vec<&str> = base.split_whitespace().collect();
        if let Some(last) = words.last() {
            if *last == "str" && words.len() > 1 {
                let mut expanded = words[..words.len() - 1].join(" ");
                expanded.push_str(" strasse");
                if seen.insert(expanded.clone()) {
                    forms.push(expanded);
                }
            } else if let Some(prefix) = last.strip_suffix("str") {
                if prefix.chars().count() >= 2 {
                    let mut joined = words[..words.len() - 1].join(" ");
                    if !joined.is_empty() {
                        joined.push(' ');
                    }
                    joined.push_str(prefix);
                    joined.push_str("strasse");
                    if seen.insert(joined.clone()) {
                        forms.push(joined);
                    }
                    let mut split = words[..words.len() - 1].join(" ");
                    if !split.is_empty() {
                        split.push(' ');
                    }
                    split.push_str(prefix);
                    split.push_str(" strasse");
                    if seen.insert(split.clone()) {
                        forms.push(split);
                    }
                }
            }
        }
        for street_variant in crate::de::street_variants(&base) {
            let street_variant = street_variant.replace('ß', "ss");
            if street_variant.len() <= 96
                && street_variant.is_ascii()
                && seen.insert(street_variant.clone())
            {
                forms.push(street_variant);
            }
        }
    }
    let canonical = de_product_normalize_street(raw_street);
    if !canonical.is_empty()
        && canonical.len() <= 96
        && canonical.is_ascii()
        && seen.insert(canonical.clone())
    {
        forms.push(canonical);
    }
    forms
}

/// Exact raw P3 surface: `street integer, five-digit-postcode locality`.
/// Suffixes, ranges, street digits and delivery noise deliberately have no
/// interpretation in this path.
fn de_strict_source_street_typo_spec(raw: &str) -> Option<DeStrictSourceStreetTypoSpec> {
    if raw.len() > 256
        || raw.trim() != raw
        || raw
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\t' | ';' | '|'))
    {
        return None;
    }
    let mut fields = raw.split(',');
    let address = fields.next()?.trim();
    let terminal = fields.next()?.trim();
    if address.is_empty() || terminal.is_empty() || fields.next().is_some() {
        return None;
    }
    let split = address.rfind(char::is_whitespace)?;
    let street = address[..split].trim_end();
    let house_raw = address[split..].trim();
    if street.is_empty()
        || !street.chars().any(char::is_alphabetic)
        || street.chars().any(|character| character.is_ascii_digit())
        || house_raw.is_empty()
        || house_raw.len() > 4
        || house_raw.as_bytes().first() == Some(&b'0')
        || !house_raw.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let house_number = house_raw
        .parse::<u32>()
        .ok()
        .filter(|number| *number != 0)?;
    let mut terminal_tokens = terminal.split_whitespace();
    let postcode_raw = terminal_tokens.next()?;
    if !is_five_digit_postcode(postcode_raw) {
        return None;
    }
    let locality_raw = terminal_tokens.collect::<Vec<_>>().join(" ");
    if locality_raw.is_empty()
        || !locality_raw.chars().any(char::is_alphabetic)
        || locality_raw
            .chars()
            .any(|character| character.is_ascii_digit())
    {
        return None;
    }
    let normalized_street = de_product_normalize_street(street);
    let normalized_locality = de_product_normalize_text(&locality_raw);
    if normalized_street.is_empty() || normalized_locality.is_empty() {
        return None;
    }
    Some(DeStrictSourceStreetTypoSpec {
        normalized_street,
        normalized_locality,
        house_number,
        postcode: postcode_raw.parse().ok()?,
        postcode_raw: postcode_raw.to_string(),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeStreetLocalityQualifierSpec {
    normalized_street: String,
    normalized_locality: String,
    house_token: String,
    postcode_raw: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeBlankPostcodeHouseSpec {
    normalized_street: String,
    normalized_locality: String,
    house_number: u32,
    house_suffix: String,
    postcode: u32,
    postcode_raw: String,
}

/// Exact Wave X2 surface: `street integer[suffix], five-digit-postcode locality`.
/// It is deliberately independent of P3: admitting a one-letter suffix here
/// must not widen the audited source-street typo mechanism.
fn de_street_locality_qualifier_spec(raw: &str) -> Option<DeStreetLocalityQualifierSpec> {
    if raw.len() > 256
        || raw.trim() != raw
        || raw
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\t' | ';' | '|'))
    {
        return None;
    }
    let mut fields = raw.split(',');
    let address = fields.next()?.trim();
    let terminal = fields.next()?.trim();
    if address.is_empty() || terminal.is_empty() || fields.next().is_some() {
        return None;
    }
    let split = address.rfind(char::is_whitespace)?;
    let street = address[..split].trim_end();
    let house_raw = address[split..].trim();
    let digit_len = house_raw
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let (house_number, house_suffix) = house_raw.split_at(digit_len);
    if street.is_empty()
        || !street.chars().any(char::is_alphabetic)
        || street.chars().any(|character| character.is_ascii_digit())
        || house_number.is_empty()
        || house_number.len() > 4
        || house_number.as_bytes().first() == Some(&b'0')
        || !house_number.bytes().all(|byte| byte.is_ascii_digit())
        || house_number
            .parse::<u32>()
            .ok()
            .is_none_or(|number| number == 0)
        || house_suffix.len() > 1
        || !house_suffix.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        return None;
    }
    let mut terminal_tokens = terminal.split_whitespace();
    let postcode_raw = terminal_tokens.next()?;
    if !is_five_digit_postcode(postcode_raw) {
        return None;
    }
    let locality_raw = terminal_tokens.collect::<Vec<_>>().join(" ");
    if locality_raw.is_empty()
        || !locality_raw.chars().any(char::is_alphabetic)
        || locality_raw
            .chars()
            .any(|character| character.is_ascii_digit())
    {
        return None;
    }
    let normalized_street = de_product_normalize_street(street);
    let normalized_locality = de_product_normalize_text(&locality_raw);
    if normalized_street.is_empty() || normalized_locality.is_empty() {
        return None;
    }
    Some(DeStreetLocalityQualifierSpec {
        normalized_street,
        normalized_locality,
        house_token: format!("{}{}", house_number, house_suffix.to_ascii_lowercase()),
        postcode_raw: postcode_raw.to_string(),
    })
}

/// Exact Wave O surface: `street integer[suffix], five-digit-postcode locality`.
/// The parser is intentionally disjoint from range/compound/delivery cleanup:
/// P5 may only arbitrate one literal address whose original fields are already
/// structurally complete.
fn de_blank_postcode_house_spec(raw: &str) -> Option<DeBlankPostcodeHouseSpec> {
    if raw.contains(['(', ')', '/', '\\']) {
        return None;
    }
    let parsed = de_street_locality_qualifier_spec(raw)?;
    let digit_len = parsed
        .house_token
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let (house_number, house_suffix) = parsed.house_token.split_at(digit_len);
    if house_number.is_empty()
        || house_suffix.len() > 1
        || !house_suffix.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        return None;
    }
    Some(DeBlankPostcodeHouseSpec {
        normalized_street: parsed.normalized_street,
        normalized_locality: parsed.normalized_locality,
        house_number: house_number.parse().ok()?,
        house_suffix: house_suffix.to_string(),
        postcode: parsed.postcode_raw.parse().ok()?,
        postcode_raw: parsed.postcode_raw,
    })
}

/// Parse only a terminal source display qualifier: `base (locality)`.
/// Nested parentheses, empty parts and any trailing text fail closed.
fn de_source_street_locality_qualifier(display: &str) -> Option<(String, String)> {
    if display.is_empty() || display.len() > 192 || display.trim() != display {
        return None;
    }
    let without_close = display.strip_suffix(')')?;
    let (base, qualifier) = without_close.rsplit_once(" (")?;
    if base.is_empty()
        || qualifier.is_empty()
        || base.contains(['(', ')'])
        || qualifier.contains(['(', ')'])
        || qualifier.chars().any(char::is_control)
    {
        return None;
    }
    let normalized_base = de_product_normalize_street(base);
    let normalized_qualifier = de_product_normalize_text(qualifier);
    if normalized_base.is_empty() || normalized_qualifier.is_empty() {
        return None;
    }
    Some((normalized_base, normalized_qualifier))
}

fn de_house_token_matches(hit: &Hit, expected: &str) -> bool {
    hit.housenumber
        .as_deref()
        .is_some_and(|house| de_product_normalize_text(house).replace(' ', "") == expected)
}

fn de_locality_is_exact_or_query_prefix(query: &str, source: &str) -> bool {
    query == source
        || source
            .strip_prefix(query)
            .is_some_and(|tail| tail.starts_with(' '))
}

fn de_compact_osa_distance(left: &str, right: &str) -> usize {
    let left: Vec<u8> = left
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let right: Vec<u8> = right
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if left.len().abs_diff(right.len()) > 2 {
        return 3;
    }
    let mut matrix = vec![vec![0usize; right.len() + 1]; left.len() + 1];
    for (index, row) in matrix.iter_mut().enumerate() {
        row[0] = index;
    }
    for (index, cell) in matrix[0].iter_mut().enumerate() {
        *cell = index;
    }
    for i in 1..=left.len() {
        for j in 1..=right.len() {
            let substitution = usize::from(left[i - 1] != right[j - 1]);
            let mut distance = (matrix[i - 1][j] + 1)
                .min(matrix[i][j - 1] + 1)
                .min(matrix[i - 1][j - 1] + substitution);
            if i > 1 && j > 1 && left[i - 1] == right[j - 2] && left[i - 2] == right[j - 1] {
                distance = distance.min(matrix[i - 2][j - 2] + 1);
            }
            matrix[i][j] = distance;
        }
    }
    matrix[left.len()][right.len()]
}

/// Python `SequenceMatcher(None, a, b).ratio()` for the bounded (<100 byte),
/// no-junk ASCII street strings admitted above.  Python's auto-junk branch is
/// inactive below 200 items, so recursively summing the earliest longest
/// contiguous matching blocks is equivalent and deterministic here.
fn de_sequence_matcher_ratio_at_least_090(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.is_empty() || right.is_empty() || left.len() > 96 || right.len() > 96 {
        return false;
    }
    let mut matched = 0usize;
    let mut pending = vec![(0usize, left.len(), 0usize, right.len())];
    while let Some((left_lo, left_hi, right_lo, right_hi)) = pending.pop() {
        let mut previous = vec![0usize; right_hi - right_lo + 1];
        let mut best = (left_lo, right_lo, 0usize);
        for (left_index, left_byte) in left.iter().enumerate().take(left_hi).skip(left_lo) {
            let mut current = vec![0usize; right_hi - right_lo + 1];
            for (right_index, right_byte) in right.iter().enumerate().take(right_hi).skip(right_lo)
            {
                if left_byte != right_byte {
                    continue;
                }
                let offset = right_index - right_lo + 1;
                current[offset] = previous[offset - 1] + 1;
                let length = current[offset];
                let left_start = left_index + 1 - length;
                let right_start = right_index + 1 - length;
                if length > best.2
                    || (length == best.2
                        && (left_start < best.0 || (left_start == best.0 && right_start < best.1)))
                {
                    best = (left_start, right_start, length);
                }
            }
            previous = current;
        }
        if best.2 == 0 {
            continue;
        }
        matched += best.2;
        if left_lo < best.0 && right_lo < best.1 {
            pending.push((left_lo, best.0, right_lo, best.1));
        }
        let left_after = best.0 + best.2;
        let right_after = best.1 + best.2;
        if left_after < left_hi && right_after < right_hi {
            pending.push((left_after, left_hi, right_after, right_hi));
        }
    }
    matched.saturating_mul(20) >= 9usize.saturating_mul(left.len() + right.len())
}

fn de_p3_locality_compatible(query: &str, source: &str) -> bool {
    query == source
        || query
            .strip_prefix(source)
            .is_some_and(|tail| tail.starts_with(' '))
        || source
            .strip_prefix(query)
            .is_some_and(|tail| tail.starts_with(' '))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeAuditedCompoundShape {
    VenueCommaHouseCommaStreet,
    StreetHouseBalancedVenueParenthetical,
    StreetHouseRangeBalancedAccessParenthetical,
    CareOfPrefixThenStreetHouseLocality,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeAuditedCompoundSpec {
    shape: DeAuditedCompoundShape,
    street: String,
    normalized_street: String,
    primary_house: u32,
    additional_houses: Vec<u32>,
    postcode: u32,
    postcode_raw: String,
    normalized_locality: String,
}

fn de_audited_postcode_locality(value: &str) -> Option<(u32, String, String)> {
    let value = value.trim();
    let boundary = value.find(char::is_whitespace)?;
    let postcode_raw = &value[..boundary];
    let locality_raw = value[boundary..].trim();
    if !is_five_digit_postcode(postcode_raw)
        || locality_raw.is_empty()
        || !locality_raw.chars().any(char::is_alphabetic)
        || locality_raw
            .chars()
            .any(|character| character.is_ascii_digit() || matches!(character, ',' | ';' | '|'))
    {
        return None;
    }
    Some((
        postcode_raw.parse().ok()?,
        postcode_raw.to_string(),
        locality_raw.to_string(),
    ))
}

fn de_audited_locality(raw: &str) -> Option<String> {
    match raw.matches('/').count() {
        0 => {
            let locality = de_product_normalize_text(raw);
            (!locality.is_empty()).then_some(locality)
        }
        1 => {
            let (left, right) = raw.split_once('/')?;
            let left = de_product_normalize_text(left);
            let right = de_product_normalize_text(right);
            if left.is_empty() || right.is_empty() {
                return None;
            }
            // Closed, product-visible German abbreviations from the audited
            // syntaxes.  Never invent both prepositions and let source data
            // choose which query-locality field supposedly existed.
            let connector = match right.as_str() {
                "breisgau" => "im",
                "teck" => "an der",
                _ => return None,
            };
            Some(format!("{left} {connector} {right}"))
        }
        _ => None,
    }
}

fn de_audited_parenthetical_spec(raw: &str) -> Option<DeAuditedCompoundSpec> {
    if raw.chars().filter(|character| *character == '(').count() != 1
        || raw.chars().filter(|character| *character == ')').count() != 1
    {
        return None;
    }
    let close = raw.rfind("),")?;
    let open = raw[..close].rfind('(')?;
    if open == 0 || !raw[..open].ends_with(char::is_whitespace) {
        return None;
    }
    if !raw
        .get(close + 2..)
        .and_then(|tail| tail.chars().next())
        .is_some_and(char::is_whitespace)
    {
        return None;
    }
    let label = raw[open + 1..close].trim();
    if label.is_empty() || label.len() > 80 || !label.chars().any(char::is_alphabetic) {
        return None;
    }
    let address = raw[..open].trim_end();
    if address.is_empty()
        || address.trim_start() != address
        || address
            .chars()
            .any(|character| matches!(character, ',' | ';' | '|'))
    {
        return None;
    }
    let split = address.rfind(char::is_whitespace)?;
    let street = address[..split].trim_end();
    let house_expression = address[split..].trim();
    if street.is_empty()
        || !street.chars().any(char::is_alphabetic)
        || street.chars().any(|character| character.is_ascii_digit())
    {
        return None;
    }
    let (shape, primary_house, additional_houses) =
        if let Some((left, right)) = house_expression.split_once('/') {
            if house_expression.matches('/').count() != 1
                || left.as_bytes().first() == Some(&b'0')
                || right.as_bytes().first() == Some(&b'0')
                || normalize(label)
                    .split_whitespace()
                    .next()
                    .is_none_or(|word| word != "aufgang")
            {
                return None;
            }
            let left = left.parse::<u32>().ok().filter(|number| *number != 0)?;
            let right = right.parse::<u32>().ok().filter(|number| *number > left)?;
            (
                DeAuditedCompoundShape::StreetHouseRangeBalancedAccessParenthetical,
                left,
                vec![right],
            )
        } else {
            if house_expression.is_empty()
                || house_expression.len() > 4
                || house_expression.as_bytes().first() == Some(&b'0')
                || !house_expression.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            (
                DeAuditedCompoundShape::StreetHouseBalancedVenueParenthetical,
                house_expression
                    .parse::<u32>()
                    .ok()
                    .filter(|number| *number != 0)?,
                Vec::new(),
            )
        };
    let (postcode, postcode_raw, locality_raw) =
        de_audited_postcode_locality(raw[close + 2..].trim_start())?;
    let normalized_street = de_product_normalize_street(street);
    let normalized_locality = de_audited_locality(&locality_raw)?;
    if normalized_street.is_empty() {
        return None;
    }
    Some(DeAuditedCompoundSpec {
        shape,
        street: street.to_string(),
        normalized_street,
        primary_house,
        additional_houses,
        postcode,
        postcode_raw,
        normalized_locality,
    })
}

fn de_parenthetical_locality_uses_slash_qualifier(raw: &str) -> bool {
    crate::de::parenthetical_subaddress_commune(raw).is_some()
        && raw
            .rsplit_once("),")
            .is_some_and(|(_, locality)| locality.contains('/'))
}

fn de_audited_venue_spec(raw: &str) -> Option<DeAuditedCompoundSpec> {
    let fields: Vec<&str> = raw.split(',').map(str::trim).collect();
    if fields.len() != 8
        || fields.iter().any(|field| {
            field.is_empty()
                || field.len() > 96
                || field
                    .chars()
                    .any(|character| matches!(character, '\n' | '\r' | '\t' | ';' | '|'))
        })
        || fields[0]
            .chars()
            .any(|character| character.is_ascii_digit())
        || !fields[0].chars().any(char::is_alphabetic)
        || fields[1].is_empty()
        || fields[1].len() > 4
        || fields[1].as_bytes().first() == Some(&b'0')
        || !fields[1].bytes().all(|byte| byte.is_ascii_digit())
        || fields[2]
            .chars()
            .any(|character| character.is_ascii_digit())
        || !fields[2].chars().any(char::is_alphabetic)
    {
        return None;
    }
    let postcode_raw = *fields.last()?;
    if !is_five_digit_postcode(postcode_raw) {
        return None;
    }
    if fields[3..7].iter().any(|field| {
        !field.chars().any(char::is_alphabetic)
            || field.chars().any(|character| character.is_ascii_digit())
    }) {
        return None;
    }
    let normalized_street = de_product_normalize_street(fields[2]);
    // In the exact eight-field venue grammar, the third field before the
    // terminal postcode is the city; the surrounding fields are administrative
    // qualifiers and are never alternative query localities.
    let normalized_locality = de_product_normalize_text(fields[5]);
    if normalized_street.is_empty() || normalized_locality.is_empty() {
        return None;
    }
    Some(DeAuditedCompoundSpec {
        shape: DeAuditedCompoundShape::VenueCommaHouseCommaStreet,
        street: fields[2].to_string(),
        normalized_street,
        primary_house: fields[1].parse().ok().filter(|number| *number != 0)?,
        additional_houses: Vec::new(),
        postcode: postcode_raw.parse().ok()?,
        postcode_raw: postcode_raw.to_string(),
        normalized_locality,
    })
}

fn de_audited_care_of_spec(raw: &str) -> Option<DeAuditedCompoundSpec> {
    if raw.len() > 384
        || !raw
            .get(..3)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("c/o"))
        || !raw
            .get(3..)
            .and_then(|tail| tail.chars().next())
            .is_some_and(char::is_whitespace)
    {
        return None;
    }
    let mut fields = raw.split(',');
    let left = fields.next()?.trim();
    let terminal = fields.next()?.trim();
    if fields.next().is_some() {
        return None;
    }
    let (postcode, postcode_raw, locality_raw) = de_audited_postcode_locality(terminal)?;
    let locality = de_product_normalize_text(&locality_raw);
    let left = de_product_normalize_text(left.get(3..)?.trim_start());
    let locality_suffix = format!(" {locality}");
    let before_locality = left.strip_suffix(&locality_suffix)?.trim_end();
    let mut tokens: Vec<&str> = before_locality.split_whitespace().collect();
    let house_raw = tokens.pop()?;
    let street = tokens.pop()?;
    if tokens.is_empty()
        || street.is_empty()
        || !street.chars().any(char::is_alphabetic)
        || house_raw.is_empty()
        || house_raw.len() > 4
        || house_raw.as_bytes().first() == Some(&b'0')
        || !house_raw.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    if tokens
        .iter()
        .any(|token| token.chars().any(|character| character.is_ascii_digit()))
    {
        return None;
    }
    let normalized_street = de_product_normalize_compound_street(street)?;
    Some(DeAuditedCompoundSpec {
        shape: DeAuditedCompoundShape::CareOfPrefixThenStreetHouseLocality,
        street: street.to_string(),
        normalized_street,
        primary_house: house_raw.parse().ok().filter(|number| *number != 0)?,
        additional_houses: Vec::new(),
        postcode,
        postcode_raw,
        normalized_locality: locality,
    })
}

fn de_audited_compound_spec(raw: &str) -> Option<DeAuditedCompoundSpec> {
    if raw.len() > 512
        || raw.trim() != raw
        || raw
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\t' | ';' | '|'))
    {
        return None;
    }
    let mut matches = Vec::new();
    if let Some(spec) = de_audited_parenthetical_spec(raw) {
        matches.push(spec);
    }
    if let Some(spec) = de_audited_venue_spec(raw) {
        matches.push(spec);
    }
    if let Some(spec) = de_audited_care_of_spec(raw) {
        matches.push(spec);
    }
    (matches.len() == 1).then(|| matches.pop().unwrap())
}

#[cfg(test)]
fn de_postcode_tail(q: &str) -> Option<String> {
    de_postcode_context(q).map(|(_, tail)| tail)
}

type RankedHit = (Hit, [f32; N_FEATS], i32, u32, u32, u32);

struct DePostcodeHouseCandidate {
    source_sid: u32,
    hit: Hit,
    features: [f32; N_FEATS],
}

impl DePostcodeHouseCandidate {
    fn same_product_projection(&self, other: &Self) -> bool {
        self.hit.precision == other.hit.precision
            && self.hit.street == other.hit.street
            && self.hit.housenumber == other.hit.housenumber
            && self.hit.postcode == other.hit.postcode
            && self.hit.commune == other.hit.commune
    }
}

fn de_same_retained_address_evidence(a: &RankedHit, b: &RankedHit) -> bool {
    a.0.score.to_bits() == b.0.score.to_bits()
        && a.1 == b.1
        && a.2 == b.2
        && a.3 == b.3
        && a.0.precision == b.0.precision
        && a.0.housenumber == b.0.housenumber
        && a.0.postcode == b.0.postcode
        && a.0.flags == b.0.flags
        && street_key(&a.0.street) == street_key(&b.0.street)
}

fn promote_de_retained_locality(
    hits: &mut Vec<RankedHit>,
    retained: DeRetainedLocality<'_>,
) -> bool {
    let Some(top) = hits.first() else {
        return false;
    };
    if top.1[0] <= 0.5 || de_retained_locality_score(retained.postcode_tail, &top.0.commune) != 0 {
        return false;
    }
    let mut best_score = 0;
    let mut best_positions = Vec::new();
    for (position, candidate) in hits.iter().take(5).enumerate().skip(1) {
        if !de_same_retained_address_evidence(candidate, top)
            || !de_dropped_tail_reaches_commune(retained.dropped_tail, &candidate.0.commune)
        {
            continue;
        }
        let score = de_retained_locality_score(retained.postcode_tail, &candidate.0.commune);
        if score > best_score {
            best_score = score;
            best_positions.clear();
            best_positions.push(position);
        } else if score == best_score && score > 0 {
            best_positions.push(position);
        }
    }
    if best_positions.len() != 1 {
        return false;
    }
    let chosen = hits.remove(best_positions[0]);
    hits.insert(0, chosen);
    hits[0].0.flags.push("de_retained_locality");
    true
}

/// DE-only c2 tail tie-break for an explicit postcode that survived suffix recovery.
///
/// This is deliberately narrower than the general ranking model: it may only move the
/// unique exact-postcode homonym from the original ranks 2..=5 ahead of a non-postcode
/// winner when both answers are exact matches for the same ordered street spelling and
/// expose exactly the same precision without weakening any non-postal feature evidence.
/// Score and rendered house spelling are intentionally outside the gate; the selected hit
/// is moved intact.
fn promote_de_postal_tail(
    hits: &mut Vec<RankedHit>,
    is_de: bool,
    is_c2_retry: bool,
    has_focus: bool,
    query_postcode: Option<u32>,
) -> bool {
    let (Some(query_postcode), Some(top)) = (query_postcode, hits.first()) else {
        return false;
    };
    let top_features = Feats::from_vec(&top.1);
    if !is_de
        || !is_c2_retry
        || has_focus
        || top_features.pc_exact
        || !top_features.street_exact
        || top.0.flags.contains(&"de_retained_locality")
    {
        return false;
    }

    let top_street = normalize(&top.0.street);
    let top_precision = top.0.precision;
    let matching_positions: Vec<usize> =
        hits.iter()
            .take(5)
            .enumerate()
            .skip(1)
            .filter_map(|(position, candidate)| {
                let features = Feats::from_vec(&candidate.1);
                let same_non_postal_features =
                    candidate.1.iter().zip(top.1.iter()).enumerate().all(
                        |(index, (candidate, top))| matches!(index, 4 | 5) || candidate == top,
                    );
                (features.pc_exact
                    && same_non_postal_features
                    && candidate.0.postcode.parse::<u32>().ok() == Some(query_postcode)
                    && normalize(&candidate.0.street) == top_street
                    && candidate.0.precision == top_precision)
                    .then_some(position)
            })
            .collect();
    if matching_positions.len() != 1 {
        return false;
    }

    let chosen = hits.remove(matching_positions[0]);
    hits.insert(0, chosen);
    hits[0].0.flags.push("de_postal_tail");
    true
}

fn apply_de_c2_tiebreaks(
    hits: &mut Vec<RankedHit>,
    retained_locality: Option<DeRetainedLocality<'_>>,
    is_de: bool,
    has_focus: bool,
    query_postcode: Option<u32>,
) {
    if !has_focus {
        if let Some(retained) = retained_locality {
            promote_de_retained_locality(hits, retained);
        }
    }
    promote_de_postal_tail(
        hits,
        is_de,
        retained_locality.is_some_and(|retained| retained.postal_tail_eligible),
        has_focus,
        query_postcode,
    );
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

#[derive(Default)]
struct DeBlankPostcodeDisplayProjection {
    ranges: HashMap<(String, String), (usize, usize)>,
    sids: Box<[u32]>,
}

impl DeBlankPostcodeDisplayProjection {
    fn get(&self, display_street: &str, locality: &str) -> Option<&[u32]> {
        let &(start, count) = self
            .ranges
            .get(&(display_street.to_string(), locality.to_string()))?;
        self.sids.get(start..start.checked_add(count)?)
    }
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
    /// DE-only exact five-digit postcode → street-id associations.  This
    /// compact, immutable open-time projection lets typed product predicates
    /// prove uniqueness independently of the street FST's source key.
    de_postcode_streets: Box<[(u32, u32)]>,
    /// DE-only normalized display-street → every source street id for display
    /// identities that contain at least one wholly postcode-less street.  P5
    /// cannot use `streets_fst`: its key is the source `nom_voie_norm`, which
    /// may legitimately differ from the product-visible display street.
    de_blank_postcode_display_streets: DeBlankPostcodeDisplayProjection,
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
        let mut index = Index {
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
            de_postcode_streets: Vec::new().into_boxed_slice(),
            de_blank_postcode_display_streets: DeBlankPostcodeDisplayProjection::default(),
            commune_coords: sl(SEC_COMMUNE_COORDS),
            rep_lookup,
            rep_suffixes,
            parser,
            rank,
            rules,
            top_anchor,
            admin: load_admin(path),
            meta: decode_meta(sl(SEC_META)).unwrap_or_default(),
        };
        if index.country() == Some("de") {
            index.de_postcode_streets = index.build_de_postcode_streets()?;
            index.de_blank_postcode_display_streets =
                index.build_de_blank_postcode_display_streets()?;
        }
        Ok(index)
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

    /// Build the global exact-postcode roster once while opening a DE sheet.
    /// Unambiguous streets contribute their exact five-digit display postcode;
    /// v7 mixed-postcode streets contribute every five-digit entry from their
    /// validated local dictionary.  Non-DE postcode grammars are deliberately
    /// absent from this product-specific projection.
    fn build_de_postcode_streets(&self) -> Result<Box<[(u32, u32)]>> {
        let street_count = self.streets_meta.len() / STREET_META_SIZE;
        let mut postings = Vec::with_capacity(street_count);
        for raw_sid in 0..street_count {
            let sid = u32::try_from(raw_sid)
                .map_err(|_| anyhow::anyhow!("DE postcode roster exceeds u32 street ids"))?;
            let metadata = self.street_meta(sid);
            if metadata.postcode_disp_off == PC_DISP_AMBIGUOUS {
                let Some((_, dictionary_byte, dictionary_count, _)) =
                    self.house_block_layout(sid, &metadata)
                else {
                    anyhow::bail!("DE street {sid} has an unreadable mixed-postcode dictionary");
                };
                for id in 1..=dictionary_count {
                    let offset = self.house_postcode_offset(dictionary_byte, dictionary_count, id);
                    let postcode = self.name(offset);
                    if is_five_digit_postcode(postcode) {
                        postings.push((postcode.parse()?, sid));
                    }
                }
                continue;
            }

            if metadata.postcode_disp_off != 0 {
                let postcode = self.name(metadata.postcode_disp_off);
                if is_five_digit_postcode(postcode) {
                    postings.push((postcode.parse()?, sid));
                }
            } else if (1..=99_999).contains(&metadata.postcode) {
                // Pre-display-postcode sheets preserve only the numeric value;
                // DE's fixed width makes its five-digit rendering unambiguous.
                postings.push((metadata.postcode, sid));
            }
        }
        postings.sort_unstable();
        postings.dedup();
        Ok(postings.into_boxed_slice())
    }

    /// Build a collision-free display-street/locality projection only for
    /// identities that have both exact typed-postcode roster support and a
    /// wholly postcode-less source street. The final pass adds every postcode
    /// state for those identities, including distinct commune ids with the
    /// same product-visible locality, so P5 sees conflicting exact houses
    /// instead of inspecting a blank row in isolation. Buckets retain one
    /// overflow sentinel beyond the request ceiling; a caller then fails
    /// closed without scanning a partial set.
    fn build_de_blank_postcode_display_streets(&self) -> Result<DeBlankPostcodeDisplayProjection> {
        if self.format_version < 7 {
            return Ok(DeBlankPostcodeDisplayProjection::default());
        }

        let street_count = self.streets_meta.len() / STREET_META_SIZE;
        let anchored_identities: HashSet<(String, String)> = self
            .de_postcode_streets
            .iter()
            .filter_map(|&(_, sid)| {
                let metadata = self.street_meta(sid);
                let display = de_product_normalize_street(self.name(metadata.name_off));
                let locality = de_product_normalize_text(self.commune_name(metadata.commune_id));
                (!display.is_empty() && !locality.is_empty()).then_some((display, locality))
            })
            .collect();
        let mut buckets: HashMap<(String, String), Vec<u32>> = HashMap::new();
        for raw_sid in 0..street_count {
            let sid = u32::try_from(raw_sid)
                .map_err(|_| anyhow::anyhow!("DE blank-postcode roster exceeds u32 street ids"))?;
            let metadata = self.street_meta(sid);
            if metadata.postcode == 0 && metadata.postcode_disp_off == 0 {
                let display = de_product_normalize_street(self.name(metadata.name_off));
                let locality = de_product_normalize_text(self.commune_name(metadata.commune_id));
                let identity = (display, locality);
                if anchored_identities.contains(&identity) {
                    buckets.entry(identity).or_default();
                }
            }
        }
        drop(anchored_identities);
        if buckets.is_empty() {
            return Ok(DeBlankPostcodeDisplayProjection::default());
        }

        for raw_sid in 0..street_count {
            let sid = u32::try_from(raw_sid)
                .map_err(|_| anyhow::anyhow!("DE blank-postcode roster exceeds u32 street ids"))?;
            let metadata = self.street_meta(sid);
            let display = de_product_normalize_street(self.name(metadata.name_off));
            let locality = de_product_normalize_text(self.commune_name(metadata.commune_id));
            let Some(bucket) = buckets.get_mut(&(display, locality)) else {
                continue;
            };
            if bucket.len() <= DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT_DEFAULT {
                bucket.push(sid);
            }
        }

        let total = buckets.values().try_fold(0usize, |total, bucket| {
            total
                .checked_add(bucket.len())
                .ok_or_else(|| anyhow::anyhow!("DE blank-postcode projection size overflow"))
        })?;
        let mut ranges = HashMap::with_capacity(buckets.len());
        let mut sids = Vec::with_capacity(total);
        for (identity, bucket) in buckets {
            let start = sids.len();
            let count = bucket.len();
            sids.extend(bucket);
            ranges.insert(identity, (start, count));
        }

        Ok(DeBlankPostcodeDisplayProjection {
            ranges,
            sids: sids.into_boxed_slice(),
        })
    }

    /// Exact-postcode street bucket for the P3 uniqueness proof.  A malformed
    /// or pathologically broad bucket fails closed at the same audited ceiling
    /// as the existing one-shot DE rescue scan.
    fn de_postcode_street_bucket(&self, postcode: u32) -> Option<&[(u32, u32)]> {
        let start = self
            .de_postcode_streets
            .partition_point(|&(candidate, _)| candidate < postcode);
        let end = self
            .de_postcode_streets
            .partition_point(|&(candidate, _)| candidate <= postcode);
        let bucket = &self.de_postcode_streets[start..end];
        (!bucket.is_empty() && bucket.len() <= de_postcode_house_rescue_scan_limit())
            .then_some(bucket)
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

    /// Numeric query postcodes intentionally use the leading digit run: this mirrors the
    /// query parser's established representation for values such as Dutch `1012AA` (1012),
    /// while preserving the full display string for output.
    fn postcode_numeric_prefix(postcode: &str) -> Option<u32> {
        let digits: String = postcode
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if digits.is_empty() {
            None
        } else {
            digits.parse().ok().filter(|&postcode| postcode != 0)
        }
    }

    /// Duplicate selection is deliberately narrower than feature reconciliation. Dresden's
    /// duplicate discriminator is a fully numeric postcode; an alphanumeric suffix cannot be
    /// reconstructed from the current numeric query feature and therefore must not reorder rows.
    fn exact_house_postcode_matches(
        &self,
        m: &StreetMeta,
        house_postcode_off: u32,
        requested_postcode: u32,
    ) -> bool {
        if m.postcode_disp_off != PC_DISP_AMBIGUOUS {
            return m.postcode != 0 && m.postcode == requested_postcode;
        }
        house_postcode_off != 0
            && self.name(house_postcode_off).parse::<u32>().ok() == Some(requested_postcode)
    }

    fn exact_house_postcode_candidate(
        &self,
        sid: u32,
        m: &StreetMeta,
        requested_number: u32,
        requested_rep: u32,
        requested_postcode: u32,
    ) -> bool {
        #[cfg(test)]
        DE_POSTCODE_HOUSE_RESCUE_HOUSE_DECODES.with(|calls| {
            calls.set(calls.get().saturating_add(1));
        });
        let Some((_, _, kind, _, _, postcode_off)) = self.find_house(
            sid,
            m,
            requested_number,
            requested_rep,
            Some(requested_postcode),
        ) else {
            return false;
        };
        kind == 2
            && Self::postcode_numeric_prefix(&self.postcode_for_house(m, postcode_off))
                == Some(requested_postcode)
    }

    /// Cheaply reject a homonymous street whose compact house-postcode dictionary cannot
    /// contain the requested postcode.  The rescue still scans every bounded FST posting to
    /// prove uniqueness, but it decodes a street's variable-length house rows only when this
    /// metadata proof says an exact address is possible.
    fn street_may_contain_postcode(
        &self,
        sid: u32,
        m: &StreetMeta,
        requested_postcode: u32,
    ) -> bool {
        if m.postcode_disp_off != PC_DISP_AMBIGUOUS {
            return m.postcode != 0 && m.postcode == requested_postcode;
        }
        let Some((_, dictionary_byte, dictionary_count, _)) = self.house_block_layout(sid, m)
        else {
            return false;
        };
        (1..=dictionary_count).any(|id| {
            let postcode_off = self.house_postcode_offset(dictionary_byte, dictionary_count, id);
            postcode_off != 0
                && self.name(postcode_off).parse::<u32>().ok() == Some(requested_postcode)
        })
    }

    fn exact_house_postcode_candidate_cached(
        &self,
        cache: &mut HashMap<(u32, u32, u32, u32), bool>,
        sid: u32,
        m: &StreetMeta,
        requested_number: u32,
        requested_rep: u32,
        requested_postcode: u32,
    ) -> bool {
        let key = (sid, requested_number, requested_rep, requested_postcode);
        if let Some(&cached) = cache.get(&key) {
            return cached;
        }
        let exact = self.street_may_contain_postcode(sid, m, requested_postcode)
            && self.exact_house_postcode_candidate(
                sid,
                m,
                requested_number,
                requested_rep,
                requested_postcode,
            );
        cache.insert(key, exact);
        exact
    }

    /// Prove the complete literal house set on one street id.  Every endpoint
    /// uses the same suffix id and numeric postcode; `find_house` must return an
    /// exact represented row (`kind == 2`) for each one.  The cache remains
    /// endpoint-specific so repeated DE variants do not re-decode house blocks.
    // Frozen release: grouping house-set inputs would rewrite the validated resolver call sites.
    #[allow(clippy::too_many_arguments)]
    fn exact_house_postcode_set_candidate_cached(
        &self,
        cache: &mut HashMap<(u32, u32, u32, u32), bool>,
        sid: u32,
        m: &StreetMeta,
        requested_number: u32,
        requested_rep: u32,
        additional_numbers: &[u32],
        requested_postcode: u32,
    ) -> bool {
        self.exact_house_postcode_candidate_cached(
            cache,
            sid,
            m,
            requested_number,
            requested_rep,
            requested_postcode,
        ) && additional_numbers.iter().all(|&number| {
            self.exact_house_postcode_candidate_cached(
                cache,
                sid,
                m,
                number,
                requested_rep,
                requested_postcode,
            )
        })
    }

    /// The typed DE product predicates carry the full five-character postcode
    /// field for every literal endpoint.  The shared numeric resolver
    /// deliberately accepts leading-digit prefixes for other countries, so
    /// this proof reasserts the rendered postcode on every represented house.
    // Frozen release: preserve the separate numeric/full-postcode inputs and existing call sites.
    #[allow(clippy::too_many_arguments)]
    fn exact_house_full_postcode_set_candidate_cached(
        &self,
        cache: &mut HashMap<(u32, u32, u32, u32), bool>,
        sid: u32,
        m: &StreetMeta,
        requested_number: u32,
        requested_rep: u32,
        additional_numbers: &[u32],
        requested_postcode: u32,
        requested_postcode_raw: &str,
    ) -> bool {
        if !self.exact_house_postcode_set_candidate_cached(
            cache,
            sid,
            m,
            requested_number,
            requested_rep,
            additional_numbers,
            requested_postcode,
        ) {
            return false;
        }
        std::iter::once(requested_number)
            .chain(additional_numbers.iter().copied())
            .all(|number| {
                let Some((_, _, kind, got, got_rep, postcode_off)) =
                    self.find_house(sid, m, number, requested_rep, Some(requested_postcode))
                else {
                    return false;
                };
                kind == 2
                    && got == number
                    && got_rep == requested_rep
                    && self.postcode_for_house(m, postcode_off) == requested_postcode_raw
            })
    }

    /// Count every physical represented row for one exact number+suffix.
    /// `find_house` returns at the first match, which is correct for ordinary
    /// ranking but cannot prove P5 uniqueness when duplicate source rows share
    /// one street id. This decoder deliberately ignores coordinates after
    /// consuming their varints: they neither select nor deduplicate a row.
    fn de_exact_house_record_postcodes(
        &self,
        sid: u32,
        metadata: &StreetMeta,
        requested_number: u32,
        requested_rep: u32,
        row_budget: &mut usize,
    ) -> std::result::Result<Vec<String>, ()> {
        if metadata.house_count == 0 {
            return Ok(Vec::new());
        }
        let (mut position, postcode_dictionary, postcode_count, house_end) =
            self.house_block_layout(sid, metadata).ok_or(())?;
        let bounded_houses = &self.houses[..house_end];
        let mut current_number = 0u32;
        let mut matches = Vec::new();
        for index in 0..metadata.house_count {
            if *row_budget == 0 {
                return Err(());
            }
            *row_budget -= 1;
            #[cfg(test)]
            DE_BLANK_POSTCODE_HOUSE_SCAN_ROWS.with(|rows| {
                rows.set(rows.get().saturating_add(1));
            });

            let delta = strict_varint(bounded_houses, &mut position)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or(())?;
            current_number = if index == 0 {
                delta
            } else {
                current_number.checked_add(delta).ok_or(())?
            };
            let rep = strict_varint(bounded_houses, &mut position)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or(())?;
            strict_varint(bounded_houses, &mut position).ok_or(())?;
            strict_varint(bounded_houses, &mut position).ok_or(())?;
            let postcode_off =
                if self.format_version >= 7 && metadata.postcode_disp_off == PC_DISP_AMBIGUOUS {
                    let id = strict_varint(bounded_houses, &mut position)
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or(())?;
                    self.house_postcode_offset(postcode_dictionary, postcode_count, id)
                } else {
                    0
                };
            if current_number == requested_number && rep == requested_rep {
                matches.push(self.postcode_for_house(metadata, postcode_off));
            }
            if current_number > requested_number {
                break;
            }
        }
        Ok(matches)
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
        requested_postcode: Option<u32>,
    ) -> Option<(f64, f64, u8, u32, u32, u32)> {
        let (mut pos, postcode_dictionary, postcode_count, house_end) =
            self.house_block_layout(street_id, m)?;
        let bounded_houses = &self.houses[..house_end];
        let mut cur = 0u32;
        let mut exact_rep: Option<(f64, f64, u32)> = None;
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
                    let postcode_matches = requested_postcode.is_some_and(|postcode| {
                        self.exact_house_postcode_matches(m, postcode_off, postcode)
                    });
                    if requested_postcode.is_none() || postcode_matches {
                        return Some((lat, lon, 2, cur, rid, postcode_off));
                    }
                    if exact_rep.is_none() {
                        exact_rep = Some((lat, lon, postcode_off));
                    }
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
        if let Some((lat, lon, postcode_off)) = exact_rep {
            return Some((lat, lon, 2, numero, rep, postcode_off));
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

    fn add_cand(cand: &mut HashMap<u32, Feats>, sid: u32, f: Feats) {
        cand.entry(sid).or_default().merge(f);
    }

    /// Candidate collection: scanning the "street | commune" boundary from the end.
    // Frozen release: keep scan budgets and caches wired through the validated call sites.
    #[allow(clippy::too_many_arguments)]
    fn collect_candidates(
        &self,
        rest: &[&str],
        postcode: Option<u32>,
        numero: Option<u32>,
        rep: u32,
        from_ml: bool,
        de_postcode_house_scan: bool,
        de_postcode_house_additional_numbers: &[u32],
        de_postcode_house_scan_budget: &mut usize,
        de_postcode_house_seen_phrases: &mut HashSet<String>,
        de_postcode_house_exact_cache: &mut HashMap<(u32, u32, u32, u32), bool>,
    ) -> (HashMap<u32, Feats>, bool) {
        let mut cand: HashMap<u32, Feats> = HashMap::new();
        let mut de_postcode_house_scan_overflowed = false;
        if rest.is_empty() {
            return (cand, de_postcode_house_scan_overflowed);
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
            let mut de_street_phrases = HashSet::new();
            if self.country() == Some("de") {
                let bases = phrases.clone();
                for base in bases {
                    for variant in crate::de::street_variants(&base) {
                        if !phrases.contains(&variant) {
                            de_street_phrases.insert(variant.clone());
                            phrases.push(variant);
                        }
                    }
                }
            }
            if c == 0 {
                // the whole string is the street; prefix search across all communes
                for phrase in &phrases {
                    let de_house_postcode_scan = de_postcode_house_scan
                        && self.country() == Some("de")
                        && self.format_version >= 7
                        && postcode.is_some()
                        && numero.is_some();
                    if de_house_postcode_scan
                        && !de_postcode_house_seen_phrases.insert(phrase.clone())
                    {
                        continue;
                    }
                    let mut lo = phrase.clone().into_bytes();
                    lo.push(KEY_SEP);
                    let mut hi = phrase.clone().into_bytes();
                    hi.push(KEY_SEP + 1);
                    let mut stream = self.streets_fst.range().ge(&lo).lt(&hi).into_stream();
                    let mut taken = 0;
                    while let Some((_, v)) = stream.next() {
                        if de_house_postcode_scan {
                            if *de_postcode_house_scan_budget == 0 {
                                de_postcode_house_scan_overflowed = true;
                                break;
                            }
                            *de_postcode_house_scan_budget -= 1;
                            #[cfg(test)]
                            DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| {
                                rows.set(rows.get().saturating_add(1));
                            });
                        }
                        let sid = v as u32;
                        let m = self.street_meta(sid);
                        let exact_house_postcode = match (postcode, numero) {
                            (Some(requested_postcode), Some(requested_number))
                                if de_house_postcode_scan =>
                            {
                                self.exact_house_postcode_set_candidate_cached(
                                    de_postcode_house_exact_cache,
                                    sid,
                                    &m,
                                    requested_number,
                                    rep,
                                    de_postcode_house_additional_numbers,
                                    requested_postcode,
                                )
                            }
                            _ => false,
                        };
                        let mut f = Feats {
                            street_exact: true,
                            from_ml,
                            de_street_type: de_street_phrases.contains(phrase),
                            ..Default::default()
                        };
                        // The narrow rescue consumes only exact street+house+postcode rows.
                        // It still visits the complete bounded posting stream above, so a hidden
                        // duplicate or overflow fails closed, while irrelevant homonyms never
                        // reach ranking or house decoding a second time.
                        if de_house_postcode_scan {
                            if !exact_house_postcode {
                                continue;
                            }
                            f.pc_exact = true;
                            f.pc_dept = true;
                            Self::add_cand(&mut cand, sid, f);
                            continue;
                        }
                        if let Some(pc) = postcode {
                            // postcode==0 = "absent from the data" — do not filter these,
                            // or a query WITH a postcode would go empty where the same
                            // query without one succeeds
                            if m.postcode != 0
                                && m.postcode / 1000 != pc / 1000
                                && !exact_house_postcode
                            {
                                continue; // different part of the country — skip
                            }
                            if m.postcode != 0 {
                                f.pc_dept = true;
                                f.pc_exact = m.postcode == pc;
                            }
                        }
                        if taken >= 300 && !exact_house_postcode {
                            continue;
                        }
                        Self::add_cand(&mut cand, sid, f);
                        if taken < 300 {
                            taken += 1;
                        }
                        if taken >= 300 && !de_house_postcode_scan {
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
                            let exact_house_postcode = if de_postcode_house_scan
                                && self.country() == Some("de")
                                && self.format_version >= 7
                            {
                                if *de_postcode_house_scan_budget == 0 {
                                    de_postcode_house_scan_overflowed = true;
                                    break;
                                }
                                *de_postcode_house_scan_budget -= 1;
                                #[cfg(test)]
                                DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| {
                                    rows.set(rows.get().saturating_add(1));
                                });
                                match (postcode, numero) {
                                    (Some(requested_postcode), Some(requested_number)) => self
                                        .exact_house_postcode_set_candidate_cached(
                                            de_postcode_house_exact_cache,
                                            sid,
                                            &m,
                                            requested_number,
                                            rep,
                                            de_postcode_house_additional_numbers,
                                            requested_postcode,
                                        ),
                                    _ => false,
                                }
                            } else {
                                false
                            };
                            if de_postcode_house_scan && !exact_house_postcode {
                                continue;
                            }
                            let mut f = Feats {
                                street_exact: true,
                                commune_exact: exact_commune,
                                commune_prefix: !exact_commune,
                                pc_exact: postcode
                                    .is_some_and(|pc| m.postcode != 0 && m.postcode == pc),
                                pc_dept: postcode.is_some_and(|pc| {
                                    m.postcode != 0 && m.postcode / 1000 == pc / 1000
                                }),
                                from_ml,
                                de_street_type: de_street_phrases.contains(phrase),
                                ..Default::default()
                            };
                            if de_postcode_house_scan {
                                f.pc_exact = true;
                                f.pc_dept = true;
                            }
                            Self::add_cand(&mut cand, sid, f);
                        }
                    }
                }
            }
        }
        (cand, de_postcode_house_scan_overflowed)
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
                    && (t.chars().any(|c| c.is_ascii_digit()) || is_safe_house_rep(t));
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

    /// Conservative bare street+house surface from the ORIGINAL query. An explicit
    /// comma context, postal token, city prefix or city suffix cannot be discarded
    /// by later variant/retry parsing to enable the prominence prior.
    fn de_cityless_street(raw: &str) -> Option<String> {
        if raw.contains(',') {
            return None;
        }
        let normalized = prepared_query_key(raw);
        let mut tokens: Vec<_> = normalized.split_whitespace().collect();
        let mut house = tokens.pop()?;
        if house.len() == 1 && house.bytes().all(|c| c.is_ascii_alphabetic()) {
            house = tokens.pop()?;
        }
        let digits = house.bytes().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0
            || digits > 4
            || !house[digits..].bytes().all(|c| c.is_ascii_alphabetic())
            || tokens.is_empty()
            || tokens.iter().any(|t| t.bytes().any(|c| c.is_ascii_digit()))
        {
            return None;
        }
        Some(street_key(&tokens.join(" ")))
    }

    /// One tie-break experiment: existing leaf address count before the weak
    /// capital anchor, only for equal-quality bare DE house homonyms.
    fn de_cityless_prominence(
        hits: &mut [RankedHit],
        country: Option<&str>,
        original_street: Option<&str>,
        focused: bool,
        postcode: Option<u32>,
    ) {
        let Some(top) = hits.first() else { return };
        let key = street_key(&top.0.street);
        let scope_allowed = country == Some("de")
            && !focused
            && postcode.is_none()
            && original_street == Some(key.as_str());
        if !scope_allowed {
            return;
        }
        let mut chosen = 0;
        for (position, candidate) in hits.iter().enumerate().skip(1) {
            let equal_address_evidence = top.0.precision == "house"
                && candidate.0.precision == "house"
                && top.1[0] > 0.5
                && candidate.1[0] > 0.5
                && street_key(&candidate.0.street) == key
                && candidate.0.score == top.0.score
                && candidate.2 == top.2
                && candidate.3 == top.3;
            let more_prominent = candidate.4 > hits[chosen].4;
            if equal_address_evidence && more_prominent {
                chosen = position;
            }
        }
        if chosen != 0 {
            hits[..=chosen].rotate_right(1);
            hits[0].0.flags.push("de_cityless_prominence");
        }
    }

    pub fn query_feats(&self, raw: &str, k: usize) -> Vec<(Hit, [f32; N_FEATS])> {
        if k == 0 {
            return Vec::new(); // k=0 asks for zero results; the city-only fallback used to ignore it
        }
        let k = bound_k(k); // cap result count before it drives allocation/sort
        let raw = bound_query(raw); // cap work before normalization
        let _rules = crate::rules::scope(self.rules); // this file's tables, not another file's
        let mut hits = self.query_feats_country_variants(raw, k, None);
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
        let mut hits = self.query_feats_country_variants(raw, k, Some(&focus));
        Self::monotone_confidence(&mut hits);
        Ok(hits)
    }

    fn de_effect_flags(effect: crate::de::Effect) -> &'static [&'static str] {
        use crate::de::Effect;
        match effect {
            Effect::Orthography => &["de_umlaut"],
            Effect::CityAlias => &["de_city_alias"],
            Effect::OfficialCommuneAlias => &["de_official_commune_alias"],
            Effect::Abbreviation => &["de_abbrev"],
            Effect::AdminTail => &["de_admin_tail"],
            Effect::RecipientPrefix => &["de_recipient_prefix"],
            Effect::SubaddressTail => &["de_subaddress_tail"],
            Effect::ParentheticalSubaddress => &["de_parenthetical_subaddress"],
            Effect::AddressField => &["de_address_field"],
            Effect::LocalityFirst => &["de_locality_first"],
            Effect::MissingCommaPostcodeBoundary => &["de_missing_postcode_comma"],
            Effect::Country => &["de_country"],
            Effect::HouseRange => &["de_house_range"],
            Effect::HouseSlash => &["de_house_slash"],
            Effect::PostcodePrefix => &["de_country", "de_postcode"],
            Effect::PostcodeZero => &["de_postcode"],
        }
    }

    fn annotate_de_effects(hits: &mut [(Hit, [f32; N_FEATS])], effects: &[crate::de::Effect]) {
        for (hit, _) in hits {
            for effect in effects {
                for flag in Self::de_effect_flags(*effect) {
                    if !hit.flags.contains(flag) {
                        hit.flags.push(flag);
                    }
                }
            }
        }
    }

    fn de_variant_quality(hits: &[(Hit, [f32; N_FEATS])]) -> Option<(i32, u8, u8, f32)> {
        let (hit, features) = hits.first()?;
        let precision = match hit.precision {
            "house" => 4,
            "interp" => 3,
            "near" => 2,
            "street" => 1,
            _ => 0,
        };
        Some((
            Feats::from_vec(features).legacy(),
            precision,
            u8::from(!hit.flags.contains(&"dropped_suffix")),
            hit.score,
        ))
    }

    fn de_quality_is_better(candidate: (i32, u8, u8, f32), current: (i32, u8, u8, f32)) -> bool {
        candidate.0 > current.0
            || (candidate.0 == current.0 && candidate.1 > current.1)
            || (candidate.0 == current.0 && candidate.1 == current.1 && candidate.2 > current.2)
            || (candidate.0 == current.0
                && candidate.1 == current.1
                && candidate.2 == current.2
                && candidate.3 > current.3)
    }

    fn de_official_commune_alias_candidate_is_exact(candidate: &[(Hit, [f32; N_FEATS])]) -> bool {
        candidate.first().is_some_and(|(hit, _)| {
            hit.precision == "house"
                && hit.flags.contains(&"street_exact")
                && hit.flags.contains(&"house_rep")
        })
    }

    fn de_ludwigshafen_official_alias_candidate_position(
        raw: &str,
        candidate: &[(Hit, [f32; N_FEATS])],
    ) -> Option<usize> {
        let spec = de_strict_source_street_typo_spec(raw)?;
        if spec.normalized_locality != "ludwigshafen rhein" {
            return None;
        }
        // `query_feats_d` is bounded at 20 for a hard commune. Hitting the cap
        // cannot prove that another eligible address is not hidden below it.
        if candidate.len() >= 20 {
            return None;
        }
        let expected_house = spec.house_number.to_string();
        let mut eligible =
            candidate
                .iter()
                .enumerate()
                .filter_map(|(position, (hit, features))| {
                    let features = Feats::from_vec(features);
                    (hit.precision == "house"
                        && features.street_exact
                        && features.house_exact_rep
                        // The product row may legitimately carry no postcode.
                        // Missing evidence is not a disagreement; a populated
                        // row still needs exact or department-level agreement.
                        && (hit.postcode.is_empty() || features.pc_exact || features.pc_dept)
                        && de_product_normalize_street(&hit.street) == spec.normalized_street
                        && de_product_normalize_text(&hit.commune) == "ludwigshafen am rhein"
                        && hit.housenumber.as_deref() == Some(expected_house.as_str()))
                    .then_some(position)
                });
        let position = eligible.next()?;
        eligible.next().is_none().then_some(position)
    }

    /// Return the single rank in the bounded established result window that
    /// satisfies the X2 product predicate. No score, coordinate, benchmark
    /// outcome or roster identity participates in this decision.
    fn de_street_locality_qualifier_position(
        raw: &str,
        current: &[(Hit, [f32; N_FEATS])],
    ) -> Option<usize> {
        let spec = de_street_locality_qualifier_spec(raw)?;
        let (top, top_features) = current.first()?;
        let top_features = Feats::from_vec(top_features);
        if top.precision != "house"
            || !top_features.street_exact
            || !top_features.house_exact_rep
            || !top_features.pc_dept
            || top_features.pc_exact
            || !is_five_digit_postcode(&top.postcode)
            || top.postcode == spec.postcode_raw
            || de_product_normalize_street(&top.street) != spec.normalized_street
            || !de_house_token_matches(top, &spec.house_token)
        {
            return None;
        }

        let mut eligible =
            current
                .iter()
                .take(5)
                .enumerate()
                .skip(1)
                .filter_map(|(position, (hit, features))| {
                    let features = Feats::from_vec(features);
                    let (base, qualifier) = de_source_street_locality_qualifier(&hit.street)?;
                    let source_commune = de_product_normalize_text(&hit.commune);
                    (hit.precision == "house"
                        && features.street_exact
                        && features.house_exact_rep
                        && hit.postcode.is_empty()
                        && de_house_token_matches(hit, &spec.house_token)
                        && base == spec.normalized_street
                        && qualifier == spec.normalized_locality
                        && de_locality_is_exact_or_query_prefix(
                            &spec.normalized_locality,
                            &source_commune,
                        ))
                    .then_some(position)
                });
        let position = eligible.next()?;
        eligible.next().is_none().then_some(position)
    }

    /// A parenthetical building/subaddress label is an additive, fill-empty
    /// hypothesis only.  It may never replace an ordinary result, and even an
    /// empty ordinary path is filled only by the top exact street+house in the
    /// exact retained terminal commune core.
    fn de_parenthetical_subaddress_may_fill(
        current: &[(Hit, [f32; N_FEATS])],
        candidate: &[(Hit, [f32; N_FEATS])],
        expected_commune: Option<&str>,
    ) -> bool {
        if !current.is_empty() {
            return false;
        }
        let (Some(expected_commune), Some((hit, features))) = (expected_commune, candidate.first())
        else {
            return false;
        };
        let features = Feats::from_vec(features);
        hit.precision == "house"
            && features.street_exact
            && features.house_exact_rep
            && de_commune_core(&hit.commune) == expected_commune
    }

    /// A comma-delimited recipient prefix is direct syntax evidence that every
    /// token after the comma belongs to the address.  When that exact cleanup
    /// produces the same street+house quality as a generic candidate that had
    /// to drop a street-name prefix, preserve the full typed street.  This is a
    /// deliberately narrow tie-break: it cannot promote fuzzy/near candidates
    /// and it is unavailable without the exact recipient-prefix predicate.
    fn de_recipient_cleanup_breaks_dropped_prefix_tie(
        candidate: &[(Hit, [f32; N_FEATS])],
        current: &[(Hit, [f32; N_FEATS])],
    ) -> bool {
        let (Some((candidate_hit, candidate_features)), Some((current_hit, current_features))) =
            (candidate.first(), current.first())
        else {
            return false;
        };
        let candidate_features = Feats::from_vec(candidate_features);
        let current_features = Feats::from_vec(current_features);
        !candidate_hit.flags.contains(&"dropped_prefix")
            && current_hit.flags.contains(&"dropped_prefix")
            && candidate_features.street_exact
            && candidate_features.house_exact_rep
            && current_features.street_exact
            && current_features.house_exact_rep
    }

    fn de_abbreviation_candidate_may_displace(
        candidate: &[(Hit, [f32; N_FEATS])],
        current: &[(Hit, [f32; N_FEATS])],
        original_postcode_tail: Option<&str>,
    ) -> bool {
        #[cfg(test)]
        DE_ABBREVIATION_GUARD_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
        if current.is_empty() {
            return true;
        }
        let Some((candidate_hit, candidate_features)) = candidate.first() else {
            return false;
        };
        let candidate_features = Feats::from_vec(candidate_features);
        let current_features = Feats::from_vec(&current[0].1);
        if candidate_hit.flags.contains(&"dropped_prefix") {
            return candidate_features.street_exact
                && candidate_features.house_exact_rep
                && (candidate_features.pc_exact || candidate_features.commune_exact);
        }
        if candidate_features.commune_prefix
            && !candidate_features.commune_exact
            && !current_features.commune_prefix
            && !current_features.commune_exact
        {
            let precision = |hit: &Hit| match hit.precision {
                "house" => 3,
                "interp" => 2,
                "near" => 1,
                _ => 0,
            };
            let candidate_evidence = [
                u8::from(candidate_features.pc_exact),
                u8::from(candidate_features.street_exact),
                u8::from(candidate_features.house_exact_rep),
                precision(candidate_hit),
            ];
            let current_evidence = [
                u8::from(current_features.pc_exact),
                u8::from(current_features.street_exact),
                u8::from(current_features.house_exact_rep),
                precision(&current[0].0),
            ];
            let no_regression = candidate_evidence
                .iter()
                .zip(current_evidence)
                .all(|(candidate, current)| *candidate >= current);
            let strict_gain = candidate_evidence
                .iter()
                .zip(current_evidence)
                .any(|(candidate, current)| *candidate > current);
            if no_regression && strict_gain {
                return true;
            }
            // An abbreviation may expose the canonical street spelling without adding
            // postcode evidence.  When exact street+house evidence otherwise ties, only
            // the locality text the user actually supplied may break that tie; the
            // variant's synthetic commune_prefix bit alone is not sufficient.
            let Some(postcode_tail) = original_postcode_tail else {
                return false;
            };
            return no_regression
                && candidate_features.street_exact
                && candidate_features.house_exact_rep
                && de_retained_locality_score(postcode_tail, &candidate_hit.commune)
                    > de_retained_locality_score(postcode_tail, &current[0].0.commune);
        }
        true
    }

    fn de_has_complete_postcode_house_evidence(hits: &[(Hit, [f32; N_FEATS])]) -> bool {
        hits.first().is_some_and(|(_, features)| {
            let features = Feats::from_vec(features);
            features.street_exact && features.house_exact_rep && features.pc_exact
        })
    }

    /// Wave A is a post-failure correction, never a fill-empty fallback.  The
    /// ordinary path must already have produced an exact-street address-level
    /// result.  A represented house is eligible only when its postcode is not
    /// the explicit query postcode; near/interp is eligible because a uniquely
    /// proven exact indexed house is strictly stronger at the same address key.
    fn de_strict_postcode_house_override_allowed(
        hits: &[(Hit, [f32; N_FEATS])],
        requested_postcode: u32,
    ) -> bool {
        let Some((hit, features)) = hits.first() else {
            return false;
        };
        let features = Feats::from_vec(features);
        match hit.precision {
            "house" => {
                features.street_exact
                    && features.house_exact_rep
                    && Self::postcode_numeric_prefix(&hit.postcode) != Some(requested_postcode)
            }
            "near" | "interp" => {
                Self::postcode_numeric_prefix(&hit.postcode) == Some(requested_postcode)
                    || (features.street_exact
                        && Self::postcode_numeric_prefix(&hit.postcode) != Some(requested_postcode))
            }
            _ => false,
        }
    }

    fn de_should_try_postcode_house_rescue(raw: &str, hits: &[(Hit, [f32; N_FEATS])]) -> bool {
        // A mechanically restored comma is only a structural hypothesis until
        // the exact street+house+postcode rescue proves it against the sheet.
        // Run that proof even when the ordinary parser happened to produce a
        // complete hit, so the result never depends on index insertion order.
        if crate::de::query_variants(raw).iter().any(|variant| {
            variant
                .effects
                .contains(&crate::de::Effect::MissingCommaPostcodeBoundary)
        }) {
            return true;
        }
        if !Self::de_has_complete_postcode_house_evidence(hits) {
            return true;
        }
        if !hits
            .first()
            .is_some_and(|(hit, _)| hit.flags.contains(&"dropped_prefix"))
        {
            return false;
        }
        de_comma_postcode_house_rescue_query(raw).is_some_and(|(_, postcode, locality)| {
            de_is_exact_locality_alias_query(&locality, postcode)
        })
    }

    fn de_postcode_house_locality_score(
        locality_tail: &str,
        hard_commune: Option<&str>,
        requested_postcode: u32,
        commune: &str,
    ) -> u8 {
        if let Some(expected) = hard_commune {
            let actual = de_commune_core(commune);
            return if actual == expected
                || (expected == "frankfurt oder"
                    && actual == "frankfurt"
                    && requested_postcode == 15230)
            {
                4
            } else {
                0
            };
        }
        let retained = de_retained_locality_score(locality_tail, commune);
        if retained == 2 {
            return 4;
        }
        let query = normalize(locality_tail);
        let candidate = de_commune_core(commune);
        if de_exact_locality_alias_matches(locality_tail, requested_postcode, commune) {
            return 3;
        }
        if query == "berlin"
            && !de_is_exact_locality_alias_query(locality_tail, requested_postcode)
            && de_is_proven_berlin_postcode(requested_postcode)
            && de_is_berlin_postal_locality(commune)
        {
            return 3;
        }
        if retained != 0
            || de_locality_qualifiers_match(locality_tail, commune)
            || de_p3_locality_compatible(&query, &candidate)
        {
            return 1;
        }
        0
    }

    fn de_postcode_house_locality_compatible(
        &self,
        locality_tail: &str,
        hard_commune: Option<&str>,
        requested_postcode: u32,
        hit: &Hit,
    ) -> bool {
        Self::de_postcode_house_locality_score(
            locality_tail,
            hard_commune,
            requested_postcode,
            &hit.commune,
        ) != 0
    }

    /// Product-visible proof that the typed postcode contains at least one
    /// indexed street in a compatible locality. The exact-postcode roster is
    /// bounded by the same fail-closed scan ceiling as the house rescue.
    fn de_postcode_has_locality_support(
        &self,
        requested_postcode: u32,
        locality_tail: &str,
    ) -> bool {
        self.de_postcode_street_bucket(requested_postcode)
            .is_some_and(|bucket| {
                bucket.iter().any(|&(_, sid)| {
                    let metadata = self.street_meta(sid);
                    Self::de_postcode_house_locality_score(
                        locality_tail,
                        None,
                        requested_postcode,
                        self.commune_name(metadata.commune_id),
                    ) != 0
                })
            })
    }

    /// A near/interpolation matching the locality in a typed-postcode request
    /// already carries product-visible locality evidence, even when that weak
    /// hit has no display postcode. Wave N permits the strict exact-house
    /// replacement only when its indexed commune is compatible with the same
    /// typed locality. Wrong-postcode exact-house corrections remain the
    /// independent P1 rule and do not acquire a synthetic constraint here.
    fn de_typed_postcode_near_has_locality_evidence(
        &self,
        hits: &[(Hit, [f32; N_FEATS])],
        requested_postcode: u32,
        locality_tail: &str,
    ) -> bool {
        hits.first().is_some_and(|(hit, features)| {
            let features = Feats::from_vec(features);
            let postcode_supports_locality = Self::postcode_numeric_prefix(&hit.postcode)
                == Some(requested_postcode)
                || (hit.postcode.trim().is_empty()
                    && self.de_postcode_has_locality_support(requested_postcode, locality_tail));
            matches!(hit.precision, "near" | "interp")
                && features.street_exact
                && postcode_supports_locality
                && self.de_postcode_house_locality_compatible(
                    locality_tail,
                    None,
                    requested_postcode,
                    hit,
                )
        })
    }

    fn de_blank_postcode_current_top_is_strict(
        current: &[(Hit, [f32; N_FEATS])],
        spec: &DeBlankPostcodeHouseSpec,
    ) -> bool {
        let Some((hit, features)) = current.first() else {
            return false;
        };
        let features = Feats::from_vec(features);
        let relies_on_disallowed_cleanup = [
            "dropped_prefix",
            "dropped_suffix",
            "de_city_alias",
            "de_official_commune_alias",
        ]
        .iter()
        .any(|flag| hit.flags.contains(flag));
        matches!(hit.precision, "near" | "interp")
            && features.street_exact
            && features.commune_exact
            && features.pc_exact
            && !features.house_exact_rep
            && hit.postcode == spec.postcode_raw
            && de_product_normalize_street(&hit.street) == spec.normalized_street
            && de_product_normalize_text(&hit.commune) == spec.normalized_locality
            && !relies_on_disallowed_cleanup
    }

    /// Anchor P5 to one exact product-visible commune identity already proven
    /// by the typed postcode roster. Prefix/alias/district compatibility is
    /// intentionally absent: two commune ids with the same display name are
    /// ambiguous and therefore fail closed.
    fn de_blank_postcode_anchor_commune_id(&self, spec: &DeBlankPostcodeHouseSpec) -> Option<u32> {
        let mut commune_ids: Vec<u32> = self
            .de_postcode_street_bucket(spec.postcode)?
            .iter()
            .filter_map(|&(_, sid)| {
                let metadata = self.street_meta(sid);
                (de_product_normalize_street(self.name(metadata.name_off))
                    == spec.normalized_street
                    && de_product_normalize_text(self.commune_name(metadata.commune_id))
                        == spec.normalized_locality)
                    .then_some(metadata.commune_id)
            })
            .collect();
        commune_ids.sort_unstable();
        commune_ids.dedup();
        let [commune_id] = commune_ids.as_slice() else {
            return None;
        };
        Some(*commune_id)
    }

    /// Wave O/P5: replace only a fully evidenced typed-postcode near/interp
    /// with one physical exact-house row whose source postcode is genuinely
    /// absent. Admission is driven by exact display fields, a single anchored
    /// commune id, exact number+suffix, and full source-row uniqueness. No
    /// non-request selector participates.
    fn de_blank_postcode_exact_house_fallback(
        &self,
        raw: &str,
        current: &[(Hit, [f32; N_FEATS])],
        k: usize,
        focus: Option<&QueryFocus>,
    ) -> Option<Vec<(Hit, [f32; N_FEATS])>> {
        if self.country() != Some("de") || self.format_version < 7 || focus.is_some() {
            return None;
        }
        let spec = de_blank_postcode_house_spec(raw)?;
        if !Self::de_blank_postcode_current_top_is_strict(current, &spec) {
            return None;
        }
        let requested_rep = if spec.house_suffix.is_empty() {
            0
        } else {
            *self.rep_lookup.get(&spec.house_suffix)?
        };
        let anchor_commune_id = self.de_blank_postcode_anchor_commune_id(&spec)?;
        let candidate_sids = self
            .de_blank_postcode_display_streets
            .get(&spec.normalized_street, &spec.normalized_locality)?;
        let scan_limit = de_postcode_house_rescue_scan_limit();
        if candidate_sids.is_empty() || candidate_sids.len() > scan_limit {
            return None;
        }

        let mut row_budget = scan_limit;
        let mut exact_records: Vec<(u32, String)> = Vec::new();
        for &sid in candidate_sids.iter() {
            let metadata = self.street_meta(sid);
            if de_product_normalize_street(self.name(metadata.name_off)) != spec.normalized_street
                || de_product_normalize_text(self.commune_name(metadata.commune_id))
                    != spec.normalized_locality
            {
                continue;
            }
            let postcodes = self
                .de_exact_house_record_postcodes(
                    sid,
                    &metadata,
                    spec.house_number,
                    requested_rep,
                    &mut row_budget,
                )
                .ok()?;
            for postcode in postcodes {
                exact_records.push((sid, postcode));
                if exact_records.len() > 1 {
                    return None;
                }
            }
        }
        let [(sid, postcode)] = exact_records.as_slice() else {
            return None;
        };
        let metadata = self.street_meta(*sid);
        if metadata.commune_id != anchor_commune_id
            || !postcode.is_empty()
            || metadata.postcode != 0
            || metadata.postcode_disp_off != 0
        {
            return None;
        }

        let query_words: Vec<Vec<char>> = spec
            .normalized_street
            .split_whitespace()
            .map(|word| word.chars().collect())
            .collect();
        let features = Feats {
            street_exact: true,
            commune_exact: true,
            ..Default::default()
        };
        let (mut hit, feature_vector, _, _, _, _) = self.make_hit(
            *sid,
            features,
            Some(spec.house_number),
            requested_rep,
            None,
            &query_words,
        );
        let made_features = Feats::from_vec(&feature_vector);
        let expected_house = self.house_number(spec.house_number, requested_rep);
        if hit.precision != "house"
            || hit.housenumber.as_deref() != Some(expected_house.as_str())
            || !hit.postcode.is_empty()
            || de_product_normalize_street(&hit.street) != spec.normalized_street
            || de_product_normalize_text(&hit.commune) != spec.normalized_locality
            || !made_features.street_exact
            || !made_features.commune_exact
            || !made_features.house_exact_rep
            || made_features.pc_exact
            || made_features.pc_dept
        {
            return None;
        }
        if !hit.flags.contains(&"de_blank_postcode_house_override") {
            hit.flags.push("de_blank_postcode_house_override");
        }
        let mut exact = vec![(hit, feature_vector)];
        exact.truncate(k);
        Some(exact)
    }

    fn de_product_fallback_arbitration(
        best: &mut Vec<(Hit, [f32; N_FEATS])>,
        p3: Option<Vec<(Hit, [f32; N_FEATS])>>,
        p4: Option<Vec<(Hit, [f32; N_FEATS])>>,
        x2: Option<usize>,
        p5: Option<Vec<(Hit, [f32; N_FEATS])>>,
    ) {
        match (p3, p4, x2, p5) {
            (Some(candidate), None, None, None)
            | (None, Some(candidate), None, None)
            | (None, None, None, Some(candidate)) => *best = candidate,
            (None, None, Some(position), None) if position < best.len() => {
                let mut chosen = best.remove(position);
                if !chosen.0.flags.contains(&"de_street_locality_qualifier") {
                    chosen.0.flags.push("de_street_locality_qualifier");
                }
                best.insert(0, chosen);
            }
            // P3, P4, P5 and X2 are intended to be disjoint typed mechanisms.
            // If a future parser change creates overlap, preserve the
            // established result instead of choosing by order or data order.
            _ => {}
        }
    }

    fn de_p3_current_top_already_complete(
        current: &[(Hit, [f32; N_FEATS])],
        spec: &DeStrictSourceStreetTypoSpec,
    ) -> bool {
        let expected_house = spec.house_number.to_string();
        current.first().is_some_and(|(hit, _)| {
            hit.precision == "house"
                && hit.housenumber.as_deref() == Some(expected_house.as_str())
                && hit.postcode == spec.postcode_raw
        })
    }

    /// A strict P3 request may name a postcode for which an older or partial
    /// index has no exact-postcode street roster at all.  In that case P3
    /// cannot manufacture the requested address, but it also must not retain
    /// a same-house hit contradicted by both of the user's explicit locality
    /// and postcode fields.  An empty row postcode is unknown, not a
    /// contradiction.  This is deliberately a narrow fail-closed filter: a
    /// hit compatible with either explicit field is preserved.
    fn de_p3_filter_absent_postcode_conflicts(
        current: &[(Hit, [f32; N_FEATS])],
        spec: &DeStrictSourceStreetTypoSpec,
    ) -> Option<Vec<(Hit, [f32; N_FEATS])>> {
        let expected_house = spec.house_number.to_string();
        let mut removed = false;
        let retained = current
            .iter()
            .filter_map(|(hit, features)| {
                let same_house = hit.housenumber.as_deref() == Some(expected_house.as_str());
                let postcode_conflicts =
                    !hit.postcode.is_empty() && hit.postcode != spec.postcode_raw;
                let source_locality = de_product_normalize_text(&hit.commune);
                let product_alias_proves_locality = hit.flags.contains(&"de_city_alias")
                    || hit.flags.contains(&"de_official_commune_alias");
                let locality_conflicts = !product_alias_proves_locality
                    && !de_p3_locality_compatible(&spec.normalized_locality, &source_locality);
                if same_house && postcode_conflicts && locality_conflicts {
                    removed = true;
                    None
                } else {
                    Some((
                        Hit {
                            lat: hit.lat,
                            lon: hit.lon,
                            precision: hit.precision,
                            score: hit.score,
                            confidence: hit.confidence,
                            street: hit.street.clone(),
                            housenumber: hit.housenumber.clone(),
                            commune: hit.commune.clone(),
                            postcode: hit.postcode.clone(),
                            flags: hit.flags.clone(),
                            region: hit.region.clone(),
                            distance_m: hit.distance_m,
                        },
                        *features,
                    ))
                }
            })
            .collect();
        removed.then_some(retained)
    }

    /// P3 is not the generic fuzzy collector.  Its bounded exact-postcode
    /// roster is independent of source FST keys; admission proves the exact
    /// source house/postcode, compatible locality, one global display-street
    /// identity, first codepoint, compact OSA 1..=2 and the frozen 0.90 ratio.
    fn de_strict_source_street_typo_fallback(
        &self,
        raw: &str,
        current: &[(Hit, [f32; N_FEATS])],
        k: usize,
    ) -> Option<Vec<(Hit, [f32; N_FEATS])>> {
        let spec = de_strict_source_street_typo_spec(raw)?;
        if Self::de_p3_current_top_already_complete(current, &spec) {
            return None;
        }

        let mut exact_cache = HashMap::new();
        let mut candidates: HashMap<String, Vec<(u32, bool)>> = HashMap::new();
        // The source FST key is intentionally absent from this loop: the
        // product predicate is defined over display street + exact index
        // metadata, and must see a qualifying identity even behind a stale or
        // unrelated `nom_voie_norm` key.
        let Some(postcode_streets) = self.de_postcode_street_bucket(spec.postcode) else {
            return Self::de_p3_filter_absent_postcode_conflicts(current, &spec);
        };
        for &(_, sid) in postcode_streets {
            let metadata = self.street_meta(sid);
            let source_street = de_product_normalize_street(self.name(metadata.name_off));
            if source_street.is_empty()
                || source_street.len() > 96
                || source_street == spec.normalized_street
                || source_street
                    .bytes()
                    .next()
                    .zip(spec.normalized_street.bytes().next())
                    .is_none_or(|(source, query)| source != query)
            {
                continue;
            }
            let distance = de_compact_osa_distance(&spec.normalized_street, &source_street);
            if !(1..=2).contains(&distance)
                || !de_sequence_matcher_ratio_at_least_090(&spec.normalized_street, &source_street)
            {
                continue;
            }
            let source_commune = de_product_normalize_text(self.commune_name(metadata.commune_id));
            if !de_p3_locality_compatible(&spec.normalized_locality, &source_commune)
                || !self.exact_house_full_postcode_set_candidate_cached(
                    &mut exact_cache,
                    sid,
                    &metadata,
                    spec.house_number,
                    0,
                    &[],
                    spec.postcode,
                    &spec.postcode_raw,
                )
            {
                continue;
            }
            candidates
                .entry(source_street)
                .or_default()
                .push((sid, source_commune == spec.normalized_locality));
        }
        if candidates.len() != 1 {
            return None;
        }
        let mut identity = candidates.into_values().next()?;
        identity.sort_by_key(|candidate| candidate.0);
        identity.dedup_by_key(|candidate| candidate.0);
        // The predicate requires one semantic street identity, not one physical
        // row.  Multiple exact rows with the same allowed display projection do
        // not change admission; the lowest stable SID only chooses the output.
        let (sid, commune_exact) = *identity.first()?;
        let query_words: Vec<Vec<char>> = spec
            .normalized_street
            .split_whitespace()
            .map(|word| word.chars().collect())
            .collect();
        let features = Feats {
            street_fuzzy: true,
            commune_exact,
            commune_prefix: !commune_exact,
            pc_exact: true,
            pc_dept: true,
            ..Default::default()
        };
        let mut ranked = self.make_hit(
            sid,
            features,
            Some(spec.house_number),
            0,
            Some(spec.postcode),
            &query_words,
        );
        let made_features = Feats::from_vec(&ranked.1);
        if ranked.0.precision != "house"
            || !made_features.house_exact_rep
            || !made_features.pc_exact
            || ranked.0.housenumber.as_deref() != Some(spec.house_number.to_string().as_str())
            || ranked.0.postcode != spec.postcode_raw
        {
            return None;
        }
        if !ranked.0.flags.contains(&"de_strict_source_street_typo") {
            ranked.0.flags.push("de_strict_source_street_typo");
        }
        let mut exact = vec![(ranked.0, ranked.1)];
        exact.truncate(k);
        Some(exact)
    }

    /// P4 accepts only one of four typed raw syntaxes. Once parsed, it scans
    /// the complete bounded typed-postcode bucket and re-proves exact display
    /// street/locality plus the full house/set/postcode contract. Recall is
    /// therefore independent of the spelling used by an internal source FST
    /// key, while ambiguity and bucket overflow still fail closed.
    fn de_audited_compound_fallback(
        &self,
        raw: &str,
        k: usize,
    ) -> Option<Vec<(Hit, [f32; N_FEATS])>> {
        let spec = de_audited_compound_spec(raw)?;
        let mut street_forms = de_product_street_forms(&spec.street);
        if !street_forms.contains(&spec.normalized_street) {
            street_forms.push(spec.normalized_street.clone());
        }
        if street_forms.len() > 32 {
            return None;
        }
        let commune_ids = self.communes_by_name(&spec.normalized_locality);
        if commune_ids.is_empty() || commune_ids.len() > 16 {
            return None;
        }
        if street_forms
            .len()
            .checked_mul(commune_ids.len())
            .is_none_or(|attempts| attempts > 64)
        {
            return None;
        }
        let postcode_streets = self.de_postcode_street_bucket(spec.postcode)?;

        let mut exact_cache = HashMap::new();
        let mut candidate_sids = Vec::new();
        for &(_, sid) in postcode_streets {
            #[cfg(test)]
            DE_P4_POSTCODE_BUCKET_SCAN_ROWS.with(|rows| {
                rows.set(rows.get().saturating_add(1));
            });
            let metadata = self.street_meta(sid);
            if de_product_normalize_text(self.commune_name(metadata.commune_id))
                != spec.normalized_locality
                || de_product_normalize_street(self.name(metadata.name_off))
                    != spec.normalized_street
                || !self.exact_house_full_postcode_set_candidate_cached(
                    &mut exact_cache,
                    sid,
                    &metadata,
                    spec.primary_house,
                    0,
                    &spec.additional_houses,
                    spec.postcode,
                    &spec.postcode_raw,
                )
            {
                continue;
            }
            #[cfg(test)]
            DE_P4_POSTCODE_BUCKET_MATCHING_SIDS.with(|matches| {
                matches.set(matches.get().saturating_add(1));
            });
            candidate_sids.push(sid);
        }
        candidate_sids.sort_unstable();
        candidate_sids.dedup();
        let [sid] = candidate_sids.as_slice() else {
            return None;
        };
        let query_words: Vec<Vec<char>> = spec
            .normalized_street
            .split_whitespace()
            .map(|word| word.chars().collect())
            .collect();
        let features = Feats {
            street_exact: true,
            commune_exact: true,
            pc_exact: true,
            pc_dept: true,
            ..Default::default()
        };
        let (mut hit, feature_vector, _, _, _, _) = self.make_hit(
            *sid,
            features,
            Some(spec.primary_house),
            0,
            Some(spec.postcode),
            &query_words,
        );
        let made_features = Feats::from_vec(&feature_vector);
        let expected_house = spec.primary_house.to_string();
        if hit.precision != "house"
            || hit.housenumber.as_deref() != Some(expected_house.as_str())
            || hit.postcode != spec.postcode_raw
            || de_product_normalize_text(&hit.commune) != spec.normalized_locality
            || de_product_normalize_street(&hit.street) != spec.normalized_street
            || !made_features.street_exact
            || !made_features.house_exact_rep
            || !made_features.pc_exact
        {
            return None;
        }
        if !hit.flags.contains(&"de_audited_compound") {
            hit.flags.push("de_audited_compound");
        }
        if matches!(
            spec.shape,
            DeAuditedCompoundShape::StreetHouseRangeBalancedAccessParenthetical
        ) {
            hit.flags.push("de_house_set_exact");
        }
        let mut exact = vec![(hit, feature_vector)];
        exact.truncate(k);
        Some(exact)
    }

    fn de_comma_postcode_house_rescue(
        &self,
        raw: &str,
        k: usize,
        focus: Option<&QueryFocus>,
    ) -> Option<Vec<(Hit, [f32; N_FEATS])>> {
        if de_parenthetical_locality_uses_slash_qualifier(raw) {
            return None;
        }
        // The parser itself remains deliberately narrow.  Let the bounded DE
        // structural variants first remove only proven recipient/subaddress
        // noise or reorder the exact locality-first shape, then require the
        // same unique exact street+house+postcode proof as a canonical query.
        for variant in crate::de::query_variants(raw) {
            let Some((query, postcode, locality_tail)) =
                de_comma_postcode_house_rescue_query(&variant.query)
            else {
                continue;
            };
            let Some(mut exact) = self.de_postcode_house_rescue_parsed(
                raw,
                &query,
                postcode,
                &locality_tail,
                k,
                focus,
                None,
                &[],
                false,
            ) else {
                continue;
            };
            Self::annotate_de_effects(&mut exact, &variant.effects);
            return Some(exact);
        }
        None
    }

    fn de_compact_house_pair_left_rescue(
        &self,
        raw: &str,
        k: usize,
        focus: Option<&QueryFocus>,
    ) -> Option<Vec<(Hit, [f32; N_FEATS])>> {
        let (query, postcode, locality_tail, effect) =
            de_compact_house_pair_left_rescue_query(raw)?;
        self.de_postcode_house_rescue_parsed(
            raw,
            &query,
            postcode,
            &locality_tail,
            k,
            focus,
            Some(effect),
            &[],
            false,
        )
    }

    /// A strict, product-only Wave-A override.  It is intentionally narrower
    /// than the delivery-cleanup rescue: only a canonical comma form or one
    /// compact two-endpoint literal is admitted.  Soft locality text may lose
    /// only after the runtime index proves one unique exact street/house-set/PLZ
    /// candidate; Frankfurt and variant-required communes remain hard guards.
    fn de_strict_postcode_house_set_override(
        &self,
        raw: &str,
        current: &[(Hit, [f32; N_FEATS])],
        k: usize,
        focus: Option<&QueryFocus>,
    ) -> Option<Vec<(Hit, [f32; N_FEATS])>> {
        let mut exact = if let Some((query, postcode, locality_tail)) =
            de_comma_postcode_house_rescue_query(raw)
        {
            if !Self::de_strict_postcode_house_override_allowed(current, postcode) {
                return None;
            }
            let allow_soft_locality_override = !self.de_typed_postcode_near_has_locality_evidence(
                current,
                postcode,
                &locality_tail,
            );
            self.de_postcode_house_rescue_parsed(
                raw,
                &query,
                postcode,
                &locality_tail,
                k,
                focus,
                None,
                &[],
                allow_soft_locality_override,
            )?
        } else {
            let spec = de_compact_house_pair_spec(raw)?;
            if !Self::de_strict_postcode_house_override_allowed(current, spec.postcode) {
                return None;
            }
            let allow_soft_locality_override = !self.de_typed_postcode_near_has_locality_evidence(
                current,
                spec.postcode,
                &spec.locality_tail,
            );
            self.de_postcode_house_rescue_parsed(
                raw,
                &spec.query,
                spec.postcode,
                &spec.locality_tail,
                k,
                focus,
                Some(spec.effect),
                &[spec.right],
                allow_soft_locality_override,
            )?
        };
        for (hit, _) in &mut exact {
            if !hit.flags.contains(&"de_exact_postcode_override") {
                hit.flags.push("de_exact_postcode_override");
            }
        }
        Some(exact)
    }

    // Frozen release: grouping parsed rescue inputs would rewrite the validated call sites.
    #[allow(clippy::too_many_arguments)]
    fn de_postcode_house_rescue_parsed(
        &self,
        raw: &str,
        query: &str,
        postcode: u32,
        locality_tail: &str,
        k: usize,
        focus: Option<&QueryFocus>,
        structural_effect: Option<crate::de::Effect>,
        additional_exact_house_numbers: &[u32],
        allow_soft_locality_override: bool,
    ) -> Option<Vec<(Hit, [f32; N_FEATS])>> {
        let hard_commune = crate::de::frankfurt_qualifier(raw).or_else(|| {
            let words: std::collections::HashSet<&str> = locality_tail.split_whitespace().collect();
            if words.contains("frankfurt") && words.contains("oder") {
                Some("frankfurt oder")
            } else if words.contains("frankfurt") && words.contains("main") {
                Some("frankfurt am main")
            } else {
                None
            }
        });
        let mut exact: Vec<DePostcodeHouseCandidate> = Vec::new();
        let mut scan_budget = de_postcode_house_rescue_scan_limit();
        let mut prepared_seen = HashSet::new();
        let mut seen_phrases = HashSet::new();
        for variant in crate::de::query_variants(query) {
            let prepared = prepared_query_key(&variant.query);
            if !prepared_seen.insert(prepared.clone()) {
                continue;
            }
            let mut candidate = match self.query_feats_prepared_postcode_house_rescue(
                &prepared,
                k.max(20),
                focus,
                additional_exact_house_numbers,
                &mut scan_budget,
                &mut seen_phrases,
            ) {
                Ok(candidate) => candidate,
                Err(()) => return None,
            };
            if let Some(expected) = variant.required_commune.as_deref() {
                candidate.retain(|candidate| normalize(&candidate.hit.commune) == expected);
            }
            candidate.retain(|candidate| {
                let hit = &candidate.hit;
                let features = Feats::from_vec(&candidate.features);
                features.street_exact
                    && features.house_exact_rep
                    && features.pc_exact
                    && Self::postcode_numeric_prefix(&hit.postcode) == Some(postcode)
                    && ((allow_soft_locality_override && hard_commune.is_none())
                        || self.de_postcode_house_locality_compatible(
                            locality_tail,
                            hard_commune,
                            postcode,
                            hit,
                        ))
            });
            let mut applied_effects = variant.effects.clone();
            if let Some(effect) = structural_effect {
                if !applied_effects.contains(&effect) {
                    applied_effects.push(effect);
                }
            }
            for candidate in &mut candidate {
                for effect in &applied_effects {
                    for flag in Self::de_effect_flags(*effect) {
                        if !candidate.hit.flags.contains(flag) {
                            candidate.hit.flags.push(flag);
                        }
                    }
                }
            }
            for item in candidate {
                if let Some(existing) = exact
                    .iter()
                    .find(|existing| existing.source_sid == item.source_sid)
                {
                    if !existing.same_product_projection(&item) {
                        return None;
                    }
                } else {
                    exact.push(item);
                }
            }
        }
        if structural_effect.is_none() && (!allow_soft_locality_override || hard_commune.is_some())
        {
            let strongest = exact
                .iter()
                .map(|candidate| {
                    Self::de_postcode_house_locality_score(
                        locality_tail,
                        hard_commune,
                        postcode,
                        &candidate.hit.commune,
                    )
                })
                .max()
                .unwrap_or(0);
            if strongest == 0 {
                return None;
            }
            exact.retain(|candidate| {
                Self::de_postcode_house_locality_score(
                    locality_tail,
                    hard_commune,
                    postcode,
                    &candidate.hit.commune,
                ) == strongest
            });
        }
        if exact.len() != 1 {
            return None;
        }
        if !exact[0].hit.flags.contains(&"de_postcode_house") {
            exact[0].hit.flags.push("de_postcode_house");
        }
        if structural_effect.is_some() && !exact[0].hit.flags.contains(&"de_house_left_endpoint") {
            exact[0].hit.flags.push("de_house_left_endpoint");
        }
        if !additional_exact_house_numbers.is_empty()
            && !exact[0].hit.flags.contains(&"de_house_set_exact")
        {
            exact[0].hit.flags.push("de_house_set_exact");
        }
        let only = exact.pop()?;
        let mut out = vec![(only.hit, only.features)];
        out.truncate(k);
        Some(out)
    }

    /// The ordinary query remains first and wins ties.  DE-only alternatives are
    /// compared by the same legacy evidence (exact street/commune/postcode/house)
    /// used to choose parse hypotheses, so a fallback can replace a weak fuzzy hit
    /// but cannot dislodge an equally supported canonical spelling.
    fn query_feats_country_variants(
        &self,
        raw: &str,
        k: usize,
        focus: Option<&QueryFocus>,
    ) -> Vec<(Hit, [f32; N_FEATS])> {
        if self.country() != Some("de") {
            return self.query_feats_d(raw, k, 0, focus, None, None, false, None);
        }
        let original_cityless_street = Self::de_cityless_street(raw);
        // Only the exact typed X2 surface needs the already-ranked frozen
        // top-5 window for a caller requesting top-1. Every other DE query
        // retains its established caller-k work bound.
        let product_k = if de_street_locality_qualifier_spec(raw).is_some() {
            k.max(5)
        } else {
            k
        };
        // Freeze the user's normalized post-postcode locality before any DE variant or
        // city-alias rewrite.  Every variant may change the lookup query, but none may
        // manufacture stronger retained-locality evidence than the text the user supplied.
        let de_original_postcode_context = de_postcode_context(&normalize(raw));
        let de_original_postcode = de_original_postcode_context
            .as_ref()
            .map(|(postcode, _)| *postcode);
        let de_original_postcode_tail = de_original_postcode_context
            .as_ref()
            .map(|(_, tail)| tail.as_str());
        let de_parenthetical_subaddress_commune = crate::de::parenthetical_subaddress_commune(raw);
        let mut best: Vec<(Hit, [f32; N_FEATS])> = Vec::new();
        let mut best_quality: Option<(i32, u8, u8, f32)> = None;
        let frankfurt = crate::de::frankfurt_qualifier(raw);
        let de_postal_tail_eligible = crate::de::postal_tail_eligible(raw);
        let variants = crate::de::query_variants(raw);
        let official_alias_constrained = variants.iter().any(|variant| {
            variant
                .effects
                .contains(&crate::de::Effect::OfficialCommuneAlias)
                && variant.required_commune.is_some()
        });
        let alias_target = variants
            .iter()
            .find_map(|variant| variant.required_commune.clone());
        let alias_constrained = alias_target.is_some();
        let hard_commune = frankfurt.map(str::to_string).or(alias_target);
        let search_k = if hard_commune.is_some() {
            product_k.max(20)
        } else {
            product_k
        };
        // Raw and normalized DE variants usually converge to the exact same
        // prepared query. A previous non-city result is reusable only when the
        // variant effects and commune constraint are also identical. City/empty
        // outcomes remain uncached because the final settlement fallback still
        // inspects raw comma-delimited segments.
        let mut reusable_prepared: Vec<(String, Vec<crate::de::Effect>, Option<String>)> =
            Vec::new();
        for variant in variants {
            if variant
                .required_commune
                .as_deref()
                .is_some_and(|target| self.communes_fst.get(target.as_bytes()).is_none())
            {
                continue;
            }
            let prepared = prepared_query_key(&variant.query);
            if reusable_prepared.iter().any(|(seen, effects, required)| {
                seen == &prepared
                    && effects == &variant.effects
                    && required == &variant.required_commune
            }) {
                continue;
            }
            #[cfg(test)]
            DE_COUNTRY_VARIANT_PREPARED_SEARCH_CALLS
                .with(|calls| calls.set(calls.get().saturating_add(1)));
            let mut candidate = self.query_feats_d(
                &variant.query,
                search_k,
                0,
                focus,
                de_original_postcode_tail,
                de_original_postcode,
                de_postal_tail_eligible,
                original_cityless_street.as_deref(),
            );
            if candidate
                .first()
                .is_some_and(|(hit, _)| hit.precision != "city")
            {
                reusable_prepared.push((
                    prepared,
                    variant.effects.clone(),
                    variant.required_commune.clone(),
                ));
            }
            if variant.effects.contains(&crate::de::Effect::PostcodeZero) {
                candidate.retain(|(_, features)| Feats::from_vec(features).pc_exact);
            }
            if let Some(expected) = hard_commune.as_deref() {
                candidate.retain(|(hit, _)| normalize(&hit.commune) == expected);
            }
            if official_alias_constrained {
                if hard_commune.as_deref() == Some("ludwigshafen am rhein") {
                    let Some(position) =
                        Self::de_ludwigshafen_official_alias_candidate_position(raw, &candidate)
                    else {
                        continue;
                    };
                    let chosen = candidate.remove(position);
                    candidate.insert(0, chosen);
                } else if !Self::de_official_commune_alias_candidate_is_exact(&candidate) {
                    continue;
                }
            }
            candidate.truncate(product_k);
            if variant
                .effects
                .contains(&crate::de::Effect::ParentheticalSubaddress)
                && !Self::de_parenthetical_subaddress_may_fill(
                    &best,
                    &candidate,
                    de_parenthetical_subaddress_commune.as_deref(),
                )
            {
                continue;
            }
            let Some(quality) = Self::de_variant_quality(&candidate) else {
                continue;
            };
            if variant.effects.contains(&crate::de::Effect::Abbreviation)
                && !Self::de_abbreviation_candidate_may_displace(
                    &candidate,
                    &best,
                    de_original_postcode_tail,
                )
            {
                continue;
            }
            if variant.effects.contains(&crate::de::Effect::PostcodeZero)
                && best_quality.is_some_and(|current| quality.1 < current.1)
            {
                // A four-digit token is ambiguous with a house number.  Padding
                // it may add exact-postcode evidence, but must never downgrade
                // an existing house/near/interpolation answer to street level.
                continue;
            }
            let proven_recipient_tie = best_quality.is_some_and(|current| {
                quality == current
                    && variant
                        .effects
                        .contains(&crate::de::Effect::RecipientPrefix)
                    && Self::de_recipient_cleanup_breaks_dropped_prefix_tie(&candidate, &best)
            });
            if best_quality.is_none_or(|current| Self::de_quality_is_better(quality, current))
                || proven_recipient_tie
            {
                let mut applied_effects = variant.effects.clone();
                if official_alias_constrained {
                    if !applied_effects.contains(&crate::de::Effect::OfficialCommuneAlias) {
                        applied_effects.push(crate::de::Effect::OfficialCommuneAlias);
                    }
                } else if alias_constrained
                    && !applied_effects.contains(&crate::de::Effect::CityAlias)
                {
                    // Even an otherwise successful original spelling is admissible only
                    // because the exonym supplied this hard commune constraint.
                    applied_effects.push(crate::de::Effect::CityAlias);
                }
                Self::annotate_de_effects(&mut candidate, &applied_effects);
                best = candidate;
                best_quality = Some(quality);
            }
        }
        let ludwigshafen_alias_selected = hard_commune.as_deref() == Some("ludwigshafen am rhein")
            && best
                .first()
                .is_some_and(|(hit, _)| hit.flags.contains(&"de_official_commune_alias"));
        if !ludwigshafen_alias_selected && Self::de_should_try_postcode_house_rescue(raw, &best) {
            if let Some(rescue) = self.de_comma_postcode_house_rescue(raw, k, focus) {
                best = rescue;
            } else if let Some(rescue) =
                self.de_strict_postcode_house_set_override(raw, &best, k, focus)
            {
                best = rescue;
            } else if !Self::de_has_complete_postcode_house_evidence(&best) {
                if let Some(rescue) = self.de_compact_house_pair_left_rescue(raw, k, focus) {
                    best = rescue;
                }
            }
        }
        let x2 = Self::de_street_locality_qualifier_position(raw, &best);
        let p3 = self.de_strict_source_street_typo_fallback(raw, &best, k);
        let p4 = self.de_audited_compound_fallback(raw, k);
        let p5 = self.de_blank_postcode_exact_house_fallback(raw, &best, k, focus);
        Self::de_product_fallback_arbitration(&mut best, p3, p4, x2, p5);
        if frankfurt.is_some() {
            for (hit, _) in &mut best {
                if !hit.flags.contains(&"de_frankfurt") {
                    hit.flags.push("de_frankfurt");
                }
            }
        }
        best.truncate(k);
        best
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

    // Frozen release: retain each provenance flag and the validated recursive call sites.
    #[allow(clippy::too_many_arguments)]
    fn query_feats_d(
        &self,
        raw: &str,
        k: usize,
        depth: u8,
        focus: Option<&QueryFocus>,
        de_original_postcode_tail: Option<&str>,
        de_original_postcode: Option<u32>,
        de_postal_tail_eligible: bool,
        original_cityless_street: Option<&str>,
    ) -> Vec<(Hit, [f32; N_FEATS])> {
        let normalized_raw = prepared_query_key(raw);
        let q = self.expand_city_aliases(&normalized_raw);
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
        let mut hits =
            self.query_feats_prepared_cityless(&q, prepared_k, focus, original_cityless_street);
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
                let h = self.query_feats_d(
                    &toks[d..].join(" "),
                    k,
                    depth + 1,
                    focus,
                    de_original_postcode_tail,
                    de_original_postcode,
                    de_postal_tail_eligible,
                    original_cityless_street,
                );
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
            let mut h = self.query_feats_prepared(&toks[drop..].join(" "), k, focus);
            if let (Some(postcode), Some(postcode_tail)) =
                (de_original_postcode, de_original_postcode_tail)
            {
                h.retain(|(hit, features)| {
                    de_prefix_drop_preserves_postcode_locality(
                        hit,
                        features,
                        postcode,
                        postcode_tail,
                    )
                });
            }
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
                let shortened = toks[..toks.len() - drop].join(" ");
                let dropped_tail = toks[toks.len() - drop..].join(" ");
                let h = self.query_feats_prepared_with_retained_locality(
                    &shortened,
                    k,
                    focus,
                    de_retained_locality(
                        de_original_postcode_tail,
                        &dropped_tail,
                        de_postal_tail_eligible,
                    ),
                );
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
                    h.truncate(k);
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
        requested_postcode: Option<u32>,
        qwords: &[Vec<char>],
    ) -> RankedHit {
        let m = self.street_meta(sid);
        f.numero_present = numero.is_some();
        let mut snap_delta = 0u32;
        let (lat, lon, precision, housenumber, house_postcode_off) =
            match numero.and_then(|nm| self.find_house(sid, &m, nm, rep, requested_postcode)) {
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
        let rendered_postcode = self.postcode_for_house(&m, house_postcode_off);
        if self.format_version >= 7
            && m.postcode_disp_off == PC_DISP_AMBIGUOUS
            && housenumber.is_some()
        {
            if let Some(requested) = requested_postcode {
                let represented = Self::postcode_numeric_prefix(&rendered_postcode);
                f.pc_exact = requested != 0 && represented == Some(requested);
                f.pc_dept = requested != 0
                    && represented.is_some_and(|postcode| postcode / 1000 == requested / 1000);
            }
        }
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
                postcode: rendered_postcode,
                flags: match_flags(&f),
                region: self.admin_at(lat, lon), // WOF region in forward answers too
                distance_m: None,
            },
            f.to_vec(),
            name_sim,
            snap_delta,
            self.commune_prominence(m.commune_id),
            sid,
        )
    }

    /// STRUCTURED INPUT: pre-parsed street/number/city/postcode fields use the
    /// direct structured resolver first.  A DE sheet also compares the canonical
    /// field join with the country-scoped free-form fallbacks.  The structured
    /// result keeps ties, so ordinary exact input is unchanged while Munich,
    /// umlaut/digraph and compound street-type behavior stays in parity with the
    /// public free-form API.
    pub fn query_structured(
        &self,
        street: &str,
        number: Option<&str>,
        city: &str,
        postcode: Option<&str>,
        k: usize,
    ) -> Vec<(Hit, [f32; N_FEATS])> {
        if self.country() != Some("de") || k == 0 {
            return self.query_structured_primary(street, number, city, postcode, k);
        }
        let k = bound_k(k);
        let street = bound_query(street);
        let number = number.map(bound_query);
        let city = bound_query(city);
        let postcode = postcode.map(bound_query);
        let _rules = crate::rules::scope(self.rules);
        let mut primary = self.query_structured_primary(street, number, city, postcode, k);
        // The city field has an explicit boundary, so apply DE city/abbreviation
        // variants directly instead of asking the free-form segmenter to infer it.
        // Alias targets remain hard postconditions; an equal-quality alias result
        // wins because the constraint itself was needed to interpret the field.
        for variant in crate::de::query_variants(city).into_iter().skip(1) {
            let mut candidate =
                self.query_structured_primary(street, number, &variant.query, postcode, k);
            if let Some(expected) = variant.required_commune.as_deref() {
                candidate.retain(|(hit, _)| normalize(&hit.commune) == expected);
            }
            let (Some(candidate_quality), current_quality) = (
                Self::de_variant_quality(&candidate),
                Self::de_variant_quality(&primary),
            ) else {
                continue;
            };
            let alias_tie = variant.effects.contains(&crate::de::Effect::CityAlias)
                && current_quality == Some(candidate_quality);
            if current_quality
                .is_none_or(|current| Self::de_quality_is_better(candidate_quality, current))
                || alias_tie
            {
                Self::annotate_de_effects(&mut candidate, &variant.effects);
                primary = candidate;
            }
        }
        let joined = [street, number.unwrap_or(""), postcode.unwrap_or(""), city]
            .iter()
            .filter(|value| !value.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        let mut fallback = self.query_feats_country_variants(&joined, k, None);
        let use_fallback = match (
            Self::de_variant_quality(&fallback),
            Self::de_variant_quality(&primary),
        ) {
            (Some(candidate), Some(current)) => Self::de_quality_is_better(candidate, current),
            (Some(_), None) => true,
            _ => false,
        };
        if use_fallback {
            Self::monotone_confidence(&mut fallback);
            fallback
        } else {
            primary
        }
    }

    /// Direct structured resolver: no street/commune boundary guessing.
    fn query_structured_primary(
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
        let mut de_street_phrases = HashSet::new();
        if self.country() == Some("de") {
            for variant in crate::de::street_variants(&sn) {
                if !phrases.contains(&variant) {
                    de_street_phrases.insert(variant.clone());
                    phrases.push(variant);
                }
            }
        }
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
                            de_street_type: de_street_phrases.contains(ph),
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
            .map(|(sid, f)| self.make_hit(sid, f, numero, rep, pc, &qwords))
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
                hits.into_iter().map(|(h, f, _, _, _, _)| (h, f)).collect();
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
        self.query_feats_prepared_with_retained_locality(q, k, focus, None)
    }

    fn query_feats_prepared_postcode_house_rescue(
        &self,
        q: &str,
        k: usize,
        focus: Option<&QueryFocus>,
        additional_exact_house_numbers: &[u32],
        scan_budget: &mut usize,
        seen_phrases: &mut HashSet<String>,
    ) -> Result<Vec<DePostcodeHouseCandidate>, ()> {
        let mut overflowed = false;
        let hits = self.query_feats_prepared_internal(
            q,
            k,
            focus,
            None,
            true,
            additional_exact_house_numbers,
            scan_budget,
            seen_phrases,
            &mut overflowed,
            None,
        );
        if overflowed {
            Err(())
        } else {
            Ok(hits
                .into_iter()
                .map(
                    |(hit, features, _, _, _, source_sid)| DePostcodeHouseCandidate {
                        source_sid,
                        hit,
                        features,
                    },
                )
                .collect())
        }
    }

    fn query_feats_prepared_with_retained_locality(
        &self,
        q: &str,
        k: usize,
        focus: Option<&QueryFocus>,
        retained_locality: Option<DeRetainedLocality<'_>>,
    ) -> Vec<(Hit, [f32; N_FEATS])> {
        self.query_feats_prepared_context(q, k, focus, retained_locality, None)
    }

    fn query_feats_prepared_cityless(
        &self,
        q: &str,
        k: usize,
        focus: Option<&QueryFocus>,
        original_cityless_street: Option<&str>,
    ) -> Vec<(Hit, [f32; N_FEATS])> {
        self.query_feats_prepared_context(q, k, focus, None, original_cityless_street)
    }

    fn query_feats_prepared_context(
        &self,
        q: &str,
        k: usize,
        focus: Option<&QueryFocus>,
        retained_locality: Option<DeRetainedLocality<'_>>,
        original_cityless_street: Option<&str>,
    ) -> Vec<(Hit, [f32; N_FEATS])> {
        let mut overflowed = false;
        let mut scan_budget = 0;
        let mut seen_phrases = HashSet::new();
        self.query_feats_prepared_internal(
            q,
            k,
            focus,
            retained_locality,
            false,
            &[],
            &mut scan_budget,
            &mut seen_phrases,
            &mut overflowed,
            original_cityless_street,
        )
        .into_iter()
        .map(|(hit, features, _, _, _, _)| (hit, features))
        .collect()
    }

    // Frozen release: preserve the existing provenance, scan-budget and overflow call wiring.
    #[allow(clippy::too_many_arguments)]
    fn query_feats_prepared_internal(
        &self,
        q: &str,
        k: usize,
        focus: Option<&QueryFocus>,
        retained_locality: Option<DeRetainedLocality<'_>>,
        de_postcode_house_scan: bool,
        de_postcode_house_additional_numbers: &[u32],
        de_postcode_house_scan_budget: &mut usize,
        de_postcode_house_seen_phrases: &mut HashSet<String>,
        de_postcode_house_scan_overflowed: &mut bool,
        original_cityless_street: Option<&str>,
    ) -> Vec<RankedHit> {
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
            if is_five_digit_postcode(t) {
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
            if let Some((d, suffix)) = compound_house_parts(t) {
                let numero = t[..d].parse().ok();
                // keep consuming suffixes after the compound token, but ONLY
                // letter+digit mixes: the rep dictionary contains junk ("rue", "5")
                // and greedy consumption of it would eat half the street
                let mut rep_s = suffix.to_string();
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
        let mut de_postcode_house_exact_cache = HashMap::new();
        for (hi, h) in hyps.iter().enumerate() {
            let rest = rest_of(h);
            if rest.is_empty() {
                continue;
            }
            let (cand, scan_overflowed) = self.collect_candidates(
                &rest,
                postcode,
                h.numero,
                h.rep,
                h.from_ml,
                de_postcode_house_scan,
                de_postcode_house_additional_numbers,
                de_postcode_house_scan_budget,
                de_postcode_house_seen_phrases,
                &mut de_postcode_house_exact_cache,
            );
            if scan_overflowed {
                *de_postcode_house_scan_overflowed = true;
                return Vec::new();
            }
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
        if best.is_none() && !de_postcode_house_scan {
            for (hi, h) in hyps.iter().enumerate().take(5) {
                let rest = rest_of(h);
                if rest.is_empty() {
                    continue;
                }
                #[cfg(test)]
                DE_POSTCODE_HOUSE_RESCUE_FUZZY_CALLS.with(|calls| {
                    calls.set(calls.get().saturating_add(1));
                });
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
        if best.is_none()
            && !de_postcode_house_scan
            && std::env::var_os("GRIDPIN_NO_SUBSET").is_none()
        {
            for (hi, h) in hyps.iter().enumerate().take(5) {
                let rest = rest_of(h);
                if rest.is_empty() {
                    continue;
                }
                #[cfg(test)]
                DE_POSTCODE_HOUSE_RESCUE_SUBSET_CALLS.with(|calls| {
                    calls.set(calls.get().saturating_add(1));
                });
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
            scored.retain(|(sid, features)| {
                let m = self.street_meta(*sid);
                let p = m.postcode;
                p == 0
                    || p / 1000 == dept
                    || (de_postcode_house_scan
                        && self.country() == Some("de")
                        && self.format_version >= 7
                        && features.street_exact
                        && numero.is_some_and(|requested_number| {
                            self.exact_house_postcode_set_candidate_cached(
                                &mut de_postcode_house_exact_cache,
                                *sid,
                                &m,
                                requested_number,
                                rep,
                                de_postcode_house_additional_numbers,
                                pc,
                            )
                        }))
            });
        }
        // A v7 mixed-postcode street stores the exact postcode at house level.  The
        // ordinary pre-ranking cap cannot see that evidence yet: it sorts StreetMeta
        // before `make_hit` decodes the represented house, so a unique exact
        // street+house+postcode can be cut behind prominent homonyms.  Preserve that one
        // causally complete candidate through the cap.  Ambiguous duplicates fail closed:
        // if more than one street has the same exact house/postcode, none receives this
        // rescue and the established ranking remains authoritative.
        let de_postcode_house_rescue = match (self.country(), postcode, numero) {
            (Some("de"), Some(requested_postcode), Some(requested_number))
                if de_postcode_house_scan && self.format_version >= 7 =>
            {
                let matches: Vec<u32> = scored
                    .iter()
                    .filter_map(|(sid, features)| {
                        let m = self.street_meta(*sid);
                        if !features.street_exact {
                            return None;
                        }
                        self.exact_house_postcode_set_candidate_cached(
                            &mut de_postcode_house_exact_cache,
                            *sid,
                            &m,
                            requested_number,
                            rep,
                            de_postcode_house_additional_numbers,
                            requested_postcode,
                        )
                        .then_some(*sid)
                    })
                    .collect();
                (matches.len() == 1).then(|| matches[0])
            }
            _ => None,
        };
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
        let pre_rank_limit = k.max(10) * 3;
        if scored.len() > pre_rank_limit {
            if let Some(rescue_sid) = de_postcode_house_rescue {
                if let Some(position) = scored
                    .iter()
                    .position(|(sid, _, _)| *sid == rescue_sid)
                    .filter(|position| *position >= pre_rank_limit)
                {
                    let rescue = scored.remove(position);
                    scored.truncate(pre_rank_limit);
                    scored.push(rescue);
                } else {
                    scored.truncate(pre_rank_limit);
                }
            } else {
                scored.truncate(pre_rank_limit);
            }
        }
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

        let mut hits: Vec<RankedHit> = Vec::new();
        for (sid, f) in scored {
            hits.push(self.make_hit(sid, f, numero, rep, postcode, &qwords));
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
        // unaffected. In Germany, an exact postcode match on the already sorted winner is
        // stronger evidence than this cityless prior: tail recovery can drop an unrecognized
        // locality while retaining its postcode, and promoting the anchor then inverts the
        // postcode-aware score order. A parsed postcode without an exact candidate is not
        // enough to disable the prior (the anchor may still be the only nearby result).
        // Keep the established behavior unchanged for every other country.
        let german_sorted_top_has_exact_postcode = self.country() == Some("de")
            && postcode.is_some()
            && hits.first().is_some_and(|t| Feats::from_vec(&t.1).pc_exact);
        if focus.is_none() && !has_named_city && !german_sorted_top_has_exact_postcode {
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
                            && de_capital_prior_candidate_allowed(
                                self.country(),
                                &hits[0].1,
                                t.0.precision,
                            )
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
        Self::de_cityless_prominence(
            &mut hits,
            self.country(),
            original_cityless_street,
            focus.is_some(),
            postcode,
        );
        // DE retained-locality tie-break for the c2 suffix retry. Only replace the current
        // winner when exactly one later homonym has stronger exact/prefix locality evidence
        // from the original post-postcode tail, and every earlier address-quality comparator
        // is byte-for-byte equal. The exact discarded slice must also reach that commune;
        // this blocks shared-generic-token conflicts such as Gross Roge -> Roge Stadt.
        // Explicit focus keeps its established ordering and never enters this path.
        // DE exact-PLZ c2 tail tie-break.  Run only after CAPITAL and retained-locality:
        // an already exact-postcode winner or a retained-locality decision is final.  The
        // helper also fail-closes outside a DE suffix retry and under explicit focus.
        apply_de_c2_tiebreaks(
            &mut hits,
            retained_locality,
            self.country() == Some("de"),
            focus.is_some(),
            postcode,
        );
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
        hits
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

    // Keep the frozen fixtures' explicit fields; grouping them would rewrite the test call sites.
    #[allow(clippy::too_many_arguments)]
    fn retained_test_hit(
        commune: &str,
        street: &str,
        housenumber: Option<&str>,
        postcode: &str,
        precision: &'static str,
        score: f32,
        features: [f32; N_FEATS],
        flags: Vec<&'static str>,
        name_similarity: i32,
        snap_delta: u32,
    ) -> RankedHit {
        (
            Hit {
                lat: 50.0,
                lon: 8.0,
                precision,
                score,
                confidence: 0.8,
                street: street.to_owned(),
                housenumber: housenumber.map(str::to_owned),
                commune: commune.to_owned(),
                postcode: postcode.to_owned(),
                flags,
                region: None,
                distance_m: None,
            },
            features,
            name_similarity,
            snap_delta,
            1,
            0,
        )
    }

    fn wave_b1_hits() -> Vec<RankedHit> {
        let make = || {
            retained_test_hit(
                "Near",
                "Teststraße",
                Some("12"),
                "",
                "house",
                8.0,
                Feats {
                    street_exact: true,
                    ..Feats::default()
                }
                .to_vec(),
                vec![],
                9,
                0,
            )
        };
        let mut a = make();
        let mut b = make();
        a.4 = 10;
        b.0.commune = "Large".to_owned();
        b.4 = 100;
        vec![a, b]
    }

    #[test]
    fn de_wave_b1_more_prominent_equal_house_wins_stably() {
        let mut hits = wave_b1_hits();
        let key = street_key("Teststraße");
        Index::de_cityless_prominence(&mut hits, Some("de"), Some(&key), false, None);
        assert_eq!(hits[0].0.commune, "Large");
        assert_eq!(hits[1].0.commune, "Near");
        assert!(hits[0].0.flags.contains(&"de_cityless_prominence"));
        hits[1].4 = 100;
        Index::de_cityless_prominence(&mut hits, Some("de"), Some(&key), false, None);
        assert_eq!(hits[0].0.commune, "Large", "ties must retain prior order");
    }

    #[test]
    fn de_wave_b1_scope_preserves_every_explicit_context() {
        let key = street_key("Teststraße");
        for (country, original, focused, pc) in [
            (Some("nl"), Some(key.as_str()), false, None),
            (Some("de"), None, false, None),
            (Some("de"), Some("teststrasseberlin"), false, None),
            (Some("de"), Some(key.as_str()), true, None),
            (Some("de"), Some(key.as_str()), false, Some(12345)),
        ] {
            let mut hits = wave_b1_hits();
            let before = serde_json::to_string(&hits).unwrap();
            Index::de_cityless_prominence(&mut hits, country, original, focused, pc);
            assert_eq!(serde_json::to_string(&hits).unwrap(), before);
        }
    }

    #[test]
    fn de_wave_b1_address_quality_cannot_be_overridden() {
        for case in 0..7 {
            let mut hits = wave_b1_hits();
            match case {
                0 => hits[1].0.precision = "interp",
                1 => hits[1].1[0] = 0.0,
                2 => hits[1].0.street = "Otherstraße".to_owned(),
                3 => hits[1].0.score -= 1.0,
                4 => hits[1].2 -= 1,
                5 => hits[1].3 += 1,
                _ => hits[0].0.precision = "interp",
            }
            Index::de_cityless_prominence(
                &mut hits,
                Some("de"),
                Some(&street_key("Teststraße")),
                false,
                None,
            );
            assert_eq!(hits[0].0.commune, "Near", "case {case}");
        }
    }

    #[test]
    fn de_wave_b1_original_surface_does_not_drop_city_or_postcode() {
        for raw in ["Teststraße 12", "Teststraße 12a", "Teststraße 12 a"] {
            assert_eq!(
                Index::de_cityless_street(raw),
                Some(street_key("Teststraße"))
            );
        }
        for raw in [
            "Teststraße 12, Berlin",
            "Teststraße 12 Berlin",
            "Teststraße 12 12345",
            "Teststraße 12 12345 Berlin",
            "Berlin Teststraße 12",
            "Teststraße Berlin 12",
        ] {
            assert_ne!(
                Index::de_cityless_street(raw),
                Some(street_key("Teststraße")),
                "{raw}"
            );
        }
    }

    #[test]
    fn de_retained_locality_core_is_exact_or_prefix_only() {
        let retained = de_retained_locality(Some("groß roge"), "roge", true).unwrap();
        assert_eq!(retained.postcode_tail, "groß roge");
        assert_eq!(retained.dropped_tail, "roge");
        assert!(de_retained_locality(None, "roge", true).is_none());
        assert_eq!(
            de_postcode_tail("x 11111 noise 22222 frankfurt").as_deref(),
            Some("frankfurt")
        );
        assert!(de_postcode_tail("x 11111").is_none());
        assert_eq!(de_retained_locality_score("lohne", "Lohne, Stadt"), 2);
        assert_eq!(de_retained_locality_score("lohne", "Stadt Lohne"), 2);
        assert_eq!(
            de_retained_locality_score("frankfurt", "Frankfurt am Main"),
            1
        );
        assert_eq!(de_retained_locality_score("frank", "Frankfurt"), 0);
        assert_eq!(
            de_retained_locality_score("neustadt weinstraße", "Neustadt an der Weinstraße"),
            0
        );
        assert_eq!(de_retained_locality_score("groß roge", "Klein Roge"), 0);
        assert!(de_dropped_tail_reaches_commune(
            "frankfurt",
            "Frankfurt am Main"
        ));
        assert!(
            de_dropped_tail_reaches_commune("roge", "Klein Roge"),
            "the full post-postcode context, not this reachability gate alone, rejects the conflict"
        );
    }

    #[test]
    fn de_capital_prior_keeps_an_exact_house_over_a_near_anchor() {
        let mut exact_house = [0.0; N_FEATS];
        exact_house[0] = 1.0;
        exact_house[8] = 1.0;

        assert!(!de_capital_prior_candidate_allowed(
            Some("de"),
            &exact_house,
            "near",
        ));
        assert!(de_capital_prior_candidate_allowed(
            Some("de"),
            &exact_house,
            "interp",
        ));
        assert!(de_capital_prior_candidate_allowed(
            Some("fr"),
            &exact_house,
            "near",
        ));

        exact_house[8] = 0.0;
        assert!(de_capital_prior_candidate_allowed(
            Some("de"),
            &exact_house,
            "near",
        ));
    }

    #[test]
    fn de_exact_locality_aliases_are_postcode_bound_without_cross_products() {
        for (query, postcode, commune) in DE_EXACT_LOCALITY_ALIASES {
            assert!(
                de_exact_locality_alias_matches(query, *postcode, commune),
                "missing exact alias {query:?} / {postcode} / {commune:?}"
            );
            assert!(de_is_exact_locality_alias_query(query, *postcode));
            assert!(!de_exact_locality_alias_matches(
                query,
                postcode.saturating_add(1),
                commune,
            ));
        }
        assert!(!de_exact_locality_alias_matches(
            "Berlin",
            13187,
            "Gesundbrunnen"
        ));
        assert!(!de_exact_locality_alias_matches(
            "Landkirchen",
            23769,
            "Lübbenau"
        ));
        assert!(!de_exact_locality_alias_matches(
            "Zerkwitz", 3222, "Fehmarn"
        ));
    }

    #[test]
    fn de_postcode_house_locality_aliases_are_explicit_and_bounded() {
        for (query, indexed) in [
            ("reichenbach vogt", "Reichenbach im Vogtland"),
            ("sankt wendel", "St. Wendel"),
            ("homburg saar", "Homburg"),
            ("kottmar ot eibau", "Eibau"),
            ("st peter ording", "Sankt Peter-Ording"),
            ("burg auf fehmarn", "Fehmarn"),
        ] {
            assert!(
                de_locality_qualifiers_match(query, indexed),
                "{query:?} must be compatible with {indexed:?}"
            );
        }
        assert!(!de_locality_qualifiers_match(
            "neustadt an der weinstraße",
            "Neustadt am Rübenberge"
        ));
        assert!(!de_locality_qualifiers_match(
            "offenbach",
            "Frankfurt am Main"
        ));
        assert!(!de_locality_qualifiers_match(
            "homburg saar",
            "Bad Homburg vor der Höhe"
        ));
        assert!(!de_locality_qualifiers_match("hallenberg", "Halle"));
        for district in [
            "Adlershof",
            "Charlottenburg",
            "Dahlem",
            "Kaulsdorf",
            "Kreuzberg",
            "Marienfelde",
            "Mitte",
            "Neukölln",
            "Niederschöneweide",
            "Nikolassee",
            "Tempelhof",
            "Wilmersdorf",
        ] {
            assert!(de_is_berlin_postal_locality(district));
        }
        assert!(!de_is_berlin_postal_locality("Offenbach"));
        assert!(de_is_proven_berlin_postcode(10117));
        assert!(!de_is_proven_berlin_postcode(12529));
    }

    #[test]
    fn de_wave_n_locality_strength_preserves_audited_same_postcode_relations() {
        for (query, postcode, commune, minimum) in [
            ("leer ostfriesland", 26789, "Leer", 1),
            ("berlin", 14195, "Lichterfelde", 3),
            ("lutherstadt wittenberg", 6886, "Wittenberg", 3),
            ("wittenberg lutherstadt", 6886, "Wittenberg", 3),
            ("forst lausitz", 3149, "Forst", 1),
            ("lubeck", 23552, "Lübeck, Hansestadt", 1),
            ("berlin", 10783, "Schöneberg", 3),
            ("berlin", 13627, "Charlottenburg-Nord", 3),
            ("berlin", 14059, "Charlottenburg", 3),
            ("weilheim teck", 73235, "Weilheim an der Teck", 3),
            ("konigstein taunus", 61462, "Königstein im Taunus", 3),
            ("freiburg breisgau", 79104, "Freiburg im Breisgau", 3),
            ("freiburg", 79115, "Freiburg im Breisgau", 1),
            ("bernburg saale", 6406, "Bernburg", 1),
            ("muhlhausen thuringen", 99974, "Mühlhausen", 1),
            ("berlin", 10587, "Charlottenburg", 3),
            ("oelsnitz vogtland", 8606, "Oelsnitz/Vogtl.", 3),
            ("frankenberg sachsen", 9669, "Frankenberg/Sa.", 3),
        ] {
            let score = Index::de_postcode_house_locality_score(query, None, postcode, commune);
            assert!(
                score >= minimum,
                "{query:?} / {postcode} must retain {commune:?}: score={score}"
            );
        }

        assert_eq!(
            Index::de_postcode_house_locality_score("kanzach", None, 88422, "Bad Buchau"),
            0
        );
        assert!(
            Index::de_postcode_house_locality_score(
                "oelsnitz vogtland",
                None,
                8606,
                "Oelsnitz/Vogtl."
            ) > Index::de_postcode_house_locality_score(
                "oelsnitz vogtland",
                None,
                8606,
                "Oelsnitz"
            )
        );
        assert!(
            Index::de_postcode_house_locality_score(
                "frankenberg sachsen",
                None,
                9669,
                "Frankenberg/Sa."
            ) > Index::de_postcode_house_locality_score(
                "frankenberg sachsen",
                None,
                9669,
                "Frankenberg"
            )
        );

        for (query, postcode, commune) in [
            ("konigstein taunus", 61462, "Königstein im Taunus"),
            ("freiburg breisgau", 79104, "Freiburg im Breisgau"),
            ("freiburg", 79115, "Freiburg im Breisgau"),
            ("oelsnitz vogtland", 8606, "Oelsnitz/Vogtl."),
            ("frankenberg sachsen", 9669, "Frankenberg/Sa."),
            ("lutherstadt wittenberg", 6886, "Wittenberg"),
            ("wittenberg lutherstadt", 6886, "Wittenberg"),
            ("weilheim teck", 73235, "Weilheim an der Teck"),
            ("berlin", 10587, "Charlottenburg"),
            ("berlin", 10783, "Schöneberg"),
            ("berlin", 13627, "Charlottenburg-Nord"),
            ("berlin", 14059, "Charlottenburg"),
            ("berlin", 14195, "Lichterfelde"),
        ] {
            assert_eq!(
                Index::de_postcode_house_locality_score(query, None, postcode, commune),
                3,
                "the audited relation must be strong only on its exact postcode"
            );
            assert!(
                Index::de_postcode_house_locality_score(query, None, 99999, commune) < 3,
                "{query:?} -> {commune:?} must not retain strong evidence across postcodes"
            );
        }
        assert!(
            Index::de_postcode_house_locality_score("berlin", None, 10587, "Schöneberg") < 3,
            "Berlin locality and postcode allowlists must not form an unproven cross-product"
        );
    }

    #[test]
    fn de_wave_n_new_berlin_relations_do_not_cross_product() {
        let relations = [
            (10587, "Charlottenburg"),
            (10783, "Schöneberg"),
            (13627, "Charlottenburg-Nord"),
            (14059, "Charlottenburg"),
            (14195, "Lichterfelde"),
        ];
        let district_candidates = [
            "Adlershof",
            "Charlottenburg",
            "Charlottenburg-Nord",
            "Dahlem",
            "Kaulsdorf",
            "Kreuzberg",
            "Lichterfelde",
            "Marienfelde",
            "Mitte",
            "Neukölln",
            "Niederschöneweide",
            "Nikolassee",
            "Schöneberg",
            "Tempelhof",
            "Wilmersdorf",
        ];

        for (postcode, exact_commune) in relations {
            assert_eq!(
                Index::de_postcode_house_locality_score("berlin", None, postcode, exact_commune,),
                3,
                "the registered Berlin triple must remain strong: {postcode} -> {exact_commune}"
            );
            let exact_core = de_commune_core(exact_commune);
            for commune in district_candidates {
                if de_commune_core(commune) == exact_core {
                    continue;
                }
                assert!(
                    Index::de_postcode_house_locality_score("berlin", None, postcode, commune) < 3,
                    "an unregistered Berlin cross-product must stay weak: {postcode} -> {commune}"
                );
            }
        }
    }

    #[test]
    fn de_prefix_drop_requires_original_postcode_or_locality_evidence() {
        let features = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let wrong = retained_test_hit(
            "Celle, Stadt",
            "Berlinstraße",
            Some("8"),
            "",
            "house",
            0.43,
            features,
            vec!["street_exact", "house_rep", "de_street_type"],
            17,
            0,
        );
        assert!(!de_prefix_drop_preserves_postcode_locality(
            &wrong.0, &wrong.1, 10117, "berlin"
        ));

        let matching_locality = retained_test_hit(
            "Frankfurt am Main",
            "Domstraße",
            Some("10"),
            "",
            "house",
            0.43,
            features,
            vec!["street_exact", "house_rep"],
            17,
            0,
        );
        assert!(de_prefix_drop_preserves_postcode_locality(
            &matching_locality.0,
            &matching_locality.1,
            60311,
            "frankfurt"
        ));

        let mut postcode_features = features;
        postcode_features[4] = 1.0;
        let matching_postcode = retained_test_hit(
            "Charlottenburg",
            "Testweg",
            Some("1"),
            "10117",
            "house",
            0.43,
            postcode_features,
            vec!["street_exact", "house_rep", "pc_exact"],
            17,
            0,
        );
        assert!(de_prefix_drop_preserves_postcode_locality(
            &matching_postcode.0,
            &matching_postcode.1,
            10117,
            "berlin"
        ));
    }

    #[test]
    fn de_abbreviation_variant_cannot_trade_a_live_result_for_weaker_invented_context() {
        let pair = |hit: RankedHit| (hit.0, hit.1);
        let current_features = [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let current = vec![pair(retained_test_hit(
            "Rinteln",
            "Paul-Erdniß-Straße",
            Some("1"),
            "",
            "house",
            -1.03,
            current_features,
            vec!["street_fuzzy", "house_rep"],
            0,
            0,
        ))];

        let mut invented_features = current_features;
        invented_features[3] = 1.0;
        let invented_commune = vec![pair(retained_test_hit(
            "Straßenhaus",
            "Paul-Mertgen-Straße",
            Some("1"),
            "",
            "house",
            99.0,
            invented_features,
            vec!["street_fuzzy", "commune_prefix", "house_rep"],
            0,
            0,
        ))];
        assert!(!Index::de_abbreviation_candidate_may_displace(
            &invented_commune,
            &current,
            None,
        ));

        let exact_address = vec![pair(retained_test_hit(
            "Rinteln",
            "Paul-Erdniß-Straße",
            Some("1"),
            "",
            "house",
            1.0,
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec!["street_exact", "house_rep"],
            0,
            0,
        ))];
        let postcode_only = vec![pair(retained_test_hit(
            "Straßenhaus",
            "Other",
            Some("1"),
            "01067",
            "house",
            99.0,
            [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0],
            vec!["commune_prefix", "pc_exact"],
            0,
            0,
        ))];
        assert!(
            !Index::de_abbreviation_candidate_may_displace(&postcode_only, &exact_address, None,),
            "postcode-only evidence must not trade away exact street+house evidence"
        );
        let equal_invented = vec![pair(retained_test_hit(
            "Straßenhaus",
            "Paul-Erdniß-Straße",
            Some("1"),
            "",
            "house",
            99.0,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec!["street_exact", "commune_prefix", "house_rep"],
            0,
            0,
        ))];
        assert!(
            !Index::de_abbreviation_candidate_may_displace(
                &equal_invented,
                &exact_address,
                Some("rinteln"),
            ),
            "invented commune context needs a strict independent-address gain"
        );

        for (tail, commune, house) in [
            ("ahlden aller", "Ahlden (Aller), Flecken", "1"),
            ("bad iburg", "Bad Iburg, Stadt", "12"),
        ] {
            let tied_wrong_locality = vec![pair(retained_test_hit(
                "Wittenburg",
                "Große Str.",
                Some(house),
                "",
                "house",
                0.43,
                [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
                vec!["street_exact", "house_rep", "dropped_suffix"],
                0,
                0,
            ))];
            let retained_candidate = vec![pair(retained_test_hit(
                commune,
                "Große Straße",
                Some(house),
                "",
                "house",
                0.08,
                [1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
                vec!["street_exact", "commune_prefix", "house_rep"],
                0,
                0,
            ))];
            assert!(
                !Index::de_abbreviation_candidate_may_displace(
                    &retained_candidate,
                    &tied_wrong_locality,
                    None,
                ),
                "a variant must not invent locality evidence without the original tail",
            );
            assert!(
                Index::de_abbreviation_candidate_may_displace(
                    &retained_candidate,
                    &tied_wrong_locality,
                    Some(tail),
                ),
                "original locality {tail:?} must break a tied exact-address arbitration",
            );
        }
        let strict_improvement = vec![pair(retained_test_hit(
            "Straßenhaus",
            "Paul-Erdniß-Straße",
            Some("1"),
            "01067",
            "house",
            99.0,
            [1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec!["street_exact", "commune_prefix", "house_rep", "pc_exact"],
            0,
            0,
        ))];
        assert!(Index::de_abbreviation_candidate_may_displace(
            &strict_improvement,
            &exact_address,
            None,
        ));

        let dropped_name = vec![pair(retained_test_hit(
            "Arnsberg",
            "Stumpfstraße",
            Some("2"),
            "",
            "house",
            0.43,
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec!["street_exact", "house_rep", "dropped_prefix"],
            0,
            0,
        ))];
        assert!(!Index::de_abbreviation_candidate_may_displace(
            &dropped_name,
            &current,
            Some("arnsberg"),
        ));
        assert!(Index::de_abbreviation_candidate_may_displace(
            &dropped_name,
            &[],
            Some("arnsberg"),
        ));

        let strong_dropped = vec![pair(retained_test_hit(
            "Dresden",
            "Hauptstraße",
            Some("1"),
            "01067",
            "house",
            2.0,
            [1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec![
                "street_exact",
                "commune_exact",
                "house_rep",
                "pc_exact",
                "dropped_prefix",
            ],
            0,
            0,
        ))];
        assert!(Index::de_abbreviation_candidate_may_displace(
            &strong_dropped,
            &current,
            None,
        ));

        let exact = vec![pair(retained_test_hit(
            "Rinteln",
            "Paul-Erdniß-Straße",
            Some("1"),
            "",
            "house",
            0.43,
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec!["street_exact", "house_rep"],
            0,
            0,
        ))];
        assert!(Index::de_abbreviation_candidate_may_displace(
            &exact, &current, None,
        ));
    }

    #[test]
    fn de_parenthetical_subaddress_admission_is_exact_commune_and_fill_empty_only() {
        let pair = |hit: RankedHit| (hit.0, hit.1);
        let exact = vec![pair(retained_test_hit(
            "Zeven, Stadt",
            "Am Markt",
            Some("4"),
            "",
            "house",
            1.0,
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec!["street_exact", "house_rep"],
            0,
            0,
        ))];
        assert!(Index::de_parenthetical_subaddress_may_fill(
            &[],
            &exact,
            Some("zeven")
        ));
        assert!(
            !Index::de_parenthetical_subaddress_may_fill(&exact, &exact, Some("zeven")),
            "PARENTHETICAL_SUBADDRESS_FILL_EMPTY_OBSERVER: a live result is immutable"
        );
        assert!(!Index::de_parenthetical_subaddress_may_fill(
            &[],
            &exact,
            Some("aachen")
        ));
        assert!(!Index::de_parenthetical_subaddress_may_fill(
            &[],
            &exact,
            None
        ));

        let weak = |precision, features| {
            vec![pair(retained_test_hit(
                "Zeven, Stadt",
                "Am Markt",
                Some("4"),
                "",
                precision,
                99.0,
                features,
                vec!["street_fuzzy"],
                0,
                0,
            ))]
        };
        assert!(!Index::de_parenthetical_subaddress_may_fill(
            &[],
            &weak("house", [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0]),
            Some("zeven")
        ));
        assert!(!Index::de_parenthetical_subaddress_may_fill(
            &[],
            &weak("interp", [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0]),
            Some("zeven")
        ));
    }

    #[test]
    fn de_abbreviation_guard_is_wired_into_country_variant_arbitration() {
        let idx = forward_postcode_index_for_country(
            "abbreviation-guard-wiring",
            "wilhelm luckert strasse,001,berlin,10115,10115,4,,13.3889,52.5170,Wilhelm-Lückert-Straße,Berlin\n",
            "de",
        );
        DE_ABBREVIATION_GUARD_CALLS.with(|calls| calls.set(0));
        let hits = idx.query("Wilhelm-Lückert-str. 4, 10115 Berlin", 1);
        assert_eq!(
            hits.first().map(|hit| hit.street.as_str()),
            Some("Wilhelm-Lückert-Straße")
        );
        assert!(
            DE_ABBREVIATION_GUARD_CALLS.with(|calls| calls.get()) > 0,
            "the production country-variant loop must consult the abbreviation guard"
        );
    }

    #[test]
    fn de_recipient_cleanup_tie_break_requires_exact_full_street_and_house() {
        let pair = |hit: RankedHit| (hit.0, hit.1);
        let current = vec![pair(retained_test_hit(
            "Wesselburen",
            "Dohrnstraße",
            Some("5"),
            "25764",
            "house",
            0.43,
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec!["street_exact", "house_rep", "dropped_prefix"],
            0,
            0,
        ))];
        let exact_full_street = vec![pair(retained_test_hit(
            "Charlottenburg-Nord",
            "Max-Dohrn-Straße",
            Some("5"),
            "10589",
            "house",
            0.43,
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec!["street_exact", "house_rep"],
            0,
            0,
        ))];
        assert!(Index::de_recipient_cleanup_breaks_dropped_prefix_tie(
            &exact_full_street,
            &current,
        ));

        let fuzzy = vec![pair(retained_test_hit(
            "Charlottenburg-Nord",
            "Max-Dorn-Straße",
            Some("5"),
            "10589",
            "house",
            -1.03,
            [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec!["street_fuzzy", "house_rep"],
            0,
            0,
        ))];
        assert!(!Index::de_recipient_cleanup_breaks_dropped_prefix_tie(
            &fuzzy, &current,
        ));
        assert!(!Index::de_recipient_cleanup_breaks_dropped_prefix_tie(
            &exact_full_street,
            &[],
        ));
    }

    #[test]
    fn de_country_variants_skip_equivalent_raw_and_normalized_base() {
        let idx = forward_postcode_index_for_country(
            "country-variant-prepared-dedup",
            "mainweg,001,berlin,10115,10115,1,,13.3889,52.5170,Mainweg,Berlin\n",
            "de",
        );
        let query = "Mainweg 1, 10115 Berlin";
        assert_eq!(
            crate::de::query_variants(query).len(),
            2,
            "the witness must generate distinct raw and normalized variants"
        );
        DE_COUNTRY_VARIANT_PREPARED_SEARCH_CALLS.with(|calls| calls.set(0));
        let hits = idx.query(query, 1);
        let top = hits.first().expect("the exact house must still resolve");
        assert_eq!(top.street, "Mainweg");
        assert_eq!(top.housenumber.as_deref(), Some("1"));
        assert_eq!(
            DE_COUNTRY_VARIANT_PREPARED_SEARCH_CALLS.with(|calls| calls.get()),
            1,
            "equivalent non-city variants must execute one prepared search"
        );
    }

    #[test]
    fn de_country_variants_keep_raw_sensitive_city_fallbacks() {
        let idx = forward_postcode_index_for_country(
            "country-variant-raw-city-fallback",
            "mainweg,001,berlin,10115,10115,1,,13.3889,52.5170,Mainweg,Berlin\n",
            "de",
        );
        DE_COUNTRY_VARIANT_PREPARED_SEARCH_CALLS.with(|calls| calls.set(0));
        let hits = idx.query("Unindexed, Berlin", 1);
        let top = hits
            .first()
            .expect("the raw comma segment must resolve Berlin");
        assert_eq!(top.precision, "city");
        assert_eq!(top.commune, "Berlin");
        assert_eq!(
            DE_COUNTRY_VARIANT_PREPARED_SEARCH_CALLS.with(|calls| calls.get()),
            2,
            "city/empty outcomes must not be reused because raw segments differ"
        );
    }

    #[test]
    fn de_abbreviation_retained_locality_breaks_exact_address_evidence_ties() {
        let idx = forward_postcode_index_for_country(
            "abbreviation-retained-locality-tie",
            "grosse str,001,wittenburg,,,1,,11.0762461,53.5107681,Große Str.,Wittenburg\n\
             grosse str,001,wittenburg,,,12,,11.0755349,53.5113574,Große Str.,Wittenburg\n\
             grosse str,002,grabow,,,1,,11.5644688,53.2784453,Große Str.,Grabow\n\
             grosse str,002,grabow,,,12,,11.5629185,53.2779808,Große Str.,Grabow\n\
             grosse str,003,crivitz,,,1,,11.6507684,53.5765525,Große Str.,Crivitz\n\
             grosse str,003,crivitz,,,12,,11.6505726,53.5770965,Große Str.,Crivitz\n\
             grosse str,004,westerkappeln,,,1,,7.8774391,52.3146747,Große Str.,Westerkappeln\n\
             grosse str,004,westerkappeln,,,12,,7.8776095,52.3132857,Große Str.,Westerkappeln\n\
             grosse str,005,ibbenburen,,,1,,7.7152364,52.2766699,Große Str.,Ibbenbüren\n\
             grosse str,005,ibbenburen,,,12,,7.7156456,52.2769733,Große Str.,Ibbenbüren\n\
             grosse strasse,006,ahlden aller flecken,,,1,,9.5577655,52.7592260,Große Straße,Ahlden (Aller) Flecken\n\
             grosse strasse,007,bad iburg stadt,,,12,,8.0452005,52.1568279,Große Straße,Bad Iburg Stadt\n",
            "de",
        );

        for (query, expected_commune, expected_house) in [
            (
                "Große Str. 1, 29693 Ahlden (Aller)",
                "Ahlden (Aller) Flecken",
                "1",
            ),
            ("Große Str. 12, 49186 Bad Iburg", "Bad Iburg Stadt", "12"),
        ] {
            let hits = idx.query(query, 5);
            let top = hits
                .first()
                .unwrap_or_else(|| panic!("retained locality must resolve {query:?}"));
            assert_eq!(top.street, "Große Straße", "wrong street for {query:?}");
            assert_eq!(top.housenumber.as_deref(), Some(expected_house));
            assert_eq!(top.commune, expected_commune, "wrong commune for {query:?}");
            assert!(top.flags.contains(&"de_abbrev"));
        }
    }

    #[test]
    fn de_retained_locality_requires_every_prior_address_comparator_to_tie() {
        let features = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let base = retained_test_hit(
            "Wrong",
            "Hamburger Allee",
            Some("2"),
            "",
            "house",
            0.43,
            features,
            vec!["street_exact", "house_rep"],
            17,
            0,
        );
        let same = retained_test_hit(
            "Frankfurt am Main",
            "Hamburger Allee",
            Some("2"),
            "",
            "house",
            0.43,
            features,
            vec!["street_exact", "house_rep"],
            17,
            0,
        );
        assert!(de_same_retained_address_evidence(&base, &same));

        let cases = [
            retained_test_hit(
                "Target",
                "Hamburger Allee",
                Some("2"),
                "",
                "house",
                0.44,
                features,
                vec!["street_exact", "house_rep"],
                17,
                0,
            ),
            retained_test_hit(
                "Target",
                "Hamburger Allee",
                Some("2"),
                "",
                "house",
                0.43,
                [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0],
                vec!["street_exact", "house_rep"],
                17,
                0,
            ),
            retained_test_hit(
                "Target",
                "Hamburger Allee",
                Some("2"),
                "",
                "house",
                0.43,
                features,
                vec!["street_exact", "house_rep"],
                16,
                0,
            ),
            retained_test_hit(
                "Target",
                "Hamburger Allee",
                Some("2"),
                "",
                "house",
                0.43,
                features,
                vec!["street_exact", "house_rep"],
                17,
                1,
            ),
            retained_test_hit(
                "Target",
                "Hamburger Allee",
                Some("2"),
                "",
                "near",
                0.43,
                features,
                vec!["street_exact", "house_rep"],
                17,
                0,
            ),
            retained_test_hit(
                "Target",
                "Hamburger Allee",
                Some("3"),
                "",
                "house",
                0.43,
                features,
                vec!["street_exact", "house_rep"],
                17,
                0,
            ),
            retained_test_hit(
                "Target",
                "Hamburger Allee",
                Some("2"),
                "60486",
                "house",
                0.43,
                features,
                vec!["street_exact", "house_rep"],
                17,
                0,
            ),
            retained_test_hit(
                "Target",
                "Hamburger Allee",
                Some("2"),
                "",
                "house",
                0.43,
                features,
                vec!["street_exact"],
                17,
                0,
            ),
            retained_test_hit(
                "Target",
                "Andere Straße",
                Some("2"),
                "",
                "house",
                0.43,
                features,
                vec!["street_exact", "house_rep"],
                17,
                0,
            ),
        ];
        for candidate in &cases {
            assert!(!de_same_retained_address_evidence(&base, candidate));
        }

        let positive_zero = retained_test_hit(
            "Wrong",
            "Hamburger Allee",
            Some("2"),
            "",
            "house",
            0.0,
            features,
            vec!["street_exact", "house_rep"],
            17,
            0,
        );
        let negative_zero = retained_test_hit(
            "Target",
            "Hamburger Allee",
            Some("2"),
            "",
            "house",
            -0.0,
            features,
            vec!["street_exact", "house_rep"],
            17,
            0,
        );
        assert!(!de_same_retained_address_evidence(
            &positive_zero,
            &negative_zero
        ));
    }

    fn retained_equal_hit(commune: &str, features: [f32; N_FEATS]) -> RankedHit {
        retained_test_hit(
            commune,
            "Hamburger Allee",
            Some("2"),
            "",
            "house",
            0.43,
            features,
            vec!["street_exact", "house_rep"],
            17,
            0,
        )
    }

    #[test]
    fn de_retained_locality_promoter_is_fixed_to_the_preregistered_top_five() {
        let features = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let mut hits = vec![
            retained_equal_hit("Wrong", features),
            retained_equal_hit("Other One", features),
            retained_equal_hit("Other Two", features),
            retained_equal_hit("Other Three", features),
            retained_equal_hit("Frankfurt am Main", features),
            retained_equal_hit("Frankfurt an der Oder", features),
        ];
        assert!(promote_de_retained_locality(
            &mut hits,
            DeRetainedLocality {
                postcode_tail: "frankfurt",
                dropped_tail: "frankfurt",
                postal_tail_eligible: true,
            },
        ));
        assert_eq!(hits[0].0.commune, "Frankfurt am Main");
        assert!(hits[0].0.flags.contains(&"de_retained_locality"));
    }

    #[test]
    fn de_retained_locality_promoter_rejects_fuzzy_or_unequal_address_evidence() {
        let fuzzy = [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let mut fuzzy_hits = vec![
            retained_equal_hit("Wrong", fuzzy),
            retained_equal_hit("Frankfurt am Main", fuzzy),
        ];
        assert!(!promote_de_retained_locality(
            &mut fuzzy_hits,
            DeRetainedLocality {
                postcode_tail: "frankfurt",
                dropped_tail: "frankfurt",
                postal_tail_eligible: true,
            },
        ));
        assert_eq!(fuzzy_hits[0].0.commune, "Wrong");

        let exact = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let mut unequal_hits = vec![
            retained_equal_hit("Wrong", exact),
            retained_test_hit(
                "Frankfurt am Main",
                "Hamburger Allee",
                Some("3"),
                "",
                "house",
                0.43,
                exact,
                vec!["street_exact", "house_rep"],
                17,
                0,
            ),
        ];
        assert!(!promote_de_retained_locality(
            &mut unequal_hits,
            DeRetainedLocality {
                postcode_tail: "frankfurt",
                dropped_tail: "frankfurt",
                postal_tail_eligible: true,
            },
        ));
        assert_eq!(unequal_hits[0].0.commune, "Wrong");
    }

    #[test]
    fn de_retained_locality_promoter_requires_full_context_and_reachable_drop() {
        let features = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let mut shared_tail = vec![
            retained_equal_hit("Wrong", features),
            retained_equal_hit("Roge Stadt", features),
        ];
        assert!(!promote_de_retained_locality(
            &mut shared_tail,
            DeRetainedLocality {
                postcode_tail: "groß roge",
                dropped_tail: "roge",
                postal_tail_eligible: true,
            },
        ));
        assert_eq!(shared_tail[0].0.commune, "Wrong");

        let mut short_drop = vec![
            retained_equal_hit("Wrong", features),
            retained_equal_hit("Frankfurt am Main", features),
        ];
        assert!(!promote_de_retained_locality(
            &mut short_drop,
            DeRetainedLocality {
                postcode_tail: "frankfurt am",
                dropped_tail: "am",
                postal_tail_eligible: true,
            },
        ));
        assert_eq!(short_drop[0].0.commune, "Wrong");
    }

    #[test]
    fn de_retained_locality_promoter_keeps_an_already_matching_top() {
        let features = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let mut hits = vec![
            retained_equal_hit("Frankfurt am Main", features),
            retained_equal_hit("Frankfurt Stadt", features),
        ];
        assert!(!promote_de_retained_locality(
            &mut hits,
            DeRetainedLocality {
                postcode_tail: "frankfurt",
                dropped_tail: "frankfurt",
                postal_tail_eligible: true,
            },
        ));
        assert_eq!(hits[0].0.commune, "Frankfurt am Main");
        assert!(!hits[0].0.flags.contains(&"de_retained_locality"));
    }

    #[test]
    fn de_retained_locality_promoter_prefers_exact_over_prefix_evidence() {
        let features = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let mut hits = vec![
            retained_equal_hit("Wrong", features),
            retained_equal_hit("Frankfurt am Main", features),
            retained_equal_hit("Frankfurt Stadt", features),
        ];
        assert!(promote_de_retained_locality(
            &mut hits,
            DeRetainedLocality {
                postcode_tail: "frankfurt",
                dropped_tail: "frankfurt",
                postal_tail_eligible: true,
            },
        ));
        assert_eq!(hits[0].0.commune, "Frankfurt Stadt");
    }

    // Keep the frozen fixtures' explicit fields; grouping them would rewrite the test call sites.
    #[allow(clippy::too_many_arguments)]
    fn postal_tail_test_hit(
        commune: &str,
        street: &str,
        housenumber: &str,
        postcode: &str,
        precision: &'static str,
        pc_exact: bool,
        house_exact_rep: bool,
        score: f32,
    ) -> RankedHit {
        let features = [
            1.0,
            0.0,
            0.0,
            0.0,
            if pc_exact { 1.0 } else { 0.0 },
            1.0,
            0.0,
            1.0,
            if house_exact_rep { 1.0 } else { 0.0 },
            1.0,
        ];
        let mut flags = vec!["street_exact"];
        if house_exact_rep {
            flags.push("house_rep");
        }
        if pc_exact {
            flags.push("pc_exact");
        } else {
            flags.push("pc_dept");
        }
        retained_test_hit(
            commune,
            street,
            Some(housenumber),
            postcode,
            precision,
            score,
            features,
            flags,
            if pc_exact { 3 } else { 97 },
            if pc_exact { 8 } else { 0 },
        )
    }

    fn postal_tail_positive_at_rank(target_rank: usize) -> Vec<RankedHit> {
        let mut hits = vec![postal_tail_test_hit(
            "Top",
            "Post Allee",
            "15b",
            "12346",
            "house",
            false,
            false,
            12.0,
        )];
        for rank in 2..=6 {
            if rank == target_rank {
                hits.push(postal_tail_test_hit(
                    "Target",
                    "Post-Allee",
                    "15a",
                    "12345",
                    "house",
                    true,
                    false,
                    -7.25,
                ));
            } else {
                hits.push(postal_tail_test_hit(
                    &format!("Filler {rank}"),
                    "Post Allee",
                    "15c",
                    "12347",
                    "house",
                    false,
                    false,
                    11.0 - rank as f32,
                ));
            }
        }
        hits
    }

    #[test]
    fn de_postal_tail_stably_promotes_unique_rank_two_three_or_five() {
        for target_rank in [2, 3, 5] {
            let mut hits = postal_tail_positive_at_rank(target_rank);
            let original_order: Vec<String> =
                hits.iter().map(|hit| hit.0.commune.clone()).collect();
            assert!(promote_de_postal_tail(
                &mut hits,
                true,
                true,
                false,
                Some(12345),
            ));
            assert_eq!(hits[0].0.commune, "Target");
            assert_eq!(hits[0].0.housenumber.as_deref(), Some("15a"));
            assert_eq!(hits[0].0.score, -7.25, "the hit must move intact");
            assert!(hits[0].0.flags.contains(&"de_postal_tail"));

            let expected_tail: Vec<String> = original_order
                .into_iter()
                .filter(|commune| commune != "Target")
                .collect();
            let actual_tail: Vec<String> =
                hits[1..].iter().map(|hit| hit.0.commune.clone()).collect();
            assert_eq!(actual_tail, expected_tail, "move-to-front must be stable");
        }
    }

    #[test]
    fn de_retained_locality_decision_precedes_and_blocks_postal_tail() {
        let features = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let initial = || {
            vec![
                retained_equal_hit("Wrong", features),
                retained_equal_hit("Frankfurt am Main", features),
                postal_tail_test_hit(
                    "Postal Target",
                    "Hamburger Allee",
                    "2",
                    "12345",
                    "house",
                    true,
                    true,
                    -8.0,
                ),
            ]
        };

        let mut postal_first = initial();
        assert!(promote_de_postal_tail(
            &mut postal_first,
            true,
            true,
            false,
            Some(12345),
        ));
        assert_eq!(postal_first[0].0.commune, "Postal Target");

        let mut production_order = initial();
        apply_de_c2_tiebreaks(
            &mut production_order,
            Some(DeRetainedLocality {
                postcode_tail: "frankfurt",
                dropped_tail: "frankfurt",
                postal_tail_eligible: true,
            }),
            true,
            false,
            Some(12345),
        );
        assert_eq!(production_order[0].0.commune, "Frankfurt am Main");
        assert!(production_order[0]
            .0
            .flags
            .contains(&"de_retained_locality"));
        assert_eq!(production_order[0].0.commune, "Frankfurt am Main");
        assert!(!production_order[0].0.flags.contains(&"de_postal_tail"));
    }

    #[test]
    fn de_postal_tail_is_fixed_to_one_candidate_in_the_original_top_five() {
        let mut rank_six = postal_tail_positive_at_rank(6);
        let rank_six_order: Vec<String> =
            rank_six.iter().map(|hit| hit.0.commune.clone()).collect();
        assert!(!promote_de_postal_tail(
            &mut rank_six,
            true,
            true,
            false,
            Some(12345),
        ));
        assert_eq!(
            rank_six
                .iter()
                .map(|hit| hit.0.commune.clone())
                .collect::<Vec<_>>(),
            rank_six_order
        );

        let mut duplicate = postal_tail_positive_at_rank(2);
        duplicate[2] = postal_tail_test_hit(
            "Duplicate Target",
            "Post Allee",
            "15d",
            "12345",
            "house",
            true,
            false,
            -8.0,
        );
        let duplicate_order: Vec<String> =
            duplicate.iter().map(|hit| hit.0.commune.clone()).collect();
        assert!(!promote_de_postal_tail(
            &mut duplicate,
            true,
            true,
            false,
            Some(12345),
        ));
        assert_eq!(
            duplicate
                .iter()
                .map(|hit| hit.0.commune.clone())
                .collect::<Vec<_>>(),
            duplicate_order
        );
    }

    #[test]
    fn de_postal_tail_fails_closed_at_every_context_and_evidence_gate() {
        let assert_rejected = |mut hits: Vec<RankedHit>, is_de, is_c2, focus, postcode| {
            let before: Vec<(String, Vec<&'static str>)> = hits
                .iter()
                .map(|hit| (hit.0.commune.clone(), hit.0.flags.clone()))
                .collect();
            assert!(!promote_de_postal_tail(
                &mut hits, is_de, is_c2, focus, postcode,
            ));
            assert_eq!(
                hits.iter()
                    .map(|hit| (hit.0.commune.clone(), hit.0.flags.clone()))
                    .collect::<Vec<_>>(),
                before
            );
        };

        assert_rejected(
            postal_tail_positive_at_rank(2),
            false,
            true,
            false,
            Some(12345),
        );
        assert_rejected(
            postal_tail_positive_at_rank(2),
            true,
            false,
            false,
            Some(12345),
        );
        assert_rejected(
            postal_tail_positive_at_rank(2),
            true,
            true,
            true,
            Some(12345),
        );
        assert_rejected(postal_tail_positive_at_rank(2), true, true, false, None);

        let mut top_postcode = postal_tail_positive_at_rank(2);
        top_postcode[0].1[4] = 1.0;
        top_postcode[0].0.postcode = "12345".to_owned();
        top_postcode[0].0.flags.push("pc_exact");
        assert_rejected(top_postcode, true, true, false, Some(12345));

        let mut retained = postal_tail_positive_at_rank(2);
        retained[0].0.flags.push("de_retained_locality");
        assert_rejected(retained, true, true, false, Some(12345));

        let mut fuzzy_top = postal_tail_positive_at_rank(2);
        fuzzy_top[0].1[0] = 0.0;
        fuzzy_top[0].1[1] = 1.0;
        assert_rejected(fuzzy_top, true, true, false, Some(12345));

        let mut fuzzy_candidate = postal_tail_positive_at_rank(2);
        fuzzy_candidate[1].1[0] = 0.0;
        fuzzy_candidate[1].1[1] = 1.0;
        assert_rejected(fuzzy_candidate, true, true, false, Some(12345));

        let mut both_fuzzy = postal_tail_positive_at_rank(2);
        for hit in &mut both_fuzzy[..2] {
            hit.1[0] = 0.0;
            hit.1[1] = 1.0;
            hit.0.flags.retain(|flag| *flag != "street_exact");
            hit.0.flags.push("street_fuzzy");
        }
        assert_rejected(both_fuzzy, true, true, false, Some(12345));

        let mut reordered_street = postal_tail_positive_at_rank(2);
        reordered_street[1].0.street = "Allee Post".to_owned();
        assert_rejected(reordered_street, true, true, false, Some(12345));

        let mut other_precision = postal_tail_positive_at_rank(2);
        other_precision[1].0.precision = "near";
        assert_rejected(other_precision, true, true, false, Some(12345));

        let mut emitted_postcode_mismatch = postal_tail_positive_at_rank(2);
        emitted_postcode_mismatch[1].0.postcode = "12346".to_owned();
        assert_rejected(emitted_postcode_mismatch, true, true, false, Some(12345));

        let mut missing_pc_feature = postal_tail_positive_at_rank(2);
        assert_eq!(missing_pc_feature[1].0.postcode, "12345");
        missing_pc_feature[1].1[4] = 0.0;
        assert_rejected(missing_pc_feature, true, true, false, Some(12345));

        for feature_index in [0, 1, 2, 3, 6, 7, 8, 9] {
            let mut widened_mask = postal_tail_positive_at_rank(2);
            widened_mask[1].1[feature_index] = if widened_mask[1].1[feature_index] > 0.5 {
                0.0
            } else {
                1.0
            };
            assert_rejected(widened_mask, true, true, false, Some(12345));
        }

        let mut non_postal_weakening = postal_tail_positive_at_rank(2);
        non_postal_weakening[0].1[8] = 1.0;
        non_postal_weakening[0].0.housenumber = Some("15".to_owned());
        non_postal_weakening[1].0.housenumber = Some("15a".to_owned());
        assert_rejected(non_postal_weakening, true, true, false, Some(12345));

        let mut foreign_wrapper = postal_tail_positive_at_rank(2);
        apply_de_c2_tiebreaks(
            &mut foreign_wrapper,
            Some(DeRetainedLocality {
                postcode_tail: "unmatched locality",
                dropped_tail: "unmatched",
                postal_tail_eligible: true,
            }),
            false,
            false,
            Some(12345),
        );
        assert_eq!(foreign_wrapper[0].0.commune, "Top");
        assert!(foreign_wrapper
            .iter()
            .all(|hit| !hit.0.flags.contains(&"de_postal_tail")));

        let mut house_effect_wrapper = postal_tail_positive_at_rank(2);
        apply_de_c2_tiebreaks(
            &mut house_effect_wrapper,
            Some(DeRetainedLocality {
                postcode_tail: "unmatched locality",
                dropped_tail: "unmatched",
                postal_tail_eligible: false,
            }),
            true,
            false,
            Some(12345),
        );
        assert_eq!(house_effect_wrapper[0].0.commune, "Top");
        assert!(house_effect_wrapper
            .iter()
            .all(|hit| !hit.0.flags.contains(&"de_postal_tail")));
    }

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

    fn forward_postcode_index_for_country(case: &str, rows: &str, country: &str) -> Index {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "gridpin-forward-postcode-country-{case}-{}-{serial}",
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
        let manifest = dir.join("manifest.json");
        std::fs::write(
            &manifest,
            format!(
                r#"{{"country":"{country}","layer":"addresses","license":"test","source_release":"test"}}"#
            ),
        )
        .unwrap();
        let bin = dir.join("addresses.bin");
        crate::builder::build(&csv, &bin, None, None, None, None, Some(&manifest)).unwrap();
        Index::open(&bin).unwrap()
    }

    fn forward_postcode_index_for_country_with_rules(
        case: &str,
        rows: &str,
        country: &str,
    ) -> Index {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "gridpin-forward-postcode-country-rules-{case}-{}-{serial}",
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
        let manifest = dir.join("manifest.json");
        std::fs::write(
            &manifest,
            format!(
                r#"{{"country":"{country}","layer":"addresses","license":"test","source_release":"test"}}"#
            ),
        )
        .unwrap();
        let bin = dir.join("addresses.bin");
        let rules_dir = crate::rules::tests::write_fixture_rules(&dir.join("rules"));
        crate::builder::build(
            &csv,
            &bin,
            None,
            None,
            Some(&rules_dir),
            None,
            Some(&manifest),
        )
        .unwrap();
        Index::open(&bin).unwrap()
    }

    fn de_wave_b_homonymous_p4_rows(
        commune_count: usize,
        source_street: &str,
        source_display: &str,
    ) -> String {
        let mut rows = (0..commune_count)
            .map(|position| {
                let insee = format!("{:03}", position + 1);
                let (street, display) = if position == 0 {
                    (source_street.to_owned(), source_display.to_owned())
                } else {
                    (
                        format!("zzdummy{position:02}"),
                        format!("ZZ Dummy {position:02}"),
                    )
                };
                format!(
                    "{street},{insee},bremen,28759,28759,1,,8.{position:07},53.{position:07},{display},Bremen"
                )
            })
            .collect::<Vec<_>>();
        rows.sort_unstable();
        format!("{}\n", rows.join("\n"))
    }

    #[test]
    fn de_prefix_drop_guard_reaches_postcode_locality_tail_end_to_end() {
        let idx = forward_postcode_index_for_country(
            "prefix-drop-postcode-locality",
            "berlinstrasse,002,celle,29221,29221,8,,10.0577218,52.6307212,Berlinstraße,Celle\n\
             franzosische strasse,001,mitte,10117,10117,8,,13.3864406,52.5144355,Französische Straße,Mitte\n",
            "de",
        );

        let hits = idx.query("Französische Straße 8, 10117 Berlin", 5);
        let top = hits
            .first()
            .expect("the postcode-preserving tail path must resolve");
        assert_eq!(top.street, "Französische Straße");
        assert_eq!(top.housenumber.as_deref(), Some("8"));
        assert_eq!(top.postcode, "10117");
        assert_eq!(top.commune, "Mitte");
        assert!(top.flags.contains(&"dropped_suffix"));
        assert!(
            hits.iter().all(|hit| hit.commune != "Celle"),
            "a generic prefix drop must not revive an exact house in the wrong postcode/locality"
        );

        DE_PREFIX_DROP_GUARD_CALLS.with(|calls| calls.set(0));
        let wrong_only = idx.query_feats_d(
            "venue berlinstrasse 8",
            5,
            0,
            None,
            Some("berlin"),
            Some(10117),
            true,
            None,
        );
        assert!(
            DE_PREFIX_DROP_GUARD_CALLS.with(|calls| calls.get()) > 0,
            "the production prefix-drop loop must execute the postcode/locality guard"
        );
        assert!(
            wrong_only.is_empty(),
            "dropping an unknown prefix must not turn a contradictory postcode/locality into a house"
        );
    }

    #[test]
    fn de_unique_house_postcode_survives_the_pre_rank_homonym_cap() {
        let mut rows =
            String::from("dummy,998,berlin,10115,10115,1,,13.3900000,52.5100000,Dummy,Berlin\n");
        for commune in 1..=340 {
            for house in [44, 45, 46] {
                rows.push_str(&format!(
                    "friedrichstrasse,{commune:03},ort{commune:03},,,{house},,{:.7},{:.7},Friedrichstraße,Ort {commune:03}\n",
                    10.0 + commune as f64 / 1000.0,
                    50.0 + commune as f64 / 1000.0,
                ));
            }
        }
        rows.push_str(
            "friedrichstrasse,999,zzztarget,10969,10969,44,,13.3900000,52.5100000,Friedrichstraße,Berlin Mitte\n\
             friedrichstrasse,999,zzztarget,10117,10117,45,,13.3910000,52.5110000,Friedrichstraße,Berlin Mitte\n",
        );
        let idx = forward_postcode_index_for_country("house-postcode-rescue", &rows, "de");

        DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| rows.set(0));
        DE_POSTCODE_HOUSE_RESCUE_HOUSE_DECODES.with(|calls| calls.set(0));
        let top = idx
            .query("Friedrichstraße 44, 10969 Berlin", 1)
            .into_iter()
            .next()
            .expect("the unique exact house/postcode must survive the homonym cap");
        assert_eq!(top.street, "Friedrichstraße");
        assert_eq!(top.housenumber.as_deref(), Some("44"));
        assert_eq!(top.postcode, "10969");
        assert_eq!(top.commune, "Berlin Mitte");
        assert!(top.flags.contains(&"pc_exact"));
        let scanned = DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| rows.get());
        assert!(
            scanned > 300,
            "the narrow rescue must prove the exact candidate beyond the ordinary 300-row cap"
        );
        assert!(
            scanned as usize <= DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT_DEFAULT,
            "all variants share one request-wide scan budget"
        );
        assert!(
            DE_POSTCODE_HOUSE_RESCUE_HOUSE_DECODES.with(|calls| calls.get()) <= 2,
            "hundreds of homonymous street postings must not trigger hundreds of house-block decodes"
        );
    }

    #[test]
    fn de_wave_n_terminal_country_tail_is_structural_and_bounded() {
        let parsed =
            de_comma_postcode_house_rescue_query("Wiesentalstraße 10, 79115 Freiburg, Deutschland")
                .expect(
                    "one terminal German country field must preserve the address/locality boundary",
                );
        assert_eq!(parsed.0, "Wiesentalstraße 10 79115");
        assert_eq!(parsed.1, 79115);
        assert_eq!(parsed.2, "freiburg");

        assert!(de_comma_postcode_house_rescue_query(
            "Wiesentalstraße 10, 79115 Freiburg, Deutschland, Europa"
        )
        .is_none());
        assert!(de_comma_postcode_house_rescue_query(
            "Wiesentalstraße 10, 79115 Freiburg, Targettown"
        )
        .is_none());
        assert!(de_comma_postcode_house_rescue_query(
            "Albertstr. 25 (Otto-Krayer-Haus), 79104 Freiburg/Breisgau"
        )
        .is_none());
    }

    #[test]
    fn de_wave_n_malformed_parenthetical_qualifier_cannot_bypass_p4() {
        let idx = forward_postcode_index_for_country(
            "wave-n-parenthetical-qualifier-boundary",
            "albertstrasse,001,freiburg im breisgau,79104,79104,25,,7.8500000,48.0100000,Albertstraße,Freiburg im Breisgau\n",
            "de",
        );
        let raw = "Albertstr. 25 (Otto-Krayer-Haus),79104 Freiburg/Breisgau";

        assert!(de_parenthetical_locality_uses_slash_qualifier(raw));
        assert!(
            idx.de_comma_postcode_house_rescue(raw, 5, None).is_none(),
            "the generic rescue itself must not own a malformed P4 qualifier surface"
        );
        let hits = idx.query(raw, 5);
        assert!(
            hits.iter().all(|hit| {
                hit.precision != "house"
                    && !hit.flags.contains(&"de_postcode_house")
                    && !hit.flags.contains(&"de_audited_compound")
                    && !hit.flags.contains(&"de_parenthetical_subaddress")
            }),
            "a malformed P4 surface must not reach an exact house through generic rescue: {} hits",
            hits.len()
        );
    }

    #[test]
    fn de_wave_n_token_bound_locality_arbitration_selects_one_exact_house() {
        let idx = forward_postcode_index_for_country(
            "wave-n-token-bound-locality",
            "grabenstrasse,001,oelsnitz,08606,08606,31,,12.1700000,50.4200000,Grabenstraße,Oelsnitz\n\
             grabenstrasse,002,oelsnitz vogtl,08606,08606,31,,12.1800000,50.4300000,Grabenstraße,Oelsnitz/Vogtl.\n\
             grabenstrasse,003,oelsnitz vogtland,08606,08606,24,,12.1600000,50.4100000,Grabenstraße,Oelsnitz/Vogtland\n",
            "de",
        );

        let exact = idx
            .de_postcode_house_rescue_parsed(
                "Grabenstr. 31, 08606 Oelsnitz/Vogtland",
                "Grabenstraße 31 08606",
                8606,
                "oelsnitz vogtland",
                1,
                None,
                None,
                &[],
                false,
            )
            .expect("the token-bound query locality must leave one exact source identity");
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].0.commune, "Oelsnitz/Vogtl.");
        assert_eq!(exact[0].0.housenumber.as_deref(), Some("31"));
        assert_eq!(exact[0].0.postcode, "08606");
    }

    #[test]
    fn de_wave_n_duplicate_source_sids_fail_closed_independent_of_coordinates() {
        for (case, second_coordinates) in [
            ("same-coordinates", "12.1800000,50.4300000"),
            ("different-coordinates", "12.1810000,50.4310000"),
        ] {
            let rows = format!(
                "grabenstrasse,001,oelsnitz vogtl,08606,08606,31,,12.1800000,50.4300000,Grabenstraße,Oelsnitz/Vogtl.\n\
                 grabenstrasse,002,oelsnitz vogtl,08606,08606,31,,{second_coordinates},Grabenstraße,Oelsnitz/Vogtl.\n"
            );
            let idx = forward_postcode_index_for_country(case, &rows, "de");
            assert!(
                idx.de_postcode_house_rescue_parsed(
                    "Grabenstr. 31, 08606 Oelsnitz/Vogtland",
                    "Grabenstraße 31 08606",
                    8606,
                    "oelsnitz vogtland",
                    1,
                    None,
                    None,
                    &[],
                    false,
                )
                .is_none(),
                "two distinct eligible source SIDs must fail closed regardless of coordinates: {case}"
            );
        }

        let suffix = forward_postcode_index_for_country(
            "wave-n-duplicate-suffix-same-coordinates",
            "grabenstrasse,001,oelsnitz vogtl,08606,08606,31,a,12.1800000,50.4300000,Grabenstraße,Oelsnitz/Vogtl.\n\
             grabenstrasse,002,oelsnitz vogtl,08606,08606,31,a,12.1800000,50.4300000,Grabenstraße,Oelsnitz/Vogtl.\n",
            "de",
        );
        assert!(
            suffix
                .de_postcode_house_rescue_parsed(
                    "Grabenstr. 31a, 08606 Oelsnitz/Vogtland",
                    "Grabenstraße 31a 08606",
                    8606,
                    "oelsnitz vogtland",
                    1,
                    None,
                    None,
                    &[],
                    false,
                )
                .is_none(),
            "same-coordinate duplicate source SIDs must also fail closed for an exact suffix"
        );

        let range = forward_postcode_index_for_country(
            "wave-n-duplicate-range-same-coordinates",
            "paarstrasse,001,oelsnitz vogtl,08606,08606,1,,12.1800000,50.4300000,Paarstraße,Oelsnitz/Vogtl.\n\
             paarstrasse,001,oelsnitz vogtl,08606,08606,3,,12.1810000,50.4310000,Paarstraße,Oelsnitz/Vogtl.\n\
             paarstrasse,002,oelsnitz vogtl,08606,08606,1,,12.1800000,50.4300000,Paarstraße,Oelsnitz/Vogtl.\n\
             paarstrasse,002,oelsnitz vogtl,08606,08606,3,,12.1810000,50.4310000,Paarstraße,Oelsnitz/Vogtl.\n",
            "de",
        );
        assert!(
            range
                .de_postcode_house_rescue_parsed(
                    "Paarstraße 1/3, 08606 Oelsnitz/Vogtland",
                    "Paarstraße 1 08606",
                    8606,
                    "oelsnitz vogtland",
                    1,
                    None,
                    None,
                    &[3],
                    false,
                )
                .is_none(),
            "same-coordinate duplicate source SIDs must fail closed for a complete endpoint set"
        );
    }

    #[test]
    fn de_wave_n_same_postcode_unrelated_locality_vetoes_soft_override() {
        let idx = forward_postcode_index_for_country(
            "wave-n-unrelated-locality-veto",
            "alte poststrasse,003,kanzach,88422,88422,2,,9.5590000,48.0780000,Alte Poststraße,Kanzach\n\
             riedlinger strasse,001,kanzach,,,13,,9.5580000,48.0770000,Riedlinger Straße,Kanzach\n\
             riedlinger strasse,002,bad buchau,88422,88422,12,,9.6010000,48.0610000,Riedlinger Straße,Bad Buchau\n",
            "de",
        );

        let top = idx
            .query("Riedlinger Straße 12, 88422 Kanzach", 1)
            .into_iter()
            .next()
            .expect("the established same-postcode locality result must remain available");
        assert_eq!(top.commune, "Kanzach");
        assert_eq!(top.postcode, "");
        assert!(matches!(top.precision, "near" | "interp"));
        assert!(!top.flags.contains(&"de_exact_postcode_override"));
    }

    #[test]
    fn de_wave_n_explicit_wrong_postcode_near_remains_p1_override() {
        let idx = forward_postcode_index_for_country(
            "wave-n-explicit-wrong-postcode-near",
            "teststrasse,001,querytown,22111,22111,13,,9.2100000,48.4900000,Teststraße,Querytown\n\
             teststrasse,002,targettown,22222,22222,12,,9.2200000,48.5000000,Teststraße,Targettown\n",
            "de",
        );

        let top = idx
            .query("Teststraße 12, 22222 Querytown", 1)
            .into_iter()
            .next()
            .expect("an explicit wrong-postcode near hit must keep the established P1 correction");
        assert_eq!(top.precision, "house");
        assert_eq!(top.housenumber.as_deref(), Some("12"));
        assert_eq!(top.postcode, "22222");
        assert_eq!(top.commune, "Targettown");
        assert!(top.flags.contains(&"de_postcode_house"));
        assert!(top.flags.contains(&"de_exact_postcode_override"));
    }

    #[test]
    fn de_wave_n_country_tail_and_qualified_localities_reach_exact_houses() {
        for (case, rows, query, expected_commune, expected_house, expected_postcode) in [
            (
                "freiburg-country-tail",
                "wiesentalstrasse,001,freiburg,79115,79115,23,,7.8255000,47.9780000,Wiesentalstraße,Freiburg\n\
                 wiesentalstrasse,002,freiburg im breisgau,79115,79115,10,,7.8260000,47.9790000,Wiesentalstraße,Freiburg im Breisgau\n",
                "Wiesentalstraße 10, 79115 Freiburg, Deutschland",
                "Freiburg im Breisgau",
                "10",
                "79115",
            ),
            (
                "frankenberg-qualified",
                "chemnitzer strasse,001,frankenberg sachsen,09669,09669,17,,13.0310000,50.9100000,Chemnitzer Straße,Frankenberg/Sachsen\n\
                 chemnitzer strasse,002,frankenberg,09669,09669,64,,13.0320000,50.9110000,Chemnitzer Straße,Frankenberg\n\
                 chemnitzer strasse,003,frankenberg sa,09669,09669,64,,13.0330000,50.9120000,Chemnitzer Straße,Frankenberg/Sa.\n",
                "Chemnitzer Str. 64, 09669 Frankenberg/Sachsen",
                "Frankenberg/Sa.",
                "64",
                "09669",
            ),
        ] {
            let idx = forward_postcode_index_for_country(case, rows, "de");
            let top = idx
                .query(query, 1)
                .into_iter()
                .next()
                .expect("the exact represented house must remain available");
            assert_eq!(top.precision, "house", "{case}");
            assert_eq!(top.housenumber.as_deref(), Some(expected_house), "{case}");
            assert_eq!(top.postcode, expected_postcode, "{case}");
            assert_eq!(top.commune, expected_commune, "{case}");
        }
    }

    #[test]
    fn de_wave_n_existing_same_postcode_locality_relations_remain_reachable() {
        for (case, rows, postcode, locality, expected_commune) in [
            (
                "muhlhausen-regression",
                "teststrasse,001,muhlhausen,99974,99974,12,,10.0000000,51.0000000,Teststraße,Mühlhausen\n\
                 teststrasse,002,targettown,99974,99974,12,,10.1000000,51.1000000,Teststraße,Targettown\n",
                99974,
                "muhlhausen thuringen",
                "Mühlhausen",
            ),
            (
                "bernburg-regression",
                "teststrasse,001,bernburg,06406,06406,12,,11.0000000,51.0000000,Teststraße,Bernburg\n\
                 teststrasse,002,targettown,06406,06406,12,,11.1000000,51.1000000,Teststraße,Targettown\n",
                6406,
                "bernburg saale",
                "Bernburg",
            ),
            (
                "berlin-regression",
                "teststrasse,001,charlottenburg,10587,10587,12,,13.3000000,52.5000000,Teststraße,Charlottenburg\n\
                 teststrasse,002,targettown,10587,10587,12,,13.4000000,52.6000000,Teststraße,Targettown\n",
                10587,
                "berlin",
                "Charlottenburg",
            ),
        ] {
            let idx = forward_postcode_index_for_country(case, rows, "de");
            let query = format!("Teststraße 12 {postcode:05}");
            let exact = idx
                .de_postcode_house_rescue_parsed(
                    &query,
                    &query,
                    postcode,
                    locality,
                    1,
                    None,
                    None,
                    &[],
                    false,
                )
                .expect("the pre-Wave-N locality relation must retain one exact house");
            assert_eq!(exact.len(), 1, "{case}");
            assert_eq!(exact[0].0.commune, expected_commune, "{case}");
            assert_eq!(exact[0].0.housenumber.as_deref(), Some("12"), "{case}");
            assert_eq!(exact[0].0.postcode, format!("{postcode:05}"), "{case}");
        }
    }

    #[test]
    fn de_wave_a_unique_exact_postcode_overrides_only_an_address_level_soft_locality() {
        let idx = forward_postcode_index_for_country(
            "wave-a-postcode-override",
            "teststrasse,001,querytown,22111,22111,1,,10.0000000,50.0000000,Teststraße,Querytown\n\
             teststrasse,002,targettown,22222,22222,1,,11.0000000,51.0000000,Teststraße,Targettown\n",
            "de",
        );

        let top = idx
            .query("Teststraße 1, 22222 Querytown", 1)
            .into_iter()
            .next()
            .expect("an address-level homonym must still return one candidate");
        assert_eq!(top.precision, "house");
        assert_eq!(top.housenumber.as_deref(), Some("1"));
        assert_eq!(top.postcode, "22222");
        assert_eq!(top.commune, "Targettown");
        assert!(top.flags.contains(&"de_exact_postcode_override"));
        let top_five = idx.query("Teststraße 1, 22222 Querytown", 5);
        assert_eq!(top_five[0].postcode, "22222");
        assert_eq!(top_five[0].commune, "Targettown");

        let duplicate = forward_postcode_index_for_country(
            "wave-a-postcode-override-duplicate",
            "teststrasse,001,querytown,22111,22111,1,,10.0000000,50.0000000,Teststraße,Querytown\n\
             teststrasse,002,targettown,22222,22222,1,,11.0000000,51.0000000,Teststraße,Targettown\n\
             teststrasse,003,othertown,22222,22222,1,,12.0000000,52.0000000,Teststraße,Othertown\n",
            "de",
        );
        let duplicate_top = duplicate
            .query("Teststraße 1, 22222 Querytown", 1)
            .into_iter()
            .next()
            .expect("the established result remains when exact PLZ candidates are ambiguous");
        assert_eq!(duplicate_top.commune, "Querytown");
        assert_ne!(duplicate_top.postcode, "22222");
        assert!(!duplicate_top.flags.contains(&"de_exact_postcode_override"));

        let suffix_isolated = forward_postcode_index_for_country(
            "wave-a-postcode-override-suffix",
            "teststrasse,001,querytown,22111,22111,1,,10.0000000,50.0000000,Teststraße,Querytown\n\
             teststrasse,002,targettown,22222,22222,1,a,11.0000000,51.0000000,Teststraße,Targettown\n",
            "de",
        );
        let suffix_top = suffix_isolated
            .query("Teststraße 1, 22222 Querytown", 1)
            .into_iter()
            .next()
            .expect("a suffix mismatch must leave the established result intact");
        assert_eq!(suffix_top.commune, "Querytown");
        assert!(!suffix_top.flags.contains(&"de_exact_postcode_override"));

        let exact_suffix = forward_postcode_index_for_country(
            "wave-a-postcode-override-exact-suffix",
            "teststrasse,001,querytown,22111,22111,3,a,10.0000000,50.0000000,Teststraße,Querytown\n\
             teststrasse,002,targettown,22222,22222,3,a,11.0000000,51.0000000,Teststraße,Targettown\n",
            "de",
        );
        let suffix_target = exact_suffix
            .query("Teststraße 3 a, 22222 Querytown", 1)
            .into_iter()
            .next()
            .expect("an exact spaced suffix must stay part of the strict product key");
        assert_eq!(suffix_target.housenumber.as_deref(), Some("3a"));
        assert_eq!(suffix_target.postcode, "22222");
        assert_eq!(suffix_target.commune, "Targettown");
        assert!(suffix_target.flags.contains(&"de_exact_postcode_override"));

        let frankfurt = forward_postcode_index_for_country(
            "wave-a-postcode-override-frankfurt",
            "domstrasse,001,frankfurt am main,15231,15231,10,,8.6800000,50.1100000,Domstraße,Frankfurt am Main\n\
             domstrasse,002,frankfurt,15230,15230,10,,14.5500000,52.3470000,Domstraße,Frankfurt (Oder)\n",
            "de",
        );
        let frankfurt_top = frankfurt
            .query("Domstraße 10, 15230 Frankfurt am Main", 1)
            .into_iter()
            .next()
            .expect("a hard Frankfurt qualifier must keep the established candidate");
        assert_ne!(frankfurt_top.postcode, "15230");
        assert!(!frankfurt_top.flags.contains(&"de_exact_postcode_override"));
    }

    #[test]
    fn de_wave_a_suffixed_house_never_falls_back_to_the_bare_number() {
        let idx = forward_postcode_index_for_country(
            "wave-a-suffix-does-not-fall-back-to-bare",
            "teststrasse,001,querytown,22111,22111,3,a,10.0000000,50.0000000,Teststraße,Querytown\n\
             teststrasse,002,targettown,22222,22222,3,,11.0000000,51.0000000,Teststraße,Targettown\n",
            "de",
        );

        let top = idx
            .query("Teststraße 3a, 22222 Querytown", 1)
            .into_iter()
            .next()
            .expect("the established exact-suffix result must remain available");
        assert_eq!(top.housenumber.as_deref(), Some("3a"));
        assert_eq!(top.commune, "Querytown");
        assert_eq!(top.postcode, "22111");
        assert!(
            !top.flags.contains(&"de_exact_postcode_override"),
            "a bare indexed house must not prove a suffixed product key"
        );

        let mut key = b"teststrasse".to_vec();
        key.push(KEY_SEP);
        key.extend_from_slice(b"002");
        let target_sid = idx
            .streets_fst
            .get(&key)
            .expect("the bare target street must exist") as u32;
        let target_meta = idx.street_meta(target_sid);
        let requested_rep = *idx
            .rep_lookup
            .get("a")
            .expect("the requested suffix must have a runtime rep id");
        assert!(
            !idx.exact_house_postcode_candidate_cached(
                &mut HashMap::new(),
                target_sid,
                &target_meta,
                3,
                requested_rep,
                22222,
            ),
            "the strict Wave-A admission proof itself must reject bare 3 for requested 3a"
        );
    }

    #[test]
    fn de_wave_a_bare_house_never_falls_forward_to_a_suffix() {
        let idx = forward_postcode_index_for_country(
            "wave-a-bare-does-not-fall-forward-to-suffix",
            "teststrasse,001,querytown,22111,22111,3,,10.0000000,50.0000000,Teststraße,Querytown\n\
             teststrasse,002,targettown,22222,22222,3,a,11.0000000,51.0000000,Teststraße,Targettown\n",
            "de",
        );

        let top = idx
            .query("Teststraße 3, 22222 Querytown", 1)
            .into_iter()
            .next()
            .expect("the established bare-number result must remain available");
        assert_eq!(top.housenumber.as_deref(), Some("3"));
        assert_eq!(top.commune, "Querytown");
        assert_eq!(top.postcode, "22111");
        assert!(
            !top.flags.contains(&"de_exact_postcode_override"),
            "a suffixed indexed house must not prove a bare product key"
        );

        let mut key = b"teststrasse".to_vec();
        key.push(KEY_SEP);
        key.extend_from_slice(b"002");
        let target_sid = idx
            .streets_fst
            .get(&key)
            .expect("the suffixed target street must exist") as u32;
        let target_meta = idx.street_meta(target_sid);
        assert!(
            !idx.exact_house_postcode_candidate_cached(
                &mut HashMap::new(),
                target_sid,
                &target_meta,
                3,
                0,
                22222,
            ),
            "the strict Wave-A admission proof itself must reject suffixed 3a for requested bare 3"
        );
    }

    #[test]
    fn de_wave_a_override_gate_is_address_level_and_fail_closed() {
        let pair = |hit: RankedHit| (hit.0, hit.1);
        let make = |precision, postcode, features, flags| {
            vec![pair(retained_test_hit(
                "Querytown",
                "Teststraße",
                Some("1"),
                postcode,
                precision,
                1.0,
                features,
                flags,
                0,
                0,
            ))]
        };

        assert!(!Index::de_strict_postcode_house_override_allowed(
            &[],
            22222
        ));
        assert!(!Index::de_strict_postcode_house_override_allowed(
            &make(
                "street",
                "22111",
                [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                vec!["street_exact"],
            ),
            22222,
        ));
        assert!(Index::de_strict_postcode_house_override_allowed(
            &make(
                "house",
                "22111",
                [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
                vec!["street_exact", "house_rep"],
            ),
            22222,
        ));
        assert!(!Index::de_strict_postcode_house_override_allowed(
            &make(
                "house",
                "22222",
                [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0],
                vec!["street_exact", "house_rep", "pc_exact"],
            ),
            22222,
        ));
        assert!(Index::de_strict_postcode_house_override_allowed(
            &make(
                "near",
                "22222",
                [0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                vec!["pc_exact"],
            ),
            22222,
        ));
        assert!(!Index::de_strict_postcode_house_override_allowed(
            &make("near", "22111", [0.0; N_FEATS], vec![],),
            22222,
        ));
    }

    #[test]
    fn de_wave_a_exact_house_beyond_cap_beats_near_without_fuzzy_fallback() {
        let mut rows = String::from(
            "teststrasse,001,querytown region,22222,22222,2,,10.0000000,50.0000000,Teststraße,Querytown Region\n\
             teststrasse,001,querytown region,22222,22222,4,,10.0010000,50.0010000,Teststraße,Querytown Region\n",
        );
        for commune in 2..=340 {
            rows.push_str(&format!(
                "teststrasse,{commune:03},ort{commune:03},22222,22222,2,,10.{commune:07},50.{commune:07},Teststraße,Ort {commune:03}\n"
            ));
        }
        rows.push_str(
            "teststrasse,999,querytown,22222,22222,3,,11.0000000,51.0000000,Teststraße,Querytown\n",
        );
        let idx = forward_postcode_index_for_country("wave-a-exact-house-beyond-cap", &rows, "de");

        DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| rows.set(0));
        DE_POSTCODE_HOUSE_RESCUE_FUZZY_CALLS.with(|calls| calls.set(0));
        DE_POSTCODE_HOUSE_RESCUE_SUBSET_CALLS.with(|calls| calls.set(0));
        let direct = idx
            .de_postcode_house_rescue_parsed(
                "Teststraße 3, 22222 Querytown Region",
                "Teststraße 3 22222",
                22222,
                "querytown region",
                1,
                None,
                None,
                &[],
                false,
            )
            .expect("the bounded exact product scan must find the unique indexed house");
        assert_eq!(direct[0].0.commune, "Querytown");
        assert!(DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| rows.get()) > 300);
        assert_eq!(
            DE_POSTCODE_HOUSE_RESCUE_FUZZY_CALLS.with(|calls| calls.get()),
            0,
            "the strict rescue must not open the fuzzy collector"
        );
        assert_eq!(
            DE_POSTCODE_HOUSE_RESCUE_SUBSET_CALLS.with(|calls| calls.get()),
            0,
            "the strict rescue must not open the subset collector"
        );

        let top = idx
            .query("Teststraße 3, 22222 Querytown Region", 1)
            .into_iter()
            .next()
            .expect("the exact indexed house must beat a capped near result");
        assert_eq!(top.precision, "house");
        assert_eq!(top.housenumber.as_deref(), Some("3"));
        assert_eq!(top.postcode, "22222");
        assert_eq!(top.commune, "Querytown");
        assert!(top.flags.contains(&"de_postcode_house"));
    }

    #[test]
    fn de_wave_a_two_endpoint_house_set_is_same_sid_postcode_and_suffix_strict() {
        let complete = forward_postcode_index_for_country(
            "wave-a-house-set-complete",
            "paarstrasse,001,querytown region,23552,23552,2,,10.0000000,50.0000000,Paarstraße,Querytown Region\n\
             paarstrasse,001,querytown region,23552,23552,4,,10.0010000,50.0010000,Paarstraße,Querytown Region\n\
             paarstrasse,999,querytown,23552,23552,1,,11.0000000,51.0000000,Paarstraße,Querytown\n\
             paarstrasse,999,querytown,23552,23552,3,,11.0010000,51.0010000,Paarstraße,Querytown\n",
            "de",
        );
        let top = complete
            .query("Paarstraße 1/3, 23552 Querytown Region", 1)
            .into_iter()
            .next()
            .expect("both exact endpoints on one SID must be eligible");
        assert_eq!(top.precision, "house");
        assert_eq!(top.housenumber.as_deref(), Some("1"));
        assert_eq!(top.commune, "Querytown");
        assert!(top.flags.contains(&"de_exact_postcode_override"));
        assert!(top.flags.contains(&"de_house_set_exact"));
        assert!(top.flags.contains(&"de_house_slash"));

        for (name, target_rows) in [
            (
                "missing-right",
                "paarstrasse,999,querytown,23552,23552,1,,11.0000000,51.0000000,Paarstraße,Querytown\n",
            ),
            (
                "split-sids",
                "paarstrasse,998,querytown,23552,23552,1,,11.0000000,51.0000000,Paarstraße,Querytown\n\
                 paarstrasse,999,querytown,23552,23552,3,,11.0010000,51.0010000,Paarstraße,Querytown\n",
            ),
            (
                "right-wrong-postcode",
                "paarstrasse,999,querytown,23552,23552,1,,11.0000000,51.0000000,Paarstraße,Querytown\n\
                 paarstrasse,999,querytown,23553,23553,3,,11.0010000,51.0010000,Paarstraße,Querytown\n",
            ),
            (
                "right-wrong-suffix",
                "paarstrasse,999,querytown,23552,23552,1,,11.0000000,51.0000000,Paarstraße,Querytown\n\
                 paarstrasse,999,querytown,23552,23552,3,a,11.0010000,51.0010000,Paarstraße,Querytown\n",
            ),
            (
                "duplicate-full-sets",
                "paarstrasse,998,querytown,23552,23552,1,,11.0000000,51.0000000,Paarstraße,Querytown\n\
                 paarstrasse,998,querytown,23552,23552,3,,11.0010000,51.0010000,Paarstraße,Querytown\n\
                 paarstrasse,999,querytown,23552,23552,1,,12.0000000,52.0000000,Paarstraße,Querytown\n\
                 paarstrasse,999,querytown,23552,23552,3,,12.0010000,52.0010000,Paarstraße,Querytown\n",
            ),
        ] {
            let rows = format!(
                "paarstrasse,001,querytown region,23552,23552,2,,10.0000000,50.0000000,Paarstraße,Querytown Region\n\
                 paarstrasse,001,querytown region,23552,23552,4,,10.0010000,50.0010000,Paarstraße,Querytown Region\n{target_rows}"
            );
            let idx = forward_postcode_index_for_country(name, &rows, "de");
            let top = idx
                .query("Paarstraße 1/3, 23552 Querytown Region", 1)
                .into_iter()
                .next()
                .expect("the established near result must remain available");
            assert_eq!(top.commune, "Querytown Region", "{name}");
            assert!(!top.flags.contains(&"de_house_set_exact"), "{name}");
            assert!(!top.flags.contains(&"de_exact_postcode_override"), "{name}");
        }
    }

    #[test]
    fn de_postcode_house_rescue_skips_fuzzy_and_subset_fallbacks() {
        let mut rows = String::new();
        for commune in 1..=340 {
            rows.push_str(&format!(
                "teststrasse,{commune:03},ort{commune:03},20202,20202,1,,10.{commune:07},50.{commune:07},Teststraße,Ort {commune:03}\n"
            ));
        }
        let idx = forward_postcode_index_for_country("postcode-house-no-fallbacks", &rows, "de");
        DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| rows.set(0));
        DE_POSTCODE_HOUSE_RESCUE_HOUSE_DECODES.with(|calls| calls.set(0));
        DE_POSTCODE_HOUSE_RESCUE_FUZZY_CALLS.with(|calls| calls.set(0));
        DE_POSTCODE_HOUSE_RESCUE_SUBSET_CALLS.with(|calls| calls.set(0));

        assert!(idx
            .de_comma_postcode_house_rescue("Teststraße 1, 10115 Alpha", 1, None)
            .is_none());
        assert!(DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| rows.get()) > 300);
        assert_eq!(
            DE_POSTCODE_HOUSE_RESCUE_HOUSE_DECODES.with(|calls| calls.get()),
            0,
            "postcode metadata must reject every incompatible homonym before house decoding"
        );
        assert_eq!(
            DE_POSTCODE_HOUSE_RESCUE_FUZZY_CALLS.with(|calls| calls.get()),
            0,
            "a rescue that ultimately requires street_exact must not run fuzzy collection"
        );
        assert_eq!(
            DE_POSTCODE_HOUSE_RESCUE_SUBSET_CALLS.with(|calls| calls.get()),
            0,
            "a rescue that ultimately requires street_exact must not run subset collection"
        );
    }

    #[test]
    fn de_postcode_house_rescue_fails_closed_when_exact_key_scan_overflows() {
        let hidden_duplicate = forward_postcode_index_for_country(
            "postcode-house-hidden-duplicate",
            "teststrasse,001,alpha,10115,10115,1,,13.3800000,52.5100000,Teststraße,Alpha\n\
             teststrasse,002,beta,20202,20202,2,,9.9900000,53.5500000,Teststraße,Beta\n\
             teststrasse,003,gamma,30303,30303,3,,11.5800000,48.1400000,Teststraße,Gamma\n\
             teststrasse,004,zzzdelta,10115,10115,1,,13.3900000,52.5200000,Teststraße,ZZZ Delta\n",
            "de",
        );
        DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT.with(|limit| limit.set(3));
        assert!(hidden_duplicate
            .de_comma_postcode_house_rescue("Teststraße 1, 10115 Alpha", 1, None)
            .is_none());

        let hidden_unique = forward_postcode_index_for_country(
            "postcode-house-hidden-unique",
            "teststrasse,001,alpha,11111,11111,1,,13.3800000,52.5100000,Teststraße,Alpha\n\
             teststrasse,002,beta,20202,20202,2,,9.9900000,53.5500000,Teststraße,Beta\n\
             teststrasse,003,gamma,30303,30303,3,,11.5800000,48.1400000,Teststraße,Gamma\n\
             teststrasse,004,zzzdelta,10115,10115,1,,13.3900000,52.5200000,Teststraße,ZZZ Delta\n",
            "de",
        );
        assert!(hidden_unique
            .de_comma_postcode_house_rescue("Teststraße 1, 10115 ZZZ Delta", 1, None)
            .is_none());
        DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT
            .with(|limit| limit.set(DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT_DEFAULT));
    }

    #[test]
    fn de_comma_postcode_house_rescue_beats_a_locality_alias_mismatch() {
        let idx = forward_postcode_index_for_country(
            "postcode-house-locality-alias",
            "wiesenstraße,001,reichenbach,,,16,,12.3000000,50.6100000,Wiesenstraße,Reichenbach\n\
             wiesenstraße,002,reichenbach im vogtland,08468,08468,62,,12.3073686,50.6160174,Wiesenstraße,Reichenbach im Vogtland\n",
            "de",
        );

        DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| rows.set(0));
        let top = idx
            .query("Wiesenstraße 62, 08468 Reichenbach (Vogt.)", 1)
            .into_iter()
            .next()
            .expect("the explicit street, house and postcode must rescue the locality alias");
        assert_eq!(top.street, "Wiesenstraße");
        assert_eq!(top.housenumber.as_deref(), Some("62"));
        assert_eq!(top.postcode, "08468");
        assert_eq!(top.commune, "Reichenbach im Vogtland");
        assert!(top.flags.contains(&"de_postcode_house"));
        assert!(
            DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| rows.get()) > 0,
            "the alias mismatch must use the narrow extended scan"
        );

        DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| rows.set(0));
        let ordinary = idx.query("Wiesenstraße 62, 08468 Reichenbach im Vogtland", 1);
        assert_eq!(
            ordinary.first().map(|hit| hit.postcode.as_str()),
            Some("08468")
        );
        assert_eq!(
            DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| rows.get()),
            0,
            "a complete ordinary query must never pay for the extended homonym scan"
        );

        assert!(
            idx.de_comma_postcode_house_rescue("Wiesenstraße 62, 08468 Hamburg", 1, None,)
                .is_none(),
            "an exact postcode candidate must not erase an explicit contradictory locality"
        );
    }

    #[test]
    fn de_postcode_house_rescue_composes_with_bounded_delivery_cleanup() {
        let idx = forward_postcode_index_for_country(
            "postcode-house-delivery-cleanup",
            "berliner straße,001,delmenhorst,,,121,,8.6536862,53.0422502,Berliner Straße,Delmenhorst\n\
             berliner straße,002,pankow,13187,13187,121,,13.4121414,52.5686551,Berliner Straße,Pankow\n\
             friedrichstraße,003,braunschweig,,,55,,10.5315859,52.2522607,Friedrichstraße,Braunschweig\n\
             friedrichstraße,004,mitte,10117,10117,55,,13.3902408,52.5092760,Friedrichstraße,Mitte\n\
             teststraße,006,berlin,10117,10117,70,,13.3913916,52.5087689,Teststraße,Berlin\n",
            "de",
        );

        for (query, expected_street, expected_postcode, effect_flag) in [
            (
                "für den Empfang, Berliner Straße 121, 13187 Berlin",
                "Berliner Straße",
                "13187",
                "de_recipient_prefix",
            ),
            (
                "Berlin, Friedrichstraße 55, 10117",
                "Friedrichstraße",
                "10117",
                "de_locality_first",
            ),
            (
                "Teststraße 70,10117 Berlin,Tel. 030 49499637",
                "Teststraße",
                "10117",
                "de_subaddress_tail",
            ),
        ] {
            let (top, _) = idx
                .de_comma_postcode_house_rescue(query, 1, None)
                .unwrap_or_else(|| panic!("bounded cleanup must rescue {query:?}"))
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("bounded cleanup must rescue {query:?}"));
            assert_eq!(top.street, expected_street, "{query}");
            assert_eq!(top.postcode, expected_postcode, "{query}");
            assert!(
                top.flags.contains(&"de_postcode_house"),
                "{query}: {:?}",
                top.flags
            );
            assert!(top.flags.contains(&effect_flag), "{query}: {:?}", top.flags);
        }

        assert!(idx
            .de_comma_postcode_house_rescue(
                "für den Empfang, Berliner Straße 121-123, 13187 Berlin",
                1,
                None,
            )
            .is_none());
    }

    #[test]
    fn de_postcode_house_rescue_accepts_only_exact_postcode_locality_aliases() {
        let idx = forward_postcode_index_for_country(
            "postcode-house-exact-locality-aliases",
            "havelweg,002,brandenburg,14770,14770,2,,12.5500000,52.4100000,Havelweg,Brandenburg\n\
             kirchweg,003,fehmarn,23769,23769,3,,11.1900000,54.4400000,Kirchweg,Fehmarn\n\
             pankower weg,001,pankow,13187,13187,1,,13.4100000,52.5700000,Pankower Weg,Pankow\n\
             spreeweg,004,lubbenau,03222,03222,4,,13.9600000,51.8600000,Spreeweg,Lübbenau\n",
            "de",
        );
        for (query, expected_commune) in [
            ("Pankower Weg 1, 13187 Berlin", "Pankow"),
            ("Havelweg 2, 14770 Brandenburg an der Havel", "Brandenburg"),
            ("Kirchweg 3, 23769 Landkirchen", "Fehmarn"),
            ("Spreeweg 4, 03222 Zerkwitz", "Lübbenau"),
        ] {
            let rescued = idx
                .de_comma_postcode_house_rescue(query, 1, None)
                .unwrap_or_else(|| panic!("the exact alias must rescue {query:?}"));
            assert_eq!(rescued[0].0.commune, expected_commune);
            assert!(rescued[0].0.flags.contains(&"de_postcode_house"));
        }
        assert!(idx
            .de_comma_postcode_house_rescue("Pankower Weg 1, 13187 Gesundbrunnen", 1, None)
            .is_none());
        assert!(idx
            .de_comma_postcode_house_rescue("Kirchweg 3, 23769 Zerkwitz", 1, None)
            .is_none());

        let ranked = forward_postcode_index_for_country(
            "postcode-house-exact-alias-ranked",
            "pankower weg,001,berlin,13187,13187,1,,13.4000000,52.5600000,Pankower Weg,Berlin\n\
             pankower weg,002,pankow,13187,13187,1,,13.4100000,52.5700000,Pankower Weg,Pankow\n",
            "de",
        );
        let exact_locality = ranked
            .de_comma_postcode_house_rescue("Pankower Weg 1, 13187 Berlin", 1, None)
            .expect("exact locality must outrank a weaker audited district relation");
        assert_eq!(exact_locality[0].0.commune, "Berlin");

        let tied = forward_postcode_index_for_country(
            "postcode-house-exact-alias-tied",
            "pankower weg,001,berlin,13187,13187,1,,13.4000000,52.5600000,Pankower Weg,Berlin\n\
             pankower weg,002,berlin,13187,13187,1,,13.4100000,52.5700000,Pankower Weg,Berlin\n",
            "de",
        );
        assert!(tied
            .de_comma_postcode_house_rescue("Pankower Weg 1, 13187 Berlin", 1, None)
            .is_none());
    }

    #[test]
    fn de_exact_alias_dropped_prefix_top_reopens_only_the_narrow_rescue() {
        let pair = |hit: RankedHit| (hit.0, hit.1);
        let complete_dropped = vec![pair(retained_test_hit(
            "Werder",
            "An der Havel",
            Some("44"),
            "14542",
            "house",
            1.0,
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec!["street_exact", "house_rep", "pc_exact", "dropped_prefix"],
            0,
            0,
        ))];
        assert!(Index::de_should_try_postcode_house_rescue(
            "Alpenstraße 44, 14542 Werder a.d.Havel",
            &complete_dropped,
        ));
        assert!(!Index::de_should_try_postcode_house_rescue(
            "Alpenstraße 44, 14543 Werder a.d.Havel",
            &complete_dropped,
        ));

        let mut complete_without_drop = complete_dropped;
        complete_without_drop[0]
            .0
            .flags
            .retain(|flag| *flag != "dropped_prefix");
        assert!(!Index::de_should_try_postcode_house_rescue(
            "Alpenstraße 44, 14542 Werder a.d.Havel",
            &complete_without_drop,
        ));
    }

    #[test]
    fn de_comma_postcode_house_rescue_rejects_explicit_frankfurt_conflict() {
        let idx = forward_postcode_index_for_country(
            "postcode-house-frankfurt-conflict",
            "collegienstrasse,003,frankfurt,15230,15230,10,,14.5500000,52.3470000,Collegienstraße,Frankfurt\n\
             domstrasse,001,frankfurt am main,60311,60311,10,,8.6820000,50.1110000,Domstraße,Frankfurt am Main\n\
             dummy,002,frankfurt oder,15230,15230,1,,14.5500000,52.3470000,Dummy,Frankfurt (Oder)\n\
             teststrasse,004,frankfurt,60311,60311,10,,8.6820000,50.1110000,Teststraße,Frankfurt\n",
            "de",
        );
        assert!(idx
            .de_comma_postcode_house_rescue("Domstraße 10, 60311 Frankfurt am Main", 1, None,)
            .is_some());
        assert!(idx
            .de_comma_postcode_house_rescue("Domstraße 10, 60311 Frankfurt an der Oder", 1, None,)
            .is_none());
        assert!(idx
            .de_comma_postcode_house_rescue("Domstraße 10, 60311 Offenbach", 1, None,)
            .is_none());
        assert!(idx
            .de_comma_postcode_house_rescue("Collegienstraße 10, 15230 Frankfurt/Oder", 1, None,)
            .is_some());
        assert!(idx
            .de_comma_postcode_house_rescue("Teststraße 10, 60311 Frankfurt/Oder", 1, None,)
            .is_none());
    }

    #[test]
    fn de_comma_postcode_house_rescue_shape_is_narrow_and_range_safe() {
        assert_eq!(
            de_comma_postcode_house_rescue_query("Wiesenstr. 62, 08468 Reichenbach (Vogt.)"),
            Some((
                "Wiesenstr. 62 08468".to_string(),
                8468,
                "reichenbach vogt".to_string(),
            ))
        );
        assert_eq!(
            de_comma_postcode_house_rescue_query("Olsdorfer Str. 6, 25826 St Peter-Ording"),
            Some((
                "Olsdorfer Str. 6 25826".to_string(),
                25826,
                "st peter ording".to_string(),
            ))
        );
        assert_eq!(
            de_comma_postcode_house_rescue_query("Breite Str. 49, 23769 Burg auf Fehmarn"),
            Some((
                "Breite Str. 49 23769".to_string(),
                23769,
                "burg auf fehmarn".to_string(),
            ))
        );
        for rejected in [
            "Wiesenstr. 62, 08468",
            "Wiesenstr. 62-64, 08468 Reichenbach",
            "Wiesenstr. 62 - 64, 08468 Reichenbach",
            "Alaunplatz 3b - 3c, 01099 Dresden",
            "Alaunplatz 3ab - 3ac, 01099 Dresden",
            "Wiesenstr. 62/64, 08468 Reichenbach",
            "Firma, Wiesenstr. 62, 08468 Reichenbach",
        ] {
            assert_eq!(de_comma_postcode_house_rescue_query(rejected), None);
        }
    }

    #[test]
    fn de_compact_house_pair_left_parser_is_narrow_and_dirty_tolerant() {
        for (raw, expected_query, expected_postcode, expected_locality, expected_effect) in [
            (
                "Hauptstraße 78/79, 12159 Berlin",
                "Hauptstraße 78 12159",
                12159,
                "berlin",
                crate::de::Effect::HouseSlash,
            ),
            (
                "Heerstraße 12–14 14052Berlin",
                "Heerstraße 12 14052",
                14052,
                "berlin",
                crate::de::Effect::HouseRange,
            ),
            (
                "Berliner Straße 46/48,16303 Schwedt/Oder",
                "Berliner Straße 46 16303",
                16303,
                "schwedt oder",
                crate::de::Effect::HouseSlash,
            ),
            (
                "Friedrichstraße 76—78 10117 Berlin",
                "Friedrichstraße 76 10117",
                10117,
                "berlin",
                crate::de::Effect::HouseRange,
            ),
        ] {
            assert_eq!(
                de_compact_house_pair_left_rescue_query(raw),
                Some((
                    expected_query.to_owned(),
                    expected_postcode,
                    expected_locality.to_owned(),
                    expected_effect,
                )),
                "unexpected parse for {raw:?}"
            );
        }
        assert_eq!(
            de_compact_house_pair_spec("Mühlendamm 1/3, 23552 Lübeck"),
            Some(DeCompactHousePairSpec {
                query: "Mühlendamm 1 23552".to_owned(),
                postcode: 23552,
                locality_tail: "lubeck".to_owned(),
                effect: crate::de::Effect::HouseSlash,
                left: 1,
                right: 3,
            })
        );
        assert_eq!(
            de_compact_house_pair_left_rescue_query("Mühlendamm 1/3, 23552 Lübeck"),
            None,
            "the legacy left-only fallback must not reinterpret a small slash pair"
        );
        for rejected in [
            "Kapuzinerstraße 1/2, 48149 Münster",
            "Berliner Straße 46/48/50, 16303 Schwedt/Oder",
            "Alaunplatz 3b-3c, 01099 Dresden",
            "Heerstraße 12 - 14, 14052 Berlin",
            "Heerstraße 12 bis 14, 14052 Berlin",
            "Heerstraße 12 und 14, 14052 Berlin",
            "Heerstraße 14-12, 14052 Berlin",
            "Heerstraße 12-14, 14052",
            "Firma, Heerstraße 12-14, 14052 Berlin",
            "Heerstraße 12-14, 14052 Berlin, Tel. 030 123456",
        ] {
            assert_eq!(de_compact_house_pair_left_rescue_query(rejected), None);
        }
    }

    #[test]
    fn de_compact_house_pair_left_rescue_requires_unique_exact_address() {
        let idx = forward_postcode_index_for_country(
            "compact-house-pair-left",
            "berliner strasse,001,schwedt,16303,16303,46,,14.2800000,53.0600000,Berliner Straße,Schwedt\n\
             friedrichstrasse,002,mitte,10117,10117,76,,13.3900000,52.5100000,Friedrichstraße,Mitte\n\
             hauptstrasse,003,friedenau,12159,12159,78,,13.3300000,52.4700000,Hauptstraße,Friedenau\n\
             heerstrasse,004,westend,14052,14052,12,,13.2600000,52.5100000,Heerstraße,Westend\n",
            "de",
        );
        for (query, expected_house, expected_commune, expected_effect_flag) in [
            (
                "Hauptstraße 78/79, 12159 Berlin",
                "78",
                "Friedenau",
                "de_house_slash",
            ),
            (
                "Heerstraße 12–14, 14052 Berlin",
                "12",
                "Westend",
                "de_house_range",
            ),
            (
                "Berliner Straße 46/48, 16303 Schwedt/Oder",
                "46",
                "Schwedt",
                "de_house_slash",
            ),
            (
                "Friedrichstraße 76-78, 10117 Berlin",
                "76",
                "Mitte",
                "de_house_range",
            ),
        ] {
            let rescued = idx
                .de_compact_house_pair_left_rescue(query, 1, None)
                .unwrap_or_else(|| panic!("left endpoint must rescue {query:?}"));
            let top = &rescued[0].0;
            assert_eq!(top.housenumber.as_deref(), Some(expected_house));
            assert_eq!(top.commune, expected_commune);
            assert!(top.flags.contains(&expected_effect_flag));
            assert!(top.flags.contains(&"de_house_left_endpoint"));
            assert!(!top.flags.contains(&"de_postal_tail"));
        }

        let duplicate = forward_postcode_index_for_country(
            "compact-house-pair-duplicate",
            "hauptstrasse,001,berlin,12159,12159,78,,13.3200000,52.4600000,Hauptstraße,Berlin\n\
             hauptstrasse,002,friedenau,12159,12159,78,,13.3300000,52.4700000,Hauptstraße,Friedenau\n",
            "de",
        );
        assert!(duplicate
            .de_compact_house_pair_left_rescue("Hauptstraße 78/79, 12159 Berlin", 1, None)
            .is_none());
        assert!(idx
            .de_compact_house_pair_left_rescue("Hauptstraße 78/79, 14052 Berlin", 1, None)
            .is_none());
        assert!(idx
            .de_compact_house_pair_left_rescue("Hauptstraße 78/79, 12159 Pankow", 1, None)
            .is_none());
    }

    #[test]
    fn de_compact_house_pair_rescue_reaches_unique_left_endpoint_beyond_ordinary_cap() {
        let mut rows = String::new();
        for commune in 1..=340 {
            rows.push_str(&format!(
                "hauptstrasse,{commune:03},ort{commune:03},20202,20202,1,,10.{commune:07},50.{commune:07},Hauptstraße,Ort {commune:03}\n"
            ));
        }
        rows.push_str(
            "hauptstrasse,999,friedenau,12159,12159,78,,13.3300000,52.4700000,Hauptstraße,Friedenau\n",
        );
        let idx = forward_postcode_index_for_country("compact-house-pair-beyond-cap", &rows, "de");
        DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| rows.set(0));
        let top = idx
            .query("Hauptstraße 78/79, 12159 Berlin", 1)
            .into_iter()
            .next()
            .expect("the unique exact left endpoint must survive the ordinary cap");
        assert_eq!(top.housenumber.as_deref(), Some("78"));
        assert_eq!(top.postcode, "12159");
        assert_eq!(top.commune, "Friedenau");
        assert!(top.flags.contains(&"de_house_left_endpoint"));
        assert!(top.flags.contains(&"de_house_slash"));
        assert!(!top.flags.contains(&"de_postal_tail"));
        assert!(DE_POSTCODE_HOUSE_RESCUE_SCAN_ROWS.with(|rows| rows.get()) > 300);
    }

    #[test]
    fn de_compact_house_pair_rescue_never_displaces_a_complete_current_result() {
        let idx = forward_postcode_index_for_country(
            "compact-house-pair-strong-current",
            "starkstrasse,001,berlin,10115,10115,10,,13.3900000,52.5300000,Starkstraße,Berlin\n",
            "de",
        );
        let top = idx
            .query("Starkstraße 10-12, 10115 Berlin", 1)
            .into_iter()
            .next()
            .expect("the ordinary compact-range rule must resolve the exact house");
        assert_eq!(top.housenumber.as_deref(), Some("10"));
        assert!(top.flags.contains(&"street_exact"));
        assert!(top.flags.contains(&"house_rep"));
        assert!(top.flags.contains(&"pc_exact"));
        assert!(!top.flags.contains(&"de_house_left_endpoint"));
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
    fn v7_exact_house_postcode_drives_pc_exact_instead_of_street_majority() {
        let idx = forward_postcode_index_for_country(
            "house-pc-exact",
            "guntzstrasse,001,dresden,1309,01309,2,,13.7400,51.0500,Güntzstraße,Dresden\n\
             guntzstrasse,001,dresden,1309,01309,4,,13.7410,51.0510,Güntzstraße,Dresden\n\
             guntzstrasse,001,dresden,1307,01307,22,,13.7420,51.0520,Güntzstraße,Dresden\n",
            "de",
        );
        let (hit, features) = idx
            .query_structured("guntzstrasse", Some("22"), "dresden", Some("01307"), 1)
            .remove(0);

        assert_eq!(hit.precision, "house");
        assert_eq!(hit.housenumber.as_deref(), Some("22"));
        assert_eq!(hit.postcode, "01307");
        assert_eq!(features[4], 1.0, "the selected house postcode is exact");
        assert_eq!(features[5], 1.0, "exact postcodes retain the dept feature");
        assert_eq!(
            hit.score, 11.0,
            "house postcode refinement precedes scoring"
        );
        assert!(hit.flags.contains(&"pc_exact"));
        assert!(!hit.flags.contains(&"pc_dept"));

        let free = idx
            .query("Güntzstraße 22, 01307 Dresden Unbekannt", 1)
            .remove(0);
        assert_eq!(free.precision, "house");
        assert_eq!(free.housenumber.as_deref(), Some("22"));
        assert_eq!(free.postcode, "01307");
        assert!(free.flags.contains(&"pc_exact"));
        assert!(!free.flags.contains(&"pc_dept"));
        assert!(free.flags.contains(&"dropped_suffix"));
        assert!(!free.flags.contains(&"de_postal_tail"));

        let (street_majority_was_wrong, features) = idx
            .query_structured("guntzstrasse", Some("22"), "dresden", Some("01309"), 1)
            .remove(0);
        assert_eq!(street_majority_was_wrong.postcode, "01307");
        assert_eq!(
            features[4], 0.0,
            "the chosen house disproves street pc_exact"
        );
        assert_eq!(features[5], 1.0);
        assert!(!street_majority_was_wrong.flags.contains(&"pc_exact"));
        assert!(street_majority_was_wrong.flags.contains(&"pc_dept"));
    }

    #[test]
    fn v7_duplicate_house_uses_query_postcode_to_choose_the_exact_row() {
        let idx = forward_postcode_index(
            "duplicate-house-postcode",
            "hauptstrasse,001,dresden,1156,01156,1,,13.6000,51.0600,Hauptstraße,Dresden\n\
             hauptstrasse,001,dresden,1097,01097,1,,13.7500,51.0700,Hauptstraße,Dresden\n\
             hauptstrasse,001,dresden,1328,01328,1,,13.8000,51.0800,Hauptstraße,Dresden\n\
             hauptstrasse,001,dresden,1156,01156,3,,13.6100,51.0610,Hauptstraße,Dresden\n",
        );
        let (hit, features) = idx
            .query_structured("hauptstrasse", Some("1"), "dresden", Some("01097"), 1)
            .remove(0);

        assert_eq!(hit.precision, "house");
        assert_eq!(hit.housenumber.as_deref(), Some("1"));
        assert_eq!(hit.postcode, "01097");
        assert_eq!(features[4], 1.0);
        assert!(hit.flags.contains(&"pc_exact"));

        let free = idx.query("hauptstrasse 1 01097 dresden", 1).remove(0);
        assert_eq!(free.postcode, "01097");
        assert!(free.flags.contains(&"pc_exact"));

        let third = idx
            .query_structured("hauptstrasse", Some("1"), "dresden", Some("01328"), 1)
            .remove(0)
            .0;
        assert_eq!(third.postcode, "01328");

        let (no_postcode, no_postcode_features) = idx
            .query_structured("hauptstrasse", Some("1"), "dresden", None, 1)
            .remove(0);
        assert_eq!(no_postcode.postcode, "01156");
        assert_eq!(no_postcode_features[8], 1.0);
        assert!(no_postcode.flags.contains(&"house_rep"));
        let (absent_postcode, absent_postcode_features) = idx
            .query_structured("hauptstrasse", Some("1"), "dresden", Some("01098"), 1)
            .remove(0);
        assert_eq!(absent_postcode.postcode, "01156");
        assert_eq!(absent_postcode_features[8], 1.0);
        assert!(absent_postcode.flags.contains(&"house_rep"));
    }

    #[test]
    fn v7_duplicate_postcode_selection_never_crosses_the_requested_suffix() {
        let idx = forward_postcode_index(
            "duplicate-house-suffix",
            "hauptstrasse,001,dresden,1156,01156,1,,13.6000,51.0600,Hauptstraße,Dresden\n\
             hauptstrasse,001,dresden,1097,01097,1,a,13.7500,51.0700,Hauptstraße,Dresden\n\
             hauptstrasse,001,dresden,1156,01156,3,,13.6100,51.0610,Hauptstraße,Dresden\n",
        );
        let blank = idx
            .query_structured("hauptstrasse", Some("1"), "dresden", Some("01097"), 1)
            .remove(0)
            .0;
        assert_eq!(blank.housenumber.as_deref(), Some("1"));
        assert_eq!(blank.postcode, "01156");
        assert!(blank.flags.contains(&"house_rep"));

        let suffixed = idx
            .query_structured("hauptstrasse", Some("1a"), "dresden", Some("01097"), 1)
            .remove(0)
            .0;
        assert_eq!(suffixed.housenumber.as_deref(), Some("1a"));
        assert_eq!(suffixed.postcode, "01097");
    }

    #[test]
    fn postcode_numeric_prefix_treats_zero_as_missing() {
        assert_eq!(Index::postcode_numeric_prefix("00000"), None);
        assert_eq!(Index::postcode_numeric_prefix(""), None);
        assert_eq!(Index::postcode_numeric_prefix("01307"), Some(1307));
        assert_eq!(Index::postcode_numeric_prefix("1012AA"), Some(1012));
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

        let (missing_with_query_postcode, features) = idx
            .query_structured("damrak", Some("1"), "amsterdam", Some("1012"), 1)
            .remove(0);
        assert_eq!(missing_with_query_postcode.postcode, "");
        assert_eq!(features[4], 0.0);
        assert_eq!(features[5], 0.0);
        assert!(!missing_with_query_postcode.flags.contains(&"pc_exact"));
        assert!(!missing_with_query_postcode.flags.contains(&"pc_dept"));
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

        let (cross_with_query_postcode, features) = interp_idx
            .query_structured("damrak", Some("25"), "amsterdam", Some("1012"), 1)
            .remove(0);
        assert_eq!(cross_with_query_postcode.postcode, "");
        assert_eq!(features[4], 0.0);
        assert_eq!(features[5], 0.0);
        assert!(!cross_with_query_postcode.flags.contains(&"pc_exact"));
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

    #[test]
    fn de_wave_b_p3_strict_source_typos_cover_the_six_product_shapes() {
        let cases = [
            (
                "ruesselheimerstrasse,001,kelsterbach,65450,65450,2,,8.5200000,50.0500000,Rüsselheimerstraße,Kelsterbach\n\
                 ruesselsheimer strasse,002,kelsterbach,65451,65451,2,,8.5300000,50.0600000,Rüsselsheimer Straße,Kelsterbach\n",
                "Rüsselheimerstr. 2, 65451 Kelsterbach",
                "Rüsselsheimer Straße",
                "2",
                "65451",
                "Kelsterbach",
            ),
            (
                "kirchgase,001,obersulm eschenau,74181,74181,16,,9.3700000,49.1300000,Kirchgase,Obersulm-Eschenau\n\
                 kirchgasse,002,obersulm,74182,74182,16,,9.3800000,49.1400000,Kirchgasse,Obersulm\n",
                "Kirchgase 16, 74182 Obersulm-Eschenau",
                "Kirchgasse",
                "16",
                "74182",
                "Obersulm",
            ),
            (
                "frankfurt strasse,001,muellrose,15298,15298,1,,14.4000000,52.2400000,Frankfurt Straße,Müllrose\n\
                 frankfurter strasse,002,muellrose,15299,15299,1,,14.4100000,52.2500000,Frankfurter Straße,Müllrose\n",
                "Frankfurt Straße 1, 15299 Müllrose",
                "Frankfurter Straße",
                "1",
                "15299",
                "Müllrose",
            ),
            (
                "oppenhaeuser strasse,001,lachendorf,29330,29330,3,,10.2400000,52.6000000,Oppenhäuser Straße,Lachendorf\n\
                 oppershaeuser strasse,002,lachendorf,29331,29331,3,,10.2500000,52.6100000,Oppershäuser Straße,Lachendorf\n",
                "Oppenhäuser Str. 3, 29331 Lachendorf",
                "Oppershäuser Straße",
                "3",
                "29331",
                "Lachendorf",
            ),
            (
                "robert schuman strasse,002,gersheim,66453,66453,2,,7.2400000,49.1500000,Robert-Schuman-Straße,Gersheim\n\
                 robert schumann strasse,001,gersheim reinheim,66452,66452,2,,7.2300000,49.1400000,Robert-Schumann-Straße,Gersheim-Reinheim\n",
                "Robert Schumann Str. 2, 66453 Gersheim-Reinheim",
                "Robert-Schuman-Straße",
                "2",
                "66453",
                "Gersheim",
            ),
            (
                "probsteistrasse,001,viersen,41748,41748,15,,6.3800000,51.2700000,Probsteistraße,Viersen\n\
                 propsteistrasse,002,viersen,41749,41749,15,,6.3900000,51.2800000,Propsteistraße,Viersen\n",
                "Probsteistr. 15, 41749 Viersen",
                "Propsteistraße",
                "15",
                "41749",
                "Viersen",
            ),
        ];

        for (position, (rows, query, street, house, postcode, commune)) in
            cases.into_iter().enumerate()
        {
            let idx =
                forward_postcode_index_for_country(&format!("wave-b-p3-{position}"), rows, "de");
            let top = idx
                .query(query, 1)
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("strict P3 source proof must resolve {query}"));
            assert_eq!(top.precision, "house", "{query}");
            assert_eq!(top.street, street, "{query}");
            assert_eq!(top.housenumber.as_deref(), Some(house), "{query}");
            assert_eq!(top.postcode, postcode, "{query}");
            assert_eq!(top.commune, commune, "{query}");
            assert!(
                top.flags.contains(&"de_strict_source_street_typo"),
                "the dedicated bounded P3 proof must own {query}: {:?}",
                top.flags
            );
        }
    }

    #[test]
    fn de_wave_b_p3_is_unique_exact_and_never_replaces_a_complete_top() {
        let ambiguous = forward_postcode_index_for_country(
            "wave-b-p3-ambiguous",
            "probsteikstrasse,002,viersen,41749,41749,15,,6.4000000,51.2900000,Probsteikstraße,Viersen\n\
             propsteistrasse,001,viersen,41749,41749,15,,6.3900000,51.2800000,Propsteistraße,Viersen\n",
            "de",
        );
        assert!(
            ambiguous
                .query("Probsteistr. 15, 41749 Viersen", 5)
                .iter()
                .all(|hit| !hit.flags.contains(&"de_strict_source_street_typo")),
            "two qualifying source-street identities must fail closed"
        );

        let repeated_identity = forward_postcode_index_for_country(
            "wave-b-p3-repeated-semantic-identity",
            "propsteistrasse,001,viersen,41749,41749,15,,6.3900000,51.2800000,Propsteistraße,Viersen\n\
             propsteistrasse,002,viersen,41749,41749,15,,6.4000000,51.2900000,Propsteistraße,Viersen\n",
            "de",
        );
        assert!(
            repeated_identity
                .query("Probsteistr. 15, 41749 Viersen", 1)
                .first()
                .is_some_and(|hit| hit.flags.contains(&"de_strict_source_street_typo")),
            "one semantic display-street identity may span multiple physical SIDs"
        );

        let already_exact = forward_postcode_index_for_country(
            "wave-b-p3-complete-top",
            "probsteistrasse,001,viersen,41749,41749,15,,6.3800000,51.2700000,Probsteistraße,Viersen\n\
             propsteistrasse,002,viersen,41749,41749,15,,6.3900000,51.2800000,Propsteistraße,Viersen\n",
            "de",
        );
        let exact_top = already_exact
            .query("Probsteistraße 15, 41749 Viersen", 1)
            .into_iter()
            .next()
            .expect("the ordinary exact address remains available");
        assert_eq!(exact_top.street, "Probsteistraße");
        assert!(!exact_top.flags.contains(&"de_strict_source_street_typo"));

        let wrong_fields = forward_postcode_index_for_country(
            "wave-b-p3-exact-fields",
            "propsteistrasse,001,otherstadt,41749,41749,15,,6.3900000,51.2800000,Propsteistraße,Otherstadt\n\
             propsteistrasse,002,viersen,41749,41749,15,a,6.4000000,51.2900000,Propsteistraße,Viersen\n",
            "de",
        );
        assert!(
            wrong_fields
                .query("Probsteistr. 15, 41749 Viersen", 5)
                .iter()
                .all(|hit| !hit.flags.contains(&"de_strict_source_street_typo")),
            "locality and suffix are exact P3 product gates"
        );

        for (case, query_street, source_street) in [
            ("similarity", "muster", "master"),
            (
                "osa-three",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklstrasse",
                "abcedfghijkymnopqrztuvwxyzabcdefghijklstrasse",
            ),
            (
                "first-codepoint",
                "abcdefghijklmnopqrstuvwxyzabcdefghijklstrasse",
                "bbcdefghijklmnopqrstuvwxyzabcdefghijklstrasse",
            ),
        ] {
            if case == "osa-three" {
                assert_eq!(
                    de_compact_osa_distance(query_street, source_street),
                    3,
                    "the negative must exercise the OSA admission guard"
                );
                assert!(
                    de_sequence_matcher_ratio_at_least_090(query_street, source_street),
                    "the independent ratio guard must pass in the OSA-only negative"
                );
            }
            let rows = format!(
                "{query_street},001,berlin,10114,10114,1,,13.3800000,52.5000000,{query_street},Berlin\n\
                 {source_street},002,berlin,10115,10115,1,,13.3900000,52.5100000,{source_street},Berlin\n"
            );
            let mut sorted = rows.lines().collect::<Vec<_>>();
            sorted.sort_unstable();
            let rows = format!("{}\n", sorted.join("\n"));
            let idx = forward_postcode_index_for_country(&format!("wave-b-p3-{case}"), &rows, "de");
            assert!(
                idx.query(&format!("{query_street} 1, 10115 Berlin"), 5)
                    .iter()
                    .all(|hit| !hit.flags.contains(&"de_strict_source_street_typo")),
                "P3 must enforce the independent {case} guard"
            );
        }

        let canonical_osa = forward_postcode_index_for_country(
            "wave-b-p3-canonical-osa",
            "muehlenabcdefghijklstrasse,001,berlin,10114,10114,1,,13.3800000,52.5000000,Mühlenabcdefghijklstraße,Berlin\n\
             muhlenabcxefgyijklstrasse,002,berlin,10115,10115,1,,13.3900000,52.5100000,Muhlenabcxefgyijklstraße,Berlin\n",
            "de",
        );
        assert!(
            canonical_osa
                .query("Mühlenabcdefghijklstr. 1, 10115 Berlin", 5)
                .iter()
                .all(|hit| !hit.flags.contains(&"de_strict_source_street_typo")),
            "a favorable lookup-only orthography variant may not replace canonical OSA admission"
        );

        let key_display_mismatch = forward_postcode_index_for_country(
            "wave-b-p3-key-display-mismatch",
            "probsteistrasse,001,viersen,41748,41748,15,,6.3800000,51.2700000,Probsteistraße,Viersen\n\
             propsteistrasse,002,viersen,41749,41749,15,,6.3900000,51.2800000,Unrelated Avenue,Viersen\n",
            "de",
        );
        assert!(
            key_display_mismatch
                .query("Probsteistr. 15, 41749 Viersen", 5)
                .iter()
                .all(|hit| !hit.flags.contains(&"de_strict_source_street_typo")),
            "a close FST key may not stand in for the product-normalized display street"
        );

        let hidden_display_identity = forward_postcode_index_for_country(
            "wave-b-p3-hidden-display-identity",
            "probsteistrasse,001,viersen,41748,41748,15,,6.3800000,51.2700000,Probsteistraße,Viersen\n\
             propsteistrasse,002,viersen,41749,41749,15,,6.3900000,51.2800000,Propsteistraße,Viersen\n\
             zzzzweg,003,viersen,41749,41749,15,,6.4000000,51.2900000,Probsteikstraße,Viersen\n",
            "de",
        );
        assert_eq!(
            hidden_display_identity
                .de_postcode_street_bucket(41749)
                .expect("exact DE postcode bucket")
                .len(),
            2,
            "the open-time roster must include both the near key and the hidden far key"
        );
        assert!(
            hidden_display_identity
                .query("Probsteistr. 15, 41749 Viersen", 5)
                .iter()
                .all(|hit| !hit.flags.contains(&"de_strict_source_street_typo")),
            "a second qualifying display identity behind an unrelated FST key must fail closed"
        );
        let previous_limit = DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT.with(|limit| limit.replace(1));
        assert!(
            hidden_display_identity
                .de_postcode_street_bucket(41749)
                .is_none(),
            "an oversized exact-postcode bucket must fail closed"
        );
        DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT.with(|limit| limit.set(previous_limit));

        let hidden_unique_identity = forward_postcode_index_for_country(
            "wave-b-p3-hidden-unique-display-identity",
            "probsteistrasse,001,viersen,41748,41748,15,,6.3800000,51.2700000,Probsteistraße,Viersen\n\
             zzzzweg,003,viersen,41749,41749,15,,6.4000000,51.2900000,Propsteistraße,Viersen\n",
            "de",
        );
        let hidden_unique_top = hidden_unique_identity
            .query("Probsteistr. 15, 41749 Viersen", 1)
            .into_iter()
            .next()
            .expect("the bounded postcode roster must find the unique display identity");
        assert_eq!(hidden_unique_top.street, "Propsteistraße");
        assert!(hidden_unique_top
            .flags
            .contains(&"de_strict_source_street_typo"));

        let mixed_postcodes = forward_postcode_index_for_country(
            "wave-b-p3-mixed-postcode-roster",
            "propsteistrasse,002,viersen,41749,41749,15,,6.3900000,51.2800000,Propsteistraße,Viersen\n\
             propsteistrasse,002,viersen,41750,41750,16,,6.3910000,51.2810000,Propsteistraße,Viersen\n",
            "de",
        );
        let mixed_bucket = mixed_postcodes
            .de_postcode_street_bucket(41750)
            .expect("v7 mixed-postcode dictionary must enter the roster");
        assert_eq!(mixed_bucket.len(), 1);
        let mixed_sid = mixed_bucket[0].1;
        let mixed_metadata = mixed_postcodes.street_meta(mixed_sid);
        assert!(
            mixed_postcodes.exact_house_full_postcode_set_candidate_cached(
                &mut HashMap::new(),
                mixed_sid,
                &mixed_metadata,
                16,
                0,
                &[],
                41750,
                "41750",
            )
        );
        assert!(
            !mixed_postcodes.exact_house_full_postcode_set_candidate_cached(
                &mut HashMap::new(),
                mixed_sid,
                &mixed_metadata,
                15,
                0,
                &[],
                41750,
                "41750",
            )
        );

        let locality_normalization = forward_postcode_index_for_country(
            "wave-b-p3-locality-normalization",
            "probsteistrasse,001,koeln,41748,41748,15,,6.3800000,51.2700000,Probsteistraße,Köln\n\
             propsteistrasse,002,koln,41749,41749,15,,6.3900000,51.2800000,Propsteistraße,Koln\n",
            "de",
        );
        assert!(
            locality_normalization
                .query("Probsteistr. 15, 41749 Köln", 5)
                .iter()
                .all(|hit| !hit.flags.contains(&"de_strict_source_street_typo")),
            "product normalization must distinguish umlaut digraphs from absent umlauts"
        );

        let lev4_uniqueness = forward_postcode_index_for_country(
            "wave-b-p3-lev4-uniqueness",
            "abcdefghijklmnopqrst,001,berlin,10114,10114,1,,13.3700000,52.4900000,abcdefghijklmnopqrst,Berlin\n\
             abcdefghijklmnopqrsx,002,berlin,10115,10115,1,,13.3800000,52.5000000,abcdefghijklmnopqrsx,Berlin\n\
             abcedfghijklmnoprqst,003,berlin,10115,10115,1,,13.3900000,52.5100000,abcedfghijklmnoprqst,Berlin\n",
            "de",
        );
        assert_eq!(
            de_compact_osa_distance("abcdefghijklmnopqrst", "abcedfghijklmnoprqst"),
            2
        );
        assert!(
            lev4_uniqueness
                .query("abcdefghijklmnopqrst 1, 10115 Berlin", 5)
                .iter()
                .all(|hit| !hit.flags.contains(&"de_strict_source_street_typo")),
            "the postcode roster must count both identities independently of FST distance"
        );
    }

    #[test]
    fn de_wave_b_p3_parser_osa_similarity_and_locality_are_bounded() {
        let spec = de_strict_source_street_typo_spec("Rüsselheimerstr. 2, 65451 Kelsterbach")
            .expect("the canonical simple P3 surface must parse");
        assert_eq!(spec.normalized_street, "ruesselheimerstrasse");
        assert_eq!(spec.normalized_locality, "kelsterbach");
        assert_eq!(spec.house_number, 2);
        assert_eq!(spec.postcode_raw, "65451");

        for raw in [
            "Haus 7 Straße 2, 65451 Kelsterbach",
            "Teststraße 2a, 65451 Kelsterbach",
            "Teststraße 2/3, 65451 Kelsterbach",
            "Teststraße 0002, 65451 Kelsterbach",
            "Teststraße 2, 6545 Kelsterbach",
            "Teststraße 2, 65451 Kelsterbach, Deutschland",
            "Teststraße 2, 65451 Kelsterbach\nEmpfang",
            "Teststraße 2, 65451 Kelsterbach ",
        ] {
            assert!(
                de_strict_source_street_typo_spec(raw).is_none(),
                "strict P3 parser must reject {raw:?}"
            );
        }

        assert_eq!(
            de_compact_osa_distance("probsteistrasse", "propsteistrasse"),
            1
        );
        assert_eq!(
            de_compact_osa_distance("frankfurt strasse", "frankfurter strasse"),
            2
        );
        assert_eq!(de_compact_osa_distance("abcdefgh", "abcxyzgh"), 3);
        assert!(de_sequence_matcher_ratio_at_least_090(
            "ruesselheimerstrasse",
            "ruesselsheimer strasse"
        ));
        assert!(!de_sequence_matcher_ratio_at_least_090("muster", "master"));
        assert!(de_sequence_matcher_ratio_at_least_090(
            "abcdefghij",
            "abcdefghix"
        ));
        for (raw, expected) in [
            ("Rüsselheimerstr.", "ruesselheimerstrasse"),
            ("Kirchgase", "kirchgase"),
            ("Frankfurt Straße", "frankfurt strasse"),
            ("Oppenhäuser Str.", "oppenhaeuser strasse"),
            ("Robert Schumann Str.", "robert schumann strasse"),
            ("Probsteistr.", "probsteistrasse"),
        ] {
            assert_eq!(de_product_normalize_street(raw), expected, "{raw}");
        }
        assert_eq!(de_product_normalize_text("Müllrose"), "muellrose");
        assert!(de_p3_locality_compatible("obersulm eschenau", "obersulm"));
        assert!(de_p3_locality_compatible("gersheim", "gersheim reinheim"));
        assert!(!de_p3_locality_compatible("obersulm", "obersulmberg"));
    }

    #[test]
    fn de_wave_b_p3_absent_postcode_roster_filters_only_double_conflicts() {
        let idx = forward_postcode_index_for_country(
            "wave-b-p3-absent-postcode-roster",
            "probsteistrasse,001,aldenhoven,52457,52457,15,,6.2800000,50.9000000,Probsteistraße,Aldenhoven\n",
            "de",
        );
        let current = |commune: &str, postcode: &str| {
            let (hit, features, ..) = retained_test_hit(
                commune,
                "Probsteistraße",
                Some("15"),
                postcode,
                "house",
                1.0,
                [0.0; N_FEATS],
                vec!["street_exact", "house_rep"],
                100,
                0,
            );
            vec![(hit, features)]
        };

        let qualifier_cases = [
            (
                "Probsteistr. 15, 41749 Viersen/Rhein",
                41749,
                "Viersen am Rhein",
                "slash",
            ),
            (
                "Probsteistr. 15, 79104 Freiburg (Breisgau)",
                79104,
                "Freiburg im Breisgau",
                "parenthesis",
            ),
            (
                "Probsteistr. 15, 18609 Binz - OT Prora",
                18609,
                "Prora",
                "OT",
            ),
            (
                "Probsteistr. 15, 74239 Hardthausen-Kochersteinsfeld",
                74239,
                "Kochersteinsfeld",
                "hyphen",
            ),
        ];
        for (query, postcode, commune, shape) in qualifier_cases {
            assert!(
                idx.de_postcode_street_bucket(postcode).is_none(),
                "the {shape} observer must exercise the absent exact-postcode roster branch"
            );
            assert!(
                idx.de_strict_source_street_typo_fallback(query, &current(commune, ""), 1)
                    .is_none(),
                "a missing row postcode is unknown and must preserve the {shape} locality qualifier"
            );
            let filtered = idx
                .de_strict_source_street_typo_fallback(query, &current(commune, "99999"), 1)
                .expect("a non-empty conflicting postcode must still fail closed");
            assert!(
                filtered.is_empty(),
                "the reverse-mutation observer must reject the {shape} qualifier when the row postcode contradicts the request"
            );
        }

        let query = "Probsteistr. 15, 41749 Viersen";
        assert!(
            idx.de_strict_source_street_typo_fallback(query, &current("Viersen", ""), 1)
                .is_none(),
            "a same-locality result with absent postcode remains eligible"
        );
        assert!(
            idx.de_strict_source_street_typo_fallback(query, &current("Aldenhoven", "41749"), 1,)
                .is_none(),
            "an exact-postcode result is not removed solely for locality disagreement"
        );
    }

    #[test]
    fn de_wave_b_p4_audited_compounds_cover_all_four_shapes() {
        let idx = forward_postcode_index_for_country(
            "wave-b-p4-five-surfaces",
            "albertstrasse,002,freiburg im breisgau,79104,79104,25,,7.8500000,48.0100000,Albertstraße,Freiburg im Breisgau\n\
             campus ring,003,bremen,28759,28759,1,,8.6500000,53.1700000,Campus Ring,Bremen\n\
             grosse meissner strasse,001,dresden,01097,01097,19,,13.7400000,51.0600000,Große Meißner Straße,Dresden\n\
             marktplatz,005,weilheim an der teck,73235,73235,4,,9.5400000,48.6200000,Marktplatz,Weilheim an der Teck\n\
             rheinstrasse,004,berlin,12161,12161,45,,13.3300000,52.4700000,Rheinstraße,Berlin\n\
             rheinstrasse,004,berlin,12161,12161,46,,13.3310000,52.4710000,Rheinstraße,Berlin\n",
            "de",
        );
        let cases = [
            (
                "Blockhaus, 19, Große Meißner Straße, Innere Neustadt, Neustadt, Dresden, Sachsen, 01097",
                "Große Meißner Straße",
                "19",
                "01097",
                "Dresden",
                "grosse meissner strasse",
                "dresden",
                false,
            ),
            (
                "Albertstr. 25 ( Otto-Krayer-Haus), 79104 Freiburg/Breisgau",
                "Albertstraße",
                "25",
                "79104",
                "Freiburg im Breisgau",
                "albertstrasse",
                "freiburg im breisgau",
                false,
            ),
            (
                "c/o Jacobs University Bremen Campusring 1 Bremen, 28759 Bremen",
                "Campus Ring",
                "1",
                "28759",
                "Bremen",
                "campus ring",
                "bremen",
                false,
            ),
            (
                "Rheinstr. 45/46 (Aufgang 6), 12161 Berlin",
                "Rheinstraße",
                "45",
                "12161",
                "Berlin",
                "rheinstrasse",
                "berlin",
                true,
            ),
            (
                "Marktplatz 4 (Weilheimer \"Bürgerhaus\"), 73235 Weilheim/Teck",
                "Marktplatz",
                "4",
                "73235",
                "Weilheim an der Teck",
                "marktplatz",
                "weilheim an der teck",
                false,
            ),
        ];

        for (
            query,
            street,
            house,
            postcode,
            commune,
            normalized_street,
            normalized_locality,
            complete_set,
        ) in cases
        {
            let spec = de_audited_compound_spec(query).expect("P4 product fields must parse");
            assert_eq!(spec.normalized_street, normalized_street, "{query}");
            assert_eq!(spec.normalized_locality, normalized_locality, "{query}");
            let top = idx
                .query(query, 1)
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("strict P4 source proof must resolve {query}"));
            assert_eq!(top.precision, "house", "{query}");
            assert_eq!(top.street, street, "{query}");
            assert_eq!(top.housenumber.as_deref(), Some(house), "{query}");
            assert_eq!(top.postcode, postcode, "{query}");
            assert_eq!(top.commune, commune, "{query}");
            assert!(
                top.flags.contains(&"de_audited_compound"),
                "the dedicated typed P4 proof must own {query}: {:?}",
                top.flags
            );
            assert_eq!(
                top.flags.contains(&"de_house_set_exact"),
                complete_set,
                "{query}"
            );
        }
    }

    #[test]
    fn de_wave_b_p4_requires_complete_set_exact_locality_and_de_scope() {
        let missing_endpoint = forward_postcode_index_for_country(
            "wave-b-p4-missing-endpoint",
            "rheinstrasse,004,berlin,12161,12161,45,,13.3300000,52.4700000,Rheinstraße,Berlin\n",
            "de",
        );
        let mut rhein_key = b"rheinstrasse".to_vec();
        rhein_key.push(KEY_SEP);
        rhein_key.extend_from_slice(b"004");
        let rhein_sid = missing_endpoint
            .streets_fst
            .get(&rhein_key)
            .expect("fixture street") as u32;
        let rhein_meta = missing_endpoint.street_meta(rhein_sid);
        assert!(
            !missing_endpoint.exact_house_postcode_set_candidate_cached(
                &mut HashMap::new(),
                rhein_sid,
                &rhein_meta,
                45,
                0,
                &[46],
                12161,
            ),
            "the low-level source proof must reject the missing right endpoint"
        );
        let mut direct_budget = DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT_DEFAULT;
        let mut direct_seen = HashSet::new();
        let direct = missing_endpoint
            .query_feats_prepared_postcode_house_rescue(
                &prepared_query_key("rheinstrasse 45, 12161 berlin"),
                20,
                None,
                &[46],
                &mut direct_budget,
                &mut direct_seen,
            )
            .expect("bounded direct scan must not overflow");
        assert!(
            direct.is_empty(),
            "the prepared exact scan must preserve the complete endpoint set"
        );
        let missing_hits = missing_endpoint.query("Rheinstr. 45/46 (Aufgang 6), 12161 Berlin", 5);
        assert!(
            missing_hits
                .iter()
                .all(|hit| !hit.flags.contains(&"de_audited_compound")),
            "a range without endpoint 46 must fail closed"
        );

        let freiburg = forward_postcode_index_for_country(
            "wave-b-p4-locality",
            "albertstrasse,002,freiburg im breisgau,79104,79104,25,,7.8500000,48.0100000,Albertstraße,Freiburg im Breisgau\n",
            "de",
        );
        assert!(
            freiburg
                .query("Albertstr. 25 (Haus), 79104 Freiburg/Oder", 5)
                .iter()
                .all(|hit| !hit.flags.contains(&"de_audited_compound")),
            "the bounded slash-locality alias must preserve its qualifier"
        );

        let invented_preposition = forward_postcode_index_for_country(
            "wave-b-p4-no-invented-preposition",
            "albertstrasse,002,freiburg an der breisgau,79104,79104,25,,7.8500000,48.0100000,Albertstraße,Freiburg an der Breisgau\n",
            "de",
        );
        assert!(
            invented_preposition
                .query(
                    "Albertstr. 25 (Otto-Krayer-Haus), 79104 Freiburg/Breisgau",
                    5,
                )
                .iter()
                .all(|hit| !hit.flags.contains(&"de_audited_compound")),
            "slash locality must project one product locality, never both prepositions"
        );

        let venue_admin_decoy = forward_postcode_index_for_country(
            "wave-b-p4-venue-admin-decoy",
            "grosse meissner strasse,001,sachsen,01097,01097,19,,13.7400000,51.0600000,Große Meißner Straße,Sachsen\n",
            "de",
        );
        assert!(
            venue_admin_decoy
                .query(
                    "Blockhaus, 19, Große Meißner Straße, Innere Neustadt, Neustadt, Dresden, Sachsen, 01097",
                    5,
                )
                .iter()
                .all(|hit| !hit.flags.contains(&"de_audited_compound")),
            "venue administrative qualifiers are not alternative query localities"
        );

        let display_mismatch = forward_postcode_index_for_country(
            "wave-b-p4-display-mismatch",
            "albertstrasse,002,freiburg im breisgau,79104,79104,25,,7.8500000,48.0100000,Alberta Straße,Freiburg im Breisgau\n",
            "de",
        );
        assert!(
            display_mismatch
                .query(
                    "Albertstr. 25 (Otto-Krayer-Haus), 79104 Freiburg/Breisgau",
                    5,
                )
                .iter()
                .all(|hit| !hit.flags.contains(&"de_audited_compound")),
            "an exact retrieval key cannot replace normalized display-street equality"
        );

        let arbitrary_care_of_split = forward_postcode_index_for_country(
            "wave-b-p4-care-of-split",
            "campusri ng,003,bremen,28759,28759,1,,8.6500000,53.1700000,Campusri Ng,Bremen\n",
            "de",
        );
        assert!(
            arbitrary_care_of_split
                .query(
                    "c/o Jacobs University Bremen Campusring 1 Bremen, 28759 Bremen",
                    5,
                )
                .iter()
                .all(|hit| !hit.flags.contains(&"de_audited_compound")),
            "c/o parsing may split only a terminal street-type word"
        );

        let non_de = forward_postcode_index_for_country(
            "wave-b-p4-country-boundary",
            "marktplatz,005,weilheim an der teck,73235,73235,4,,9.5400000,48.6200000,Marktplatz,Weilheim an der Teck\n",
            "fr",
        );
        assert!(
            non_de
                .query(
                    "Marktplatz 4 (Weilheimer \"Bürgerhaus\"), 73235 Weilheim/Teck",
                    5,
                )
                .iter()
                .all(|hit| !hit.flags.contains(&"de_audited_compound")),
            "the audited compound fallback is DE-only"
        );

        let duplicate = forward_postcode_index_for_country(
            "wave-b-p4-duplicate",
            "albertstrasse,001,freiburg im breisgau,79104,79104,25,,7.8500000,48.0100000,Albertstraße,Freiburg im Breisgau\n\
             albertstrasse,002,freiburg im breisgau,79104,79104,25,,7.8600000,48.0200000,Albertstraße,Freiburg im Breisgau\n",
            "de",
        );
        assert!(
            duplicate
                .query(
                    "Albertstr. 25 (Otto-Krayer-Haus), 79104 Freiburg/Breisgau",
                    5,
                )
                .iter()
                .all(|hit| !hit.flags.contains(&"de_audited_compound")),
            "two exact source candidates must fail closed"
        );
        assert!(
            duplicate
                .de_audited_compound_fallback(
                    "Albertstr. 25 (Otto-Krayer-Haus), 79104 Freiburg/Breisgau",
                    5,
                )
                .is_none(),
            "the dedicated P4 admission path itself must reject duplicate source rows"
        );

        let wrong_endpoint_postcode = forward_postcode_index_for_country(
            "wave-b-p4-endpoint-display-postcode",
            "rheinstrasse,004,berlin,12161,12161,45,,13.3300000,52.4700000,Rheinstraße,Berlin\n\
             rheinstrasse,004,berlin,12161,12161A,46,,13.3310000,52.4710000,Rheinstraße,Berlin\n",
            "de",
        );
        assert!(
            wrong_endpoint_postcode
                .de_audited_compound_fallback("Rheinstr. 45/46 (Aufgang 6), 12161 Berlin", 5,)
                .is_none(),
            "every literal range endpoint must carry the full requested display postcode"
        );

        let wrong_primary_postcode = forward_postcode_index_for_country(
            "wave-b-p4-primary-display-postcode",
            "albertstrasse,002,freiburg im breisgau,79104,79104A,25,,7.8500000,48.0100000,Albertstraße,Freiburg im Breisgau\n",
            "de",
        );
        assert!(
            wrong_primary_postcode
                .de_audited_compound_fallback(
                    "Albertstr. 25 (Otto-Krayer-Haus), 79104 Freiburg/Breisgau",
                    5,
                )
                .is_none(),
            "the primary house must carry the full requested display postcode"
        );

        let wrong_primary_house = forward_postcode_index_for_country(
            "wave-b-p4-primary-house",
            "albertstrasse,002,freiburg im breisgau,79104,79104,26,,7.8500000,48.0100000,Albertstraße,Freiburg im Breisgau\n",
            "de",
        );
        assert!(
            wrong_primary_house
                .de_audited_compound_fallback(
                    "Albertstr. 25 (Otto-Krayer-Haus), 79104 Freiburg/Breisgau",
                    5,
                )
                .is_none(),
            "P4 must prove the exact primary house rather than a near/interpolated result"
        );

        let duplicate_same_coordinates = forward_postcode_index_for_country(
            "wave-b-p4-duplicate-same-coordinates",
            "albertstrasse,001,freiburg im breisgau,79104,79104,25,,7.8500000,48.0100000,Albertstraße,Freiburg im Breisgau\n\
             albertstrasse,002,freiburg im breisgau,79104,79104,25,,7.8500000,48.0100000,Albertstraße,Freiburg im Breisgau\n",
            "de",
        );
        assert_eq!(
            duplicate_same_coordinates
                .query(
                    "Albertstr. 25 (Otto-Krayer-Haus), 79104 Freiburg/Breisgau",
                    5,
                )
                .iter()
                .any(|hit| hit.flags.contains(&"de_audited_compound")),
            duplicate
                .query(
                    "Albertstr. 25 (Otto-Krayer-Haus), 79104 Freiburg/Breisgau",
                    5,
                )
                .iter()
                .any(|hit| hit.flags.contains(&"de_audited_compound")),
            "P4 admission must be invariant to equal versus different result coordinates"
        );

        let suffix_only = forward_postcode_index_for_country(
            "wave-b-p4-suffix",
            "albertstrasse,001,freiburg im breisgau,79104,79104,25,a,7.8500000,48.0100000,Albertstraße,Freiburg im Breisgau\n",
            "de",
        );
        assert!(
            suffix_only
                .query(
                    "Albertstr. 25 (Otto-Krayer-Haus), 79104 Freiburg/Breisgau",
                    5,
                )
                .iter()
                .all(|hit| !hit.flags.contains(&"de_audited_compound")),
            "a bare P4 house may not fall forward to a suffixed source house"
        );
    }

    #[test]
    fn de_wave_b_p4_commune_and_attempt_budgets_have_boundary_observers() {
        let campus_query = "Campus Ring 1 (Haus), 28759 Bremen";
        for (communes, admitted) in [(16usize, true), (17usize, false)] {
            let idx = forward_postcode_index_for_country(
                &format!("wave-b-p4-commune-budget-{communes}"),
                &de_wave_b_homonymous_p4_rows(communes, "campus ring", "Campus Ring"),
                "de",
            );
            let _rules = crate::rules::scope(idx.rules);
            let spec = de_audited_compound_spec(campus_query).expect("typed P4 query");
            let forms = de_product_street_forms(&spec.street);
            assert_eq!(forms.len(), 1, "the commune boundary must be isolated");
            assert_eq!(
                idx.communes_by_name(&spec.normalized_locality).len(),
                communes
            );
            assert_eq!(
                idx.de_audited_compound_fallback(campus_query, 1).is_some(),
                admitted,
                "the 16/17 commune boundary must be observable"
            );
        }

        let attempts_query = "Aastr Bbstr 1 (Haus), 28759 Bremen";
        let attempts_spec =
            de_audited_compound_spec(attempts_query).expect("typed P4 attempt-budget query");
        for (communes, expected_attempts, admitted) in
            [(9usize, 63usize, true), (10usize, 70usize, false)]
        {
            let idx = forward_postcode_index_for_country_with_rules(
                &format!("wave-b-p4-attempt-budget-{communes}"),
                &de_wave_b_homonymous_p4_rows(
                    communes,
                    &attempts_spec.normalized_street,
                    "Aastr Bbstr",
                ),
                "de",
            );
            let _rules = crate::rules::scope(idx.rules);
            let forms = de_product_street_forms(&attempts_spec.street);
            assert_eq!(forms.len() * communes, expected_attempts);
            assert!(forms.len() <= 32 && communes <= 16);
            assert_eq!(
                idx.de_audited_compound_fallback(attempts_query, 1)
                    .is_some(),
                admitted,
                "the 64-attempt boundary must be observable independently"
            );
        }
    }

    #[test]
    fn de_wave_b_p4_street_form_budget_has_a_boundary_observer() {
        let cases = [
            (
                "Aastr Bbstr Ccstr Ddstr Eestr Ffstr Ggstr Hhstr Iistr Jjstr",
                31usize,
                true,
            ),
            (
                "Aastr Bbstr Ccstr Ddstr Eestr Ffstr Ggstr Hhstr Iistr Jjstr Kkstr",
                34usize,
                false,
            ),
        ];
        for (street, expected_forms, admitted) in cases {
            let query = format!("{street} 1 (Haus), 28759 Bremen");
            let spec = de_audited_compound_spec(&query).expect("typed P4 form-budget query");
            let rows = format!(
                "{},001,bremen,28759,28759,1,,8.6500000,53.1700000,{street},Bremen\n",
                spec.normalized_street
            );
            let idx = forward_postcode_index_for_country_with_rules(
                &format!("wave-b-p4-street-form-budget-{expected_forms}"),
                &rows,
                "de",
            );
            let _rules = crate::rules::scope(idx.rules);
            let forms = de_product_street_forms(&spec.street);
            assert_eq!(forms.len(), expected_forms);
            assert_eq!(idx.communes_by_name(&spec.normalized_locality).len(), 1);
            assert_eq!(
                idx.de_audited_compound_fallback(&query, 1).is_some(),
                admitted,
                "the 32-form boundary must be observable"
            );
        }
    }

    #[test]
    fn de_wave_p_p4_recall_does_not_depend_on_a_street_fst_projection() {
        let mut idx = forward_postcode_index_for_country(
            "wave-p-p4-fst-projection-independent",
            "campus ring,002,bremen,28759,28759,1,,8.6500000,53.1700000,Campus Ring,Bremen\n\
             zzdummy,001,bremen,28759,28759,1,,8.6600000,53.1800000,ZZ Dummy,Bremen\n",
            "de",
        );
        let rules = idx.rules;
        let _rules = crate::rules::scope(rules);
        let query = "Campus Ring 1 (Haus), 28759 Bremen";
        assert!(
            idx.de_audited_compound_fallback(query, 1).is_some(),
            "the clean fixture must prove that all non-corrupt product gates pass"
        );

        let mut source_key = b"campus ring".to_vec();
        source_key.push(KEY_SEP);
        source_key.extend_from_slice(b"002");
        let sid = idx
            .streets_fst
            .get(&source_key)
            .expect("clean source street") as u32;
        let source_metadata = idx.street_meta(sid);

        let mut mismatched_key = b"campus ring".to_vec();
        mismatched_key.push(KEY_SEP);
        mismatched_key.extend_from_slice(b"001");
        let mut builder = fst::MapBuilder::memory();
        builder.insert(&mismatched_key, sid as u64).unwrap();
        let bytes: &'static [u8] = Box::leak(builder.into_inner().unwrap().into_boxed_slice());
        idx.streets_fst = Map::new(bytes).unwrap();

        let lookup_commune_id = idx
            .communes_by_name("bremen")
            .into_iter()
            .find(|&commune_id| idx.commune_insee(commune_id) == "001")
            .expect("mismatched lookup commune");
        assert_ne!(source_metadata.commune_id, lookup_commune_id);
        let recovered = idx
            .de_audited_compound_fallback(query, 1)
            .expect("exact display metadata must remain reachable without the FST projection");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].0.street, "Campus Ring");
        assert_eq!(recovered[0].0.commune, "Bremen");
        assert_eq!(recovered[0].0.housenumber.as_deref(), Some("1"));
        assert_eq!(recovered[0].0.postcode, "28759");
        assert!(recovered[0].0.flags.contains(&"de_audited_compound"));
    }

    #[test]
    fn de_wave_p_p4_recovers_three_non_ascii_source_key_archetypes() {
        let cases = [
            (
                "venue",
                "Speicher, 19, Große Hafenstraße, Altquartier, Neustadt, Elbstadt, Sachsen, 01097",
                "große hafenstraße,001,elbstadt,01097,01097,19,,11.1000000,51.1000000,Große Hafenstraße,Elbstadt\n",
                "Große Hafenstraße",
                "19",
                "01097",
                "Elbstadt",
                false,
            ),
            (
                "parenthetical",
                "Gartenstr. 25 (Haus A), 79104 Bergheim/Breisgau",
                "gartenstraße,002,bergheim im breisgau,79104,79104,25,,11.2000000,51.2000000,Gartenstraße,Bergheim im Breisgau\n",
                "Gartenstraße",
                "25",
                "79104",
                "Bergheim im Breisgau",
                false,
            ),
            (
                "range",
                "Uferstr. 45/46 (Aufgang 6), 12161 Neustadt",
                "uferstraße,003,neustadt,12161,12161,45,,11.3000000,51.3000000,Uferstraße,Neustadt\n\
                 uferstraße,003,neustadt,12161,12161,46,,11.3010000,51.3010000,Uferstraße,Neustadt\n",
                "Uferstraße",
                "45",
                "12161",
                "Neustadt",
                true,
            ),
        ];

        for (case, query, rows, street, house, postcode, locality, complete_set) in cases {
            let idx = forward_postcode_index_for_country(&format!("wave-p-p4-{case}"), rows, "de");
            let spec = de_audited_compound_spec(query).expect("the audited P4 shape must parse");
            let commune_id = idx
                .communes_by_name(&spec.normalized_locality)
                .into_iter()
                .next()
                .expect("fixture locality");
            let mut old_lookup_key = spec.normalized_street.as_bytes().to_vec();
            old_lookup_key.push(KEY_SEP);
            old_lookup_key.extend_from_slice(idx.commune_insee(commune_id).as_bytes());
            assert!(
                idx.streets_fst.get(&old_lookup_key).is_none(),
                "{case} must be invisible to the former normalized-key lookup"
            );

            DE_P4_POSTCODE_BUCKET_SCAN_ROWS.with(|rows| rows.set(0));
            let direct = idx
                .de_audited_compound_fallback(query, 1)
                .unwrap_or_else(|| panic!("bounded display-metadata recall must resolve {case}"));
            assert_eq!(direct.len(), 1, "{case}");
            assert!(
                DE_P4_POSTCODE_BUCKET_SCAN_ROWS.with(|rows| rows.get()) > 0,
                "{case} must traverse the runtime postcode bucket"
            );

            let top = idx
                .query(query, 1)
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("runtime arbitration must retain {case}"));
            assert_eq!(top.precision, "house", "{case}");
            assert_eq!(top.street, street, "{case}");
            assert_eq!(top.housenumber.as_deref(), Some(house), "{case}");
            assert_eq!(top.postcode, postcode, "{case}");
            assert_eq!(top.commune, locality, "{case}");
            assert!(top.flags.contains(&"de_audited_compound"), "{case}");
            assert_eq!(
                top.flags.contains(&"de_house_set_exact"),
                complete_set,
                "{case}"
            );
        }
    }

    #[test]
    fn de_wave_p_p4_hidden_duplicate_sid_ambiguity_and_bucket_overflow_fail_closed() {
        let query = "Gartenstr. 25 (Haus A), 79104 Bergheim/Breisgau";
        let hidden_duplicate = forward_postcode_index_for_country(
            "wave-p-p4-hidden-duplicate",
            "gartenstrasse,001,bergheim im breisgau,79104,79104,25,,11.2000000,51.2000000,Gartenstraße,Bergheim im Breisgau\n\
             gartenstraße,002,bergheim im breisgau,79104,79104,25,,11.9000000,51.9000000,Gartenstrasse,Bergheim im Breisgau\n",
            "de",
        );
        assert_eq!(
            hidden_duplicate
                .de_postcode_street_bucket(79104)
                .expect("two-SID postcode bucket")
                .len(),
            2
        );
        assert!(
            hidden_duplicate
                .de_audited_compound_fallback(query, 1)
                .is_none(),
            "two source SIDs with one product projection must fail closed"
        );

        let mut duplicate_posting = forward_postcode_index_for_country(
            "wave-p-p4-duplicate-posting",
            "gartenstraße,001,bergheim im breisgau,79104,79104,25,,11.2000000,51.2000000,Gartenstraße,Bergheim im Breisgau\n",
            "de",
        );
        let posting = duplicate_posting.de_postcode_streets[0];
        duplicate_posting.de_postcode_streets = vec![posting, posting].into_boxed_slice();
        assert!(
            duplicate_posting
                .de_audited_compound_fallback(query, 1)
                .is_some(),
            "duplicate postings for one source SID must collapse before uniqueness"
        );

        let overflow = forward_postcode_index_for_country(
            "wave-p-p4-postcode-overflow",
            "gartenstrasse,001,bergheim im breisgau,79104,79104,25,,11.2000000,51.2000000,Gartenstraße,Bergheim im Breisgau\n\
             nebenweg,002,anderstadt,79104,79104,9,,11.3000000,51.3000000,Nebenweg,Anderstadt\n",
            "de",
        );
        let previous_limit = DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT.with(|limit| limit.replace(1));
        let result = overflow.de_audited_compound_fallback(query, 1);
        DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT.with(|limit| limit.set(previous_limit));
        assert!(
            result.is_none(),
            "an exact-postcode bucket above the shared audited ceiling must fail closed"
        );
    }

    #[test]
    fn de_wave_p_p4_incompatible_candidate_locality_is_vetoed_after_bucket_recall() {
        let idx = forward_postcode_index_for_country(
            "wave-p-p4-candidate-locality-veto",
            "gartenstraße,001,anderstadt,79104,79104,25,,11.2000000,51.2000000,Gartenstraße,Anderstadt\n\
             nebenweg,002,bergheim im breisgau,79104,79104,9,,11.3000000,51.3000000,Nebenweg,Bergheim im Breisgau\n",
            "de",
        );
        let query = "Gartenstr. 25 (Haus A), 79104 Bergheim/Breisgau";
        assert_eq!(
            idx.communes_by_name("bergheim im breisgau").len(),
            1,
            "an exact query-locality anchor must make the post-recall veto observable"
        );
        DE_P4_POSTCODE_BUCKET_MATCHING_SIDS.with(|matches| matches.set(0));
        assert!(
            idx.de_audited_compound_fallback(query, 1).is_none(),
            "an exact street/house/PLZ in another locality must fail closed"
        );
        assert_eq!(
            DE_P4_POSTCODE_BUCKET_MATCHING_SIDS.with(|matches| matches.get()),
            0,
            "the incompatible SID must be vetoed during product-visible admission"
        );
    }

    #[test]
    fn de_wave_p_p4_row_order_and_non_product_metadata_do_not_select_a_candidate() {
        let query = "Gartenstr. 25 (Haus A), 79104 Bergheim/Breisgau";
        let rows = [
            "gartenstraße,001,bergheim im breisgau,79104,79104,25,,11.2000000,51.2000000,Gartenstraße,Bergheim im Breisgau\n\
             nebenweg,002,anderstadt,79104,79104,9,,11.3000000,51.3000000,Nebenweg,Anderstadt\n",
            "gartenstraße,001,bergheim im breisgau,79104,79104,25,,18.8000000,58.8000000,Gartenstraße,Bergheim im Breisgau\n\
             nebenweg,002,anderstadt,79104,79104,9,,19.9000000,59.9000000,Nebenweg,Anderstadt\n",
        ];
        let mut projections = Vec::new();
        for (variant, rows) in rows.into_iter().enumerate() {
            let mut idx = forward_postcode_index_for_country(
                &format!("wave-p-p4-order-{variant}"),
                rows,
                "de",
            );
            if variant == 1 {
                idx.de_postcode_streets.reverse();
            }
            let hit = idx
                .de_audited_compound_fallback(query, 1)
                .expect("the unique product candidate must survive source order")
                .remove(0)
                .0;
            projections.push((
                hit.precision,
                hit.street,
                hit.housenumber,
                hit.postcode,
                hit.commune,
                hit.flags,
            ));
        }
        assert_eq!(projections[0], projections[1]);
    }

    #[test]
    fn de_wave_p_p4_overlap_preserves_the_established_answer() {
        let idx = forward_postcode_index_for_country(
            "wave-p-p4-overlap",
            "gartenstraße,001,bergheim im breisgau,79104,79104,25,,11.2000000,51.2000000,Gartenstraße,Bergheim im Breisgau\n",
            "de",
        );
        let p4 = idx
            .de_audited_compound_fallback("Gartenstr. 25 (Haus A), 79104 Bergheim/Breisgau", 1)
            .expect("P4 witness");
        let (established_hit, established_features, ..) = retained_test_hit(
            "Bergheim im Breisgau",
            "Gartenstraße",
            Some("24"),
            "79104",
            "near",
            1.0,
            [0.0; N_FEATS],
            vec!["street_exact", "commune_exact", "pc_exact"],
            24,
            0,
        );
        let mut established = vec![(established_hit, established_features)];
        let competing = idx
            .de_audited_compound_fallback("Gartenstr. 25 (Haus A), 79104 Bergheim/Breisgau", 1)
            .expect("second mechanism witness");
        Index::de_product_fallback_arbitration(
            &mut established,
            None,
            Some(p4),
            None,
            Some(competing),
        );
        assert_eq!(established.len(), 1);
        assert_eq!(established[0].0.precision, "near");
        assert_eq!(established[0].0.street, "Gartenstraße");
        assert_eq!(established[0].0.housenumber.as_deref(), Some("24"));
        assert_eq!(established[0].0.postcode, "79104");
        assert!(!established[0].0.flags.contains(&"de_audited_compound"));
    }

    #[test]
    fn de_wave_p_p4_runtime_path_and_forbidden_source_audit_are_nonvacuous() {
        fn segment<'a>(source: &'a str, start_marker: &str, end_marker: &str) -> &'a str {
            let start = source.find(start_marker).expect("P4 source start marker");
            let end = source[start..]
                .find(end_marker)
                .map(|offset| start + offset)
                .expect("P4 source end marker");
            &source[start..end]
        }

        let source = include_str!("query.rs");
        let p4_source = segment(
            source,
            "fn de_audited_compound_fallback",
            "fn de_comma_postcode_house_rescue",
        );
        for forbidden in [
            "streets_fst.get",
            ".lat",
            ".lon",
            "dist_km(",
            ".distance_m",
            ".score",
            ".confidence",
            "take(1)",
            "candidate_sids.first",
            "candidate_sids[0]",
            "roster_id",
            "ordinal",
            "physical_id",
            "truth_coordinate",
            "result_coordinate",
            "benchmark",
            "outcome",
            "competitor",
            "distance_threshold",
            "de_sequence_matcher",
        ] {
            assert!(
                !p4_source.contains(forbidden),
                "P4 admission must not read forbidden selector {forbidden}"
            );
        }
        for required in [
            "de_postcode_street_bucket(spec.postcode)?",
            "de_product_normalize_text(self.commune_name(metadata.commune_id))",
            "de_product_normalize_street(self.name(metadata.name_off))",
            "exact_house_full_postcode_set_candidate_cached",
            "DE_P4_POSTCODE_BUCKET_SCAN_ROWS.with",
            "DE_P4_POSTCODE_BUCKET_MATCHING_SIDS.with",
            "candidate_sids.sort_unstable()",
            "candidate_sids.dedup()",
            "let [sid] = candidate_sids.as_slice()",
        ] {
            assert!(
                p4_source.contains(required),
                "P4 source audit must observe guard {required}"
            );
        }
    }

    #[test]
    fn de_wave_x_x1_promotes_only_the_unique_official_commune_alias_house() {
        let idx = forward_postcode_index_for_country(
            "wave-x-x1-official-alias",
            "rathausplatz,001,ludwigshafen rhein,67061,67061,20,,8.4000000,49.4800000,Rathausplatz,Ludwigshafen Rhein\n\
             rathausplatz,002,ludwigshafen am rhein,67059,67059,20,,8.4400000,49.4900000,Rathausplatz,Ludwigshafen am Rhein\n\
             rathausplatz,003,ludwigshafen am rhein,67058,67058,21,,8.4500000,49.5000000,Rathausplatz,Ludwigshafen am Rhein\n",
            "de",
        );
        let top = idx
            .query("Rathausplatz 20, 67061 Ludwigshafen/Rhein", 1)
            .into_iter()
            .next()
            .expect("the bounded official alias must retain one exact house");
        assert_eq!(top.commune, "Ludwigshafen am Rhein");
        assert_eq!(top.street, "Rathausplatz");
        assert_eq!(top.housenumber.as_deref(), Some("20"));
        assert_eq!(top.postcode, "67059");
        assert!(top.flags.contains(&"street_exact"));
        assert!(top.flags.contains(&"house_rep"));
        assert!(top.flags.contains(&"pc_dept"));
        assert!(top.flags.contains(&"de_official_commune_alias"));
        let expanded = idx.query("Rathausplatz 20, 67061 Ludwigshafen/Rhein", 5);
        assert_eq!(
            expanded.first().map(|hit| hit.postcode.as_str()),
            Some("67059")
        );
        assert!(
            expanded
                .iter()
                .skip(1)
                .any(|hit| hit.housenumber.as_deref() == Some("21")),
            "promotion must preserve legitimate lower hard-commune alternatives"
        );

        let duplicate = forward_postcode_index_for_country(
            "wave-x-x1-official-alias-duplicate",
            "rathausplatz,001,ludwigshafen rhein,67061,67061,20,,8.4000000,49.4800000,Rathausplatz,Ludwigshafen Rhein\n\
             rathausplatz,002,ludwigshafen am rhein,67059,67059,20,,8.4400000,49.4900000,Rathausplatz,Ludwigshafen am Rhein\n\
             rathausplatz,003,ludwigshafen am rhein,67058,67058,20,,8.4500000,49.5000000,Rathausplatz,Ludwigshafen am Rhein\n",
            "de",
        );
        let duplicate_top = duplicate
            .query("Rathausplatz 20, 67061 Ludwigshafen/Rhein", 1)
            .into_iter()
            .next()
            .expect("the established exact-postcode result remains available");
        assert_eq!(duplicate_top.postcode, "67061");
        assert!(!duplicate_top.flags.contains(&"de_official_commune_alias"));
    }

    #[test]
    fn de_wave_regression_official_alias_treats_only_an_empty_postcode_as_unknown() {
        let candidate = |postcode: &str, pc_dept: bool| {
            let (hit, features, ..) = retained_test_hit(
                "Ludwigshafen am Rhein",
                "Rathausplatz",
                Some("20"),
                postcode,
                "house",
                1.0,
                Feats {
                    street_exact: true,
                    pc_dept,
                    house_exact_rep: true,
                    ..Default::default()
                }
                .to_vec(),
                vec!["street_exact", "house_rep"],
                100,
                0,
            );
            vec![(hit, features)]
        };
        let query = "Rathausplatz 20, 67061 Ludwigshafen/Rhein";

        assert_eq!(
            Index::de_ludwigshafen_official_alias_candidate_position(query, &candidate("", false)),
            Some(0),
            "an empty row postcode is missing evidence, not contradictory evidence"
        );
        assert_eq!(
            Index::de_ludwigshafen_official_alias_candidate_position(
                query,
                &candidate("67059", true)
            ),
            Some(0),
            "the existing same-department witness remains admissible"
        );
        assert!(
            Index::de_ludwigshafen_official_alias_candidate_position(
                query,
                &candidate("99999", false)
            )
            .is_none(),
            "a non-empty conflicting postcode must remain fail-closed"
        );
    }

    #[test]
    fn de_wave_x_x2_promotes_only_one_exact_street_locality_qualifier() {
        let idx = forward_postcode_index_for_country(
            "wave-x-x2-street-locality-qualifier",
            "markt,001,quedlinburg,06485,06485,1,,11.1000000,51.7900000,Markt,Quedlinburg\n\
             markt quedlinburg,002,quedlinburg welterbestadt,,,1,,11.1400000,51.8000000,Markt (Quedlinburg),\"Quedlinburg, Welterbestadt\"\n",
            "de",
        );
        let top = idx
            .query("Markt 1, 06484 Quedlinburg", 1)
            .into_iter()
            .next()
            .expect("the unique qualified source street must be reachable");
        assert_eq!(top.street, "Markt (Quedlinburg)");
        assert_eq!(top.commune, "Quedlinburg, Welterbestadt");
        assert_eq!(top.housenumber.as_deref(), Some("1"));
        assert_eq!(top.postcode, "");
        assert!(top.flags.contains(&"street_exact"));
        assert!(top.flags.contains(&"house_rep"));
        assert!(top.flags.contains(&"de_street_locality_qualifier"));

        let duplicate = forward_postcode_index_for_country(
            "wave-x-x2-street-locality-qualifier-duplicate",
            "markt,001,quedlinburg,06485,06485,1,,11.1000000,51.7900000,Markt,Quedlinburg\n\
             markt quedlinburg,002,quedlinburg welterbestadt,,,1,,11.1400000,51.8000000,Markt (Quedlinburg),\"Quedlinburg, Welterbestadt\"\n\
             markt quedlinburg,003,quedlinburg welterbestadt nord,,,1,,11.1500000,51.8100000,Markt (Quedlinburg),\"Quedlinburg, Welterbestadt Nord\"\n",
            "de",
        );
        let duplicate_top = duplicate
            .query("Markt 1, 06484 Quedlinburg", 1)
            .into_iter()
            .next()
            .expect("the established department-level result remains available");
        assert_eq!(duplicate_top.street, "Markt");
        assert_eq!(duplicate_top.postcode, "06485");
        assert!(!duplicate_top
            .flags
            .contains(&"de_street_locality_qualifier"));
    }

    #[test]
    fn de_wave_x_x1_rejects_an_index_key_display_street_mismatch() {
        let idx = forward_postcode_index_for_country(
            "wave-x-x1-display-mismatch",
            "rathausplatz,001,ludwigshafen rhein,67061,67061,20,,8.4000000,49.4800000,Rathausplatz,Ludwigshafen Rhein\n\
             rathausplatz,002,ludwigshafen am rhein,67059,67059,20,,8.4400000,49.4900000,Bürgerplatz,Ludwigshafen am Rhein\n",
            "de",
        );
        let top = idx
            .query("Rathausplatz 20, 67061 Ludwigshafen/Rhein", 1)
            .into_iter()
            .next()
            .expect("the established exact-postcode result remains available");
        assert_eq!(top.street, "Rathausplatz");
        assert_eq!(top.postcode, "67061");
        assert!(!top.flags.contains(&"de_official_commune_alias"));
    }

    #[test]
    fn de_wave_x_x2_parsers_accept_only_the_typed_product_surfaces() {
        let spec = de_street_locality_qualifier_spec("Markt 12A, 06484 Quedlinburg")
            .expect("a one-letter exact house suffix is part of the typed surface");
        assert_eq!(spec.normalized_street, "markt");
        assert_eq!(spec.normalized_locality, "quedlinburg");
        assert_eq!(spec.house_token, "12a");
        assert_eq!(spec.postcode_raw, "06484");

        for raw in [
            "Markt 1-3, 06484 Quedlinburg",
            "Markt 1/3, 06484 Quedlinburg",
            "Markt 01, 06484 Quedlinburg",
            "Markt 1, Quedlinburg",
            "Markt 1, 06484 Quedlinburg, Sachsen-Anhalt",
            "Quedlinburg, Markt 1, 06484",
        ] {
            assert!(
                de_street_locality_qualifier_spec(raw).is_none(),
                "{raw} must stay outside the X2 parser"
            );
        }

        assert_eq!(
            de_source_street_locality_qualifier("Markt (Quedlinburg)"),
            Some(("markt".to_string(), "quedlinburg".to_string()))
        );
        for display in [
            "Markt",
            "Markt ()",
            "Markt ((Quedlinburg))",
            "Markt (Quedlinburg) Zufahrt",
            "Markt (Quedlinburg) ",
        ] {
            assert!(
                de_source_street_locality_qualifier(display).is_none(),
                "{display} must not become a source qualifier"
            );
        }
    }

    #[test]
    fn de_wave_x_x2_product_predicate_rejects_an_exact_postcode_top_and_suffix_mismatch() {
        let make_hit = |street: &str, commune: &str, postcode: &str, house: &str| Hit {
            lat: 0.0,
            lon: 0.0,
            precision: "house",
            score: 0.0,
            confidence: 0.0,
            street: street.to_string(),
            housenumber: Some(house.to_string()),
            commune: commune.to_string(),
            postcode: postcode.to_string(),
            flags: Vec::new(),
            region: None,
            distance_m: None,
        };
        let top_features = Feats {
            street_exact: true,
            pc_exact: true,
            pc_dept: true,
            house_exact_rep: true,
            ..Default::default()
        }
        .to_vec();
        let candidate_features = Feats {
            street_exact: true,
            house_exact_rep: true,
            ..Default::default()
        }
        .to_vec();
        let mut current = vec![
            (make_hit("Markt", "Quedlinburg", "06484", "1"), top_features),
            (
                make_hit("Markt (Quedlinburg)", "Quedlinburg, Welterbestadt", "", "1"),
                candidate_features,
            ),
        ];
        assert!(
            Index::de_street_locality_qualifier_position("Markt 1, 06484 Quedlinburg", &current,)
                .is_none(),
            "an exact-postcode top is already final"
        );

        current[0].1[4] = 0.0;
        current[0].0.postcode = "06485".to_string();
        current[1].0.housenumber = Some("1a".to_string());
        assert!(
            Index::de_street_locality_qualifier_position("Markt 1, 06484 Quedlinburg", &current,)
                .is_none(),
            "the candidate house suffix must equal the typed suffix"
        );
        current[1].0.housenumber = Some("1".to_string());
        assert_eq!(
            Index::de_street_locality_qualifier_position("Markt 1, 06484 Quedlinburg", &current,),
            Some(1),
            "the same product-only window becomes admissible once both guards hold"
        );

        for street in ["Markt Nord", "Markt Süd", "Markt Ost"] {
            current.push((make_hit(street, "Quedlinburg", "", "1"), candidate_features));
        }
        current.push((
            make_hit(
                "Markt (Quedlinburg)",
                "Quedlinburg, Welterbestadt Süd",
                "",
                "1",
            ),
            candidate_features,
        ));
        assert_eq!(
            Index::de_street_locality_qualifier_position("Markt 1, 06484 Quedlinburg", &current,),
            Some(1),
            "an eligible rank six is outside the fixed audited top-five window"
        );
        let rank_six = current.pop().expect("the rank-six witness exists");
        current[4] = rank_six;
        assert!(
            Index::de_street_locality_qualifier_position("Markt 1, 06484 Quedlinburg", &current,)
                .is_none(),
            "the same duplicate at rank five must fail closed"
        );
    }

    #[test]
    fn de_wave_x_x2_preserves_exact_postcodes_and_rejects_nonempty_or_wrong_qualifiers() {
        let canonical = forward_postcode_index_for_country(
            "wave-x-x2-top-and-bare-guards",
            "markt,001,quedlinburg,06485,06485,1,,11.1000000,51.7900000,Markt,Quedlinburg\n\
             markt quedlinburg,002,quedlinburg welterbestadt,,,1,,11.1400000,51.8000000,Markt (Quedlinburg),\"Quedlinburg, Welterbestadt\"\n",
            "de",
        );
        for query in ["Markt 1, 06485 Quedlinburg", "Markt 1"] {
            let top = canonical
                .query(query, 1)
                .into_iter()
                .next()
                .expect("the established Markt result remains available");
            assert_eq!(top.street, "Markt", "{query}");
            assert!(!top.flags.contains(&"de_street_locality_qualifier"));
        }

        let populated = forward_postcode_index_for_country(
            "wave-x-x2-populated-candidate-postcode",
            "markt,001,quedlinburg,06485,06485,1,,11.1000000,51.7900000,Markt,Quedlinburg\n\
             markt quedlinburg,002,quedlinburg welterbestadt,06486,06486,1,,11.1400000,51.8000000,Markt (Quedlinburg),\"Quedlinburg, Welterbestadt\"\n",
            "de",
        );
        let populated_top = populated
            .query("Markt 1, 06484 Quedlinburg", 1)
            .into_iter()
            .next()
            .expect("the established department-level result remains available");
        assert_eq!(populated_top.street, "Markt");
        assert!(!populated_top
            .flags
            .contains(&"de_street_locality_qualifier"));

        let wrong = forward_postcode_index_for_country(
            "wave-x-x2-wrong-source-qualifier",
            "markt,001,quedlinburg,06485,06485,1,,11.1000000,51.7900000,Markt,Quedlinburg\n\
             markt quedlinburg,002,quedlinburg welterbestadt,,,1,,11.1400000,51.8000000,Markt (Gernrode),\"Quedlinburg, Welterbestadt\"\n",
            "de",
        );
        let wrong_top = wrong
            .query("Markt 1, 06484 Quedlinburg", 1)
            .into_iter()
            .next()
            .expect("the established department-level result remains available");
        assert_eq!(wrong_top.street, "Markt");
        assert!(!wrong_top.flags.contains(&"de_street_locality_qualifier"));

        let wrong_commune = forward_postcode_index_for_country(
            "wave-x-x2-wrong-source-commune",
            "markt,001,quedlinburg,06485,06485,1,,11.1000000,51.7900000,Markt,Quedlinburg\n\
             markt quedlinburg,002,gernrode,,,1,,11.1400000,51.8000000,Markt (Quedlinburg),Gernrode\n",
            "de",
        );
        let wrong_commune_top = wrong_commune
            .query("Markt 1, 06484 Quedlinburg", 1)
            .into_iter()
            .next()
            .expect("the established department-level result remains available");
        assert_eq!(wrong_commune_top.street, "Markt");
        assert!(!wrong_commune_top
            .flags
            .contains(&"de_street_locality_qualifier"));

        let foreign = forward_postcode_index_for_country(
            "wave-x-x2-foreign-country",
            "markt,001,quedlinburg,06485,06485,1,,11.1000000,51.7900000,Markt,Quedlinburg\n\
             markt quedlinburg,002,quedlinburg welterbestadt,,,1,,11.1400000,51.8000000,Markt (Quedlinburg),\"Quedlinburg, Welterbestadt\"\n",
            "fr",
        );
        let foreign_top = foreign
            .query("Markt 1, 06484 Quedlinburg", 1)
            .into_iter()
            .next()
            .expect("the foreign sheet's established result remains available");
        assert_eq!(foreign_top.street, "Markt");
        assert!(!foreign_top.flags.contains(&"de_street_locality_qualifier"));
    }

    #[test]
    fn de_wave_b_p4_parser_accepts_only_the_four_audited_shapes() {
        let positives = [
            (
                "Blockhaus, 19, Große Meißner Straße, Innere Neustadt, Neustadt, Dresden, Sachsen, 01097",
                DeAuditedCompoundShape::VenueCommaHouseCommaStreet,
                vec![],
                "grosse meissner strasse",
                "dresden",
            ),
            (
                "Albertstr. 25 ( Otto-Krayer-Haus), 79104 Freiburg/Breisgau",
                DeAuditedCompoundShape::StreetHouseBalancedVenueParenthetical,
                vec![],
                "albertstrasse",
                "freiburg im breisgau",
            ),
            (
                "c/o Jacobs University Bremen Campusring 1 Bremen, 28759 Bremen",
                DeAuditedCompoundShape::CareOfPrefixThenStreetHouseLocality,
                vec![],
                "campus ring",
                "bremen",
            ),
            (
                "Rheinstr. 45/46 (Aufgang 6), 12161 Berlin",
                DeAuditedCompoundShape::StreetHouseRangeBalancedAccessParenthetical,
                vec![46],
                "rheinstrasse",
                "berlin",
            ),
            (
                "Marktplatz 4 (Weilheimer \"Bürgerhaus\"), 73235 Weilheim/Teck",
                DeAuditedCompoundShape::StreetHouseBalancedVenueParenthetical,
                vec![],
                "marktplatz",
                "weilheim an der teck",
            ),
        ];
        for (raw, shape, additional_houses, normalized_street, normalized_locality) in positives {
            let spec = de_audited_compound_spec(raw)
                .unwrap_or_else(|| panic!("audited P4 surface must parse: {raw}"));
            assert_eq!(spec.shape, shape, "{raw}");
            assert_eq!(spec.additional_houses, additional_houses, "{raw}");
            assert_eq!(spec.normalized_street, normalized_street, "{raw}");
            assert_eq!(spec.normalized_locality, normalized_locality, "{raw}");
            assert_eq!(spec.postcode_raw.len(), 5, "{raw}");
        }

        for raw in [
            "Albertstr. 25 ((Haus)), 79104 Freiburg/Breisgau",
            "Albertstr. 25(Haus), 79104 Freiburg/Breisgau",
            "Albertstr. 025 (Haus), 79104 Freiburg/Breisgau",
            "Albertstr. 25 (Haus),79104 Freiburg/Breisgau",
            "Albertstr. 25 (Haus, 79104 Freiburg/Breisgau",
            "Rheinstr. 45/46 (Haus 6), 12161 Berlin",
            "Rheinstr. 45/46/47 (Aufgang 6), 12161 Berlin",
            "Blockhaus, Große Meißner Straße, 19, Dresden, 01097",
            "Blockhaus, 19a, Große Meißner Straße, Dresden, 01097",
            "Blockhaus, 19, Große Meißner Straße, Innere Neustadt, 7, Dresden, Sachsen, 01097",
            "c/o Campusring 1 Bremen, 28759 Bremen",
            "c/oops Jacobs Campusring 1 Bremen, 28759 Bremen",
            "c/o Jacobs Campusring 1 Hamburg, 28759 Bremen",
            "c/o Jacobs Campusring 1 Bremen, 28759 Bremen, Deutschland",
            "Marktplatz 4 (Haus), 7323 Weilheim/Teck",
            "Marktplatz 4 (Haus), 73235 Weilheim/Teck/Bayern",
        ] {
            assert!(
                de_audited_compound_spec(raw).is_none(),
                "typed P4 parser must reject {raw:?}"
            );
        }
    }

    #[test]
    fn de_wave_o_blank_postcode_exact_house_replaces_typed_postcode_near() {
        let idx = forward_postcode_index_for_country(
            "wave-o-blank-house-near",
            "ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n\
             source-key-differs,001,probingen,,,12,,10.0010000,50.0010000,Ankerstraße,Probingen\n",
            "de",
        );

        let top = idx
            .query("Ankerstraße 12, 12345 Probingen", 1)
            .into_iter()
            .next()
            .expect("the established typed-postcode near result must exist");
        assert_eq!(top.precision, "house");
        assert_eq!(top.street, "Ankerstraße");
        assert_eq!(top.housenumber.as_deref(), Some("12"));
        assert_eq!(top.commune, "Probingen");
        assert_eq!(top.postcode, "");
        assert!(top.flags.contains(&"de_blank_postcode_house_override"));
        assert!(!top.flags.contains(&"pc_exact"));
        assert!(!top.flags.contains(&"pc_dept"));
    }

    #[test]
    fn de_wave_o_blank_postcode_exact_house_replaces_typed_postcode_interp() {
        let idx = forward_postcode_index_for_country(
            "wave-o-blank-house-interp",
            "bogenstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Bogenstraße,Probingen\n\
             bogenstrasse,001,probingen,12345,12345,14,,10.0010000,50.0010000,Bogenstraße,Probingen\n\
             legacy-bogen-key,001,probingen,,,12,,10.0005000,50.0005000,Bogenstraße,Probingen\n",
            "de",
        );

        let top = idx
            .query("Bogenstraße 12, 12345 Probingen", 1)
            .into_iter()
            .next()
            .expect("the established typed-postcode interpolation must exist");
        assert_eq!(top.precision, "house");
        assert_eq!(top.housenumber.as_deref(), Some("12"));
        assert_eq!(top.postcode, "");
        assert!(top.flags.contains(&"de_blank_postcode_house_override"));
    }

    #[test]
    fn de_wave_o_blank_postcode_exact_suffix_is_preserved() {
        let idx = forward_postcode_index_for_country(
            "wave-o-blank-house-suffix",
            "legacy-ufer-key,001,probingen,,,12,a,10.0005000,50.0005000,Uferweg,Probingen\n\
             uferweg,001,probingen,12345,12345,10,a,10.0000000,50.0000000,Uferweg,Probingen\n\
             uferweg,001,probingen,12345,12345,14,a,10.0010000,50.0010000,Uferweg,Probingen\n",
            "de",
        );

        let top = idx
            .query("Uferweg 12a, 12345 Probingen", 1)
            .into_iter()
            .next()
            .expect("the established typed-postcode interpolation must exist");
        assert_eq!(top.precision, "house");
        assert_eq!(top.housenumber.as_deref(), Some("12a"));
        assert_eq!(top.postcode, "");
        assert!(top.flags.contains(&"de_blank_postcode_house_override"));
    }

    fn de_wave_o_base_index(case: &str, country: &str) -> Index {
        forward_postcode_index_for_country(
            case,
            "a-legacy-source-key,001,probingen,,,12,,10.0005000,50.0005000,Ankerstraße,Probingen\n\
             ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n\
             ankerstrasse,001,probingen,12345,12345,14,,10.0010000,50.0010000,Ankerstraße,Probingen\n",
            country,
        )
    }

    fn de_wave_o_current(
        precision: &'static str,
        street: &str,
        commune: &str,
        postcode: &str,
        features: Feats,
        flags: Vec<&'static str>,
    ) -> Vec<(Hit, [f32; N_FEATS])> {
        vec![(
            Hit {
                lat: 0.0,
                lon: 0.0,
                precision,
                score: 0.0,
                confidence: 0.0,
                street: street.to_string(),
                housenumber: Some("12".to_string()),
                commune: commune.to_string(),
                postcode: postcode.to_string(),
                flags,
                region: None,
                distance_m: None,
            },
            features.to_vec(),
        )]
    }

    fn de_wave_o_strict_current(precision: &'static str) -> Vec<(Hit, [f32; N_FEATS])> {
        de_wave_o_current(
            precision,
            "Ankerstraße",
            "Probingen",
            "12345",
            Feats {
                street_exact: true,
                commune_exact: true,
                pc_exact: true,
                pc_dept: true,
                ..Default::default()
            },
            vec!["street_exact", "commune_exact", "pc_exact"],
        )
    }

    #[test]
    fn de_wave_o_parser_is_single_literal_and_two_field_only() {
        for raw in [
            "Ankerstraße 12, 12345 Probingen",
            "Ankerstraße 12a, 12345 Probingen",
        ] {
            assert!(
                de_blank_postcode_house_spec(raw).is_some(),
                "strict P5 surface must parse: {raw}"
            );
        }
        for raw in [
            "Ankerstraße 12/14, 12345 Probingen",
            "Ankerstraße 12-14, 12345 Probingen",
            "Ankerstraße 12 bis 14, 12345 Probingen",
            "Ankerstraße 12 und 14, 12345 Probingen",
            "Ankerstraße 12 (Haus), 12345 Probingen",
            "c/o Ankerstraße 12, 12345 Probingen",
            "Ankerstraße 12, 12345 Probingen, Deutschland",
            "Ankerstraße 12, 12345",
            "Ankerstraße 12, 1234 Probingen",
            "Straße 7 12, 12345 Probingen",
            " Ankerstraße 12, 12345 Probingen",
        ] {
            assert!(
                de_blank_postcode_house_spec(raw).is_none(),
                "P5 must reject noncanonical or compound surface: {raw}"
            );
        }
    }

    #[test]
    fn de_wave_o_current_top_requires_exact_original_fields() {
        let idx = de_wave_o_base_index("wave-o-current-gates", "de");
        let raw = "Ankerstraße 12, 12345 Probingen";
        assert!(
            idx.de_blank_postcode_exact_house_fallback(
                raw,
                &de_wave_o_strict_current("near"),
                1,
                None,
            )
            .is_some()
        );

        for (case, current) in [
            ("house", de_wave_o_strict_current("house")),
            ("street", de_wave_o_strict_current("street")),
            (
                "wrong-postcode",
                de_wave_o_current(
                    "near",
                    "Ankerstraße",
                    "Probingen",
                    "12346",
                    Feats {
                        street_exact: true,
                        commune_exact: true,
                        pc_exact: true,
                        pc_dept: true,
                        ..Default::default()
                    },
                    vec![],
                ),
            ),
            (
                "pc-dept-only",
                de_wave_o_current(
                    "near",
                    "Ankerstraße",
                    "Probingen",
                    "12345",
                    Feats {
                        street_exact: true,
                        commune_exact: true,
                        pc_dept: true,
                        ..Default::default()
                    },
                    vec![],
                ),
            ),
            (
                "incumbent-exact-house",
                de_wave_o_current(
                    "near",
                    "Ankerstraße",
                    "Probingen",
                    "12345",
                    Feats {
                        street_exact: true,
                        commune_exact: true,
                        pc_exact: true,
                        pc_dept: true,
                        house_exact_rep: true,
                        ..Default::default()
                    },
                    vec![],
                ),
            ),
            (
                "fuzzy-street",
                de_wave_o_current(
                    "near",
                    "Ankerstraße",
                    "Probingen",
                    "12345",
                    Feats {
                        street_fuzzy: true,
                        commune_exact: true,
                        pc_exact: true,
                        pc_dept: true,
                        ..Default::default()
                    },
                    vec![],
                ),
            ),
            (
                "mismatched-hit-street",
                de_wave_o_current(
                    "near",
                    "Seitenstraße",
                    "Probingen",
                    "12345",
                    Feats {
                        street_exact: true,
                        commune_exact: true,
                        pc_exact: true,
                        pc_dept: true,
                        ..Default::default()
                    },
                    vec![],
                ),
            ),
            (
                "mismatched-hit-locality",
                de_wave_o_current(
                    "near",
                    "Ankerstraße",
                    "Nebenstadt",
                    "12345",
                    Feats {
                        street_exact: true,
                        commune_exact: true,
                        pc_exact: true,
                        pc_dept: true,
                        ..Default::default()
                    },
                    vec![],
                ),
            ),
            (
                "locality-prefix",
                de_wave_o_current(
                    "near",
                    "Ankerstraße",
                    "Probingen",
                    "12345",
                    Feats {
                        street_exact: true,
                        commune_prefix: true,
                        pc_exact: true,
                        pc_dept: true,
                        ..Default::default()
                    },
                    vec![],
                ),
            ),
            (
                "alias-derived",
                de_wave_o_current(
                    "near",
                    "Ankerstraße",
                    "Probingen",
                    "12345",
                    Feats {
                        street_exact: true,
                        commune_exact: true,
                        pc_exact: true,
                        pc_dept: true,
                        ..Default::default()
                    },
                    vec!["de_city_alias"],
                ),
            ),
            (
                "official-alias-derived",
                de_wave_o_current(
                    "near",
                    "Ankerstraße",
                    "Probingen",
                    "12345",
                    Feats {
                        street_exact: true,
                        commune_exact: true,
                        pc_exact: true,
                        pc_dept: true,
                        ..Default::default()
                    },
                    vec!["de_official_commune_alias"],
                ),
            ),
            (
                "dropped-prefix",
                de_wave_o_current(
                    "near",
                    "Ankerstraße",
                    "Probingen",
                    "12345",
                    Feats {
                        street_exact: true,
                        commune_exact: true,
                        pc_exact: true,
                        pc_dept: true,
                        ..Default::default()
                    },
                    vec!["dropped_prefix"],
                ),
            ),
            (
                "dropped-suffix",
                de_wave_o_current(
                    "near",
                    "Ankerstraße",
                    "Probingen",
                    "12345",
                    Feats {
                        street_exact: true,
                        commune_exact: true,
                        pc_exact: true,
                        pc_dept: true,
                        ..Default::default()
                    },
                    vec!["dropped_suffix"],
                ),
            ),
        ] {
            assert!(
                idx.de_blank_postcode_exact_house_fallback(raw, &current, 1, None)
                    .is_none(),
                "strict current-top gate must reject {case}"
            );
        }
    }

    #[test]
    fn de_wave_o_current_selector_values_do_not_affect_admission() {
        let idx = de_wave_o_base_index("wave-o-current-selector-invariance", "de");
        let mut baseline = de_wave_o_strict_current("near");
        let mut altered = de_wave_o_strict_current("near");
        baseline[0].0.lat = 0.0;
        baseline[0].0.lon = 0.0;
        baseline[0].0.score = 0.0;
        baseline[0].0.confidence = 0.0;
        baseline[0].0.distance_m = None;
        altered[0].0.lat = 89.0;
        altered[0].0.lon = -179.0;
        altered[0].0.score = -10_000.0;
        altered[0].0.confidence = 1.0;
        altered[0].0.distance_m = Some(9_999_999.0);

        let summarize = |candidate: Vec<(Hit, [f32; N_FEATS])>| {
            candidate
                .into_iter()
                .map(|(hit, features)| {
                    (
                        hit.precision,
                        hit.street,
                        hit.housenumber,
                        hit.commune,
                        hit.postcode,
                        hit.flags,
                        features,
                    )
                })
                .collect::<Vec<_>>()
        };
        let first = idx
            .de_blank_postcode_exact_house_fallback(
                "Ankerstraße 12, 12345 Probingen",
                &baseline,
                1,
                None,
            )
            .expect("baseline selector values must admit the strict P5 candidate");
        let second = idx
            .de_blank_postcode_exact_house_fallback(
                "Ankerstraße 12, 12345 Probingen",
                &altered,
                1,
                None,
            )
            .expect("changed selector-only values must preserve P5 admission");
        assert_eq!(summarize(first), summarize(second));
    }

    #[test]
    fn de_wave_o_p3_p5_overlap_preserves_the_established_answer() {
        let idx = forward_postcode_index_for_country(
            "wave-o-p3-p5-overlap",
            "a-blank-exact-display,001,probingen,,,12,,10.0005000,50.0005000,Ankerstrase,Probingen\n\
             ankerstrase,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstrase,Probingen\n\
             ankerstrasse,001,probingen,12345,12345,12,,10.0007000,50.0007000,Ankerstraße,Probingen\n",
            "de",
        );
        let raw = "Ankerstrase 12, 12345 Probingen";
        let mut established = de_wave_o_current(
            "near",
            "Ankerstrase",
            "Probingen",
            "12345",
            Feats {
                street_exact: true,
                commune_exact: true,
                pc_exact: true,
                pc_dept: true,
                ..Default::default()
            },
            vec!["street_exact", "commune_exact", "pc_exact"],
        );
        let p3 = idx
            .de_strict_source_street_typo_fallback(raw, &established, 1)
            .expect("the independent exact-postcode typo mechanism must be nonempty");
        let p5 = idx
            .de_blank_postcode_exact_house_fallback(raw, &established, 1, None)
            .expect("the independent blank-postcode exact-house mechanism must be nonempty");
        assert!(p3[0].0.flags.contains(&"de_strict_source_street_typo"));
        assert!(p5[0].0.flags.contains(&"de_blank_postcode_house_override"));

        Index::de_product_fallback_arbitration(&mut established, Some(p3), None, None, Some(p5));
        assert_eq!(established.len(), 1);
        assert_eq!(established[0].0.precision, "near");
        assert_eq!(established[0].0.street, "Ankerstrase");
        assert_eq!(established[0].0.postcode, "12345");
        assert!(!established[0]
            .0
            .flags
            .contains(&"de_strict_source_street_typo"));
        assert!(!established[0]
            .0
            .flags
            .contains(&"de_blank_postcode_house_override"));
    }

    #[test]
    fn de_wave_o_distinct_source_sids_fail_closed_independent_of_coordinates() {
        for (case, second_coordinates) in [
            ("same-coordinates", "10.0005000,50.0005000"),
            ("different-coordinates", "10.0007000,50.0007000"),
        ] {
            let rows = format!(
                "a-blank-key,001,probingen,,,12,,10.0005000,50.0005000,Ankerstraße,Probingen\n\
                 ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n\
                 b-blank-key,001,probingen,,,12,,{second_coordinates},Ankerstraße,Probingen\n"
            );
            let idx = forward_postcode_index_for_country(case, &rows, "de");
            let top = idx
                .query("Ankerstraße 12, 12345 Probingen", 1)
                .into_iter()
                .next()
                .expect("the established near result must remain");
            assert_eq!(top.precision, "near", "{case}");
            assert_eq!(top.postcode, "12345", "{case}");
            assert!(
                !top.flags.contains(&"de_blank_postcode_house_override"),
                "two source SIDs must fail closed regardless of coordinates: {case}"
            );
        }
    }

    #[test]
    fn de_wave_o_duplicate_rows_inside_one_sid_fail_closed() {
        for (case, second_coordinates) in [
            ("same-coordinates", "10.0005000,50.0005000"),
            ("different-coordinates", "10.0007000,50.0007000"),
        ] {
            let rows = format!(
                "a-blank-key,001,probingen,,,12,,10.0005000,50.0005000,Ankerstraße,Probingen\n\
                 a-blank-key,001,probingen,,,12,,{second_coordinates},Ankerstraße,Probingen\n\
                 ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n"
            );
            let idx = forward_postcode_index_for_country(case, &rows, "de");
            let candidate = idx.de_blank_postcode_exact_house_fallback(
                "Ankerstraße 12, 12345 Probingen",
                &de_wave_o_strict_current("near"),
                1,
                None,
            );
            assert!(
                candidate.is_none(),
                "two represented rows inside one SID must fail closed: {case}"
            );
        }
    }

    #[test]
    fn de_wave_o_any_competing_exact_house_postcode_fails_closed() {
        for (case, competing_postcode) in [("typed-postcode", "12345"), ("wrong-postcode", "54321")]
        {
            let rows = format!(
                "a-blank-key,001,probingen,,,12,,10.0005000,50.0005000,Ankerstraße,Probingen\n\
                 ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n\
                 z-competing-key,001,probingen,{competing_postcode},{competing_postcode},12,,10.0007000,50.0007000,Ankerstraße,Probingen\n"
            );
            let idx = forward_postcode_index_for_country(case, &rows, "de");
            assert!(
                idx.de_blank_postcode_exact_house_fallback(
                    "Ankerstraße 12, 12345 Probingen",
                    &de_wave_o_strict_current("near"),
                    1,
                    None,
                )
                .is_none(),
                "blank candidate plus {case} exact house must fail closed"
            );
        }
    }

    #[test]
    fn de_wave_o_populated_only_exact_target_is_never_relabelled_blank() {
        let idx = forward_postcode_index_for_country(
            "wave-o-populated-only-target",
            "a-blank-projection-seed,001,probingen,,,13,,10.0005000,50.0005000,Ankerstraße,Probingen\n\
             ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n\
             z-populated-target,001,probingen,54321,54321,12,,10.0007000,50.0007000,Ankerstraße,Probingen\n",
            "de",
        );
        assert!(
            idx.de_blank_postcode_exact_house_fallback(
                "Ankerstraße 12, 12345 Probingen",
                &de_wave_o_strict_current("near"),
                1,
                None,
            )
            .is_none(),
            "a blank row at another house may seed the projection but cannot relabel the populated exact target"
        );
    }

    #[test]
    fn de_wave_o_mixed_postcode_street_is_not_a_blank_source_candidate() {
        let idx = forward_postcode_index_for_country(
            "wave-o-mixed-postcode-negative",
            "ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n\
             legacy-mixed-key,001,probingen,,,12,,10.0005000,50.0005000,Ankerstraße,Probingen\n\
             legacy-mixed-key,001,probingen,12345,12345,14,,10.0007000,50.0007000,Ankerstraße,Probingen\n",
            "de",
        );
        assert!(idx
            .de_blank_postcode_exact_house_fallback(
                "Ankerstraße 12, 12345 Probingen",
                &de_wave_o_strict_current("near"),
                1,
                None,
            )
            .is_none());
    }

    #[test]
    fn de_wave_o_candidate_requires_the_exact_anchored_commune_id() {
        for (case, blank_commune_id, blank_commune) in [
            ("same-name-other-id", "002", "probingen"),
            ("prefix-only", "002", "probingen nord"),
        ] {
            let rows = format!(
                "a-blank-key,{blank_commune_id},{blank_commune},,,12,,10.0005000,50.0005000,Ankerstraße,{}\n\
                 ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n",
                if blank_commune == "probingen" {
                    "Probingen"
                } else {
                    "Probingen Nord"
                }
            );
            let idx = forward_postcode_index_for_country(case, &rows, "de");
            assert!(
                idx.de_blank_postcode_exact_house_fallback(
                    "Ankerstraße 12, 12345 Probingen",
                    &de_wave_o_strict_current("near"),
                    1,
                    None,
                )
                .is_none(),
                "P5 must reject candidate locality relation {case}"
            );
        }

        let duplicate_locality_identity = forward_postcode_index_for_country(
            "wave-o-duplicate-locality-identity",
            "a-blank-anchor,001,probingen,,,12,,10.0005000,50.0005000,Ankerstraße,Probingen\n\
             ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n\
             z-blank-homonym,002,probingen,,,12,,10.0007000,50.0007000,Ankerstraße,Probingen\n",
            "de",
        );
        assert!(
            duplicate_locality_identity
                .de_blank_postcode_exact_house_fallback(
                    "Ankerstraße 12, 12345 Probingen",
                    &de_wave_o_strict_current("near"),
                    1,
                    None,
                )
                .is_none(),
            "a second exact physical row behind the same locality display must fail closed"
        );

        let ambiguous_anchor = forward_postcode_index_for_country(
            "wave-o-ambiguous-anchor",
            "a-blank-anchor,001,probingen,,,12,,10.0005000,50.0005000,Ankerstraße,Probingen\n\
             ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n\
             z-second-anchor,002,probingen,12345,12345,20,,10.0007000,50.0007000,Ankerstraße,Probingen\n",
            "de",
        );
        assert!(
            ambiguous_anchor
                .de_blank_postcode_exact_house_fallback(
                    "Ankerstraße 12, 12345 Probingen",
                    &de_wave_o_strict_current("near"),
                    1,
                    None,
                )
                .is_none(),
            "two typed-postcode commune ids behind one display locality must fail closed"
        );

        let unrelated_homonym = forward_postcode_index_for_country(
            "wave-o-unrelated-homonym",
            "a-blank-anchor,001,probingen,,,12,,10.0005000,50.0005000,Ankerstraße,Probingen\n\
             ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n\
             z-unrelated-homonym,002,nebenstadt,,,12,,10.0007000,50.0007000,Ankerstraße,Nebenstadt\n",
            "de",
        );
        assert!(
            unrelated_homonym
                .de_blank_postcode_exact_house_fallback(
                    "Ankerstraße 12, 12345 Probingen",
                    &de_wave_o_strict_current("near"),
                    1,
                    None,
                )
                .is_some(),
            "an unrelated locality homonym must not veto the unique anchored candidate"
        );
    }

    #[test]
    fn de_wave_o_bare_and_suffix_addresses_never_coalesce() {
        for (case, query_house, blank_suffix) in [
            ("query-suffix-source-bare", "12a", ""),
            ("query-bare-source-suffix", "12", "a"),
        ] {
            let rows = format!(
                "a-blank-key,001,probingen,,,12,{blank_suffix},10.0005000,50.0005000,Ankerstraße,Probingen\n\
                 ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n"
            );
            let idx = forward_postcode_index_for_country(case, &rows, "de");
            assert!(
                idx.de_blank_postcode_exact_house_fallback(
                    &format!("Ankerstraße {query_house}, 12345 Probingen"),
                    &de_wave_o_strict_current("near"),
                    1,
                    None,
                )
                .is_none(),
                "exact suffix identity must be preserved: {case}"
            );
        }
    }

    #[test]
    fn de_wave_o_source_order_and_single_sid_coordinates_do_not_change_admission() {
        let mut outcomes = Vec::new();
        for (case, blank_key, blank_coordinates) in [
            ("blank-before", "a-blank-key", "10.0005000,50.0005000"),
            ("blank-after", "z-blank-key", "11.5000000,51.5000000"),
        ] {
            let mut rows = [
                format!(
                    "{blank_key},001,probingen,,,12,,{blank_coordinates},Ankerstraße,Probingen"
                ),
                "ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen"
                    .to_string(),
            ];
            rows.sort();
            let idx =
                forward_postcode_index_for_country(case, &format!("{}\n", rows.join("\n")), "de");
            let top = idx
                .query("Ankerstraße 12, 12345 Probingen", 1)
                .into_iter()
                .next()
                .expect("one exact blank source row must remain admissible");
            outcomes.push((
                top.precision,
                top.street,
                top.housenumber,
                top.commune,
                top.postcode,
                top.flags.contains(&"de_blank_postcode_house_override"),
            ));
        }
        assert_eq!(outcomes[0], outcomes[1]);
        assert!(outcomes[0].5);
    }

    #[test]
    fn de_wave_o_non_de_focus_and_scan_overflow_remain_closed() {
        let mut foreign = de_wave_o_base_index("wave-o-foreign", "fr");
        foreign.de_postcode_streets = foreign.build_de_postcode_streets().unwrap();
        foreign.de_blank_postcode_display_streets =
            foreign.build_de_blank_postcode_display_streets().unwrap();
        assert!(foreign
            .de_blank_postcode_exact_house_fallback(
                "Ankerstraße 12, 12345 Probingen",
                &de_wave_o_strict_current("near"),
                1,
                None,
            )
            .is_none());

        let mut legacy = de_wave_o_base_index("wave-o-pre-v7", "de");
        legacy.format_version = 6;
        assert!(legacy
            .de_blank_postcode_exact_house_fallback(
                "Ankerstraße 12, 12345 Probingen",
                &de_wave_o_strict_current("near"),
                1,
                None,
            )
            .is_none());

        let idx = de_wave_o_base_index("wave-o-focus-overflow", "de");
        let focus = QueryFocus {
            lat: 50.0,
            lon: 10.0,
            streets: Vec::new(),
        };
        assert!(idx
            .de_blank_postcode_exact_house_fallback(
                "Ankerstraße 12, 12345 Probingen",
                &de_wave_o_strict_current("near"),
                1,
                Some(&focus),
            )
            .is_none());

        let mut bucket_overflow = forward_postcode_index_for_country(
            "wave-o-bucket-overflow",
            "a-blank-anchor,001,probingen,,,12,,10.0005000,50.0005000,Ankerstraße,Probingen\n\
             ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n\
             z-unrelated-locality,002,nebenstadt,,,99,,10.0007000,50.0007000,Ankerstraße,Nebenstadt\n",
            "de",
        );
        let mut projected = bucket_overflow
            .de_blank_postcode_display_streets
            .get("ankerstrasse", "probingen")
            .expect("the complete target projection must exist")
            .to_vec();
        assert_eq!(projected.len(), 2);
        let unrelated_sid = (0..bucket_overflow.streets_meta.len() / STREET_META_SIZE)
            .map(|sid| u32::try_from(sid).expect("test street id fits u32"))
            .find(|sid| {
                let metadata = bucket_overflow.street_meta(*sid);
                de_product_normalize_text(bucket_overflow.commune_name(metadata.commune_id))
                    == "nebenstadt"
            })
            .expect("the unrelated-locality observer SID must exist");
        projected.push(unrelated_sid);
        bucket_overflow.de_blank_postcode_display_streets = DeBlankPostcodeDisplayProjection {
            ranges: HashMap::from([(
                ("ankerstrasse".to_string(), "probingen".to_string()),
                (0, projected.len()),
            )]),
            sids: projected.into_boxed_slice(),
        };
        DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT.with(|limit| limit.set(2));
        let overflow = bucket_overflow.de_blank_postcode_exact_house_fallback(
            "Ankerstraße 12, 12345 Probingen",
            &de_wave_o_strict_current("near"),
            1,
            None,
        );
        DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT
            .with(|limit| limit.set(DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT_DEFAULT));
        assert!(
            overflow.is_none(),
            "a display bucket above its independent SID ceiling must fail closed"
        );

        let row_overflow = forward_postcode_index_for_country(
            "wave-o-row-overflow",
            "a-blank-anchor,001,probingen,,,1,,10.0001000,50.0001000,Ankerstraße,Probingen\n\
             a-blank-anchor,001,probingen,,,2,,10.0002000,50.0002000,Ankerstraße,Probingen\n\
             a-blank-anchor,001,probingen,,,12,,10.0005000,50.0005000,Ankerstraße,Probingen\n\
             ankerstrasse,001,probingen,12345,12345,10,,10.0000000,50.0000000,Ankerstraße,Probingen\n",
            "de",
        );
        DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT.with(|limit| limit.set(3));
        let overflow = row_overflow.de_blank_postcode_exact_house_fallback(
            "Ankerstraße 12, 12345 Probingen",
            &de_wave_o_strict_current("near"),
            1,
            None,
        );
        DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT
            .with(|limit| limit.set(DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT_DEFAULT));
        assert!(
            overflow.is_none(),
            "cumulative physical house rows above their independent ceiling must fail closed"
        );
    }

    #[test]
    fn de_wave_o_runtime_path_and_forbidden_field_audit_are_nonvacuous() {
        let idx = de_wave_o_base_index("wave-o-runtime-audit", "de");
        DE_BLANK_POSTCODE_HOUSE_SCAN_ROWS.with(|rows| rows.set(0));
        let candidate = idx
            .de_blank_postcode_exact_house_fallback(
                "Ankerstraße 12, 12345 Probingen",
                &de_wave_o_strict_current("interp"),
                1,
                None,
            )
            .expect("strict P5 runtime path must produce one candidate");
        assert_eq!(candidate.len(), 1);
        assert!(DE_BLANK_POSTCODE_HOUSE_SCAN_ROWS.with(|rows| rows.get()) > 0);

        fn segment<'a>(source: &'a str, start_marker: &str, end_marker: &str) -> &'a str {
            let start = source.find(start_marker).expect("P5 source start marker");
            let end = source[start..]
                .find(end_marker)
                .map(|offset| start + offset)
                .expect("P5 source end marker");
            &source[start..end]
        }

        let source = include_str!("query.rs");
        let p5_source = [
            segment(
                source,
                "fn de_blank_postcode_house_spec",
                "fn de_source_street_locality_qualifier",
            ),
            segment(
                source,
                "struct DeBlankPostcodeDisplayProjection",
                "pub struct Index",
            ),
            segment(
                source,
                "fn build_de_blank_postcode_display_streets",
                "/// Exact-postcode street bucket",
            ),
            segment(
                source,
                "fn de_exact_house_record_postcodes",
                "fn commune_insee",
            ),
            segment(
                source,
                "fn de_blank_postcode_current_top_is_strict",
                "fn de_p3_current_top_already_complete",
            ),
            segment(
                source,
                "let x2 = Self::de_street_locality_qualifier_position",
                "if frankfurt.is_some()",
            ),
        ]
        .join("\n");
        for forbidden in [
            ".lat",
            ".lon",
            "dist_km(",
            ".distance_m",
            ".score",
            ".confidence",
            "take(1)",
            "roster_id",
            "ordinal",
            "physical_id",
            "truth_coordinate",
            "result_coordinate",
            "benchmark",
            "competitor",
            "distance_threshold",
        ] {
            assert!(
                !p5_source.contains(forbidden),
                "P5 admission must not read forbidden selector {forbidden}"
            );
        }
        for required in [
            "metadata.commune_id != anchor_commune_id",
            "candidate_sids.len() > scan_limit",
            "exact_records.as_slice()",
            "postcode.is_empty()",
            "made_features.house_exact_rep",
            "HashMap<(String, String), (usize, usize)>",
            "anchored_identities.contains(&identity)",
            "drop(anchored_identities)",
            "bucket.len() <= DE_POSTCODE_HOUSE_RESCUE_SCAN_LIMIT_DEFAULT",
            ".get(&spec.normalized_street, &spec.normalized_locality)",
            "match (p3, p4, x2, p5)",
            "(None, None, None, Some(candidate))",
            "Self::de_product_fallback_arbitration(&mut best, p3, p4, x2, p5)",
        ] {
            assert!(
                p5_source.contains(required),
                "P5 source audit must observe guard {required}"
            );
        }
    }
}
