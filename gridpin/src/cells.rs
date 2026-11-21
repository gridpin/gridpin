//! Codes of statistical grid cells a point lies in (plan T1.3): the European census grid on ETRS89-LAEA
//! (EPSG:3035) at 1 km and 100 m, written as its publishers write it (`CRS3035RES1000mN<north>E<east>`, the
//! south-west corner in metres), and the Japanese standard grid square (JIS X 0410) of the first, second and third
//! level (4, 6 and 8 digits). A point outside the frame of a grid has no code.
//!
//! These are keys for statistics published by cell (Eurostat's census grid, Germany's Zensus 2022, Japan's census
//! mesh); `gridpin attach` joins such a table by the code computed from each row's point.

/// The cell kinds `--with cell:<name>` and a table key `cell:<name>` know: (name, CSV column).
pub const KINDS: [(&str, &str); 5] = [
    ("eu1km", "gp_cell_eu1km"),
    ("de100m", "gp_cell_de100m"),
    ("jp1", "gp_cell_jp1"),
    ("jp2", "gp_cell_jp2"),
    ("jp3", "gp_cell_jp3"),
];

/// The area of use of EPSG:3035 (EPSG dataset: Europe, onshore and offshore), lat and lon bounds.
const EU_FRAME: (f64, f64, f64, f64) = (24.6, 84.73, -35.58, 44.83);
/// The frame of the Japanese grid: the first-level squares cover 20°–46° N, 122°–154° E.
const JP_FRAME: (f64, f64, f64, f64) = (20.0, 46.0, 122.0, 154.0);

fn inside(frame: (f64, f64, f64, f64), lat: f64, lon: f64) -> bool {
    (frame.0..frame.1).contains(&lat) && (frame.2..frame.3).contains(&lon)
}

/// The code of the European grid cell of `step` metres that holds the point: `CRS3035RES<step>mN<n>E<e>` with the
/// south-west corner rounded down to the step.
pub fn eu_grid(lat: f64, lon: f64, step: u32) -> Option<String> {
    if !inside(EU_FRAME, lat, lon) {
        return None;
    }
    let (e, n) = crate::crs::project(3035, lat, lon)?;
    let s = f64::from(step);
    let corner = |v: f64| ((v / s).floor() * s) as i64;
    Some(format!("CRS3035RES{step}mN{}E{}", corner(n), corner(e)))
}

/// The JIS X 0410 grid square code of `level` 1 (4 digits, 40′ × 1°), 2 (6 digits, 5′ × 7′30″) or 3 (8 digits,
/// 30″ × 45″). Latitude and longitude are taken as JGD2011, which agrees with WGS 84 to about a metre.
pub fn jp_mesh(lat: f64, lon: f64, level: u8) -> Option<String> {
    if !inside(JP_FRAME, lat, lon) || !(1..=3).contains(&level) {
        return None;
    }
    // work in whole seconds × 1000 so the borders of the squares do not drift with floating point
    let lat_ms = (lat * 3_600_000.0).round() as i64;
    let lon_ms = (lon * 3_600_000.0).round() as i64;
    let p = lat_ms / 2_400_000; // 40′
    let a = lat_ms % 2_400_000;
    let u = lon_ms / 3_600_000 - 100; // 1°
    let f = lon_ms % 3_600_000;
    let mut code = format!("{p:02}{u:02}");
    if level >= 2 {
        let (q, v) = (a / 300_000, f / 450_000); // 5′ and 7′30″
        code.push_str(&format!("{q}{v}"));
        if level == 3 {
            let (r, w) = ((a % 300_000) / 30_000, (f % 450_000) / 45_000); // 30″ and 45″
            code.push_str(&format!("{r}{w}"));
        }
    }
    Some(code)
}

/// The code of cell kind `name` (one of `KINDS`) for a point; `None` outside the grid's frame.
pub fn code(name: &str, lat: f64, lon: f64) -> Option<String> {
    match name {
        "eu1km" => eu_grid(lat, lon, 1000),
        "de100m" => eu_grid(lat, lon, 100),
        "jp1" => jp_mesh(lat, lon, 1),
        "jp2" => jp_mesh(lat, lon, 2),
        "jp3" => jp_mesh(lat, lon, 3),
        _ => None,
    }
}

/// The CSV column of a cell kind.
pub fn column(name: &str) -> Option<&'static str> {
    KINDS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokyo_station_is_the_square_of_the_statistics_bureau_example() {
        assert_eq!(jp_mesh(35.681236, 139.767125, 3).unwrap(), "53394611");
        assert_eq!(jp_mesh(35.681236, 139.767125, 2).unwrap(), "533946");
        assert_eq!(jp_mesh(35.681236, 139.767125, 1).unwrap(), "5339");
    }

    /// The worked example of the Statistics Bureau (stat.go.jp/data/mesh/pdf/gaiyo1.pdf, table 4: 5438, 543823,
    /// 54382343 — the third level is row 4 and column 3, so a swap of the two shows) and the four end squares of
    /// Japan named under its figure 1 (Minami-Torishima 3653, Yonaguni 3622, Okinotorishima 3036, Etorofu 6848).
    #[test]
    fn the_squares_of_the_statistics_bureau_document() {
        assert_eq!(jp_mesh(36.1, 138.5, 1).unwrap(), "5438");
        assert_eq!(jp_mesh(36.2, 138.42, 2).unwrap(), "543823");
        // 36°12′15″ N, 138°25′07″ E: inside row 4 (36°12′00″–36°12′30″) and column 3 (138°24′45″–138°25′30″)
        assert_eq!(jp_mesh(36.204167, 138.418611, 3).unwrap(), "54382343");
        for (lat, lon, code) in [
            (24.2867, 153.9806, "3653"),
            (24.4500, 122.9800, "3622"),
            (20.4253, 136.0808, "3036"),
            (45.5000, 148.8000, "6848"),
        ] {
            assert_eq!(jp_mesh(lat, lon, 1).unwrap(), code, "{lat} {lon}");
        }
    }

    #[test]
    fn a_point_on_a_border_of_squares_belongs_to_the_square_north_and_east_of_it() {
        // 35°40′ N, 139°45′ E is the south-west corner of 5339 46 00
        assert_eq!(jp_mesh(35.0 + 40.0 / 60.0, 139.75, 3).unwrap(), "53394600");
        // a hair south and west is the neighbouring square
        assert_eq!(
            jp_mesh(35.0 + 40.0 / 60.0 - 1e-6, 139.75 - 1e-6, 3).unwrap(),
            "53393599"
        );
    }

    #[test]
    fn outside_its_frame_a_grid_has_no_code() {
        assert_eq!(jp_mesh(48.85, 2.35, 3), None);
        assert_eq!(jp_mesh(35.68, 139.77, 4), None);
        assert_eq!(eu_grid(35.68, 139.77, 1000), None);
        assert_eq!(eu_grid(-33.86, 151.21, 1000), None);
        assert_eq!(eu_grid(89.9, 0.0, 1000), None);
        assert_eq!(eu_grid(10.0, 179.99, 1000), None);
        assert_eq!(code("eu5km", 50.0, 5.0), None);
    }

    #[test]
    fn the_european_cell_is_the_south_west_corner_rounded_down_to_the_step() {
        // EPSG guidance example: 50° N 5° E -> E 3 962 799.45, N 2 999 718.85
        assert_eq!(
            eu_grid(50.0, 5.0, 1000).unwrap(),
            "CRS3035RES1000mN2999000E3962000"
        );
        assert_eq!(
            eu_grid(50.0, 5.0, 100).unwrap(),
            "CRS3035RES100mN2999700E3962700"
        );
        assert_eq!(
            code("de100m", 50.0, 5.0).unwrap(),
            "CRS3035RES100mN2999700E3962700"
        );
        assert_eq!(column("jp3"), Some("gp_cell_jp3"));
    }
}
