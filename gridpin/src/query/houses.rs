//! The sheet as a table: walk its houses with a filter instead of asking for one address.
//!
//! `gridpin list`, `export`, `sample` and `stats` share this walker. It reads the house blocks in street
//! order and keeps no index of its own, so a sheet needs nothing rebuilt and the walk costs one pass over
//! the mapped file. Range points (`RANGE_MARKER`, sampled from address ranges) are not addresses and never
//! come out; `WalkStats` says how many were passed over.

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use super::{normalize, point_in_rings, query_cascade, Index, StreetMeta, PC_DISP_AMBIGUOUS};

/// Mean Earth radius of every distance the project reports (`eval/wikidata4_regress.py`).
const EARTH_RADIUS_M: f64 = 6_371_000.0;

/// Great-circle distance in metres.
/// How far across the street a neighbour may be for `street_point`. 100 m, not 60: on the 300 owner addresses of
/// the United States (07.11.2026) 60 m found a point for 98 of 291 house answers, 100 m for 138, and 98.6 % of
/// them stay 5-40 m from the house; wide avenues (Congress Avenue, Austin: 64 m across) need it.
pub const STREET_POINT_REACH_M: f64 = 100.0;

/// How far apart the closest houses of two streets may be for the streets to count as crossing.
pub const CROSSING_GAP_M: f64 = 80.0;

pub fn distance_m(lat_a: f64, lon_a: f64, lat_b: f64, lon_b: f64) -> f64 {
    let (p1, p2) = (lat_a.to_radians(), lat_b.to_radians());
    let (dp, dl) = (p2 - p1, (lon_b - lon_a).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.clamp(0.0, 1.0).sqrt().asin()
}

/// One stored point of a street, before any string is made.
#[derive(Clone, Copy, Debug)]
pub struct StreetHouse {
    pub street_id: u32,
    pub number: u32,
    pub rep: u32,
    pub lat: f64,
    pub lon: f64,
    pub(super) postcode_off: u32,
}

/// One address as `list` and `export` write it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct HouseRow {
    pub lat: f64,
    pub lon: f64,
    pub street: String,
    pub housenumber: String,
    pub postcode: String,
    pub commune: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

/// One suggestion of `Index::suggest`: a street, or the house of the typed number on it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Suggestion {
    /// what the field shows and puts in: "[number ]street, commune"
    pub label: String,
    pub street: String,
    pub commune: String,
    pub lat: f64,
    pub lon: f64,
    /// "house": the typed number is a house of this street and the point is that house; "street": the point is the
    /// street's middle (no number typed, or the sheet has no house of that number on the street).
    pub precision: &'static str,
}

/// A prefix shorter than this suggests nothing (every street starts with one letter).
pub const SUGGEST_MIN_CHARS: usize = 3;
/// How many matching streets are read before ranking: enough for the common starts, bounded for "ma".
pub const SUGGEST_SCAN: usize = 5_000;
/// How many of the biggest matching streets are searched for the typed house number.
pub const SUGGEST_HOUSE_LOOKUP: usize = 200;

/// A word typed as a house number at the end of a street ("1", "58a", "12-14", "5/2"): starts with a digit, short,
/// letters, digits, "-" and "/" only.
fn is_house_number(word: &str) -> bool {
    word.chars().next().is_some_and(|c| c.is_ascii_digit())
        && word.chars().count() <= 8
        && word
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '/')
}

/// A rectangle in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BBox {
    pub min_lat: f64,
    pub min_lon: f64,
    pub max_lat: f64,
    pub max_lon: f64,
}

impl BBox {
    /// `MINLON,MINLAT,MAXLON,MAXLAT`, the order of a GeoJSON `bbox`.
    pub fn parse(raw: &str) -> std::result::Result<BBox, String> {
        let parts: Vec<f64> = raw
            .split(',')
            .map(|p| p.trim().parse::<f64>())
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| "expected MINLON,MINLAT,MAXLON,MAXLAT as numbers".to_string())?;
        let &[min_lon, min_lat, max_lon, max_lat] = parts.as_slice() else {
            return Err("expected four numbers: MINLON,MINLAT,MAXLON,MAXLAT".to_string());
        };
        if !(min_lat <= max_lat && min_lon <= max_lon) {
            return Err("expected MINLON <= MAXLON and MINLAT <= MAXLAT".to_string());
        }
        if !(min_lat >= -90.0 && max_lat <= 90.0 && min_lon >= -180.0 && max_lon <= 180.0) {
            return Err("the box must lie within WGS84 degrees".to_string());
        }
        Ok(BBox {
            min_lat,
            min_lon,
            max_lat,
            max_lon,
        })
    }

    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        (self.min_lat..=self.max_lat).contains(&lat) && (self.min_lon..=self.max_lon).contains(&lon)
    }
}

/// A point and a radius in metres: `LAT,LON,METERS`.
pub fn parse_circle(raw: &str) -> std::result::Result<(f64, f64, f64), String> {
    let parts: Vec<f64> = raw
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| "expected LAT,LON,METERS as numbers".to_string())?;
    let &[lat, lon, metres] = parts.as_slice() else {
        return Err("expected three numbers: LAT,LON,METERS".to_string());
    };
    if !(lat.abs() <= 90.0 && lon.abs() <= 180.0 && metres > 0.0 && metres.is_finite()) {
        return Err("expected WGS84 degrees and a positive radius in metres".to_string());
    }
    Ok((lat, lon, metres))
}

/// One polygon: its outer ring and holes (lat, lon x 1e7), with a bounding box.
#[derive(Clone, Debug)]
struct Polygon {
    rings: Vec<Vec<(i32, i32)>>,
    min_lat: i32,
    min_lon: i32,
    max_lat: i32,
    max_lon: i32,
}

/// Polygons a point may fall in, each read on its own, so two that overlap do not cancel out.
#[derive(Clone, Debug, Default)]
pub struct PolygonSet {
    polygons: Vec<Polygon>,
}

impl PolygonSet {
    /// A GeoJSON `Polygon`, `MultiPolygon`, `Feature` or `FeatureCollection` text. Anything else in a
    /// collection is refused rather than dropped: a filter that silently loses an area answers wrongly.
    pub fn from_geojson(text: &str) -> Result<PolygonSet> {
        let value: serde_json::Value =
            serde_json::from_str(text).context("the polygon file is not JSON")?;
        let mut set = PolygonSet::default();
        set.add_value(&value)?;
        if set.polygons.is_empty() {
            bail!("the polygon file holds no polygon");
        }
        Ok(set)
    }

    fn add_value(&mut self, value: &serde_json::Value) -> Result<()> {
        match value["type"].as_str() {
            Some("FeatureCollection") => {
                let features = value["features"]
                    .as_array()
                    .ok_or_else(|| anyhow!("a FeatureCollection without features"))?;
                features.iter().try_for_each(|f| self.add_value(f))
            }
            Some("Feature") => self.add_value(&value["geometry"]),
            Some("Polygon") => self.add_polygon(&value["coordinates"]),
            Some("MultiPolygon") => value["coordinates"]
                .as_array()
                .ok_or_else(|| anyhow!("a MultiPolygon without coordinates"))?
                .iter()
                .try_for_each(|p| self.add_polygon(p)),
            other => bail!("expected Polygon or MultiPolygon, found {other:?}"),
        }
    }

    fn add_polygon(&mut self, coordinates: &serde_json::Value) -> Result<()> {
        let bad = || anyhow!("a polygon ring must be a list of [lon, lat] numbers");
        let mut rings = Vec::new();
        for ring in coordinates.as_array().ok_or_else(bad)? {
            let mut points = Vec::new();
            for point in ring.as_array().ok_or_else(bad)? {
                let (lon, lat) = (
                    point[0].as_f64().ok_or_else(bad)?,
                    point[1].as_f64().ok_or_else(bad)?,
                );
                if lat.abs() > 90.0 || lon.abs() > 180.0 {
                    bail!("a polygon point lies outside WGS84 degrees: [{lon}, {lat}]");
                }
                points.push(((lat * 1e7) as i32, (lon * 1e7) as i32));
            }
            rings.push(points);
        }
        self.push_rings(rings);
        Ok(())
    }

    pub(super) fn push_rings(&mut self, rings: Vec<Vec<(i32, i32)>>) {
        let all = || rings.iter().flatten();
        let (Some(min_lat), Some(max_lat), Some(min_lon), Some(max_lon)) = (
            all().map(|p| p.0).min(),
            all().map(|p| p.0).max(),
            all().map(|p| p.1).min(),
            all().map(|p| p.1).max(),
        ) else {
            return;
        };
        self.polygons.push(Polygon {
            rings,
            min_lat,
            min_lon,
            max_lat,
            max_lon,
        });
    }

    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        let (la, lo) = ((lat * 1e7) as i32, (lon * 1e7) as i32);
        self.polygons.iter().any(|p| {
            la >= p.min_lat
                && la <= p.max_lat
                && lo >= p.min_lon
                && lo <= p.max_lon
                && point_in_rings(la, lo, &p.rings)
        })
    }
}

/// Named areas from a polygon file in the `<sheet>_admin.bin` format, found by a grid of one degree: a layer of
/// every census tract of a country (85,000 areas) has to answer a point without reading them all.
pub struct RegionSet {
    regions: Vec<super::AdminRegion>,
    cells: HashMap<(i32, i32), Vec<u32>>,
}

const REGION_CELL: i32 = 10_000_000; // one degree, in the file's units of 1e-7 degree

impl RegionSet {
    pub fn load(path: &std::path::Path) -> Result<RegionSet> {
        let bytes =
            std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        RegionSet::from_bytes(&bytes).with_context(|| format!("in {}", path.display()))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<RegionSet> {
        if bytes.len() <= 8 || &bytes[..4] != b"WOFA" {
            bail!("not a polygon file");
        }
        let regions = super::parse_admin_body(bytes);
        let declared = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        if regions.len() != declared {
            bail!(
                "the file names {declared} areas and holds {}: truncated",
                regions.len()
            );
        }
        let mut cells: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (at, r) in regions.iter().enumerate() {
            for la in r.min_lat.div_euclid(REGION_CELL)..=r.max_lat.div_euclid(REGION_CELL) {
                for lo in r.min_lon.div_euclid(REGION_CELL)..=r.max_lon.div_euclid(REGION_CELL) {
                    cells.entry((la, lo)).or_default().push(at as u32);
                }
            }
        }
        Ok(RegionSet { regions, cells })
    }

    pub fn len(&self) -> usize {
        self.regions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// The names of the areas holding a point, in file order.
    pub fn regions_at(&self, lat: f64, lon: f64) -> Vec<&str> {
        let (la, lo) = ((lat * 1e7) as i32, (lon * 1e7) as i32);
        let Some(list) = self
            .cells
            .get(&(la.div_euclid(REGION_CELL), lo.div_euclid(REGION_CELL)))
        else {
            return Vec::new();
        };
        list.iter()
            .map(|&at| &self.regions[at as usize])
            .filter(|r| {
                la >= r.min_lat
                    && la <= r.max_lat
                    && lo >= r.min_lon
                    && lo <= r.max_lon
                    && point_in_rings(la, lo, &r.rings)
            })
            .map(|r| r.name.as_str())
            .collect()
    }
}

/// What to keep; every given part must hold.
#[derive(Clone, Debug, Default)]
pub struct HouseFilter {
    /// street ids; absent means every street
    pub streets: Option<HashSet<u32>>,
    pub postcode: Option<String>,
    pub circle: Option<(f64, f64, f64)>,
    pub bbox: Option<BBox>,
    pub polygons: Option<PolygonSet>,
}

/// The area options as the command line and the Python layer give them.
#[derive(Clone, Debug, Default)]
pub struct Area {
    pub street: Option<String>,
    pub city: Option<String>,
    pub postcode: Option<String>,
    pub circle: Option<(f64, f64, f64)>,
    pub bbox: Option<BBox>,
    pub polygons: Option<PolygonSet>,
    pub region: Option<String>,
}

impl Area {
    pub fn is_empty(&self) -> bool {
        self.street.is_none()
            && self.city.is_none()
            && self.postcode.is_none()
            && self.circle.is_none()
            && self.bbox.is_none()
            && self.polygons.is_none()
            && self.region.is_none()
    }
}

/// How a walk went.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WalkStats {
    pub kept: u64,
    pub range_points: u64,
}

fn postcode_key(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_uppercase)
        .collect()
}

impl Index {
    /// Visit every kept house in street order, numbers ascending within a street.
    pub fn walk_houses(
        &self,
        filter: &HouseFilter,
        visit: &mut dyn FnMut(&StreetHouse) -> ControlFlow<()>,
    ) -> WalkStats {
        let _rules = crate::rules::scope(self.rules);
        let wanted_code = filter.postcode.as_deref().map(postcode_key);
        let mut ids: Vec<u32> = match &filter.streets {
            Some(set) => set.iter().copied().collect(),
            None => (0..self.street_count()).collect(),
        };
        ids.sort_unstable();
        let mut stats = WalkStats::default();
        for sid in ids {
            let m = self.street_meta(sid);
            if let Some(code) = &wanted_code {
                if m.postcode_disp_off != PC_DISP_AMBIGUOUS
                    && postcode_key(&self.postcode_out(&m)) != *code
                {
                    continue;
                }
            }
            let mut stop = false;
            self.street_houses(sid, &m, |house| {
                if stop {
                    return;
                }
                if Some(house.rep) == self.range_marker_rep {
                    stats.range_points += 1;
                    return;
                }
                if !self.keep_house(filter, wanted_code.as_deref(), &m, &house) {
                    return;
                }
                stats.kept += 1;
                stop = visit(&house).is_break();
            });
            if stop {
                break;
            }
        }
        stats
    }

    fn keep_house(
        &self,
        filter: &HouseFilter,
        code: Option<&str>,
        m: &StreetMeta,
        h: &StreetHouse,
    ) -> bool {
        if let Some(code) = code {
            if m.postcode_disp_off == PC_DISP_AMBIGUOUS
                && postcode_key(&self.postcode_for_house(m, h.postcode_off)) != code
            {
                return false;
            }
        }
        if filter.bbox.is_some_and(|b| !b.contains(h.lat, h.lon)) {
            return false;
        }
        if let Some((lat, lon, metres)) = filter.circle {
            if distance_m(lat, lon, h.lat, h.lon) > metres {
                return false;
            }
        }
        filter
            .polygons
            .as_ref()
            .is_none_or(|p| p.contains(h.lat, h.lon))
    }

    /// A point on the street in front of a house: the house is projected on the segment between the two
    /// nearest houses across the street (same street name and commune, the other parity of number, each
    /// within `STREET_POINT_REACH_M`), and the point is half way between the house and that projection.
    /// Without two such neighbours there is no point: nothing is made up.
    pub fn street_point(
        &self,
        lat: f64,
        lon: f64,
        street: &str,
        commune: &str,
        number: u32,
    ) -> Option<(f64, f64)> {
        let _rules = crate::rules::scope(self.rules);
        let coslat = lat.to_radians().cos().max(0.01);
        let xy = |la: f64, lo: f64| ((lo - lon) * 111_320.0 * coslat, (la - lat) * 111_320.0);
        let mut across: Vec<(f64, (f64, f64))> = Vec::new();
        for sid in self.streets_in_square(lat, lon, 1) {
            let m = self.street_meta(sid);
            if self.name(m.name_off) != street || self.commune_name(m.commune_id) != commune {
                continue;
            }
            self.street_houses(sid, &m, |house| {
                if Some(house.rep) == self.range_marker_rep || house.number % 2 == number % 2 {
                    return;
                }
                let p = xy(house.lat, house.lon);
                let d = (p.0 * p.0 + p.1 * p.1).sqrt();
                if d <= STREET_POINT_REACH_M {
                    across.push((d, p));
                }
            });
        }
        across.sort_by(|a, b| a.0.total_cmp(&b.0));
        across.dedup_by(|a, b| (a.1 .0 - b.1 .0).abs() < 0.01 && (a.1 .1 - b.1 .1).abs() < 0.01);
        let (a, b) = (across.first()?.1, across.get(1)?.1);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        if len2 < 1.0 {
            return None;
        }
        // the house sits at the origin
        let t = (-(a.0 * dx + a.1 * dy) / len2).clamp(0.0, 1.0);
        let (px, py) = ((a.0 + t * dx) / 2.0, (a.1 + t * dy) / 2.0);
        Some((lat + py / 111_320.0, lon + px / (111_320.0 * coslat)))
    }

    /// The middle of the closest pair of houses of two streets (each given by name and commune) and their gap
    /// in metres, when the gap is at most `CROSSING_GAP_M`. Houses of the second street go into a grid of
    /// ~100 m cells, so a long street costs one pass.
    pub fn crossing_point(&self, a: (&str, &str), b: (&str, &str)) -> Option<(f64, f64, f64)> {
        let _rules = crate::rules::scope(self.rules);
        let mut points_a = Vec::new();
        let mut points_b = Vec::new();
        for sid in self.streets_matching(|name, commune| {
            (name == a.0 && commune == a.1) || (name == b.0 && commune == b.1)
        }) {
            let m = self.street_meta(sid);
            let is_a = self.name(m.name_off) == a.0;
            self.street_houses(sid, &m, |house| {
                if Some(house.rep) == self.range_marker_rep {
                    return;
                }
                if is_a {
                    points_a.push((house.lat, house.lon));
                } else {
                    points_b.push((house.lat, house.lon));
                }
            });
        }
        let cell =
            |lat: f64, lon: f64| ((lat * 1000.0).floor() as i64, (lon * 1000.0).floor() as i64);
        let mut grid: HashMap<(i64, i64), Vec<(f64, f64)>> = HashMap::new();
        for &(lat, lon) in &points_b {
            grid.entry(cell(lat, lon)).or_default().push((lat, lon));
        }
        type Pair = (f64, (f64, f64), (f64, f64)); // gap, house of the first street, house of the second
        let mut best: Option<Pair> = None;
        for &(lat, lon) in &points_a {
            let (ci, cj) = cell(lat, lon);
            for di in -1..=1 {
                for dj in -1..=1 {
                    for &(blat, blon) in grid.get(&(ci + di, cj + dj)).into_iter().flatten() {
                        let d = distance_m(lat, lon, blat, blon);
                        if best.is_none_or(|(bd, _, _)| d < bd) {
                            best = Some((d, (lat, lon), (blat, blon)));
                        }
                    }
                }
            }
        }
        let (gap, p, q) = best?;
        (gap <= CROSSING_GAP_M).then(|| {
            (
                (p.0 + q.0) / 2.0,
                (p.1 + q.1) / 2.0,
                (gap * 10.0).round() / 10.0,
            )
        })
    }

    /// Whether `street` is the only street of that name within about 3 km of a point (other communes counted): a
    /// person then needs no town to find it.
    pub fn street_name_alone_near(&self, lat: f64, lon: f64, street: &str, commune: &str) -> bool {
        let _rules = crate::rules::scope(self.rules);
        self.streets_in_square(lat, lon, 2).into_iter().all(|sid| {
            let m = self.street_meta(sid);
            self.name(m.name_off) != street || self.commune_name(m.commune_id) == commune
        })
    }

    /// The sheet writes the house number before the street ("12 Main Street": rule `street_type_last`).
    pub fn number_first(&self) -> bool {
        let _rules = crate::rules::scope(self.rules);
        super::street_type_last()
    }

    /// Streets whose name starts as `text` does, for an address field that suggests as one types (`/v1/suggest`).
    ///
    /// The text is read as "[number] street start[, commune start]": a leading number is kept for the label, the
    /// part before the first comma is matched against the start of street names, the part after it against the start
    /// of the commune's name. When that finds nothing and the street part ends in a house number ("Kneza Mihaila 1",
    /// as most countries write it), the number is taken off the end and kept after the street in the label. The
    /// streets with the most addresses come first. With a number typed, a street that has a house of that number
    /// comes before one that has not, and its point is that house (`precision` "house"); otherwise the point is the
    /// street's middle (`precision` "street"), and the label only repeats what was typed.
    pub fn suggest(&self, text: &str, k: usize) -> Vec<Suggestion> {
        let _rules = crate::rules::scope(self.rules);
        let k = crate::query::bound_k(k);
        let (head, tail) = text.split_once(',').unwrap_or((text, ""));
        let head = head.trim();
        let town = normalize(tail);
        let (number, street) = match head.split_once(char::is_whitespace) {
            Some((n, rest)) if n.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
                (Some(n.to_string()), rest)
            }
            _ => (None, head),
        };
        let found = self.suggest_scan(&normalize(street), &town);
        if found.is_empty() && number.is_none() {
            if let Some((rest, n)) = head.rsplit_once(char::is_whitespace) {
                if is_house_number(n) {
                    let after = self.suggest_scan(&normalize(rest), &town);
                    return self.suggestions(after, Some(n), true, k);
                }
            }
        }
        self.suggestions(found, number.as_deref(), false, k)
    }

    /// (addresses, street id) of the streets whose normalized name starts with `prefix`, in a commune starting
    /// with `town` when one is given; at most `SUGGEST_SCAN`.
    fn suggest_scan(&self, prefix: &str, town: &str) -> Vec<(u32, u32)> {
        use fst::automaton::Automaton;
        use fst::{IntoStreamer, Streamer};
        if prefix.chars().count() < SUGGEST_MIN_CHARS {
            return Vec::new();
        }
        let mut found: Vec<(u32, u32)> = Vec::new();
        let mut stream = self
            .streets_fst
            .search(fst::automaton::Str::new(prefix).starts_with())
            .into_stream();
        while let Some((_, id)) = stream.next() {
            let sid = id as u32;
            let m = self.street_meta(sid);
            if !town.is_empty() && !normalize(self.commune_name(m.commune_id)).starts_with(town) {
                continue;
            }
            found.push((m.house_count, sid));
            if found.len() >= SUGGEST_SCAN {
                break;
            }
        }
        found
    }

    /// The scanned streets as suggestions, the biggest first, one per street and commune, the number in the label
    /// where it was typed: before the street, or after it when `number_last`.
    fn suggestions(
        &self,
        mut found: Vec<(u32, u32)>,
        number: Option<&str>,
        number_last: bool,
        k: usize,
    ) -> Vec<Suggestion> {
        found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        // One suggestion per street name and commune. A commune can hold several streets of one name (one per
        // village): with a number typed, the first of them that has that house speaks for the name.
        let mut out: Vec<Suggestion> = Vec::new();
        let mut at: HashMap<(String, String), usize> = HashMap::new();
        let wanted = if number.is_some() {
            SUGGEST_HOUSE_LOOKUP.max(k)
        } else {
            k
        };
        for (_, sid) in found {
            let m = self.street_meta(sid);
            let (street, commune) = (
                self.name(m.name_off).to_string(),
                self.commune_name(m.commune_id).to_string(),
            );
            let slot = at.get(&(street.clone(), commune.clone())).copied();
            if slot.is_some_and(|i| out[i].precision == "house")
                || (slot.is_none() && out.len() >= wanted)
            {
                continue;
            }
            let house = number.and_then(|n| self.house_on_street(sid, &m, n));
            if slot.is_some() && house.is_none() {
                continue;
            }
            let label = match number {
                Some(n) if number_last => format!("{street} {n}, {commune}"),
                Some(n) => format!("{n} {street}, {commune}"),
                None => format!("{street}, {commune}"),
            };
            let (lat, lon) = house.unwrap_or((f64::from(m.lat_c) / 1e7, f64::from(m.lon_c) / 1e7));
            let suggestion = Suggestion {
                label,
                street: street.clone(),
                commune: commune.clone(),
                lat,
                lon,
                precision: if house.is_some() { "house" } else { "street" },
            };
            match slot {
                Some(i) => out[i] = suggestion,
                None => {
                    at.insert((street, commune), out.len());
                    out.push(suggestion);
                }
            }
        }
        // a street that has the typed house first; the sort is stable, so the biggest streets stay first within each
        out.sort_by_key(|s| s.precision != "house");
        out.truncate(k);
        out
    }

    /// The point of the house numbered `typed` on street `sid` ("20", "12b", "5/2"; case ignored), if the sheet has it.
    fn house_on_street(&self, sid: u32, m: &StreetMeta, typed: &str) -> Option<(f64, f64)> {
        let typed = typed.to_lowercase();
        let mut hit = None;
        self.street_houses(sid, m, |house| {
            if hit.is_none()
                && Some(house.rep) != self.range_marker_rep
                && self.house_number(house.number, house.rep).to_lowercase() == typed
            {
                hit = Some((house.lat, house.lon));
            }
        });
        hit
    }

    /// The source key of every street by street id: `nom_voie_norm`, 0x1F, `code_insee` — the key the builder
    /// writes and `tools/sheet_diff.py` joins two build streams on.
    pub fn street_keys(&self) -> Vec<String> {
        use fst::Streamer;
        let mut keys = vec![String::new(); self.street_count() as usize];
        let mut stream = self.streets_fst.stream();
        while let Some((key, id)) = stream.next() {
            if let Some(slot) = keys.get_mut(id as usize) {
                *slot = String::from_utf8_lossy(key).into_owned();
            }
        }
        keys
    }

    /// The suffix of a walked house's number ("bis", "A"; empty when none), as the build stream's `rep`.
    pub fn house_rep(&self, h: &StreetHouse) -> &str {
        self.rep_suffixes
            .get(h.rep as usize)
            .map_or("", String::as_str)
    }

    /// The postcode of a walked house as the sheet shows it.
    pub fn house_postcode(&self, h: &StreetHouse) -> String {
        let _rules = crate::rules::scope(self.rules);
        let m = self.street_meta(h.street_id);
        self.postcode_for_house(&m, h.postcode_off)
    }

    /// The strings of one walked house.
    pub fn house_row(&self, h: &StreetHouse) -> HouseRow {
        let _rules = crate::rules::scope(self.rules);
        let m = self.street_meta(h.street_id);
        HouseRow {
            lat: h.lat,
            lon: h.lon,
            street: self.name(m.name_off).to_string(),
            housenumber: self.house_number(h.number, h.rep),
            postcode: self.postcode_for_house(&m, h.postcode_off),
            commune: self.commune_name(m.commune_id).to_string(),
            region: self.admin_at(h.lat, h.lon),
        }
    }

    /// Up to `limit` kept houses as rows.
    pub fn houses(&self, filter: &HouseFilter, limit: Option<u64>) -> (Vec<HouseRow>, WalkStats) {
        let mut rows = Vec::new();
        let stats = self.walk_houses(filter, &mut |h| {
            rows.push(self.house_row(h));
            if limit.is_some_and(|n| rows.len() as u64 >= n) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        (rows, stats)
    }

    /// Turn the options of a command into a filter. A street is read the way a query reads it, so "Main
    /// St" finds "Main Street": the top answer for the street and its place names the street, and every
    /// block of that name in that commune is kept. `read_as` says what it was read as.
    pub fn house_filter(&self, area: &Area) -> Result<(HouseFilter, Option<String>)> {
        let _rules = crate::rules::scope(self.rules);
        let mut filter = HouseFilter {
            streets: None,
            postcode: area.postcode.clone(),
            circle: area.circle,
            bbox: area.bbox,
            polygons: area.polygons.clone(),
        };
        if let Some(region) = &area.region {
            let found = self.region_polygons(region)?;
            filter.polygons = Some(match filter.polygons.take() {
                None => found,
                Some(_) => bail!("--region and --polygon exclude each other"),
            });
        }
        let mut read_as = None;
        match (&area.street, &area.city) {
            (Some(street), city) => {
                let place = city.as_deref().or(area.postcode.as_deref()).ok_or_else(|| {
                    anyhow!("--street needs --city or --postcode: a street name alone is many streets")
                })?;
                let line = format!("{street}, {place}");
                let top = query_cascade(self, None, &line, 1).into_iter().next();
                let hit = top
                    .filter(|h| !matches!(h.precision, "city" | "approximate"))
                    .ok_or_else(|| anyhow!("no street found for {line:?}"))?;
                filter.streets = Some(self.streets_matching(|name, commune| {
                    name == hit.street && commune == hit.commune
                }));
                read_as = Some(format!("{} in {}", hit.street, hit.commune));
            }
            (None, Some(city)) => {
                let wanted = normalize(city);
                let streets = self.streets_matching(|_, commune| normalize(commune) == wanted);
                if streets.is_empty() {
                    bail!(
                        "no commune named {city:?} on this sheet; a larger area may be a region: \
                         use --region, --bbox or --polygon"
                    );
                }
                filter.streets = Some(streets);
            }
            (None, None) => {}
        }
        Ok((filter, read_as))
    }

    fn streets_matching(&self, keep: impl Fn(&str, &str) -> bool) -> HashSet<u32> {
        (0..self.street_count())
            .filter(|&sid| {
                let m = self.street_meta(sid);
                keep(self.name(m.name_off), self.commune_name(m.commune_id))
            })
            .collect()
    }

    pub(super) fn street_count(&self) -> u32 {
        (self.streets_meta.len() / super::STREET_META_SIZE) as u32
    }

    /// The names of the region polygons beside this sheet (`<stem>_admin.bin`).
    pub fn region_names(&self) -> Vec<&str> {
        self.admin.iter().map(|r| r.name.as_str()).collect()
    }

    fn region_polygons(&self, name: &str) -> Result<PolygonSet> {
        if self.admin.is_empty() {
            bail!("--region needs the polygon file <sheet>_admin.bin beside the sheet; use --bbox or --polygon");
        }
        let wanted = normalize(name);
        let mut set = PolygonSet::default();
        for region in self.admin.iter().filter(|r| normalize(&r.name) == wanted) {
            set.push_rings(region.rings.clone());
        }
        if set.polygons.is_empty() {
            bail!(
                "no region {name:?}; this sheet has: {}",
                self.region_names().join(", ")
            );
        }
        Ok(set)
    }

    /// A uniform sample of the kept houses: reservoir over one walk, memory for `n` rows only.
    pub fn sample_houses(
        &self,
        filter: &HouseFilter,
        n: usize,
        seed: u64,
    ) -> (Vec<HouseRow>, WalkStats) {
        let mut random = SplitMix64(seed);
        let mut picked: Vec<StreetHouse> = Vec::with_capacity(n.min(1 << 20));
        let mut seen = 0u64;
        let stats = self.walk_houses(filter, &mut |h| {
            seen += 1;
            if picked.len() < n {
                picked.push(*h);
            } else if n > 0 {
                let at = random.below(seen);
                if (at as usize) < n {
                    picked[at as usize] = *h;
                }
            }
            ControlFlow::Continue(())
        });
        (picked.iter().map(|h| self.house_row(h)).collect(), stats)
    }

    /// Houses per grid cell (0.01 degree, the cell the sheet indexes streets by), by cell id.
    pub fn house_counts(&self, filter: &HouseFilter) -> (HashMap<u32, u64>, WalkStats) {
        let mut cells: HashMap<u32, u64> = HashMap::new();
        let stats = self.walk_houses(filter, &mut |h| {
            *cells
                .entry(crate::index::cell_of(h.lat, h.lon))
                .or_default() += 1;
            ControlFlow::Continue(())
        });
        (cells, stats)
    }
}

/// The south-west corner of a grid cell id from `index::cell_of`.
pub fn cell_corner(cell: u32) -> (f64, f64) {
    (
        f64::from(cell / 36_000) * 0.01 - 90.0,
        f64::from(cell % 36_000) * 0.01 - 180.0,
    )
}

/// SplitMix64: small, seedable, no dependency. The stream is part of what `--seed` promises.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..bound` (rejection removes the modulo bias).
    fn below(&mut self, bound: u64) -> u64 {
        let zone = u64::MAX - (u64::MAX % bound);
        loop {
            let v = self.next();
            if v < zone {
                return v % bound;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_use_the_radius_of_the_project() {
        // one degree of latitude on a sphere of 6 371 000 m
        let d = distance_m(10.0, 20.0, 11.0, 20.0);
        assert!((d - 111_194.93).abs() < 0.01, "{d}");
        assert_eq!(distance_m(10.0, 20.0, 10.0, 20.0), 0.0);
    }

    #[test]
    fn a_box_is_read_in_geojson_order_and_refused_when_it_cannot_hold_anything() {
        let b = BBox::parse("7.4,43.7,7.5,43.8").unwrap();
        assert!(b.contains(43.75, 7.45));
        assert!(
            !b.contains(7.45, 43.75),
            "latitude and longitude not swapped"
        );
        assert!(BBox::parse("7.5,43.7,7.4,43.8").is_err());
        assert!(BBox::parse("7.4,43.8,7.5,43.7").is_err());
        assert!(BBox::parse("7.4,43.7,7.5").is_err());
        assert!(BBox::parse("7.4,91,7.5,92").is_err());
        assert!(BBox::parse("a,b,c,d").is_err());
    }

    #[test]
    fn a_circle_needs_degrees_and_a_positive_radius() {
        assert_eq!(
            parse_circle("-33.9,151.2,300").unwrap(),
            (-33.9, 151.2, 300.0)
        );
        for bad in [
            "1,2", "1,2,3,4", "91,2,3", "1,181,3", "1,2,0", "1,2,-5", "1,2,inf",
        ] {
            assert!(parse_circle(bad).is_err(), "{bad}");
        }
    }

    const SQUARE: &str = r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[4,4],[6,4],[6,6],[4,6],[4,4]]]}"#;

    #[test]
    fn a_polygon_keeps_its_inside_and_its_hole_stays_out() {
        let set = PolygonSet::from_geojson(SQUARE).unwrap();
        assert!(set.contains(2.0, 2.0));
        assert!(!set.contains(5.0, 5.0), "inside the hole");
        assert!(!set.contains(11.0, 2.0));
        assert!(!set.contains(2.0, 11.0));
    }

    #[test]
    fn overlapping_polygons_do_not_cancel_each_other() {
        // read as one even-odd ring set the overlap would count as outside
        let text = r#"{"type":"MultiPolygon","coordinates":[
            [[[0,0],[10,0],[10,10],[0,10],[0,0]]],
            [[[5,5],[15,5],[15,15],[5,15],[5,5]]]]}"#;
        let set = PolygonSet::from_geojson(text).unwrap();
        assert!(set.contains(7.0, 7.0), "in both");
        assert!(set.contains(12.0, 12.0), "in the second only");
    }

    #[test]
    fn every_geojson_wrapper_is_read_and_anything_else_is_refused() {
        let feature = format!(r#"{{"type":"Feature","properties":{{}},"geometry":{SQUARE}}}"#);
        assert!(PolygonSet::from_geojson(&feature)
            .unwrap()
            .contains(2.0, 2.0));
        let collection = format!(r#"{{"type":"FeatureCollection","features":[{feature}]}}"#);
        assert!(PolygonSet::from_geojson(&collection)
            .unwrap()
            .contains(2.0, 2.0));
        for bad in [
            r#"{"type":"Point","coordinates":[1,2]}"#,
            r#"{"type":"FeatureCollection","features":[]}"#,
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
            r#"{"type":"Polygon","coordinates":[[[200,0],[1,1],[2,2],[200,0]]]}"#,
            "not json",
        ] {
            assert!(PolygonSet::from_geojson(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_seeded_stream_is_fixed_and_stays_below_its_bound() {
        let mut r = SplitMix64(1);
        // the first outputs of SplitMix64 with seed 1 (reference implementation)
        assert_eq!(r.next(), 0x910A_2DEC_8902_5CC1);
        let mut r = SplitMix64(7);
        assert!((0..1000).all(|_| r.below(3) < 3));
    }

    /// A polygon file with square areas `(name, min_lat, min_lon, max_lat, max_lon)` in degrees.
    fn wofa(areas: &[(&str, f64, f64, f64, f64)]) -> Vec<u8> {
        let mut out = b"WOFA".to_vec();
        out.extend_from_slice(&(areas.len() as u32).to_le_bytes());
        let e7 = |d: f64| (d * 1e7) as i32;
        for &(name, a, b, c, d) in areas {
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
            for v in [e7(a), e7(b), e7(c), e7(d)] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&1u16.to_le_bytes());
            let ring = [(a, b), (a, d), (c, d), (c, b), (a, b)];
            out.extend_from_slice(&(ring.len() as u16).to_le_bytes());
            for (la, lo) in ring {
                out.extend_from_slice(&e7(la).to_le_bytes());
                out.extend_from_slice(&e7(lo).to_le_bytes());
            }
        }
        out
    }

    #[test]
    fn a_region_set_names_the_areas_that_hold_a_point() {
        let bytes = wofa(&[
            ("west", 40.0, -75.5, 41.0, -74.6),
            ("east", 40.0, -74.6, 41.0, -73.9),
            ("overlap", 40.5, -74.7, 40.6, -74.5),
        ]);
        let set = RegionSet::from_bytes(&bytes).unwrap();
        assert_eq!(set.len(), 3);
        assert_eq!(set.regions_at(40.2, -75.0), ["west"]);
        assert_eq!(
            set.regions_at(40.2, -74.1),
            ["east"],
            "the same degree cell as its neighbour"
        );
        assert_eq!(set.regions_at(40.55, -74.65), ["west", "overlap"]);
        assert!(set.regions_at(42.5, -75.0).is_empty(), "no cell at all");
        assert!(set.regions_at(40.2, -73.5).is_empty(), "a cell, no area");
    }

    #[test]
    fn a_point_in_the_box_of_an_area_but_outside_its_shape_is_not_in_it() {
        // a triangle with its corner at (0,0); (0.9, 0.1) lies in its box, beyond the slanted side
        let e7 = |d: f64| (d * 1e7) as i32;
        let mut bytes = b"WOFA".to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.push(1);
        bytes.push(b't');
        for v in [e7(0.0), e7(0.0), e7(1.0), e7(1.0)] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.extend_from_slice(&1u16.to_le_bytes());
        let ring = [(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.0, 0.0)];
        bytes.extend_from_slice(&(ring.len() as u16).to_le_bytes());
        for (la, lo) in ring {
            bytes.extend_from_slice(&e7(la).to_le_bytes());
            bytes.extend_from_slice(&e7(lo).to_le_bytes());
        }
        let set = RegionSet::from_bytes(&bytes).unwrap();
        assert_eq!(set.regions_at(0.1, 0.9), ["t"]);
        assert!(set.regions_at(0.9, 0.1).is_empty());
    }

    #[test]
    fn an_area_across_many_cells_is_found_in_each_and_in_negative_cells() {
        let set = RegionSet::from_bytes(&wofa(&[("big", -3.5, -3.5, 2.5, 2.5)])).unwrap();
        for (lat, lon) in [(-3.4, -3.4), (2.4, 2.4), (-0.5, 0.5), (0.5, -0.5)] {
            assert_eq!(set.regions_at(lat, lon), ["big"], "{lat} {lon}");
        }
    }

    #[test]
    fn a_polygon_file_that_is_not_one_or_is_cut_short_is_refused() {
        assert!(RegionSet::from_bytes(b"NOPE1234567").is_err());
        let whole = wofa(&[("a", 0.0, 0.0, 1.0, 1.0), ("b", 1.0, 1.0, 2.0, 2.0)]);
        assert!(RegionSet::from_bytes(&whole[..whole.len() - 10]).is_err());
    }

    #[test]
    fn a_cell_id_maps_back_to_its_corner() {
        let cell = crate::index::cell_of(43.7345, 7.4215);
        let (lat, lon) = cell_corner(cell);
        assert!(
            (lat - 43.73).abs() < 1e-9 && (lon - 7.42).abs() < 1e-9,
            "{lat} {lon}"
        );
    }
}
