//! Publishing a geocoded table without its points: `batch --aggregate` (counts by cell, no coordinates written at
//! all) and `batch --mask` (each point moved by a random distance in a ring, far enough that k addresses of the sheet
//! lie within the move).
//!
//! The aggregate is the safer of the two: a cell with fewer rows than `--min-count` goes into the line `other`, so a
//! lone address is never a cell of its own. The mask is the practice of health geography (donut masking, Hampton et
//! al. 2010; k by addresses, Allshouse et al. 2010): k is counted on the addresses of the sheet, not on households.

use std::collections::BTreeMap;
use std::io::Write;

use anyhow::{bail, Context, Result};

use crate::query::{hit_is_weak, Hit, Index, PolygonSet};

/// How rows are grouped.
pub enum Cells {
    H3(u8),
    Geohash(usize),
    /// named polygons of a GeoJSON file, in file order; a point takes the first that holds it
    Zones(Vec<(String, PolygonSet)>),
}

impl Cells {
    /// `h3:8`, `geohash:6` or a path to a GeoJSON file of zones (each feature named by its `name` or `id` property).
    pub fn parse(spec: &str) -> Result<Cells> {
        let (kind, arg) = spec.split_once(':').unwrap_or((spec, ""));
        match kind {
            "h3" => {
                let res: u8 = if arg.is_empty() {
                    8
                } else {
                    arg.parse().context("--aggregate h3:<0-15>")?
                };
                h3o::Resolution::try_from(res)
                    .map_err(|_| anyhow::anyhow!("--aggregate h3:{res}: the level is 0 to 15"))?;
                Ok(Cells::H3(res))
            }
            "geohash" => {
                let len: usize = if arg.is_empty() {
                    6
                } else {
                    arg.parse().context("--aggregate geohash:<1-12>")?
                };
                if !(1..=12).contains(&len) {
                    bail!("--aggregate geohash:{len}: the length is 1 to 12");
                }
                Ok(Cells::Geohash(len))
            }
            _ => Cells::zones(std::path::Path::new(spec)),
        }
    }

    fn zones(path: &std::path::Path) -> Result<Cells> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "--aggregate {}: not h3:<level>, geohash:<length> or a GeoJSON file",
                path.display()
            )
        })?;
        let value: serde_json::Value =
            serde_json::from_str(&text).context("the zones file is not JSON")?;
        let features: Vec<serde_json::Value> = match value["type"].as_str() {
            Some("FeatureCollection") => value["features"].as_array().cloned().unwrap_or_default(),
            _ => vec![value],
        };
        let mut zones = Vec::new();
        for (i, f) in features.iter().enumerate() {
            let props = &f["properties"];
            let name = [&props["name"], &props["id"], &f["id"]]
                .iter()
                .find_map(|v| match v {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| format!("zone {}", i + 1));
            zones.push((name, PolygonSet::from_geojson(&f.to_string())?));
        }
        if zones.is_empty() {
            bail!("{}: no zones", path.display());
        }
        Ok(Cells::Zones(zones))
    }

    /// The cell of a point; `None` outside every zone.
    pub fn of(&self, lat: f64, lon: f64) -> Option<String> {
        match self {
            Cells::H3(res) => {
                let r = h3o::Resolution::try_from(*res).ok()?;
                Some(h3o::LatLng::new(lat, lon).ok()?.to_cell(r).to_string())
            }
            Cells::Geohash(len) => Some(crate::codes::geohash_encode(lat, lon, *len)),
            Cells::Zones(z) => z
                .iter()
                .find(|(_, p)| p.contains(lat, lon))
                .map(|(n, _)| n.clone()),
        }
    }
}

/// The counts of one cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    pub rows: u64,
    pub at_house: u64,
    pub weak: u64,
}

/// Rows without an answer, rows outside every zone, and cells under the bar have lines of their own names.
pub const NO_ANSWER: &str = "no answer";
pub const OUTSIDE: &str = "outside the zones";
pub const OTHER: &str = "other";

/// Counts by cell; the points are dropped as they are counted.
#[derive(Default)]
pub struct Aggregate {
    cells: BTreeMap<String, Count>,
}

impl Aggregate {
    pub fn add(&mut self, cells: &Cells, hit: Option<&Hit>) {
        let key = match hit {
            None => NO_ANSWER.to_string(),
            Some(h) => cells
                .of(h.lat, h.lon)
                .unwrap_or_else(|| OUTSIDE.to_string()),
        };
        let c = self.cells.entry(key).or_default();
        c.rows += 1;
        if let Some(h) = hit {
            c.at_house += u64::from(h.precision == "house");
            c.weak += u64::from(hit_is_weak(h));
        }
    }

    /// The lines to write: every cell with at least `min_count` rows, then `other` (the cells under the bar
    /// together, their number in the last column), then the rows outside the zones and without an answer.
    pub fn lines(&self, min_count: u64) -> Vec<(String, Count, u64)> {
        let mut out = Vec::new();
        let (mut other, mut merged) = (Count::default(), 0u64);
        for (k, c) in &self.cells {
            if k == NO_ANSWER || k == OUTSIDE {
                continue;
            }
            if c.rows >= min_count {
                out.push((k.clone(), *c, 1));
            } else {
                other.rows += c.rows;
                other.at_house += c.at_house;
                other.weak += c.weak;
                merged += 1;
            }
        }
        if merged > 0 {
            out.push((OTHER.to_string(), other, merged));
        }
        for k in [OUTSIDE, NO_ANSWER] {
            if let Some(c) = self.cells.get(k) {
                out.push((k.to_string(), *c, 0));
            }
        }
        out
    }

    pub fn write(&self, min_count: u64, delimiter: u8, out: &mut dyn Write) -> Result<()> {
        let mut w = csv::WriterBuilder::new()
            .delimiter(delimiter)
            .terminator(csv::Terminator::Any(b'\n'))
            .from_writer(out);
        w.write_record(["gp_cell", "rows", "at_house", "weak", "cells"])?;
        for (k, c, n) in self.lines(min_count) {
            w.write_record([
                k,
                c.rows.to_string(),
                c.at_house.to_string(),
                c.weak.to_string(),
                n.to_string(),
            ])?;
        }
        w.flush()?;
        Ok(())
    }
}

/// `--mask donut:200-500` with an optional `,k:5`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mask {
    pub inner_m: f64,
    pub outer_m: f64,
    pub k: Option<usize>,
    pub seed: u64,
}

impl Mask {
    pub fn parse(spec: &str, seed: u64) -> Result<Mask> {
        let mut mask = Mask {
            inner_m: 0.0,
            outer_m: 0.0,
            k: None,
            seed,
        };
        let mut ring = false;
        for part in spec.split(',').map(str::trim) {
            if let Some(r) = part.strip_prefix("donut:") {
                let (a, b) = r
                    .split_once('-')
                    .context("--mask donut:<inner>-<outer> in metres, as donut:200-500")?;
                let (a, b): (f64, f64) = (a.trim().parse()?, b.trim().parse()?);
                if !(a >= 0.0 && b > a && b <= 100_000.0) {
                    bail!("--mask donut:{a}-{b}: need 0 <= inner < outer <= 100000 metres");
                }
                (mask.inner_m, mask.outer_m, ring) = (a, b, true);
            } else if let Some(k) = part.strip_prefix("k:") {
                let k: usize = k.parse().context("--mask k:<addresses>")?;
                if !(1..=100).contains(&k) {
                    bail!("--mask k:{k}: from 1 to 100 addresses");
                }
                mask.k = Some(k);
            } else {
                bail!("--mask {part:?}: expected donut:<inner>-<outer> and optionally k:<n>");
            }
        }
        if !ring {
            bail!("--mask needs donut:<inner>-<outer> (k alone does not move a point)");
        }
        Ok(mask)
    }

    /// The moved point of row `row`, how far it moved, and how many sheet addresses lie within that distance of the
    /// true point (at least k, unless the place is too sparse for the search: then fewer, and the row says so).
    /// The draw depends on the seed and the row only, so a run is the same whatever the threads.
    pub fn apply(
        &self,
        idx: &Index,
        row: u64,
        lat: f64,
        lon: f64,
    ) -> (f64, f64, f64, Option<usize>) {
        let mut state = self.seed ^ row.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let (u, v) = (unit(&mut state), unit(&mut state));
        // uniform over the area of the ring, not over the radius
        let (r0, r1) = (self.inner_m, self.outer_m);
        let mut d = (u * (r1 * r1 - r0 * r0) + r0 * r0).sqrt();
        let mut within = None;
        if let Some(k) = self.k {
            let near = idx.reverse(lat, lon, k).unwrap_or_default();
            let kth = near
                .iter()
                .filter_map(|h| h.distance_m)
                .fold(0.0f64, f64::max);
            d = d.max(kth);
            within = Some(
                near.iter()
                    .filter(|h| h.distance_m.is_some_and(|m| m <= d))
                    .count(),
            );
        }
        let bearing = v * std::f64::consts::TAU;
        let (dlat, dlon) = (
            d * bearing.cos() / 111_320.0,
            d * bearing.sin() / (111_320.0 * lat.to_radians().cos().max(0.01)),
        );
        (lat + dlat, lon + dlon, d, within)
    }
}

/// splitmix64 to a number in [0, 1).
fn unit(state: &mut u64) -> f64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// A seed from the clock when none is given (printed by the caller, so the run can be repeated).
pub fn fresh_seed() -> u64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    (t as u64) ^ ((t >> 64) as u64) ^ u64::from(std::process::id()).rotate_left(32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_cells_go_into_other_and_the_counts_add_up() {
        let mut a = Aggregate::default();
        let cells = Cells::Geohash(4);
        let hit = |lat: f64, lon: f64| Hit {
            lat,
            lon,
            precision: "house",
            score: 1.0,
            confidence: 0.9,
            street: String::new(),
            housenumber: None,
            commune: String::new(),
            postcode: String::new(),
            flags: vec!["street_exact"],
            region: None,
            distance_m: None,
        };
        for _ in 0..6 {
            a.add(&cells, Some(&hit(43.73, 7.42)));
        }
        for _ in 0..2 {
            a.add(&cells, Some(&hit(48.85, 2.35)));
        }
        a.add(&cells, Some(&hit(40.0, -3.7)));
        a.add(&cells, None);
        let lines = a.lines(5);
        let names: Vec<&str> = lines.iter().map(|l| l.0.as_str()).collect();
        assert_eq!(
            names,
            [
                crate::codes::geohash_encode(43.73, 7.42, 4).as_str(),
                OTHER,
                NO_ANSWER
            ]
        );
        assert_eq!(
            (lines[1].1.rows, lines[1].2),
            (3, 2),
            "two cells of 2 and 1 rows merged"
        );
        assert_eq!(lines.iter().map(|l| l.1.rows).sum::<u64>(), 10);
        assert_eq!(
            a.lines(1).len(),
            4,
            "with a bar of 1 every cell stands alone"
        );
    }

    #[test]
    fn the_mask_spec_is_checked() {
        assert_eq!(
            Mask::parse("donut:200-500, k:5", 1).unwrap(),
            Mask {
                inner_m: 200.0,
                outer_m: 500.0,
                k: Some(5),
                seed: 1
            }
        );
        for bad in [
            "donut:500-200",
            "k:5",
            "donut:200",
            "ring:1-2",
            "donut:0-200000",
        ] {
            assert!(Mask::parse(bad, 1).is_err(), "{bad}");
        }
        assert!(Cells::parse("h3:16").is_err() && Cells::parse("geohash:0").is_err());
        assert!(matches!(Cells::parse("h3").unwrap(), Cells::H3(8)));
    }

    #[test]
    fn the_draw_is_uniform_over_the_ring_and_the_same_for_one_seed() {
        let mut state = 7u64;
        let draws: Vec<f64> = (0..10_000).map(|_| unit(&mut state)).collect();
        assert!(draws.iter().all(|u| (0.0..1.0).contains(u)));
        let mean = draws.iter().sum::<f64>() / draws.len() as f64;
        assert!((mean - 0.5).abs() < 0.02, "{mean}");
    }
}
