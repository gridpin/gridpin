//! `gridpin postcodes <sheet> --res 9`: an outline of each postcode, drawn from the points of its houses.
//!
//! A postcode's area is the set of H3 cells (resolution `res`, default 9: about 0.1 km²) that hold its houses, with a
//! cell whose six neighbours all belong to the code added (holes of one cell filled). The outline is stitched from
//! the cells' edges: an edge two cells of the code share is inside, the rest form rings; a ring turning clockwise is
//! a hole and goes to the outer ring that holds it. Written as GeoJSON, one MultiPolygon feature per code with
//! `postcode`, `addresses` and `cells`. An outline of where the houses are, not the postal service's own boundary.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::ControlFlow;

use anyhow::{anyhow, Result};
use h3o::{CellIndex, LatLng, Resolution};
use serde_json::{json, Value};

use crate::query::{HouseFilter, Index, PolygonSet};

type Pt = (i64, i64);

/// Vertices rounded to 1e-9 degree, so the two cells of an edge name it alike.
fn key(ll: LatLng) -> Pt {
    (
        (ll.lng() * 1e9).round() as i64,
        (ll.lat() * 1e9).round() as i64,
    )
}

fn filled(cells: &HashSet<CellIndex>) -> HashSet<CellIndex> {
    let mut out = cells.clone();
    for c in cells {
        for n in c.grid_disk::<Vec<_>>(1) {
            if !out.contains(&n)
                && n.grid_disk::<Vec<_>>(1)
                    .iter()
                    .filter(|m| **m != n)
                    .all(|m| cells.contains(m))
            {
                out.insert(n);
            }
        }
    }
    out
}

fn area2(ring: &[Pt]) -> f64 {
    ring.windows(2)
        .map(|w| w[0].0 as f64 * w[1].1 as f64 - w[1].0 as f64 * w[0].1 as f64)
        .sum()
}

/// The rings of a set of cells: (outer rings counter-clockwise, holes clockwise), each closed.
pub fn rings(cells: &HashSet<CellIndex>) -> Vec<Vec<Pt>> {
    let mut edges: HashSet<(Pt, Pt)> = HashSet::new();
    for c in cells {
        let b: Vec<Pt> = c.boundary().iter().map(|ll| key(*ll)).collect();
        for i in 0..b.len() {
            let (a, z) = (b[i], b[(i + 1) % b.len()]);
            if !edges.remove(&(z, a)) {
                edges.insert((a, z));
            }
        }
    }
    let mut next: HashMap<Pt, Vec<Pt>> = HashMap::new();
    for (a, z) in &edges {
        next.entry(*a).or_default().push(*z);
    }
    let mut out = Vec::new();
    while let Some(start) = next.keys().next().copied() {
        let mut ring = vec![start];
        let mut at = start;
        while let Some(list) = next.get_mut(&at) {
            let Some(z) = list.pop() else { break };
            if list.is_empty() {
                next.remove(&at);
            }
            ring.push(z);
            at = z;
            if z == start {
                break;
            }
        }
        if ring.len() > 3 && ring.first() == ring.last() {
            out.push(ring);
        }
    }
    out
}

fn to_lonlat(ring: &[Pt]) -> Vec<[f64; 2]> {
    ring.iter()
        .map(|(x, y)| [*x as f64 / 1e9, *y as f64 / 1e9])
        .collect()
}

fn inside(pt: Pt, ring: &[Pt]) -> bool {
    let (x, y) = (pt.0 as f64, pt.1 as f64);
    let mut c = false;
    for w in ring.windows(2) {
        let ((x1, y1), (x2, y2)) = (
            (w[0].0 as f64, w[0].1 as f64),
            (w[1].0 as f64, w[1].1 as f64),
        );
        if (y1 > y) != (y2 > y) && x < (x2 - x1) * (y - y1) / (y2 - y1) + x1 {
            c = !c;
        }
    }
    c
}

/// The MultiPolygon coordinates of a set of cells.
pub fn multipolygon(cells: &HashSet<CellIndex>) -> Vec<Vec<Vec<[f64; 2]>>> {
    let all = rings(cells);
    let (outer, holes): (Vec<_>, Vec<_>) = all.into_iter().partition(|r| area2(r) > 0.0);
    let mut polys: Vec<Vec<Vec<Pt>>> = outer.into_iter().map(|r| vec![r]).collect();
    for h in holes {
        if let Some(p) = polys.iter_mut().find(|p| inside(h[0], &p[0])) {
            p.push(h);
        }
    }
    polys
        .iter()
        .map(|p| p.iter().map(|r| to_lonlat(r)).collect())
        .collect()
}

/// The area of a code (its cells with one-cell holes filled) and its MultiPolygon.
pub fn outline(cells: &HashSet<CellIndex>) -> (usize, Vec<Vec<Vec<[f64; 2]>>>) {
    let area = filled(cells);
    (area.len(), multipolygon(&area))
}

pub fn postcodes(idx: &Index, res: u8) -> Result<Value> {
    let r = Resolution::try_from(res).map_err(|_| anyhow!("--res {res}: a resolution 0..=15"))?;
    let mut by_code: BTreeMap<String, (u64, HashSet<CellIndex>)> = BTreeMap::new();
    idx.walk_houses(&HouseFilter::default(), &mut |h| {
        let row = idx.house_row(h);
        if !row.postcode.is_empty() {
            if let Ok(ll) = LatLng::new(h.lat, h.lon) {
                let e = by_code.entry(row.postcode).or_default();
                e.0 += 1;
                e.1.insert(ll.to_cell(r));
            }
        }
        ControlFlow::Continue(())
    });
    let features: Vec<Value> = by_code
        .into_iter()
        .map(|(code, (n, cells))| {
            let (count, coords) = outline(&cells);
            json!({
                "type": "Feature",
                "properties": {"postcode": code, "addresses": n, "cells": count},
                "geometry": {"type": "MultiPolygon", "coordinates": coords},
            })
        })
        .collect();
    Ok(json!({"type": "FeatureCollection", "features": features}))
}

/// Does the outline of a feature hold (lat, lon)? Read back through the engine's own GeoJSON reader.
pub fn holds(feature: &Value, lat: f64, lon: f64) -> bool {
    PolygonSet::from_geojson(&feature["geometry"].to_string()).is_ok_and(|p| p.contains(lat, lon))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(lat: f64, lon: f64) -> CellIndex {
        LatLng::new(lat, lon).unwrap().to_cell(Resolution::Nine)
    }

    #[test]
    fn one_cell_is_one_ring_of_six_edges_and_a_ring_of_cells_has_a_filled_hole() {
        let c = cell(45.0, 20.0);
        let one: HashSet<_> = [c].into();
        assert_eq!(rings(&one).len(), 1);
        assert_eq!(rings(&one)[0].len(), 7);
        let ring: HashSet<_> = c.grid_ring::<Vec<_>>(1).into_iter().collect();
        assert_eq!(rings(&ring).len(), 2, "outer ring and a one-cell hole");
        let full = filled(&ring);
        assert!(full.contains(&c));
        assert_eq!(rings(&full).len(), 1, "the hole of one cell is filled");
        let (count, coords) = outline(&ring);
        assert_eq!(
            (count, coords.len(), coords[0].len()),
            (7, 1, 1),
            "one polygon without a hole"
        );
    }
}
