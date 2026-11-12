//! Coordinates of an answer in a national projection (`--with crs:EPSG:2154`), by formula, without a crate.
//!
//! The point is taken as given in WGS 84 = ETRS89 = GDA2020 = SIRGAS 2000 (the frames agree to a few decimetres
//! to two metres; no datum shift is applied, so a GDA94/MGA94 or ED50 code is refused rather than answered wrong).
//! Families: UTM north and south on WGS 84 (EPSG 326zz, 327zz), ETRS89 UTM (258zz), SIRGAS 2000 UTM (31965-31985),
//! MGA2020 (7849-7856); Lambert-93 (2154, conic conformal on two parallels); ETRS89-LAEA Europe (3035); RD New
//! (28992, the published polynomial approximation of RDNAPTRANS, within about 1 m); Web Mercator (3857).
//! State Plane (124 zones) is not here.

use std::f64::consts::{FRAC_PI_4, PI};

/// GRS 80 (and WGS 84 to a tenth of a millimetre in these formulas).
const A: f64 = 6_378_137.0;
const F: f64 = 1.0 / 298.257_222_101;

/// What one code is and how it projects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Crs {
    /// zone, false northing of the south (10 000 km) always
    Utm {
        zone: u8,
        south: bool,
    },
    Lambert93,
    Laea3035,
    RdNew,
    WebMercator,
}

/// The projection of an EPSG code; `None` for a code this build does not project.
pub fn of(code: u32) -> Option<Crs> {
    let utm = |zone: u32, south: bool| {
        (1..=60).contains(&zone).then_some(Crs::Utm {
            zone: zone as u8,
            south,
        })
    };
    match code {
        32601..=32660 => utm(code - 32600, false),
        32701..=32760 => utm(code - 32700, true),
        25828..=25838 => utm(code - 25800, false),
        31965..=31976 => utm(code - 31965 + 11, false),
        31977..=31985 => utm(code - 31977 + 17, true),
        7849..=7856 => utm(code - 7849 + 49, true),
        2154 => Some(Crs::Lambert93),
        3035 => Some(Crs::Laea3035),
        28992 => Some(Crs::RdNew),
        3857 => Some(Crs::WebMercator),
        _ => None,
    }
}

/// `EPSG:2154`, `epsg:2154` or `2154`.
pub fn parse(spec: &str) -> Option<u32> {
    let s = spec.trim();
    let s = s
        .strip_prefix("EPSG:")
        .or_else(|| s.strip_prefix("epsg:"))
        .unwrap_or(s);
    s.parse().ok().filter(|c| of(*c).is_some())
}

/// Easting and northing in metres of (lat, lon) in `code`.
pub fn project(code: u32, lat: f64, lon: f64) -> Option<(f64, f64)> {
    let out = match of(code)? {
        Crs::Utm { zone, south } => {
            let (e, n) = transverse_mercator(lat, lon, f64::from(zone) * 6.0 - 183.0)?;
            (e, if south { n + 10_000_000.0 } else { n })
        }
        Crs::Lambert93 => lcc2sp(
            Ellipsoid { a: A, f: F },
            (46.5, 3.0),
            (49.0, 44.0),
            (700_000.0, 6_600_000.0),
            lat,
            lon,
        ),
        Crs::Laea3035 => laea(
            Ellipsoid { a: A, f: F },
            (52.0, 10.0),
            (4_321_000.0, 3_210_000.0),
            lat,
            lon,
        ),
        Crs::RdNew => rd_new(lat, lon),
        Crs::WebMercator => {
            if lat.abs() > 85.06 {
                return None;
            }
            (
                A * lon.to_radians(),
                A * (FRAC_PI_4 + lat.to_radians() / 2.0).tan().ln(),
            )
        }
    };
    (out.0.is_finite() && out.1.is_finite()).then_some(out)
}

#[derive(Clone, Copy)]
pub struct Ellipsoid {
    pub a: f64,
    pub f: f64,
}

impl Ellipsoid {
    fn e(self) -> f64 {
        (self.f * (2.0 - self.f)).sqrt()
    }
}

/// Transverse Mercator of the UTM grid (scale 0.9996, false easting 500 km), Krüger series to n⁶ (as in `codes.rs`);
/// northing without a false northing.
fn transverse_mercator(lat: f64, lon: f64, lon0: f64) -> Option<(f64, f64)> {
    if !(-80.0..=84.0).contains(&lat) || (lon - lon0).abs() > 20.0 {
        return None;
    }
    let n = F / (2.0 - F);
    let a_big = A / (1.0 + n) * (1.0 + n * n / 4.0 + n.powi(4) / 64.0 + n.powi(6) / 256.0);
    let alpha = [
        n / 2.0 - 2.0 * n * n / 3.0 + 5.0 * n.powi(3) / 16.0 + 41.0 * n.powi(4) / 180.0,
        13.0 * n * n / 48.0 - 3.0 * n.powi(3) / 5.0 + 557.0 * n.powi(4) / 1440.0,
        61.0 * n.powi(3) / 240.0 - 103.0 * n.powi(4) / 140.0,
        49_561.0 * n.powi(4) / 161_280.0,
    ];
    let e = Ellipsoid { a: A, f: F }.e();
    let phi = lat.to_radians();
    let lam = (lon - lon0).to_radians();
    let t = (phi.sin().atanh() - e * (e * phi.sin()).atanh()).sinh();
    let xi_p = t.atan2(lam.cos());
    let eta_p = (lam.sin() / (1.0 + t * t).sqrt()).atanh();
    let (mut xi, mut eta) = (xi_p, eta_p);
    for (j, a) in alpha.iter().enumerate() {
        let k = 2.0 * (j as f64 + 1.0);
        xi += a * (k * xi_p).sin() * (k * eta_p).cosh();
        eta += a * (k * xi_p).cos() * (k * eta_p).sinh();
    }
    Some((500_000.0 + 0.9996 * a_big * eta, 0.9996 * a_big * xi))
}

/// Lambert conic conformal on two standard parallels (EPSG method 9802). `origin` (lat, lon), `parallels`
/// (first, second), `false_en` (easting, northing), all in degrees and metres.
pub fn lcc2sp(
    el: Ellipsoid,
    origin: (f64, f64),
    parallels: (f64, f64),
    false_en: (f64, f64),
    lat: f64,
    lon: f64,
) -> (f64, f64) {
    let e = el.e();
    let m = |phi: f64| phi.cos() / (1.0 - e * e * phi.sin().powi(2)).sqrt();
    let t = |phi: f64| {
        (FRAC_PI_4 - phi / 2.0).tan()
            / ((1.0 - e * phi.sin()) / (1.0 + e * phi.sin())).powf(e / 2.0)
    };
    let (p1, p2) = (parallels.0.to_radians(), parallels.1.to_radians());
    let n = (m(p1).ln() - m(p2).ln()) / (t(p1).ln() - t(p2).ln());
    let f_big = m(p1) / (n * t(p1).powf(n));
    let r = |phi: f64| el.a * f_big * t(phi).powf(n);
    let r_f = r(origin.0.to_radians());
    let theta = n * (lon - origin.1).to_radians();
    let rho = r(lat.to_radians());
    (
        false_en.0 + rho * theta.sin(),
        false_en.1 + r_f - rho * theta.cos(),
    )
}

/// Lambert azimuthal equal area, oblique, on the ellipsoid (EPSG method 9820).
pub fn laea(
    el: Ellipsoid,
    origin: (f64, f64),
    false_en: (f64, f64),
    lat: f64,
    lon: f64,
) -> (f64, f64) {
    let e = el.e();
    let q = |phi: f64| {
        let s = phi.sin();
        (1.0 - e * e)
            * (s / (1.0 - e * e * s * s) - (1.0 / (2.0 * e)) * ((1.0 - e * s) / (1.0 + e * s)).ln())
    };
    let qp = q(PI / 2.0);
    let (phi0, lam0) = (origin.0.to_radians(), origin.1.to_radians());
    let (phi, lam) = (lat.to_radians(), lon.to_radians());
    let beta = |phi: f64| (q(phi) / qp).asin();
    let (b0, b) = (beta(phi0), beta(phi));
    let rq = el.a * (qp / 2.0).sqrt();
    let d = el.a * (phi0.cos() / (1.0 - e * e * phi0.sin().powi(2)).sqrt()) / (rq * b0.cos());
    let bb =
        rq * (2.0 / (1.0 + b0.sin() * b.sin() + b0.cos() * b.cos() * (lam - lam0).cos())).sqrt();
    let x = false_en.0 + bb * d * b.cos() * (lam - lam0).sin();
    let y = false_en.1 + (bb / d) * (b0.cos() * b.sin() - b0.sin() * b.cos() * (lam - lam0).cos());
    (x, y)
}

/// RD New from ETRS89 by the published polynomial (Schreutelkamp & Strang van Hees), within about a metre in the
/// Netherlands; outside it the polynomial is meaningless, so only 50-54 N, 3-8 E is answered.
fn rd_new(lat: f64, lon: f64) -> (f64, f64) {
    if !(50.0..=54.0).contains(&lat) || !(3.0..=8.0).contains(&lon) {
        return (f64::NAN, f64::NAN);
    }
    let dp = 0.36 * (lat - 52.155_174_40);
    let dl = 0.36 * (lon - 5.387_206_21);
    const R: [(i32, i32, f64); 9] = [
        (0, 1, 190_094.945),
        (1, 1, -11_832.228),
        (2, 1, -114.221),
        (0, 3, -32.391),
        (1, 0, -0.705),
        (3, 1, -2.340),
        (1, 3, -0.608),
        (0, 2, -0.008),
        (2, 3, 0.148),
    ];
    const S: [(i32, i32, f64); 10] = [
        (1, 0, 309_056.544),
        (0, 2, 3_638.893),
        (2, 0, 73.077),
        (1, 2, -157.984),
        (3, 0, 59.788),
        (0, 1, 0.433),
        (2, 2, -6.439),
        (1, 1, -0.032),
        (0, 4, 0.092),
        (1, 4, -0.054),
    ];
    let sum = |t: &[(i32, i32, f64)]| {
        t.iter()
            .map(|(p, q, c)| c * dp.powi(*p) * dl.powi(*q))
            .sum::<f64>()
    };
    (155_000.0 + sum(&R), 463_000.0 + sum(&S))
}

#[cfg(test)]
mod tests {
    use super::*;

    const US_FT: f64 = 1200.0 / 3937.0;

    fn dms(d: f64, m: f64, s: f64) -> f64 {
        d + m / 60.0 + s / 3600.0
    }

    #[test]
    fn lcc2sp_matches_the_epsg_guidance_example() {
        // EPSG Guidance Note 7-2, Lambert Conic Conformal (2SP): NAD27 / Texas South Central, Clarke 1866
        let clarke = Ellipsoid {
            a: 6_378_206.400,
            f: 1.0 / 294.978_698_2,
        };
        let (e, n) = lcc2sp(
            clarke,
            (dms(27.0, 50.0, 0.0), -99.0),
            (dms(28.0, 23.0, 0.0), dms(30.0, 17.0, 0.0)),
            (2_000_000.0 * US_FT, 0.0),
            28.5,
            -96.0,
        );
        assert!((e / US_FT - 2_963_503.91).abs() < 0.5, "E {}", e / US_FT);
        assert!((n / US_FT - 254_759.80).abs() < 0.5, "N {}", n / US_FT);
    }

    #[test]
    fn laea_matches_the_epsg_guidance_example() {
        let (e, n) = project(3035, 50.0, 5.0).unwrap();
        assert!((e - 3_962_799.45).abs() < 0.5, "E {e}");
        assert!((n - 2_999_718.85).abs() < 0.5, "N {n}");
    }

    #[test]
    fn web_mercator_matches_the_epsg_guidance_example() {
        let (e, n) = project(3857, dms(24.0, 22.0, 54.433), -dms(100.0, 20.0, 0.0)).unwrap();
        assert!((e + 11_169_055.58).abs() < 0.5, "E {e}");
        assert!((n - 2_800_000.0).abs() < 0.5, "N {n}");
    }

    #[test]
    fn lambert93_is_700000_6600000_at_its_origin_and_rd_new_at_amersfoort() {
        let (e, n) = project(2154, 46.5, 3.0).unwrap();
        assert!((e - 700_000.0).abs() < 0.01 && (n - 6_600_000.0).abs() < 0.01);
        let (x, y) = project(28992, 52.155_174_40, 5.387_206_21).unwrap();
        assert!((x - 155_000.0).abs() < 0.5 && (y - 463_000.0).abs() < 0.5);
        assert!(
            project(28992, 45.0, 5.0).is_none(),
            "RD New outside the Netherlands is refused"
        );
    }

    #[test]
    fn utm_codes_agree_with_the_grid_of_codes_rs() {
        let (lat, lon) = (45.2671, 19.8335); // Novi Sad, zone 34
        let u = crate::codes::utm_encode(lat, lon).unwrap();
        for code in [32634, 25834] {
            let (e, n) = project(code, lat, lon).unwrap();
            assert!(
                (e - u.easting).abs() < 0.01 && (n - u.northing).abs() < 0.01,
                "{code}"
            );
        }
        let (_, n) = project(32734, lat, lon).unwrap();
        assert!(
            (n - u.northing - 10_000_000.0).abs() < 0.01,
            "a south code keeps its false northing"
        );
        assert_eq!(
            of(31983),
            Some(Crs::Utm {
                zone: 23,
                south: true
            })
        );
        assert_eq!(
            of(7856),
            Some(Crs::Utm {
                zone: 56,
                south: true
            })
        );
        assert_eq!(
            parse("EPSG:4326"),
            None,
            "an unknown code is refused, not answered"
        );
    }
}
