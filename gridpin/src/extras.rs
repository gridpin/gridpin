//! Optional answer fields, asked for with `--with` (CLI), `with=` (server, Python) and kept out of `Hit`.
//!
//! Without the flag an answer is byte for byte what it was: the fields are computed at output and land in one
//! object `extras` (JSON) or in `gp_<name>` columns after the usual ones (CSV). Every surface passes the same
//! spec string to `Extras::new`, so a field added here reaches all of them at once.
//!
//! Fields: `h3[:res]` and `geohash[:len]` (cells), `placekey` (the "where" part of a Placekey, which is H3 at
//! resolution 10 in Placekey's own alphabet), `codes` (Plus Code, MGRS, degrees-minutes-seconds), `street_point`
//! (a point on the street in front of the house, from the houses across it), `links` (the point on five public
//! maps) and `provenance` (the sheet: release, data date, licence, sources, SHA-256). Time zones are not here:
//! the zone boundaries every library ships are ODbL data, and putting them in the engine is the owner's call.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

use crate::codes;
use crate::query::{Hit, Index};

/// What a spec asks for. `Default` asks for nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct With {
    pub h3: Option<u8>,
    pub geohash: Option<usize>,
    pub placekey: bool,
    pub codes: bool,
    pub street_point: bool,
    pub links: bool,
    pub provenance: bool,
    /// `place[:level]`: the answer as a person writes it, at house, street, locality or region level
    pub place: Option<&'static str>,
    /// what happened to each field of the query (`verdict`); needs the query (`for_query`)
    pub verdict: bool,
    /// `postal[:cc]`: the answer laid out as the post of country `cc` writes an address (`postal_formats.tsv`);
    /// without `cc` the default layout. The layout is named by the caller: the engine does not branch on the
    /// sheet's country (ANSWERS §60).
    pub postal: Option<&'static str>,
    /// `crs:EPSG:<code>`: easting and northing in a national projection (`crs.rs`)
    pub crs: Option<u32>,
    /// does the asked address exist: yes, maybe, no and the reason (`exists.rs`); needs the query (`for_query`)
    pub exists: bool,
    /// where the address may be: the candidates of the answer's level with weights, their box and spread
    pub area: bool,
    /// the flat, office or floor the query names ("apt 4B", "Suite 300", "2º B"), from `unit_words.tsv`
    pub unit: bool,
    /// the ISO 20022 postal address (`PstlAdr`): structured, hybrid or none with the reasons (`iso20022.rs`)
    pub iso20022: bool,
}

pub const NAMES: [&str; 15] = [
    "h3",
    "geohash",
    "placekey",
    "codes",
    "street_point",
    "links",
    "provenance",
    "place",
    "verdict",
    "postal",
    "crs",
    "exists",
    "area",
    "unit",
    "iso20022",
];

/// The CSV columns of `--with iso20022`: the mode, the elements of `PstlAdr`, two address lines and the checks.
pub const ISO_COLUMNS: [&str; 13] = [
    "gp_iso_mode",
    "gp_iso_strt_nm",
    "gp_iso_bldg_nb",
    "gp_iso_flr",
    "gp_iso_room",
    "gp_iso_pst_bx",
    "gp_iso_pst_cd",
    "gp_iso_twn_nm",
    "gp_iso_ctry_sub_dvsn",
    "gp_iso_ctry",
    "gp_iso_adr_line_1",
    "gp_iso_adr_line_2",
    "gp_iso_checks",
];

/// The cells of `ISO_COLUMNS` from the `iso20022` object of an answer.
fn iso_cells(v: Option<&Value>) -> Vec<String> {
    let s = |x: Option<&Value>| x.and_then(Value::as_str).unwrap_or("").to_string();
    let adr = v.and_then(|v| v.get("PstlAdr"));
    let line = |i: usize| s(adr.and_then(|a| a.get("AdrLine")).and_then(|l| l.get(i)));
    let mut out = vec![s(v.and_then(|v| v.get("mode")))];
    for k in [
        "StrtNm",
        "BldgNb",
        "Flr",
        "Room",
        "PstBx",
        "PstCd",
        "TwnNm",
        "CtrySubDvsn",
        "Ctry",
    ] {
        out.push(s(adr.and_then(|a| a.get(k))));
    }
    out.extend([line(0), line(1)]);
    let checks = v.and_then(|v| v.get("checks")).and_then(Value::as_array);
    out.push(checks.map_or(String::new(), |c| {
        c.iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(";")
    }));
    out
}

/// Levels of `place`, finest first.
pub const PLACE_LEVELS: [&str; 4] = ["house", "street", "locality", "region"];

impl With {
    /// `tz,h3:9,placekey` style: names separated by commas, `h3` and `geohash` take a level after a colon.
    pub fn parse(spec: &str) -> Result<With> {
        let mut w = With::default();
        for raw in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (name, arg) = match raw.split_once(':') {
                Some((n, a)) => (n.trim(), Some(a.trim())),
                None => (raw, None),
            };
            let level = |lo: u32, hi: u32| -> Result<Option<u32>> {
                arg.map(|a| match a.parse::<u32>() {
                    Ok(v) if (lo..=hi).contains(&v) => Ok(v),
                    _ => bail!("with {name}:{a}: the level is a whole number {lo}..={hi}"),
                })
                .transpose()
            };
            if arg.is_some() && !matches!(name, "h3" | "geohash" | "place" | "postal" | "crs") {
                bail!("with {raw}: only h3, geohash, place, postal and crs take a level");
            }
            match name.to_ascii_lowercase().as_str() {
                "h3" => w.h3 = Some(level(0, 15)?.unwrap_or(9) as u8),
                "geohash" => w.geohash = Some(level(1, 12)?.unwrap_or(9) as usize),
                "placekey" => w.placekey = true,
                "codes" => w.codes = true,
                "street_point" => w.street_point = true,
                "links" => w.links = true,
                "provenance" => w.provenance = true,
                "verdict" => w.verdict = true,
                "exists" => w.exists = true,
                "area" => w.area = true,
                "unit" => w.unit = true,
                "iso20022" => w.iso20022 = true,
                "crs" => {
                    let code = arg.ok_or_else(|| anyhow::anyhow!("with crs: name the code, e.g. crs:EPSG:2154"))?;
                    w.crs = Some(crate::crs::parse(code).ok_or_else(|| {
                        anyhow::anyhow!(
                            "with crs:{code}: not a projection of this build (UTM 326zz/327zz/258zz, SIRGAS 31965-31985, \
                             MGA2020 7849-7856, 2154, 3035, 28992, 3857)"
                        )
                    })?);
                }
                "postal" => {
                    // no code: the layout of the sheet's own country, read when the answer is laid out (§278)
                    let cc = arg.unwrap_or(SHEET_LAYOUT);
                    w.postal = Some(postal_layout_key(cc).ok_or_else(|| {
                        anyhow::anyhow!("with postal:{cc}: no layout for {cc:?}; known: {}", postal_layouts().join(", "))
                    })?);
                }
                "place" => {
                    let level = arg.unwrap_or("house");
                    w.place = Some(PLACE_LEVELS.into_iter().find(|l| *l == level).ok_or_else(|| {
                        anyhow::anyhow!("with place:{level}: the level is one of {}", PLACE_LEVELS.join(", "))
                    })?);
                }
                "tz" => bail!(
                    "with tz: not in this build: the time zone boundaries are ODbL data (timezone-boundary-builder, \
                     from OpenStreetMap), and they are not shipped inside the engine"
                ),
                other => bail!("with {other}: unknown field; known: {}", NAMES.join(", ")),
            }
        }
        Ok(w)
    }

    pub fn is_empty(&self) -> bool {
        *self == With::default()
    }

    /// The CSV columns these fields add, in output order.
    pub fn columns(&self) -> Vec<&'static str> {
        let mut c = Vec::new();
        if self.h3.is_some() {
            c.push("gp_h3");
        }
        if self.geohash.is_some() {
            c.push("gp_geohash");
        }
        if self.placekey {
            c.push("gp_placekey_where");
        }
        if self.codes {
            c.extend(["gp_plus_code", "gp_mgrs", "gp_dms"]);
        }
        if self.street_point {
            c.extend(["gp_street_lat", "gp_street_lon", "gp_street_point_m"]);
        }
        if self.links {
            c.extend(LINKS.iter().map(|(col, _, _)| *col));
        }
        if self.place.is_some() {
            c.push("gp_place");
        }
        if self.verdict {
            c.extend([
                "gp_verdict_street",
                "gp_verdict_number",
                "gp_verdict_postcode",
                "gp_verdict_city",
            ]);
        }
        if self.crs.is_some() {
            c.extend(["gp_x", "gp_y", "gp_crs"]);
        }
        if self.exists {
            c.extend(["gp_exists", "gp_exists_reason"]);
        }
        if self.area {
            c.extend(["gp_area_spread_m", "gp_area_candidates"]);
        }
        if self.unit {
            c.extend(["gp_unit", "gp_unit_kind"]);
        }
        if self.iso20022 {
            c.extend(ISO_COLUMNS);
        }
        if self.postal.is_some() {
            c.push("gp_postal");
        }
        if self.provenance {
            c.extend([
                "gp_sheet_release",
                "gp_data_date",
                "gp_license",
                "gp_sources",
                "gp_sheet_sha256",
            ]);
        }
        c
    }
}

const UNIT_WORDS: &str = include_str!("unit_words.tsv");
/// The unit words of Cyrillic writing (kept as literals: the table file is Latin text).
const UNIT_WORDS_CYRILLIC: [(&str, &str); 3] =
    [("кв", "flat"), ("квартира", "flat"), ("этаж", "floor")];

/// The token after a unit word is a unit: digits with at most two letters, or one or two letters.
fn unit_token(t: &str) -> bool {
    let t = t.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '.'));
    let digits = t.chars().filter(char::is_ascii_digit).count();
    let letters = t.chars().filter(|c| c.is_alphabetic()).count();
    !t.is_empty()
        && t.chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '/')
        && ((digits >= 1 && letters <= 2) || (digits == 0 && (1..=2).contains(&letters)))
}

/// A token written like a postcode, not like a unit number: five digits or more ("33173", "89642-000").
fn postcode_shaped(t: &str) -> bool {
    t.chars().filter(char::is_ascii_digit).count() >= 5
}

/// The flat, office or floor a query names, and its kind: `apt 4B`, `Suite 300`, `kv. 12`, `#12`, `2º B`.
/// Read from the text only; the address path does not change.
pub fn unit_of(q: &str) -> Option<(String, &'static str)> {
    let toks: Vec<&str> = q
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect();
    for (i, t) in toks.iter().enumerate() {
        if let Some(rest) = t.strip_prefix('#') {
            if unit_token(rest) {
                return Some((rest.to_string(), "flat"));
            }
        }
        // "2º B", "3º izq": a floor ordinal and the door; an ordinal that opens the line is the street's name
        // ("1ª Avenida", "4º Sub-Distrito de Alegrete"): a floor never comes first
        if let Some(n) = t.strip_suffix('º').or_else(|| t.strip_suffix('ª')) {
            if i > 0 && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                let door = toks
                    .get(i + 1)
                    .filter(|d| d.chars().count() <= 4)
                    .map(|d| format!(" {d}"))
                    .unwrap_or_default();
                return Some((format!("{n}º{door}"), "floor"));
            }
        }
        let word = crate::norm::normalize(t);
        let kind = UNIT_WORDS
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split_once('\t'))
            .chain(UNIT_WORDS_CYRILLIC)
            .find(|(w, _)| *w == word)
            .map(|(_, k)| k);
        if let (Some(kind), Some(next)) = (kind, toks.get(i + 1)) {
            // "Miami, FL 33173": a region code that spells a unit word ("FL", floor), before a postcode; "Interior,
            // 89642-000" the same in Brazil. No flat, floor or door is numbered with a postcode (ANSWERS §308)
            if unit_token(next) && !postcode_shaped(next) {
                let kind: &'static str = match kind {
                    "floor" => "floor",
                    "office" => "office",
                    "room" => "room",
                    "door" => "door",
                    _ => "flat",
                };
                let value = next.trim_matches(|c: char| matches!(c, '.' | ';' | ':'));
                return Some((value.to_string(), kind));
            }
        }
    }
    None
}

/// Candidates asked for `area`.
pub const AREA_K: usize = 5;
/// A candidate with a weight below this does not widen the area.
pub const AREA_MIN_WEIGHT: f64 = 0.1;

/// Where the address may be: the candidates of the top answer's level, weights `exp(score - best)` normalized to
/// a sum of 1, the box and the largest distance between candidates of weight `AREA_MIN_WEIGHT` or more. One
/// candidate: an empty area of spread 0. The answer itself (the first point) is not changed.
pub fn area(hits: &[Hit]) -> Value {
    let Some(top) = hits.first() else {
        return Value::Null;
    };
    let level = crate::query::match_level(top).0;
    let same: Vec<&Hit> = hits
        .iter()
        .filter(|h| crate::query::match_level(h).0 == level)
        .collect();
    let best = same
        .iter()
        .map(|h| f64::from(h.score))
        .fold(f64::NEG_INFINITY, f64::max);
    let raw: Vec<f64> = same
        .iter()
        .map(|h| (f64::from(h.score) - best).exp())
        .collect();
    let total: f64 = raw.iter().sum();
    let weights: Vec<f64> = raw.iter().map(|w| w / total).collect();
    let kept: Vec<(&Hit, f64)> = same
        .iter()
        .copied()
        .zip(weights.iter().copied())
        .filter(|(_, w)| *w >= AREA_MIN_WEIGHT)
        .collect();
    let mut spread = 0.0_f64;
    for (a, _) in &kept {
        for (b, _) in &kept {
            spread = spread.max(codes::haversine_m((a.lat, a.lon), (b.lat, b.lon)));
        }
    }
    let lat = kept.iter().map(|(h, _)| h.lat);
    let lon = kept.iter().map(|(h, _)| h.lon);
    let bbox = json!([
        lon.clone().fold(f64::INFINITY, f64::min),
        lat.clone().fold(f64::INFINITY, f64::min),
        lon.fold(f64::NEG_INFINITY, f64::max),
        lat.fold(f64::NEG_INFINITY, f64::max)
    ]);
    let candidates: Vec<Value> = same
        .iter()
        .zip(&weights)
        .map(|(h, w)| json!({"lat": round7(h.lat), "lon": round7(h.lon), "weight": (w * 1e6).round() / 1e6}))
        .collect();
    json!({
        "level": level,
        "bbox": if same.len() > 1 { bbox } else { Value::Null },
        "candidates": if same.len() > 1 { json!(candidates) } else { json!([]) },
        "spread_m": (spread * 10.0).round() / 10.0,
    })
}

/// Postal layouts by country code (`postal_formats.tsv`): data, not a branch of code per country.
const POSTAL_FORMATS: &str = include_str!("postal_formats.tsv");

fn postal_rows() -> impl Iterator<Item = (&'static str, &'static str)> {
    POSTAL_FORMATS
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
}

/// The layout names of the table (`default` and country codes).
pub fn postal_layouts() -> Vec<&'static str> {
    postal_rows().map(|(k, _)| k).collect()
}

/// `--with postal` without a code: the layout follows the sheet's country.
pub const SHEET_LAYOUT: &str = "sheet";

/// The layout of `--with postal`: the one named, or for `sheet` the one of the sheet's country (`postal_formats.tsv`
/// row; the default row when the country has none). A lookup in a data table, not a reading rule per country: the
/// owner allowed this one read of the sheet's country, ANSWERS §278.
pub fn postal_layout_of(layout: &'static str, idx: &Index) -> &'static str {
    if layout != SHEET_LAYOUT {
        return layout;
    }
    idx.country()
        .and_then(postal_layout_key)
        .unwrap_or("default")
}

/// The table's own spelling of a layout name, if there is one (`sheet`: the sheet's country, chosen later).
pub fn postal_layout_key(name: &str) -> Option<&'static str> {
    if name == SHEET_LAYOUT {
        return Some(SHEET_LAYOUT);
    }
    postal_rows()
        .map(|(k, _)| k)
        .find(|k| k.eq_ignore_ascii_case(name))
}

/// The template of layout `country`, else the default one.
pub fn postal_template(country: Option<&str>) -> &'static str {
    let rows = postal_rows;
    let find = |cc: &str| {
        rows()
            .find(|(k, _)| k.eq_ignore_ascii_case(cc))
            .map(|(_, t)| t)
    };
    country
        .and_then(find)
        .or_else(|| find("default"))
        .unwrap_or("{street} {number}\\n{postcode} {city}")
}

/// The answer as the post of its country lays out an address: lines separated by "\n". A field the answer does
/// not have is dropped together with the separator before it (or after it, at the start of a line).
pub fn postal(h: &Hit, country: Option<&str>) -> String {
    let field = |name: &str| -> &str {
        match name {
            "number" => h.housenumber.as_deref().unwrap_or(""),
            "street" => &h.street,
            "postcode" => &h.postcode,
            "city" => &h.commune,
            "region" => h.region.as_deref().unwrap_or(""),
            _ => "", // neighborhood: a sheet keeps no neighbourhood yet
        }
    };
    let mut lines = Vec::new();
    for line in postal_template(country).split("\\n") {
        // pieces: text between placeholders, and placeholders filled or empty
        let mut out = String::new();
        let mut pending_sep = String::new();
        let mut rest = line;
        while let Some(open) = rest.find('{') {
            pending_sep.push_str(&rest[..open]);
            let close = rest[open..].find('}').map_or(rest.len(), |c| open + c);
            let value = field(&rest[open + 1..close]).trim();
            if !value.is_empty() {
                if !out.is_empty() {
                    out.push_str(&pending_sep);
                }
                out.push_str(value);
            }
            pending_sep.clear();
            rest = &rest[(close + 1).min(rest.len())..];
        }
        if !out.is_empty() {
            lines.push(out);
        }
    }
    lines.join("\n")
}

/// (CSV column, JSON key, URL template with {lat} {lon})
const LINKS: [(&str, &str, &str); 5] = [
    (
        "gp_osm_url",
        "osm",
        "https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map=19/{lat}/{lon}",
    ),
    (
        "gp_google_url",
        "google",
        "https://www.google.com/maps/search/?api=1&query={lat},{lon}",
    ),
    (
        "gp_apple_url",
        "apple",
        "https://maps.apple.com/?ll={lat},{lon}&q={lat},{lon}",
    ),
    (
        "gp_mapillary_url",
        "mapillary",
        "https://www.mapillary.com/app/?lat={lat}&lng={lon}&z=18",
    ),
    (
        "gp_bing_url",
        "bing",
        "https://www.bing.com/maps?cp={lat}~{lon}&lvl=19",
    ),
];

/// A spec read against one sheet: the sheet card is computed once here, not per answer.
pub struct Extras {
    pub with: With,
    card: Option<Map<String, Value>>,
    /// the card of the `--poi` layer, for its `poi_layer` answers (`set_poi`)
    poi_card: Option<Map<String, Value>>,
}

impl Extras {
    /// `None` spec or an empty one gives `None`: the caller then writes answers exactly as before.
    pub fn new(spec: Option<&str>, idx: &Index) -> Result<Option<Extras>> {
        let Some(spec) = spec else { return Ok(None) };
        let with = With::parse(spec)?;
        if with.is_empty() {
            return Ok(None);
        }
        let card = with.provenance.then(|| cached_card(idx));
        Ok(Some(Extras {
            with,
            card,
            poi_card: None,
        }))
    }

    /// The `--poi` layer asked beside the sheet: its answers (`poi_layer`) carry its own card.
    pub fn set_poi(&mut self, poi: &Index) {
        if self.with.provenance {
            self.poi_card = Some(cached_card(poi));
        }
    }

    /// The `ISO_COLUMNS` cells of a line the sheet did not answer: `PstlAdr` can still be hybrid (a town of the sheet)
    /// or a post box, and `none` says why. `None` without `--with iso20022`.
    pub fn iso_cells_without_answer(&self, q: &str, idx: &Index) -> Option<Vec<String>> {
        self.with.iso20022.then(|| {
            let o = crate::iso20022::build(idx, q, None);
            iso_cells(Some(&crate::iso20022::json(&o, false)))
        })
    }

    /// The fields of one answer, keys sorted.
    pub fn of(&self, h: &Hit, idx: &Index) -> Map<String, Value> {
        let w = &self.with;
        let mut out = BTreeMap::<String, Value>::new();
        if let Some(res) = w.h3 {
            out.insert(
                "h3".into(),
                h3_cell(h.lat, h.lon, res).map_or(Value::Null, Value::from),
            );
        }
        if let Some(len) = w.geohash {
            out.insert(
                "geohash".into(),
                codes::geohash_encode(h.lat, h.lon, len).into(),
            );
        }
        if w.placekey {
            out.insert(
                "placekey_where".into(),
                placekey_where(h.lat, h.lon).map_or(Value::Null, Value::from),
            );
        }
        if w.codes {
            out.insert(
                "plus_code".into(),
                codes::plus_code_encode(h.lat, h.lon, 11).into(),
            );
            out.insert(
                "mgrs".into(),
                codes::mgrs_encode(h.lat, h.lon).map_or(Value::Null, Value::from),
            );
            out.insert("dms".into(), codes::dms_format(h.lat, h.lon).into());
        }
        if w.street_point {
            let point = street_point(h, idx);
            out.insert(
                "street_point".into(),
                point.map_or(
                    Value::Null,
                    |(lat, lon)| json!({"lat": round7(lat), "lon": round7(lon)}),
                ),
            );
            out.insert(
                "street_point_m".into(),
                point.map_or(Value::Null, |p| {
                    json!((codes::haversine_m((h.lat, h.lon), p) * 10.0).round() / 10.0)
                }),
            );
        }
        if w.links {
            let links: Map<String, Value> = LINKS
                .iter()
                .map(|(_, key, tpl)| ((*key).to_string(), link(tpl, h.lat, h.lon).into()))
                .collect();
            out.insert("links".into(), Value::Object(links));
        }
        if let Some(level) = w.place {
            out.insert("place".into(), place(h, level, idx));
        }
        if w.verdict {
            out.insert(
                "verdict".into(),
                match (query_now(), asked_now(idx)) {
                    (Some(q), Some(a)) => json!(crate::verdict::field_verdicts_in(idx, &q, &a, h)),
                    _ => Value::Null,
                },
            );
        }
        if w.unit {
            let q = QUERY.with(|c| c.borrow().as_ref().map(|q| q.0.clone()));
            out.insert(
                "unit".into(),
                q.and_then(|q| unit_of(&q))
                    .map_or(Value::Null, |(u, k)| json!({"unit": u, "kind": k})),
            );
        }
        if w.iso20022 {
            let q = QUERY.with(|c| c.borrow().as_ref().map(|q| q.0.clone()));
            out.insert(
                "iso20022".into(),
                q.map_or(Value::Null, |q| {
                    crate::iso20022::json(&crate::iso20022::build(idx, &q, Some(h)), false)
                }),
            );
        }
        if w.area {
            let q = QUERY.with(|c| c.borrow().as_ref().map(|q| q.0.clone()));
            out.insert(
                "area".into(),
                q.map_or(Value::Null, |q| {
                    area(&crate::query::query_cascade(idx, None, &q, AREA_K))
                }),
            );
        }
        if w.exists {
            let q = QUERY.with(|c| c.borrow().as_ref().map(|q| q.0.clone()));
            out.insert(
                "exists".into(),
                match (q, asked_now(idx)) {
                    (Some(q), Some(a)) => json!(crate::exists::judge_answer(idx, &q, &a, Some(h))),
                    _ => Value::Null,
                },
            );
        }
        if let Some(code) = w.crs {
            out.insert(
                "crs".into(),
                crate::crs::project(code, h.lat, h.lon).map_or(Value::Null, |(x, y)| {
                    json!({"code": format!("EPSG:{code}"), "x": (x * 100.0).round() / 100.0, "y": (y * 100.0).round() / 100.0})
                }),
            );
        }
        if let Some(layout) = w.postal {
            // a weak answer is a guess: laid out as an address it would reach a label with nothing to say so; the row
            // keeps the answer and gp_weak, the layout stays empty (ANSWERS §298, §300)
            let text = (!crate::query::hit_is_weak(h))
                .then(|| postal(h, Some(postal_layout_of(layout, idx))));
            out.insert("postal".into(), text.map_or(Value::Null, Value::from));
        }
        if let Some(card) = &self.card {
            // the card of the sheet that gave this answer: an answer of `--also` (own_sheet) or `--also-after`
            // (world_place) is not data of the country's sheet, nor under its licence (revision 0.9.0)
            let card = match (answering_sheet(h, idx), &self.poi_card) {
                (Some(other), _) => cached_card(other),
                (None, Some(poi)) if h.flags.contains(&"poi_layer") => poi.clone(),
                (None, _) => card.clone(),
            };
            out.insert("provenance".into(), Value::Object(card));
        }
        out.into_iter().collect()
    }

    /// The CSV cells of `columns()` for one answer, in the order of `columns()`: each cell is keyed by its column
    /// name and laid out by `columns()`, so the header and the cells cannot drift apart (revision 0.9.0: two
    /// hand-kept orders put `gp_x=yes` under `--with exists,crs:…`).
    pub fn cells(&self, h: &Hit, idx: &Index) -> Vec<String> {
        let map = self.of(h, idx);
        let text = |v: Option<&Value>| match v {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
        };
        let mut by: BTreeMap<&'static str, String> = BTreeMap::new();
        let w = &self.with;
        if w.h3.is_some() {
            by.insert("gp_h3", text(map.get("h3")));
        }
        if w.geohash.is_some() {
            by.insert("gp_geohash", text(map.get("geohash")));
        }
        if w.placekey {
            by.insert("gp_placekey_where", text(map.get("placekey_where")));
        }
        if w.codes {
            for (col, k) in [
                ("gp_plus_code", "plus_code"),
                ("gp_mgrs", "mgrs"),
                ("gp_dms", "dms"),
            ] {
                by.insert(col, text(map.get(k)));
            }
        }
        if w.street_point {
            let p = map.get("street_point");
            by.insert("gp_street_lat", text(p.and_then(|p| p.get("lat"))));
            by.insert("gp_street_lon", text(p.and_then(|p| p.get("lon"))));
            by.insert("gp_street_point_m", text(map.get("street_point_m")));
        }
        if w.links {
            let l = map.get("links");
            for (col, key, _) in LINKS {
                by.insert(col, text(l.and_then(|l| l.get(key))));
            }
        }
        if w.place.is_some() {
            by.insert(
                "gp_place",
                text(map.get("place").and_then(|p| p.get("short"))),
            );
        }
        if w.verdict {
            let v = map.get("verdict");
            for (col, k) in [
                ("gp_verdict_street", "street"),
                ("gp_verdict_number", "number"),
                ("gp_verdict_postcode", "postcode"),
                ("gp_verdict_city", "city"),
            ] {
                by.insert(col, text(v.and_then(|v| v.get(k))));
            }
        }
        if w.unit {
            let u = map.get("unit");
            by.insert("gp_unit", text(u.and_then(|u| u.get("unit"))));
            by.insert("gp_unit_kind", text(u.and_then(|u| u.get("kind"))));
        }
        if w.iso20022 {
            by.extend(ISO_COLUMNS.into_iter().zip(iso_cells(map.get("iso20022"))));
        }
        if w.area {
            let a = map.get("area");
            by.insert("gp_area_spread_m", text(a.and_then(|a| a.get("spread_m"))));
            by.insert(
                "gp_area_candidates",
                text(
                    a.and_then(|a| a.get("candidates"))
                        .and_then(Value::as_array)
                        .map(|v| Value::from(v.len()))
                        .as_ref(),
                ),
            );
        }
        if w.exists {
            let e = map.get("exists");
            by.insert("gp_exists", text(e.and_then(|e| e.get("exists"))));
            by.insert("gp_exists_reason", text(e.and_then(|e| e.get("reason"))));
        }
        if w.crs.is_some() {
            let p = map.get("crs");
            for (col, k) in [("gp_x", "x"), ("gp_y", "y"), ("gp_crs", "code")] {
                by.insert(col, text(p.and_then(|p| p.get(k))));
            }
        }
        if w.postal.is_some() {
            by.insert("gp_postal", text(map.get("postal")));
        }
        if w.provenance {
            let p = map.get("provenance");
            for (col, k) in [
                ("gp_sheet_release", "release"),
                ("gp_data_date", "data_date"),
                ("gp_license", "license"),
                ("gp_sources", "sources"),
                ("gp_sheet_sha256", "sha256"),
            ] {
                by.insert(col, text(p.and_then(|p| p.get(k))));
            }
        }
        let cols = w.columns();
        debug_assert_eq!(by.len(), cols.len(), "every column has its cell: {cols:?}");
        cols.into_iter()
            .map(|c| by.remove(c).unwrap_or_default())
            .collect()
    }

    /// The JSON text of one answer: its own fields as `Hit` writes them (same order, same numbers), then
    /// `alias_name` when given, then `extras`.
    pub fn hit_json(&self, h: &Hit, idx: &Index, alias: Option<&str>) -> Result<String> {
        hit_text(h, Some(self), idx, alias)
    }
}

/// One answer as JSON text with or without extras: the single place every surface writes a hit through.
/// Without extras and alias it is exactly `serde_json::to_string(hit)`.
pub fn hit_text(
    h: &Hit,
    extras: Option<&Extras>,
    idx: &Index,
    alias: Option<&str>,
) -> Result<String> {
    let mut s = serde_json::to_string(h)?;
    let mut tail = String::new();
    if let Some(name) = alias {
        tail.push_str(",\"alias_name\":");
        tail.push_str(&serde_json::to_string(name)?);
    }
    if let Some(e) = extras {
        tail.push_str(",\"extras\":");
        tail.push_str(&serde_json::to_string(&Value::Object(e.of(h, idx)))?);
    }
    if !tail.is_empty() {
        debug_assert!(s.ends_with('}'));
        s.pop();
        s.push_str(&tail);
        s.push('}');
    }
    Ok(s)
}

/// A list of answers as a JSON array text.
pub fn hits_text(
    hits: &[Hit],
    extras: Option<&Extras>,
    idx: &Index,
    alias: Option<&str>,
) -> Result<String> {
    let rows = hits
        .iter()
        .map(|h| hit_text(h, extras, idx, alias))
        .collect::<Result<Vec<_>>>()?;
    Ok(format!("[{}]", rows.join(",")))
}

thread_local! {
    /// The query whose answers are being written, and how the engine read it (filled on first use).
    static QUERY: std::cell::RefCell<Option<(String, Option<crate::verdict::Asked>)>> =
        const { std::cell::RefCell::new(None) };
}

/// Write the answers of `q` inside `f`: `verdict` compares them with `q` as the engine read it, read once.
/// Outside such a scope (reverse answers, for one) `verdict` is null.
pub fn for_query<T>(q: &str, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<(String, Option<crate::verdict::Asked>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            QUERY.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(QUERY.with(|c| c.borrow_mut().replace((q.to_string(), None))));
    f()
}

fn query_now() -> Option<String> {
    QUERY.with(|c| c.borrow().as_ref().map(|q| q.0.clone()))
}

fn asked_now(idx: &Index) -> Option<crate::verdict::Asked> {
    let q = QUERY.with(|c| c.borrow().clone())?;
    if let Some(a) = q.1 {
        return Some(a);
    }
    let a = crate::verdict::asked(idx, &q.0);
    QUERY.with(|c| {
        if let Some(cur) = c.borrow_mut().as_mut() {
            cur.1 = Some(a.clone());
        }
    });
    Some(a)
}

fn round7(v: f64) -> f64 {
    (v * 1e7).round() / 1e7
}

fn link(tpl: &str, lat: f64, lon: f64) -> String {
    tpl.replace("{lat}", &format!("{:.6}", lat))
        .replace("{lon}", &format!("{:.6}", lon))
}

fn h3_cell(lat: f64, lon: f64, res: u8) -> Option<String> {
    let r = h3o::Resolution::try_from(res).ok()?;
    let ll = h3o::LatLng::new(lat, lon).ok()?;
    Some(ll.to_cell(r).to_string())
}

/// Placekey's alphabet and the words it replaces (placekey-py 0.0.36, `placekey/placekey.py`, MIT).
const PK_ALPHABET: &[u8; 28] = b"23456789bcdfghjkmnpqrstvwxyz";
const PK_REPLACE: [(&str, &str); 13] = [
    ("prn", "pre"),
    ("f4nny", "f4nne"),
    ("tw4t", "tw4e"),
    ("ngr", "ngu"),
    ("dck", "dce"),
    ("vjn", "vju"),
    ("fck", "fce"),
    ("pns", "pne"),
    ("sht", "she"),
    ("kkk", "kke"),
    ("fgt", "fgu"),
    ("dyk", "dye"),
    ("bch", "bce"),
];

/// The "where" part of a Placekey: H3 at resolution 10, shortened and written in Placekey's alphabet
/// ("@5vg-82n-kzz"). The "what" part is issued by Placekey's own service and is not made here.
pub fn placekey_where(lat: f64, lon: f64) -> Option<String> {
    const BASE_RESOLUTION: u32 = 12;
    let ll = h3o::LatLng::new(lat, lon).ok()?;
    let cell = u64::from(ll.to_cell(h3o::Resolution::Ten));
    // placekey-py `_shorten_h3_integer`: add one to the base cell, keep the 52 location bits, drop the
    // digits finer than resolution 12
    let base_cell_shift = 1u64 << (3 * 15);
    let short = (cell.wrapping_add(base_cell_shift) % (1u64 << 52)) >> (3 * (15 - BASE_RESOLUTION));
    let mut digits = Vec::new();
    let mut x = short;
    if x == 0 {
        digits.push(PK_ALPHABET[0]);
    }
    while x > 0 {
        digits.push(PK_ALPHABET[(x % 28) as usize]);
        x /= 28;
    }
    digits.reverse();
    let mut s = String::from_utf8(digits).ok()?;
    for (bad, good) in PK_REPLACE {
        if s.contains(bad) {
            s = s.replace(bad, good);
        }
    }
    while s.len() < 9 {
        s.insert(0, 'a');
    }
    let groups: Vec<&str> = (0..s.len())
        .step_by(3)
        .map(|i| &s[i..(i + 3).min(s.len())])
        .collect();
    Some(format!("@{}", groups.join("-")))
}

fn street_point(h: &Hit, idx: &Index) -> Option<(f64, f64)> {
    if !matches!(h.precision, "house" | "interp" | "near") {
        return None;
    }
    let number: u32 = h
        .housenumber
        .as_deref()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()?;
    idx.street_point(h.lat, h.lon, &h.street, &h.commune, number)
}

/// The answer as a person writes it: `levels` from the house to the region (what the answer has), and `short`,
/// the shortest of them at `level` or coarser that is still one place: the town is added to a street only when
/// another street of that name lies within about 3 km, and to a house the same way.
fn place(h: &Hit, level: &str, idx: &Index) -> Value {
    let house = match (&h.housenumber, h.street.is_empty()) {
        (Some(n), false) if idx.number_first() || number_written_first(idx) => {
            Some(format!("{n} {}", h.street))
        }
        (Some(n), false) => Some(format!("{} {n}", h.street)),
        _ => None,
    };
    let street = (!h.street.is_empty()).then(|| h.street.clone());
    let locality = (!h.commune.is_empty()).then(|| h.commune.clone());
    let region = h.region.clone().filter(|r| !r.is_empty());
    let alone = || {
        street
            .as_ref()
            .is_some_and(|s| idx.street_name_alone_near(h.lat, h.lon, s, &h.commune))
    };
    let with_town = |s: &str| match &locality {
        Some(town) if !alone() => format!("{s}, {town}"),
        _ => s.to_string(),
    };
    let start = PLACE_LEVELS.iter().position(|l| *l == level).unwrap_or(0);
    let short = PLACE_LEVELS[start..].iter().find_map(|l| match *l {
        "house" => house.as_deref().map(with_town),
        "street" => street.as_deref().map(with_town),
        "locality" => locality.as_ref().map(|t| match &region {
            Some(r) => format!("{t}, {r}"),
            None => t.clone(),
        }),
        _ => region.clone(),
    });
    json!({
        "short": short,
        "levels": {"house": house, "street": street, "locality": locality, "region": region},
    })
}

/// The sheet beside `idx` that gave `h`, when it is not `idx` itself: the sheet of one's own places for an
/// `own_sheet` answer, the sheet asked after for a `world_place` one. The flags are set only by those two paths
/// (`query.rs`), so an answer of the country's sheet never names another sheet.
fn answering_sheet<'a>(h: &Hit, idx: &'a Index) -> Option<&'a Index> {
    if h.flags.contains(&"own_sheet") {
        idx.also()
    } else if h.flags.contains(&"world_place") {
        idx.also_after()
    } else {
        None
    }
}

/// The post of the sheet's country writes the number before the street (`postal_formats.tsv`, the first line
/// of the layout `--with postal` takes for the sheet): "37 Rue Sainte-Croix", not "Rue Sainte-Croix 37". The
/// same lookup in a data table as `postal_layout_of` (ANSWERS §278), not a reading rule per country.
fn number_written_first(idx: &Index) -> bool {
    let line = postal_template(Some(postal_layout_of(SHEET_LAYOUT, idx)))
        .split("\\n")
        .next()
        .unwrap_or("");
    matches!((line.find("{number}"), line.find("{street}")), (Some(n), Some(s)) if n < s)
}

/// The card of a sheet, hashed once per mapped file: the server builds `Extras` per request.
fn cached_card(idx: &Index) -> Map<String, Value> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Cards = HashMap<(usize, usize), Map<String, Value>>;
    static CARDS: OnceLock<Mutex<Cards>> = OnceLock::new();
    let bytes = idx.file_bytes();
    let key = (bytes.as_ptr() as usize, bytes.len());
    let cards = CARDS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(card) = cards.lock().expect("card cache").get(&key) {
        return card.clone();
    }
    let card = provenance_card(idx);
    cards.lock().expect("card cache").insert(key, card.clone());
    card
}

fn provenance_card(idx: &Index) -> Map<String, Value> {
    use sha2::{Digest, Sha256};
    let meta: BTreeMap<&str, &str> = idx
        .meta()
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let sha: String = Sha256::digest(idx.file_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let reg = crate::sheets::registry();
    let release = reg
        .sheets
        .iter()
        .find(|s| s.sha256 == sha)
        .map(|_| reg.release.clone());
    let get = |k: &str| meta.get(k).map(|v| Value::from(*v)).unwrap_or(Value::Null);
    let mut card = Map::new();
    card.insert("release".into(), release.map_or(Value::Null, Value::from));
    card.insert("country".into(), get("country"));
    card.insert("data_date".into(), get("source_release"));
    card.insert("license".into(), get("license"));
    card.insert("sources".into(), get("sources"));
    card.insert("attribution".into(), get("attribution"));
    card.insert("sha256".into(), sha.into());
    card
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_postcode_after_a_unit_word_is_not_a_unit() {
        // "FL" is Florida before its ZIP, "Interior" a Brazilian address part before its CEP (ANSWERS §308)
        assert_eq!(unit_of("11481 SW 84th St, Miami, FL 33173, USA"), None);
        assert_eq!(unit_of("Rua X 5, Interior, 89642-000"), None);
        // an ordinal that opens the line names the street; after the house it is the floor
        assert_eq!(
            unit_of("1ª Avenida do Centro Administrativo 150, Salvador"),
            None
        );
        assert_eq!(
            unit_of("Calle Mayor 5, 2º B, Madrid"),
            Some(("2º B".to_string(), "floor"))
        );
        // the near misses: a floor and a suite of four digits stay units
        assert_eq!(
            unit_of("350 Fifth Ave, Fl 3, New York"),
            Some(("3".to_string(), "floor"))
        );
        assert_eq!(
            unit_of("101 West Ohio Street, Suite 1200, Indianapolis"),
            Some(("1200".to_string(), "office"))
        );
    }

    use super::*;

    fn addr(street: &str, number: &str, postcode: &str, city: &str, region: Option<&str>) -> Hit {
        Hit {
            lat: 0.0,
            lon: 0.0,
            precision: "house",
            score: 1.0,
            confidence: 1.0,
            street: street.into(),
            housenumber: (!number.is_empty()).then(|| number.into()),
            commune: city.into(),
            postcode: postcode.into(),
            flags: vec![],
            region: region.map(String::from),
            distance_m: None,
        }
    }

    #[test]
    fn unit_is_read_from_the_query_and_a_word_inside_a_street_name_is_not_one() {
        let u = |q: &str| unit_of(q).map(|(u, k)| (u, k.to_string()));
        let p = |a: &str, b: &str| Some((a.to_string(), b.to_string()));
        assert_eq!(u("Main St 12, apt 4B, Springfield"), p("4B", "flat"));
        assert_eq!(
            u("Suite 300, 1 Market St, San Francisco"),
            p("300", "office")
        );
        assert_eq!(u("Calle Mayor 12, 2º B, Madrid"), p("2º B", "floor"));
        assert_eq!(u("ul. Lenina 5, kv. 12"), p("12", "flat"));
        assert_eq!(u("12 Rue X #7"), p("7", "flat"));
        assert_eq!(u("ул. Ленина 5, кв. 12"), p("12", "flat"));
        assert_eq!(u("Unit Street 5, Springfield"), None);
        assert_eq!(u("Floor Avenue 12"), None);
    }

    #[test]
    fn area_weights_the_candidates_of_one_level_and_measures_their_spread() {
        let mut far = addr("Main Street", "1", "", "Springfield", None);
        far.lat = 39.8;
        far.lon = -89.6;
        far.score = 5.0;
        let mut near = far.clone();
        near.lat = 42.1;
        near.lon = -72.6;
        near.score = 4.0;
        let a = area(&[far.clone(), near.clone()]);
        let w: Vec<f64> = a["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["weight"].as_f64().unwrap())
            .collect();
        assert!((w.iter().sum::<f64>() - 1.0).abs() < 1e-5);
        assert!(w[0] > w[1], "the better score weighs more: {w:?}");
        assert!(a["spread_m"].as_f64().unwrap() > 50_000.0);
        let one = area(&[far]);
        assert_eq!(one["spread_m"], 0.0);
        assert_eq!(one["candidates"], json!([]));
    }

    #[test]
    fn postal_lays_the_answer_out_as_the_country_writes_it() {
        let de = addr("Hauptstraße", "5", "10115", "Berlin", None);
        let us = addr(
            "Main Street",
            "100",
            "90012",
            "Los Angeles",
            Some("California"),
        );
        let br = addr("Rua Augusta", "1500", "01304", "São Paulo", Some("SP"));
        let cases = [
            (&de, Some("de"), "Hauptstraße 5\n10115 Berlin"),
            (&de, Some("fr"), "5 Hauptstraße\n10115 Berlin"),
            (
                &us,
                Some("us"),
                "100 Main Street\nLos Angeles, California 90012",
            ),
            (&br, Some("br"), "Rua Augusta, 1500\nSão Paulo - SP\n01304"),
            (&de, Some("zz"), "Hauptstraße 5\n10115 Berlin"),
            (&de, None, "Hauptstraße 5\n10115 Berlin"),
        ];
        for (h, cc, want) in cases {
            assert_eq!(postal(h, cc), want, "{cc:?}");
        }
    }

    #[test]
    fn postal_drops_a_missing_field_with_its_separator() {
        // a street answer: no number; the US line keeps "city postcode" without a dangling comma
        let us = addr("Main Street", "", "90012", "Los Angeles", None);
        assert_eq!(postal(&us, Some("us")), "Main Street\nLos Angeles 90012");
        let mx = addr(
            "Calle Moneda",
            "4",
            "06000",
            "Cuauhtémoc",
            Some("Ciudad de México"),
        );
        assert_eq!(
            postal(&mx, Some("mx")),
            "Calle Moneda 4\n06000 Cuauhtémoc, Ciudad de México"
        );
    }

    #[test]
    fn postal_is_a_column_only_when_asked() {
        assert!(!With::parse("h3").unwrap().columns().contains(&"gp_postal"));
        assert_eq!(With::parse("postal").unwrap().columns(), ["gp_postal"]);
        assert_eq!(With::parse("postal:US").unwrap().postal, Some("us"));
        assert!(
            With::parse("postal:zz").is_err(),
            "an unknown layout is an error, not the default"
        );
    }

    #[test]
    fn a_spec_reads_names_and_levels_and_refuses_the_rest() {
        let w = With::parse("h3, geohash:7,placekey,codes,street_point,links,provenance").unwrap();
        assert_eq!(w.h3, Some(9), "h3 without a level is 9");
        assert_eq!(w.geohash, Some(7));
        assert!(w.placekey && w.codes && w.street_point && w.links && w.provenance);
        assert_eq!(With::parse("h3:12").unwrap().h3, Some(12));
        assert!(With::parse("").unwrap().is_empty());
        for bad in ["h3:16", "geohash:0", "placekey:3", "zip", "h3:x"] {
            assert!(With::parse(bad).is_err(), "{bad}");
        }
        let tz = With::parse("tz").unwrap_err().to_string();
        assert!(tz.contains("ODbL"), "{tz}");
    }

    #[test]
    fn the_columns_follow_the_spec_in_a_fixed_order() {
        let w = With::parse("links,h3").unwrap();
        let cols = w.columns();
        assert_eq!(cols[0], "gp_h3");
        assert_eq!(cols.len(), 1 + LINKS.len());
        assert!(With::default().columns().is_empty());
    }

    #[test]
    fn h3_is_the_cell_of_the_h3_documentation() {
        // h3 documentation, "latLngToCell": 37.769377, -122.388903 at resolution 9
        assert_eq!(
            h3_cell(37.769377, -122.388903, 9).unwrap(),
            "89283082e73ffff"
        );
        assert_ne!(
            h3_cell(37.769377, -122.388903, 8).unwrap(),
            "89283082e73ffff"
        );
    }

    #[test]
    fn the_placekey_where_part_matches_placekey_py() {
        // placekey-py 0.0.36 README: geo_to_placekey(0.0, 0.0) is "@dvt-smp-tvz" (H3 8a754e64992ffff), and the
        // service answers 37.7371, -122.44283 with the where part "@5vg-82n-kzz"
        assert_eq!(h3_cell(0.0, 0.0, 10).unwrap(), "8a754e64992ffff");
        assert_eq!(placekey_where(0.0, 0.0).unwrap(), "@dvt-smp-tvz");
        assert_eq!(placekey_where(37.7371, -122.44283).unwrap(), "@5vg-82n-kzz");
    }

    #[test]
    fn links_put_the_point_in_each_template() {
        let l = link(LINKS[0].2, 44.8125, 20.4612);
        assert_eq!(l, "https://www.openstreetmap.org/?mlat=44.812500&mlon=20.461200#map=19/44.812500/20.461200");
    }
}
