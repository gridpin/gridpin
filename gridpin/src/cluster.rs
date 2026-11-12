//! `gridpin cluster points.csv --k 12 [--max-size 80]`: delivery zones and rounds from points.
//!
//! k-means in local metres (latitude × 111 194, longitude × that × cos of the mean latitude), seeded k-means++ start,
//! 20 rounds; then, while a zone holds more than `max_size` points, its point farthest from the centre moves to the
//! nearest zone that still has room. Columns `gp_cluster`, `gp_cluster_lat`, `gp_cluster_lon`, `gp_cluster_size`;
//! `--geojson` writes the convex hull of each zone (Andrew's monotone chain).

use anyhow::{bail, Result};
use serde_json::{json, Value};

const M_PER_DEG: f64 = 111_194.0;
pub const ROUNDS: usize = 20;

/// splitmix64, so a seed gives the same zones on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn d2(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)
}

pub struct Zones {
    /// zone of each point
    pub of: Vec<usize>,
    /// centre of each zone, (lat, lon)
    pub centres: Vec<(f64, f64)>,
    pub sizes: Vec<usize>,
}

/// Zones of `points` (lat, lon).
pub fn cluster(
    points: &[(f64, f64)],
    k: usize,
    max_size: Option<usize>,
    seed: u64,
) -> Result<Zones> {
    if k == 0 || points.is_empty() {
        bail!("cluster: k must be at least 1 and there must be points");
    }
    let k = k.min(points.len());
    if let Some(m) = max_size {
        if m * k < points.len() {
            bail!(
                "cluster: {k} zones of at most {m} points cannot hold {} points",
                points.len()
            );
        }
    }
    let lat0 = points.iter().map(|p| p.0).sum::<f64>() / points.len() as f64;
    let kx = M_PER_DEG * lat0.to_radians().cos();
    let xy: Vec<(f64, f64)> = points.iter().map(|p| (p.1 * kx, p.0 * M_PER_DEG)).collect();
    let mut rng = Rng(seed ^ 0xA076_1D64_78BD_642F);
    // k-means++ start
    let mut centres = vec![xy[(rng.next() % xy.len() as u64) as usize]];
    while centres.len() < k {
        let dist: Vec<f64> = xy
            .iter()
            .map(|p| {
                centres
                    .iter()
                    .map(|c| d2(*p, *c))
                    .fold(f64::INFINITY, f64::min)
            })
            .collect();
        let total: f64 = dist.iter().sum();
        let mut x = rng.unit() * total;
        let mut pick = xy.len() - 1;
        for (i, d) in dist.iter().enumerate() {
            if x < *d {
                pick = i;
                break;
            }
            x -= d;
        }
        centres.push(xy[pick]);
    }
    let nearest = |p: (f64, f64), c: &[(f64, f64)]| {
        (0..c.len())
            .min_by(|a, b| d2(p, c[*a]).total_cmp(&d2(p, c[*b])))
            .unwrap_or(0)
    };
    let mut of: Vec<usize> = xy.iter().map(|p| nearest(*p, &centres)).collect();
    for _ in 0..ROUNDS {
        let mut sum = vec![(0.0, 0.0, 0usize); k];
        for (p, z) in xy.iter().zip(&of) {
            sum[*z].0 += p.0;
            sum[*z].1 += p.1;
            sum[*z].2 += 1;
        }
        for (c, s) in centres.iter_mut().zip(&sum) {
            if s.2 > 0 {
                *c = (s.0 / s.2 as f64, s.1 / s.2 as f64);
            }
        }
        let next: Vec<usize> = xy.iter().map(|p| nearest(*p, &centres)).collect();
        if next == of {
            break;
        }
        of = next;
    }
    if let Some(max) = max_size {
        balance(&xy, &mut of, &centres, max);
    }
    let mut sizes = vec![0usize; k];
    let mut sums = vec![(0.0, 0.0); k];
    for (p, z) in points.iter().zip(&of) {
        sizes[*z] += 1;
        sums[*z].0 += p.0;
        sums[*z].1 += p.1;
    }
    let centres = sums
        .iter()
        .zip(&sizes)
        .map(|(s, n)| {
            if *n > 0 {
                (s.0 / *n as f64, s.1 / *n as f64)
            } else {
                (f64::NAN, f64::NAN)
            }
        })
        .collect();
    Ok(Zones { of, centres, sizes })
}

/// While a zone holds more than `max`, its farthest point moves to the nearest zone with room.
fn balance(xy: &[(f64, f64)], of: &mut [usize], centres: &[(f64, f64)], max: usize) {
    let k = centres.len();
    let mut size = vec![0usize; k];
    for z in of.iter() {
        size[*z] += 1;
    }
    loop {
        let Some(over) = (0..k).find(|z| size[*z] > max) else {
            break;
        };
        let far = (0..xy.len())
            .filter(|i| of[*i] == over)
            .max_by(|a, b| d2(xy[*a], centres[over]).total_cmp(&d2(xy[*b], centres[over])))
            .expect("an over-full zone has points");
        let to = (0..k)
            .filter(|z| size[*z] < max)
            .min_by(|a, b| d2(xy[far], centres[*a]).total_cmp(&d2(xy[far], centres[*b])))
            .expect("checked: the zones can hold every point");
        of[far] = to;
        size[over] -= 1;
        size[to] += 1;
    }
}

/// Convex hull of points (lon, lat), Andrew's monotone chain; closed ring.
pub fn hull(mut pts: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    pts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    pts.dedup();
    if pts.len() < 3 {
        let mut r = pts.clone();
        if let Some(f) = pts.first() {
            r.push(*f);
        }
        return r;
    }
    let cross = |o: (f64, f64), a: (f64, f64), b: (f64, f64)| {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };
    let mut lower: Vec<(f64, f64)> = Vec::new();
    for p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0.0 {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper: Vec<(f64, f64)> = Vec::new();
    for p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0.0 {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    let first = lower[0];
    lower.push(first);
    lower
}

/// A valid GeoJSON geometry for a closed hull ring (RFC 7946: a Polygon ring has at least four positions): one
/// distinct point is a Point, two (or points on one line) a LineString, three and more a Polygon.
fn geometry(ring: Vec<[f64; 2]>) -> Value {
    let distinct = ring.len().saturating_sub(1);
    match distinct {
        0 | 1 => {
            json!({"type": "Point", "coordinates": ring.first().copied().unwrap_or([f64::NAN; 2])})
        }
        2 => json!({"type": "LineString", "coordinates": &ring[..2]}),
        _ => json!({"type": "Polygon", "coordinates": [ring]}),
    }
}

/// The zones' hulls as a FeatureCollection.
pub fn hulls(points: &[(f64, f64)], z: &Zones) -> Value {
    let features: Vec<Value> = (0..z.centres.len())
        .filter(|c| z.sizes[*c] > 0)
        .map(|c| {
            let ring: Vec<[f64; 2]> = hull(
                points
                    .iter()
                    .zip(&z.of)
                    .filter(|(_, o)| **o == c)
                    .map(|(p, _)| (p.1, p.0))
                    .collect(),
            )
            .into_iter()
            .map(|(x, y)| [x, y])
            .collect();
            json!({"type": "Feature", "properties": {"cluster": c + 1, "size": z.sizes[c]},
                   "geometry": geometry(ring)})
        })
        .collect();
    json!({"type": "FeatureCollection", "features": features})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blobs() -> Vec<(f64, f64)> {
        let mut p = Vec::new();
        for i in 0..50 {
            let t = i as f64 * 0.0001;
            p.push((43.70 + t, 7.40 + t));
            p.push((45.25 + t, 19.84 - t));
        }
        p
    }

    #[test]
    fn two_clear_blobs_are_two_zones_without_mixing() {
        let p = blobs();
        let z = cluster(&p, 2, None, 1).unwrap();
        for i in (0..p.len()).step_by(2) {
            assert_eq!(z.of[i], z.of[0]);
            assert_eq!(z.of[i + 1], z.of[1]);
        }
        assert_ne!(z.of[0], z.of[1]);
    }

    #[test]
    fn max_size_is_kept_and_a_seed_repeats_the_zones() {
        let mut rng = Rng(7);
        let p: Vec<(f64, f64)> = (0..1000)
            .map(|_| (44.0 + rng.unit(), 20.0 + rng.unit()))
            .collect();
        let z = cluster(&p, 12, Some(90), 3).unwrap();
        assert!(z.sizes.iter().all(|s| *s <= 90), "{:?}", z.sizes);
        assert_eq!(z.of, cluster(&p, 12, Some(90), 3).unwrap().of);
        assert!(
            cluster(&p, 2, Some(10), 3).is_err(),
            "two zones of ten cannot hold a thousand"
        );
    }

    #[test]
    fn a_zone_of_one_or_two_places_is_a_point_or_a_line_not_a_broken_polygon() {
        let p = vec![
            (45.0, 20.0),
            (45.0, 20.0),
            (45.0, 20.0),
            (44.0, 19.0),
            (44.1, 19.1),
            (44.0, 19.0),
        ];
        let z = Zones {
            of: vec![0, 0, 0, 1, 1, 1],
            centres: vec![(45.0, 20.0), (44.05, 19.05)],
            sizes: vec![3, 3],
        };
        let f = hulls(&p, &z);
        let g = |i: usize| f["features"][i]["geometry"].clone();
        assert_eq!(g(0)["type"], "Point", "{}", g(0));
        assert_eq!(g(0)["coordinates"], json!([20.0, 45.0]));
        assert_eq!(g(1)["type"], "LineString", "{}", g(1));
        assert_eq!(g(1)["coordinates"].as_array().unwrap().len(), 2);
        // three places make a polygon whose ring has four positions
        let p3 = vec![(0.0, 0.0), (0.0, 1.0), (1.0, 0.0)];
        let z3 = Zones {
            of: vec![0, 0, 0],
            centres: vec![(0.3, 0.3)],
            sizes: vec![3],
        };
        let h = hulls(&p3, &z3);
        assert_eq!(h["features"][0]["geometry"]["type"], "Polygon");
        assert_eq!(
            h["features"][0]["geometry"]["coordinates"][0]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }

    #[test]
    fn the_hull_of_a_square_and_its_centre_is_the_square() {
        let h = hull(vec![
            (0.0, 0.0),
            (1.0, 0.0),
            (1.0, 1.0),
            (0.0, 1.0),
            (0.5, 0.5),
        ]);
        assert_eq!(h.len(), 5);
        assert!(!h.contains(&(0.5, 0.5)));
    }
}
