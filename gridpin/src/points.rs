//! `gridpin nearest` and `gridpin join`: work on two lists of your own, by straight-line distance and by zone.
//!
//! A file holds points when it has `lat` and `lon` columns (or `latitude`, `longitude`); otherwise its address
//! column is geocoded on the sheet given, the way `gridpin batch` does it. Rows keep their order and their
//! columns. The distance is the project's one great-circle formula; a driving time is a routing engine's job.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use rayon::prelude::*;

use crate::index::cell_of;
use crate::query::{distance_m, Hit, PolygonSet};
use crate::tabular::{coordinate_column, query_columns, query_text, Format, LAT_NAMES, LON_NAMES};

const CELL_DEGREES: f64 = 0.01;
const METRES_PER_DEGREE: f64 = 111_194.0;
/// Rings of cells searched before the whole list is read: about 35 km at the equator.
const MAX_RINGS: i64 = 32;

/// One table read from a file, with a point per row where it has one.
pub struct Points {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub points: Vec<Option<(f64, f64)>>,
    /// the answer behind each point, when the file was geocoded
    pub hits: Option<Vec<Option<Hit>>>,
}

fn delimiter(path: &Path) -> Result<u8> {
    Ok(match Format::of_input(None, path)? {
        Format::Tsv => b'\t',
        _ => b',',
    })
}

fn coordinate_columns(headers: &csv::StringRecord) -> Option<(usize, usize)> {
    Some((
        coordinate_column(headers, &LAT_NAMES)?,
        coordinate_column(headers, &LON_NAMES)?,
    ))
}

fn parse_point(lat: Option<&str>, lon: Option<&str>) -> Option<(f64, f64)> {
    let lat: f64 = lat?.trim().parse().ok()?;
    let lon: f64 = lon?.trim().parse().ok()?;
    (lat.abs() <= 90.0 && lon.abs() <= 180.0).then_some((lat, lon))
}

/// The sheet behind a file without coordinates: the top answers of one line.
pub type Geocode<'a> = dyn Fn(&str) -> Vec<Hit> + Sync + 'a;

/// Read a file. With coordinates in it the sheet is not needed; `column` picks the address column otherwise.
pub fn read_points(path: &Path, column: Option<&str>, geocode: Option<&Geocode>) -> Result<Points> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter(path)?)
        .from_reader(crate::tabular::open_table(path)?);
    let headers = reader.headers()?.clone();
    let mut rows = Vec::new();
    for record in reader.records() {
        rows.push(record.with_context(|| format!("in {}", path.display()))?);
    }
    let names: Vec<String> = headers.iter().map(String::from).collect();
    let cells = |rows: &[csv::StringRecord]| -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(String::from).collect())
            .collect()
    };
    if column.is_none() {
        if let Some((lat, lon)) = coordinate_columns(&headers) {
            let points = rows
                .iter()
                .map(|r| parse_point(r.get(lat), r.get(lon)))
                .collect();
            return Ok(Points {
                headers: names,
                rows: cells(&rows),
                points,
                hits: None,
            });
        }
    }
    let geocode = geocode.ok_or_else(|| {
        anyhow!(
            "{} has no lat and lon columns: give a sheet with --sheet to geocode its addresses",
            path.display()
        )
    })?;
    let cols = query_columns(&headers, column)?;
    let hits: Vec<Option<Hit>> = rows
        .par_iter()
        .map(|r| {
            let q = query_text(r, &cols);
            if q.is_empty() {
                None
            } else {
                geocode(&q).into_iter().next()
            }
        })
        .collect();
    Ok(Points {
        headers: names,
        rows: cells(&rows),
        points: hits
            .iter()
            .map(|h| h.as_ref().map(|h| (h.lat, h.lon)))
            .collect(),
        hits: Some(hits),
    })
}

/// A list of points indexed by grid cell. Finding a nearest point reads rings of cells around the query and
/// stops when no cell farther out can hold anything closer.
pub struct NearestIndex {
    points: Vec<(usize, f64, f64)>,
    cells: HashMap<u32, Vec<u32>>,
}

/// One answer: the row of the list, and how far.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Near {
    pub row: usize,
    pub metres: f64,
}

impl NearestIndex {
    pub fn new(points: &[Option<(f64, f64)>]) -> NearestIndex {
        let points: Vec<(usize, f64, f64)> = points
            .iter()
            .enumerate()
            .filter_map(|(row, p)| p.map(|(lat, lon)| (row, lat, lon)))
            .collect();
        let mut cells: HashMap<u32, Vec<u32>> = HashMap::new();
        for (at, &(_, lat, lon)) in points.iter().enumerate() {
            cells.entry(cell_of(lat, lon)).or_default().push(at as u32);
        }
        NearestIndex { points, cells }
    }

    /// The `k` nearest points, closest first; equal distances in file order; none beyond `max_m`.
    pub fn nearest(&self, lat: f64, lon: f64, k: usize, max_m: Option<f64>) -> Vec<Near> {
        let k = k.min(self.points.len());
        let mut found: Vec<Near> = Vec::new();
        if k == 0 {
            return found;
        }
        let cell = cell_of(lat, lon);
        let (base_la, base_lo) = (i64::from(cell / 36_000), i64::from(cell % 36_000));
        let limit = max_m.unwrap_or(f64::INFINITY);
        let mut seen = 0usize;
        for ring in 0..=MAX_RINGS {
            for (la, lo) in ring_cells(base_la, base_lo, ring) {
                let Some(list) = self.cells.get(&((la * 36_000 + lo) as u32)) else {
                    continue;
                };
                seen += list.len();
                for &at in list {
                    self.consider(at as usize, lat, lon, limit, &mut found);
                }
            }
            if seen == self.points.len() {
                return closest(found, k);
            }
            // everything outside this ring is at least this far; cells narrow towards the poles
            let safe = f64::from(ring as i32)
                * CELL_DEGREES
                * METRES_PER_DEGREE
                * (lat.abs() + (ring + 1) as f64 * CELL_DEGREES)
                    .to_radians()
                    .cos()
                    .max(0.0);
            let done = {
                found.sort_by(order);
                found.len() >= k && found[k - 1].metres <= safe
            };
            if done || safe > limit {
                return closest(found, k);
            }
        }
        found.clear();
        for at in 0..self.points.len() {
            self.consider(at, lat, lon, limit, &mut found);
        }
        closest(found, k)
    }

    fn consider(&self, at: usize, lat: f64, lon: f64, limit: f64, found: &mut Vec<Near>) {
        let (row, p_lat, p_lon) = self.points[at];
        let metres = distance_m(lat, lon, p_lat, p_lon);
        if metres <= limit {
            found.push(Near { row, metres });
        }
    }
}

fn order(a: &Near, b: &Near) -> std::cmp::Ordering {
    a.metres.total_cmp(&b.metres).then(a.row.cmp(&b.row))
}

fn closest(mut found: Vec<Near>, k: usize) -> Vec<Near> {
    found.sort_by(order);
    found.truncate(k);
    found
}

/// The cells of one square ring; longitude wraps at the antimeridian, latitude stops at the poles.
fn ring_cells(la: i64, lo: i64, ring: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    for dla in -ring..=ring {
        let row = la + dla;
        if !(0..18_000).contains(&row) {
            continue;
        }
        let step = if dla.abs() == ring {
            1
        } else {
            (2 * ring).max(1)
        };
        let mut dlo = -ring;
        while dlo <= ring {
            out.push((row, (lo + dlo).rem_euclid(36_000)));
            dlo += step;
        }
    }
    out
}

/// What `nearest` was asked.
pub struct NearestOptions<'a> {
    pub k: usize,
    pub max_km: Option<f64>,
    pub id_column: Option<&'a str>,
}

/// What a run did, for the closing lines on stderr.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub rows: usize,
    pub rows_without_point: usize,
    pub list: usize,
    pub list_without_point: usize,
    pub rows_without_answer: usize,
}

fn id_at(list: &Points, column: Option<&str>) -> Result<Option<usize>> {
    match column {
        Some(name) => list
            .headers
            .iter()
            .position(|h| h.eq_ignore_ascii_case(name))
            .map(Some)
            .ok_or_else(|| anyhow!("no column {name:?} in the second file")),
        None => Ok(list
            .headers
            .iter()
            .position(|h| h.eq_ignore_ascii_case("id"))),
    }
}

/// The columns a geocoded file adds, so a reader can see how a row was placed.
fn placed_cells(file: &Points, row: usize) -> Vec<String> {
    let Some(hits) = &file.hits else {
        return Vec::new();
    };
    match &hits[row] {
        Some(h) => vec![
            h.lat.to_string(),
            h.lon.to_string(),
            h.precision.to_string(),
            crate::query::hit_is_weak(h).to_string(),
        ],
        None => vec![String::new(); PLACED.len()],
    }
}

const PLACED: [&str; 4] = ["gp_lat", "gp_lon", "gp_precision", "gp_weak"];

/// For each row of `rows`, its nearest of `list`, written as CSV: the row's columns, then the answer's.
pub fn write_nearest<W: Write>(
    rows: &Points,
    list: &Points,
    options: &NearestOptions,
    out: W,
) -> Result<Counts> {
    if options.k == 0 {
        bail!("nearest: --k must be at least 1");
    }
    if options
        .max_km
        .is_some_and(|km| !km.is_finite() || km <= 0.0)
    {
        bail!("nearest: --max-km must be a positive distance");
    }
    let id = id_at(list, options.id_column)?;
    let index = NearestIndex::new(&list.points);
    let max_m = options.max_km.map(|km| km * 1000.0);
    let answers: Vec<Vec<Near>> = rows
        .points
        .par_iter()
        .map(|p| match p {
            Some((lat, lon)) => index.nearest(*lat, *lon, options.k, max_m),
            None => Vec::new(),
        })
        .collect();
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(out);
    let mut head: Vec<&str> = rows.headers.iter().map(String::as_str).collect();
    if rows.hits.is_some() {
        head.extend(PLACED);
    }
    head.extend([
        "nearest_id",
        "nearest_distance_m",
        "nearest_lat",
        "nearest_lon",
    ]);
    if options.k > 1 {
        head.push("nearest_rank");
    }
    writer.write_record(&head)?;
    let mut counts = Counts {
        rows: rows.rows.len(),
        rows_without_point: rows.points.iter().filter(|p| p.is_none()).count(),
        list: list.rows.len(),
        list_without_point: list.points.iter().filter(|p| p.is_none()).count(),
        rows_without_answer: 0,
    };
    for (at, found) in answers.iter().enumerate() {
        let start: Vec<String> = rows.rows[at]
            .iter()
            .cloned()
            .chain(placed_cells(rows, at))
            .collect();
        if found.is_empty() {
            counts.rows_without_answer += 1;
            let blanks = 4 + usize::from(options.k > 1);
            writer.write_record(start.iter().map(String::as_str).chain(vec![""; blanks]))?;
            continue;
        }
        for (rank, near) in found.iter().enumerate() {
            let (lat, lon) = list.points[near.row].expect("the index holds rows with a point");
            let id = match id {
                Some(col) => list.rows[near.row][col].clone(),
                None => (near.row + 1).to_string(),
            };
            let mut line = start.clone();
            line.extend([
                id,
                format!("{:.1}", near.metres),
                lat.to_string(),
                lon.to_string(),
            ]);
            if options.k > 1 {
                line.push((rank + 1).to_string());
            }
            writer.write_record(&line)?;
        }
    }
    writer.flush()?;
    Ok(counts)
}

/// One zone of `gridpin join`: its polygons and properties.
pub struct Zone {
    polygons: PolygonSet,
    properties: serde_json::Map<String, serde_json::Value>,
}

/// Each `Feature` of a GeoJSON file with a polygon, one zone per feature.
pub fn read_zones(text: &str) -> Result<Vec<Zone>> {
    let value: serde_json::Value =
        serde_json::from_str(text).context("the zones file is not JSON")?;
    let features: Vec<serde_json::Value> = match value["type"].as_str() {
        Some("FeatureCollection") => value["features"]
            .as_array()
            .cloned()
            .ok_or_else(|| anyhow!("a FeatureCollection without features"))?,
        Some("Feature") => vec![value],
        other => bail!("the zones file must hold features, found {other:?}"),
    };
    let mut zones = Vec::new();
    for (i, feature) in features.iter().enumerate() {
        let polygons = PolygonSet::from_geojson(&feature["geometry"].to_string())
            .with_context(|| format!("feature {}", i + 1))?;
        let properties = feature["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        zones.push(Zone {
            polygons,
            properties,
        });
    }
    if zones.is_empty() {
        bail!("the zones file holds no feature");
    }
    Ok(zones)
}

fn property_text(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Every row with the properties of the first zone holding its point, in columns `zone_<property>`.
/// `properties` limits the columns; by default every property any zone has, in the order first seen.
pub fn write_join<W: Write>(
    rows: &Points,
    zones: &[Zone],
    properties: Option<&[String]>,
    out: W,
) -> Result<Counts> {
    let columns: Vec<String> = match properties {
        Some(names) => {
            for name in names {
                if !zones.iter().any(|z| z.properties.contains_key(name)) {
                    bail!("no zone has the property {name:?}");
                }
            }
            names.to_vec()
        }
        None => {
            let mut all: Vec<String> = Vec::new();
            for zone in zones {
                for key in zone.properties.keys() {
                    if !all.contains(key) {
                        all.push(key.clone());
                    }
                }
            }
            all
        }
    };
    let zone_of: Vec<Option<usize>> = rows
        .points
        .par_iter()
        .map(|p| {
            let (lat, lon) = (*p)?;
            zones.iter().position(|z| z.polygons.contains(lat, lon))
        })
        .collect();
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(out);
    let mut head: Vec<String> = rows.headers.clone();
    if rows.hits.is_some() {
        head.extend(PLACED.map(String::from));
    }
    head.extend(columns.iter().map(|c| format!("zone_{c}")));
    writer.write_record(&head)?;
    let mut counts = Counts {
        rows: rows.rows.len(),
        rows_without_point: rows.points.iter().filter(|p| p.is_none()).count(),
        ..Counts::default()
    };
    for (at, zone) in zone_of.iter().enumerate() {
        let mut line: Vec<String> = rows.rows[at].clone();
        line.extend(placed_cells(rows, at));
        match zone {
            Some(z) => line.extend(
                columns
                    .iter()
                    .map(|c| property_text(zones[*z].properties.get(c))),
            ),
            None => {
                counts.rows_without_answer += 1;
                line.extend(columns.iter().map(|_| String::new()));
            }
        }
        writer.write_record(&line)?;
    }
    writer.flush()?;
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a scan of the whole list answers.
    fn scan(
        points: &[Option<(f64, f64)>],
        lat: f64,
        lon: f64,
        k: usize,
        max_m: Option<f64>,
    ) -> Vec<Near> {
        let mut all: Vec<Near> = points
            .iter()
            .enumerate()
            .filter_map(|(row, p)| {
                let (a, b) = (*p)?;
                let metres = distance_m(lat, lon, a, b);
                (metres <= max_m.unwrap_or(f64::INFINITY)).then_some(Near { row, metres })
            })
            .collect();
        all.sort_by(order);
        all.truncate(k);
        all
    }

    /// A small fixed stream of numbers in 0..1.
    fn stream(seed: u64) -> impl FnMut() -> f64 {
        let mut x = seed;
        move || {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (x >> 40) as f64 / (1u64 << 24) as f64
        }
    }

    #[test]
    fn the_index_answers_what_a_scan_of_the_list_answers() {
        // three places of very different density, and one list far from every query: the rings stop
        // early only when nothing outside can be closer, at any latitude and across the antimeridian
        for (centre_lat, centre_lon, spread) in [
            (48.0, 2.0, 0.05),
            (48.0, 2.0, 3.0),
            (70.0, 20.0, 1.0),
            (-17.0, 179.99, 0.3),
            (0.0, 0.0, 40.0),
        ] {
            let mut next = stream(7);
            let mut p = |c: f64, max: f64| (c + (next() - 0.5) * spread).clamp(-max, max);
            let list: Vec<Option<(f64, f64)>> = (0..300)
                .map(|_| {
                    let (a, b) = (p(centre_lat, 89.0), p(centre_lon, 180.0));
                    Some((a, b))
                })
                .collect();
            let index = NearestIndex::new(&list);
            for _ in 0..200 {
                let (lat, lon) = (p(centre_lat, 89.0), p(centre_lon, 180.0));
                for k in [1, 3] {
                    let (got, want) = (
                        index.nearest(lat, lon, k, None),
                        scan(&list, lat, lon, k, None),
                    );
                    assert_eq!(got, want, "{lat} {lon} k={k}");
                }
            }
        }
    }

    #[test]
    fn across_the_antimeridian_the_neighbour_is_the_near_one() {
        // row 1 is on the query's side and within the first rings; row 0 is closer, across the line
        let list = [Some((-17.0, -179.999)), Some((-17.0, 179.995))];
        let index = NearestIndex::new(&list);
        let found = index.nearest(-17.0, 179.999, 1, None);
        assert_eq!(found[0].row, 0, "{found:?}");
        assert!(found[0].metres < 300.0);
    }

    #[test]
    fn a_limit_keeps_what_is_within_it_and_rows_without_a_point_are_not_in_the_list() {
        let list = [None, Some((10.0, 10.0)), Some((10.0, 10.1))];
        let index = NearestIndex::new(&list);
        let found = index.nearest(10.0, 10.0, 5, Some(1000.0));
        assert_eq!(found.iter().map(|n| n.row).collect::<Vec<_>>(), [1]);
        assert_eq!(index.nearest(10.0, 10.0, 5, None).len(), 2);
        assert!(index.nearest(20.0, 20.0, 1, Some(1000.0)).is_empty());
        // within the rings read, yet beyond the limit
        assert!(index.nearest(10.0, 10.004, 1, Some(100.0)).is_empty());
    }

    #[test]
    fn equal_distances_keep_the_order_of_the_file() {
        let list = [Some((10.0, 10.001)), Some((10.0, 9.999))];
        let found = NearestIndex::new(&list).nearest(10.0, 10.0, 2, None);
        assert_eq!(found[0].metres.round(), found[1].metres.round());
        assert_eq!(found[0].row, 0);
    }

    #[test]
    fn a_ring_is_its_outline_only() {
        assert_eq!(ring_cells(100, 100, 0), [(100, 100)]);
        let ring = ring_cells(100, 100, 2);
        assert_eq!(ring.len(), 16);
        assert!(
            !ring.contains(&(100, 100)) && ring.contains(&(102, 102)) && ring.contains(&(100, 98))
        );
        assert!(
            ring_cells(100, 35_999, 1).contains(&(100, 0)),
            "wraps at 180"
        );
        assert!(ring_cells(0, 100, 1).iter().all(|&(la, _)| la >= 0));
    }

    #[test]
    fn coordinates_are_read_only_when_they_are_coordinates() {
        assert_eq!(parse_point(Some(" 1.5"), Some("2.5 ")), Some((1.5, 2.5)));
        for (a, b) in [
            (Some(""), Some("1")),
            (Some("91"), Some("1")),
            (Some("1"), Some("181")),
            (Some("x"), Some("1")),
            (None, Some("1")),
        ] {
            assert_eq!(parse_point(a, b), None);
        }
    }
}
