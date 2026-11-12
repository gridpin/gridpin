//! Point codes, both ways: geohash, Plus Code (Open Location Code), UTM, MGRS and degrees-minutes-seconds.
//!
//! The answer can carry them (`--with codes`), and a query can be one of them (the coordinate reader of the
//! query path). No crate: each format is a published, short algorithm, and none of the crates reads a UTM
//! string with its zone. UTM uses the Krüger series to the sixth order (Karney 2011), good to well under a
//! millimetre inside a zone; MGRS is UTM with the 100 km square letters of the AA scheme (WGS84).

use std::f64::consts::PI;

const BASE32: &[u8; 32] = b"0123456789bcdefghjkmnpqrstuvwxyz";

/// Geohash of `len` characters (1..=12).
pub fn geohash_encode(lat: f64, lon: f64, len: usize) -> String {
    let len = len.clamp(1, 12);
    let (mut lat_lo, mut lat_hi, mut lon_lo, mut lon_hi) = (-90.0, 90.0, -180.0, 180.0);
    let mut out = String::with_capacity(len);
    let (mut bits, mut ch, mut even) = (0, 0usize, true);
    while out.len() < len {
        if even {
            let mid = (lon_lo + lon_hi) / 2.0;
            if lon >= mid {
                ch = (ch << 1) | 1;
                lon_lo = mid;
            } else {
                ch <<= 1;
                lon_hi = mid;
            }
        } else {
            let mid = (lat_lo + lat_hi) / 2.0;
            if lat >= mid {
                ch = (ch << 1) | 1;
                lat_lo = mid;
            } else {
                ch <<= 1;
                lat_hi = mid;
            }
        }
        even = !even;
        bits += 1;
        if bits == 5 {
            out.push(BASE32[ch] as char);
            bits = 0;
            ch = 0;
        }
    }
    out
}

/// Centre of a geohash cell, or `None` when a character is outside the alphabet.
pub fn geohash_decode(code: &str) -> Option<(f64, f64)> {
    if code.is_empty() || code.len() > 12 {
        return None;
    }
    let (mut lat_lo, mut lat_hi, mut lon_lo, mut lon_hi) = (-90.0, 90.0, -180.0, 180.0);
    let mut even = true;
    for c in code.bytes() {
        let v = BASE32.iter().position(|&b| b == c.to_ascii_lowercase())?;
        for shift in (0..5).rev() {
            let bit = (v >> shift) & 1 == 1;
            if even {
                let mid = (lon_lo + lon_hi) / 2.0;
                if bit {
                    lon_lo = mid
                } else {
                    lon_hi = mid
                }
            } else {
                let mid = (lat_lo + lat_hi) / 2.0;
                if bit {
                    lat_lo = mid
                } else {
                    lat_hi = mid
                }
            }
            even = !even;
        }
    }
    Some(((lat_lo + lat_hi) / 2.0, (lon_lo + lon_hi) / 2.0))
}

const OLC_ALPHABET: &[u8; 20] = b"23456789CFGHJMPQRVWX";

/// Full Plus Code of 10 digits plus the 11th grid digit ("8FVC9G8F+6XQ" style has 11; 10 is ~14 m).
pub fn plus_code_encode(lat: f64, lon: f64, digits: usize) -> String {
    let digits = digits.clamp(10, 11);
    let lat = lat.clamp(-90.0, 90.0);
    // a latitude of exactly 90 belongs to the cell below
    let lat = if lat >= 90.0 { 90.0 - 1e-10 } else { lat };
    let mut lon = lon;
    while lon < -180.0 {
        lon += 360.0;
    }
    while lon >= 180.0 {
        lon -= 360.0;
    }
    // integer arithmetic as in the reference implementation: the pair part has a 1/8000 degree step and the
    // grid digit a further 5x4 split, so lat * 8000 * 5 and lon * 8000 * 4 are exact for the 11th digit
    // ... after rounding to 1e-6 of a step, as the reference does (`Math.floor(Math.round(x * 1e6) / 1e6)`): a
    // point on a grid line, 52.3012 * 40000 = 2 092 048, comes out of floating point as 2 092 047.999… and fell
    // into the cell below (revision 0.9.0)
    let steps = |x: f64| ((x * 1e6).round() / 1e6).floor() as i64;
    let lat_i = steps((lat + 90.0) * 8000.0 * 5.0);
    let lon_i = steps((lon + 180.0) * 8000.0 * 4.0);
    let mut code = Vec::with_capacity(12);
    if digits == 11 {
        let (r, c) = (lat_i % 5, lon_i % 4);
        code.push(OLC_ALPHABET[(r * 4 + c) as usize]);
    }
    let (mut la, mut lo) = (lat_i / 5, lon_i / 4);
    let mut pairs = Vec::with_capacity(10);
    for _ in 0..5 {
        pairs.push(OLC_ALPHABET[(lo % 20) as usize]);
        pairs.push(OLC_ALPHABET[(la % 20) as usize]);
        la /= 20;
        lo /= 20;
    }
    pairs.reverse();
    let mut out: Vec<u8> = pairs;
    out.insert(8, b'+');
    if digits == 11 {
        out.extend(code);
    }
    String::from_utf8(out).expect("ascii")
}

/// Centre of a full Plus Code. A short code (fewer than 8 digits before "+") needs a reference point and is
/// refused, as is padding ("0").
pub fn plus_code_decode(code: &str) -> Option<(f64, f64)> {
    let code = code.trim().to_ascii_uppercase();
    let (head, tail) = code.split_once('+')?;
    if head.len() != 8 || tail.len() > 3 || tail.len() == 1 {
        return None;
    }
    let value = |c: u8| OLC_ALPHABET.iter().position(|&b| b == c).map(|v| v as f64);
    let digits: Vec<u8> = head.bytes().chain(tail.bytes()).collect();
    let (mut lat, mut lon) = (-90.0, -180.0);
    let mut step = 20.0;
    let pair_digits = digits.len().min(10);
    for pair in digits[..pair_digits].chunks(2) {
        lat += value(pair[0])? * step;
        lon += value(*pair.get(1)?)? * step;
        step /= 20.0;
    }
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    let (mut lat_step, mut lon_step) = (step * 20.0, step * 20.0);
    if let Some(&grid) = digits.get(10) {
        let v = OLC_ALPHABET.iter().position(|&b| b == grid)? as f64;
        lat_step /= 5.0;
        lon_step /= 4.0;
        lat += (v / 4.0).floor() * lat_step;
        lon += (v % 4.0) * lon_step;
    }
    Some((lat + lat_step / 2.0, lon + lon_step / 2.0))
}

// WGS84
const A: f64 = 6_378_137.0;
const F: f64 = 1.0 / 298.257_223_563;
const K0: f64 = 0.9996;

/// One UTM coordinate: zone 1..=60, band letter (C..X without I and O), easting and northing in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Utm {
    pub zone: u8,
    pub band: char,
    pub easting: f64,
    pub northing: f64,
}

const BANDS: &[u8; 20] = b"CDEFGHJKLMNPQRSTUVWX";

fn series() -> (f64, [f64; 6], [f64; 6]) {
    let n = F / (2.0 - F);
    let (n2, n3, n4, n5, n6) = (n * n, n.powi(3), n.powi(4), n.powi(5), n.powi(6));
    let a_big = A / (1.0 + n) * (1.0 + n2 / 4.0 + n4 / 64.0 + n6 / 256.0);
    let alpha = [
        n / 2.0 - 2.0 * n2 / 3.0 + 5.0 * n3 / 16.0 + 41.0 * n4 / 180.0 - 127.0 * n5 / 288.0
            + 7891.0 * n6 / 37800.0,
        13.0 * n2 / 48.0 - 3.0 * n3 / 5.0 + 557.0 * n4 / 1440.0 + 281.0 * n5 / 630.0
            - 1983433.0 * n6 / 1935360.0,
        61.0 * n3 / 240.0 - 103.0 * n4 / 140.0 + 15061.0 * n5 / 26880.0 + 167603.0 * n6 / 181440.0,
        49561.0 * n4 / 161280.0 - 179.0 * n5 / 168.0 + 6601661.0 * n6 / 7257600.0,
        34729.0 * n5 / 80640.0 - 3418889.0 * n6 / 1995840.0,
        212378941.0 * n6 / 319334400.0,
    ];
    let beta = [
        n / 2.0 - 2.0 * n2 / 3.0 + 37.0 * n3 / 96.0 - n4 / 360.0 - 81.0 * n5 / 512.0
            + 96199.0 * n6 / 604800.0,
        n2 / 48.0 + n3 / 15.0 - 437.0 * n4 / 1440.0 + 46.0 * n5 / 105.0
            - 1118711.0 * n6 / 3870720.0,
        17.0 * n3 / 480.0 - 37.0 * n4 / 840.0 - 209.0 * n5 / 4480.0 + 5569.0 * n6 / 90720.0,
        4397.0 * n4 / 161280.0 - 11.0 * n5 / 504.0 - 830251.0 * n6 / 7257600.0,
        4583.0 * n5 / 161280.0 - 108847.0 * n6 / 3991680.0,
        20648693.0 * n6 / 638668800.0,
    ];
    (a_big, alpha, beta)
}

/// The UTM zone of a point, with the Norway and Svalbard exceptions.
pub fn utm_zone(lat: f64, lon: f64) -> u8 {
    let mut zone = (((lon + 180.0) / 6.0).floor() as i64 % 60 + 1) as u8;
    if (56.0..64.0).contains(&lat) && (3.0..12.0).contains(&lon) {
        zone = 32;
    }
    if (72.0..=84.0).contains(&lat) {
        zone = match lon {
            l if (0.0..9.0).contains(&l) => 31,
            l if (9.0..21.0).contains(&l) => 33,
            l if (21.0..33.0).contains(&l) => 35,
            l if (33.0..42.0).contains(&l) => 37,
            _ => zone,
        };
    }
    zone
}

fn band_of(lat: f64) -> Option<char> {
    if !(-80.0..=84.0).contains(&lat) {
        return None;
    }
    let i = (((lat + 80.0) / 8.0).floor() as usize).min(19);
    Some(BANDS[i] as char)
}

/// UTM of a point between 80°S and 84°N (outside, the polar UPS grid applies and `None` is returned).
pub fn utm_encode(lat: f64, lon: f64) -> Option<Utm> {
    let band = band_of(lat)?;
    let zone = utm_zone(lat, lon);
    utm_encode_in_zone(lat, lon, zone).map(|(e, n)| Utm {
        zone,
        band,
        easting: e,
        northing: n,
    })
}

fn utm_encode_in_zone(lat: f64, lon: f64, zone: u8) -> Option<(f64, f64)> {
    let (a_big, alpha, _) = series();
    let n = F / (2.0 - F);
    let lon0 = (zone as f64 - 1.0) * 6.0 - 180.0 + 3.0;
    let phi = lat.to_radians();
    let lam = (lon - lon0).to_radians();
    let e = (F * (2.0 - F)).sqrt();
    let t = (phi.sin().atanh() - e * (e * phi.sin()).atanh()).sinh();
    let xi_p = t.atan2(lam.cos());
    let eta_p = (lam.sin() / (1.0 + t * t).sqrt()).atanh();
    let _ = n;
    let (mut xi, mut eta) = (xi_p, eta_p);
    for (j, a) in alpha.iter().enumerate() {
        let k = 2.0 * (j as f64 + 1.0);
        xi += a * (k * xi_p).sin() * (k * eta_p).cosh();
        eta += a * (k * xi_p).cos() * (k * eta_p).sinh();
    }
    let easting = 500_000.0 + K0 * a_big * eta;
    let mut northing = K0 * a_big * xi;
    if lat < 0.0 {
        northing += 10_000_000.0;
    }
    (easting.is_finite() && northing.is_finite()).then_some((easting, northing))
}

/// Latitude and longitude of a UTM coordinate. `south` says the northing counts from the false origin of the
/// southern hemisphere (band letter below N).
pub fn utm_decode(zone: u8, south: bool, easting: f64, northing: f64) -> Option<(f64, f64)> {
    if !(1..=60).contains(&zone) {
        return None;
    }
    let (a_big, _, beta) = series();
    let e = (F * (2.0 - F)).sqrt();
    let y = if south {
        northing - 10_000_000.0
    } else {
        northing
    };
    let xi = y / (K0 * a_big);
    let eta = (easting - 500_000.0) / (K0 * a_big);
    let (mut xi_p, mut eta_p) = (xi, eta);
    for (j, b) in beta.iter().enumerate() {
        let k = 2.0 * (j as f64 + 1.0);
        xi_p -= b * (k * xi).sin() * (k * eta).cosh();
        eta_p -= b * (k * xi).cos() * (k * eta).sinh();
    }
    let tau_p = xi_p.sin() / (eta_p.sinh().powi(2) + xi_p.cos().powi(2)).sqrt();
    // Newton's method for tau from tau' (Karney 2011, eq. 19-21)
    let mut tau = tau_p;
    for _ in 0..8 {
        let sigma = (e * (e * tau / (1.0 + tau * tau).sqrt()).atanh()).sinh();
        let tau_i = tau * (1.0 + sigma * sigma).sqrt() - sigma * (1.0 + tau * tau).sqrt();
        let d = (tau_p - tau_i) / (1.0 + tau_i * tau_i).sqrt() * (1.0 + (1.0 - e * e) * tau * tau)
            / ((1.0 - e * e) * (1.0 + tau * tau).sqrt());
        tau += d;
        if d.abs() < 1e-12 {
            break;
        }
    }
    let lat = tau.atan().to_degrees();
    let lon0 = (zone as f64 - 1.0) * 6.0 - 180.0 + 3.0;
    let lon = lon0 + eta_p.sinh().atan2(xi_p.cos()).to_degrees();
    (lat.is_finite() && lon.is_finite() && (-90.0..=90.0).contains(&lat)).then_some((lat, lon))
}

const COL_SETS: [&[u8; 8]; 3] = [b"ABCDEFGH", b"JKLMNPQR", b"STUVWXYZ"];
const ROW_LETTERS: &[u8; 20] = b"ABCDEFGHJKLMNPQRSTUV";

/// MGRS at 1 m ("31UDQ4825211954"); `None` outside the UTM bands.
pub fn mgrs_encode(lat: f64, lon: f64) -> Option<String> {
    let u = utm_encode(lat, lon)?;
    let set = (u.zone as usize - 1) % 3;
    let col = (u.easting / 100_000.0).floor() as usize;
    if !(1..=8).contains(&col) {
        return None;
    }
    let col_letter = COL_SETS[set][col - 1] as char;
    let offset = if u.zone.is_multiple_of(2) { 5 } else { 0 };
    let row = ((u.northing / 100_000.0).floor() as usize + offset) % 20;
    let row_letter = ROW_LETTERS[row] as char;
    let e = (u.easting % 100_000.0).floor() as u32;
    let n = (u.northing % 100_000.0).floor() as u32;
    Some(format!(
        "{}{}{}{}{:05}{:05}",
        u.zone, u.band, col_letter, row_letter, e, n
    ))
}

/// Centre of an MGRS square ("31U DQ 48252 11954", any precision from 10 km to 1 m; spaces allowed).
pub fn mgrs_decode(code: &str) -> Option<(f64, f64)> {
    let s: String = code
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    let zone_len = s.bytes().take_while(u8::is_ascii_digit).count();
    if !(1..=2).contains(&zone_len) {
        return None;
    }
    let zone: u8 = s[..zone_len].parse().ok()?;
    let rest = &s.as_bytes()[zone_len..];
    if rest.len() < 3 || !(1..=60).contains(&zone) {
        return None;
    }
    let band = rest[0];
    let band_i = BANDS.iter().position(|&b| b == band)?;
    let set = (zone as usize - 1) % 3;
    let col = COL_SETS[set].iter().position(|&b| b == rest[1])? + 1;
    let offset = if zone.is_multiple_of(2) { 5 } else { 0 };
    let row = (ROW_LETTERS.iter().position(|&b| b == rest[2])? + 20 - offset) % 20;
    let digits = &rest[3..];
    if !digits.len().is_multiple_of(2)
        || digits.len() > 10
        || !digits.iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let half = digits.len() / 2;
    let scale = 10f64.powi(5 - half as i32);
    let num = |d: &[u8]| -> f64 {
        if d.is_empty() {
            0.0
        } else {
            std::str::from_utf8(d)
                .ok()
                .and_then(|t| t.parse::<f64>().ok())
                .unwrap_or(0.0)
        }
    };
    let easting = col as f64 * 100_000.0 + num(&digits[..half]) * scale + scale / 2.0;
    let north_in = row as f64 * 100_000.0 + num(&digits[half..]) * scale + scale / 2.0;
    // the row letters repeat every 2 000 km: take the cycle whose latitude falls in the band
    let band_lat = -80.0 + 8.0 * band_i as f64;
    let south = band < b'N';
    let (_, band_north) =
        utm_encode_in_zone(band_lat.max(-80.0), (zone as f64 - 1.0) * 6.0 - 177.0, zone)?;
    let mut northing = north_in;
    while northing < band_north - 100_000.0 {
        northing += 2_000_000.0;
    }
    utm_decode(zone, south, easting, northing)
}

/// "33U 448251 5411932" or "33N 448251 5411932" (hemisphere letter N/S, or a band letter).
pub fn utm_parse(text: &str) -> Option<(f64, f64)> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let (zone_band, e, n) = match parts.as_slice() {
        [zb, e, n] => (*zb, *e, *n),
        [z, b, e, n] if z.bytes().all(|c| c.is_ascii_digit()) && b.len() == 1 => {
            return utm_parse(&format!("{z}{b} {e} {n}"))
        }
        _ => return None,
    };
    let zone_len = zone_band.bytes().take_while(u8::is_ascii_digit).count();
    if !(1..=2).contains(&zone_len) || zone_band.len() != zone_len + 1 {
        return None;
    }
    let zone: u8 = zone_band[..zone_len].parse().ok()?;
    let letter = zone_band.as_bytes()[zone_len].to_ascii_uppercase();
    let south = match letter {
        b'S' => {
            // "S" is both the band 32°-40°N and "south": north is meant only when the northern reading
            // lands in that band
            let (e, n) = (e.parse::<f64>().ok()?, n.parse::<f64>().ok()?);
            !utm_decode(zone, false, e, n).is_some_and(|(lat, _)| (32.0..40.0).contains(&lat))
        }
        b'N' => false,
        l if BANDS.contains(&l) => l < b'N',
        _ => return None,
    };
    let easting: f64 = e.parse().ok()?;
    let northing: f64 = n.parse().ok()?;
    if !(100_000.0..=900_000.0).contains(&easting) || !(0.0..=10_000_000.0).contains(&northing) {
        return None;
    }
    utm_decode(zone, south, easting, northing)
}

/// One angle written as degrees, minutes and seconds with a hemisphere letter, or as signed decimal degrees.
fn parse_angle(raw: &str) -> Option<(f64, Option<char>)> {
    let s = raw.trim();
    let mut hemi = None;
    let mut body = String::new();
    for c in s.chars() {
        match c.to_ascii_uppercase() {
            'N' | 'S' | 'E' | 'W' | 'O' if hemi.is_none() => hemi = Some(c.to_ascii_uppercase()),
            '°' | '\'' | '"' | '′' | '″' | 'º' | ':' => body.push(' '),
            '-' | '+' | '.' | ',' => body.push(if c == ',' { '.' } else { c }),
            d if d.is_ascii_digit() => body.push(d),
            ' ' => body.push(' '),
            _ => return None,
        }
    }
    let nums: Vec<f64> = body
        .split_whitespace()
        .map(|t| t.parse::<f64>().ok())
        .collect::<Option<Vec<f64>>>()?;
    let value = match nums.as_slice() {
        [d] => *d,
        [d, m] if (0.0..60.0).contains(m) => d.signum() * (d.abs() + m / 60.0),
        [d, m, s] if (0.0..60.0).contains(m) && (0.0..60.0).contains(s) => {
            d.signum() * (d.abs() + m / 60.0 + s / 3600.0)
        }
        _ => return None,
    };
    // "-0 30" keeps its sign through the zero degrees
    let value = if body.trim_start().starts_with('-') && value > 0.0 {
        -value
    } else {
        value
    };
    Some((value, hemi))
}

/// A pair of angles with hemisphere letters: "48°51'24"N 2°21'03"E", "48 51 24 N 2 21 3 E", "N48°51.4' E2°21.05'".
/// Both letters are required (a bare pair is decimal degrees, read elsewhere), and N/S must come with the
/// latitude. "O" (oeste/ouest) counts as west.
pub fn dms_parse(text: &str) -> Option<(f64, f64)> {
    let t = text.trim();
    // split after the first hemisphere letter that closes the latitude, or before the one that opens the longitude
    let upper: Vec<char> = t.chars().collect();
    let letter_at: Vec<usize> = upper
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c.to_ascii_uppercase(), 'N' | 'S' | 'E' | 'W' | 'O'))
        .map(|(i, _)| i)
        .collect();
    if letter_at.len() != 2 {
        return None;
    }
    let leading = letter_at[0] == 0 || !upper[..letter_at[0]].iter().any(|c| c.is_ascii_digit());
    let cut = if leading {
        letter_at[1]
    } else {
        letter_at[0] + 1
    };
    let first: String = upper[..cut].iter().collect();
    let second: String = upper[cut..].iter().collect();
    let second = second.trim_start_matches([',', ';', ' ']);
    let (a, ha) = parse_angle(first.trim_end_matches([',', ';', ' ']))?;
    let (b, hb) = parse_angle(second)?;
    let (lat, lon) = match (ha?, hb?) {
        (h1 @ ('N' | 'S'), h2 @ ('E' | 'W' | 'O')) => (signed(a, h1), signed(b, h2)),
        (h1 @ ('E' | 'W' | 'O'), h2 @ ('N' | 'S')) => (signed(b, h2), signed(a, h1)),
        _ => return None,
    };
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

fn signed(v: f64, hemi: char) -> f64 {
    if matches!(hemi, 'S' | 'W' | 'O') {
        -v.abs()
    } else {
        v.abs()
    }
}

/// "48°51'24.0"N 2°21'03.0"E" with one decimal of a second (~3 m).
pub fn dms_format(lat: f64, lon: f64) -> String {
    fn one(v: f64, pos: char, neg: char) -> String {
        let hemi = if v < 0.0 { neg } else { pos };
        let total = (v.abs() * 36_000.0).round() as u64; // tenths of a second
        let (d, rest) = (total / 36_000, total % 36_000);
        let (m, s10) = (rest / 600, rest % 600);
        format!("{d}°{m:02}'{:02}.{}\"{hemi}", s10 / 10, s10 % 10)
    }
    format!("{} {}", one(lat, 'N', 'S'), one(lon, 'E', 'W'))
}

/// Great-circle distance in metres on the sphere the evaluation tools use (6 371 000 m).
pub fn haversine_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (p1, p2) = (a.0.to_radians(), b.0.to_radians());
    let (dp, dl) = (p2 - p1, (b.1 - a.1).to_radians());
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6_371_000.0 * h.clamp(0.0, 1.0).sqrt().asin()
}

#[allow(dead_code)]
const _PI: f64 = PI;

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: (f64, f64), b: (f64, f64), metres: f64) -> bool {
        haversine_m(a, b) <= metres
    }

    #[test]
    fn geohash_matches_the_published_example_and_decodes_back() {
        // Wikipedia "Geohash": 57.64911, 10.40744 -> u4pruydqqvj
        assert_eq!(geohash_encode(57.64911, 10.40744, 11), "u4pruydqqvj");
        let back = geohash_decode("u4pruydqqvj").unwrap();
        assert!(near(back, (57.64911, 10.40744), 1.0));
        assert_eq!(
            geohash_decode("u4pruydqqva"),
            None,
            "'a' is not in the alphabet"
        );
        assert_eq!(geohash_encode(44.8125, 20.4612, 9).len(), 9);
    }

    #[test]
    fn plus_codes_match_the_reference_test_data() {
        // openlocationcode test_data/encoding.csv
        assert_eq!(plus_code_encode(47.365590, 8.524997, 10), "8FVC9G8F+6X");
        assert_eq!(plus_code_encode(20.3700625, 2.7821875, 10), "7FG49QCJ+2V");
        // the 11th digit splits the 10-digit cell 5 x 4 and keeps the point inside it
        let eleven = plus_code_encode(20.3700625, 2.7821875, 11);
        assert!(
            eleven.starts_with("7FG49QCJ+2V") && eleven.len() == 12,
            "{eleven}"
        );
        let back = plus_code_decode("8FVC9G8F+6X").unwrap();
        assert!(near(back, (47.365590, 8.524997), 10.0), "{back:?}");
        let back = plus_code_decode(&eleven.to_lowercase()).unwrap();
        assert!(near(back, (20.3700625, 2.7821875), 3.0), "{back:?}");
        assert_eq!(
            plus_code_decode("9G8F+6X"),
            None,
            "a short code needs a reference point"
        );
        assert_eq!(plus_code_decode("8FVC9G8F"), None);
    }

    #[test]
    fn a_point_on_a_grid_line_gets_the_cell_above_it_as_in_the_reference() {
        // revision 0.9.0, DE owner row 234: (52.3012 + 90) * 40000 is 5 692 048 exactly, floating point gives
        // 5 692 047.999…; the reference rounds to 1e-6 of a step first
        assert_eq!(plus_code_encode(52.3012, 8.0593977, 11), "9F4C8325+FQJ");
        // a point just below the line stays below it
        assert_eq!(
            plus_code_encode(52.301_199_99, 8.0593977, 11),
            "9F4C8325+FQC"
        );
    }

    #[test]
    fn utm_matches_pyproj_to_a_centimetre_and_comes_back() {
        // reference: pyproj 3.6.1, EPSG:4326 -> EPSG:326zz / 327zz (2026-11-08)
        let table = [
            (48.8584, 2.2945, 31, 'U', 448_252.00, 5_411_954.91),
            (40.748817, -73.985428, 18, 'T', 585_650.84, 4_511_369.00),
            (-33.856784, 151.215297, 56, 'H', 334_900.26, 6_252_290.52),
            (44.8125, 20.4612, 34, 'T', 457_396.04, 4_962_263.06),
        ];
        for (lat, lon, zone, band, e, n) in table {
            let u = utm_encode(lat, lon).unwrap();
            assert_eq!((u.zone, u.band), (zone, band), "{lat},{lon}");
            assert!(
                (u.easting - e).abs() < 0.05,
                "{lat},{lon}: {} vs {e}",
                u.easting
            );
            assert!(
                (u.northing - n).abs() < 0.05,
                "{lat},{lon}: {} vs {n}",
                u.northing
            );
            let back = utm_decode(zone, lat < 0.0, u.easting, u.northing).unwrap();
            assert!(near(back, (lat, lon), 0.001), "{back:?}");
        }
        assert_eq!(utm_encode(85.0, 0.0), None, "polar: UPS, not UTM");
        assert_eq!(utm_zone(60.0, 5.0), 32, "Norway exception");
    }

    #[test]
    fn mgrs_writes_the_eiffel_tower_as_in_the_grid_and_reads_any_precision() {
        assert_eq!(mgrs_encode(48.8584, 2.2945).unwrap(), "31UDQ4825211954");
        let back = mgrs_decode("31U DQ 48252 11954").unwrap();
        assert!(near(back, (48.8584, 2.2945), 2.0), "{back:?}");
        let coarse = mgrs_decode("31UDQ4811").unwrap();
        assert!(near(coarse, (48.8584, 2.2945), 1500.0), "{coarse:?}");
        // southern hemisphere, even zone
        let code = mgrs_encode(-33.856784, 151.215297).unwrap();
        let back = mgrs_decode(&code).unwrap();
        assert!(
            near(back, (-33.856784, 151.215297), 2.0),
            "{code} -> {back:?}"
        );
        assert_eq!(
            mgrs_decode("31UIQ4825211954"),
            None,
            "I is not a column letter"
        );
        assert_eq!(mgrs_decode("31UDQ482521195"), None, "odd number of digits");
    }

    #[test]
    fn a_utm_string_with_its_zone_is_read() {
        let p = utm_parse("31U 448252.00 5411954.91").unwrap();
        assert!(near(p, (48.8584, 2.2945), 0.01), "{p:?}");
        let p = utm_parse("56H 334900.26 6252290.52").unwrap();
        assert!(near(p, (-33.856784, 151.215297), 0.01), "{p:?}");
        let p = utm_parse("56S 334900.26 6252290.52").unwrap();
        assert!(near(p, (-33.856784, 151.215297), 0.01), "S as south: {p:?}");
        assert_eq!(utm_parse("31U 448251"), None);
        assert_eq!(utm_parse("31A 448251 5411932"), None);
    }

    #[test]
    fn degrees_minutes_seconds_are_read_in_the_forms_people_write() {
        let paris = (48.856_666_7, 2.350_833_3);
        for text in [
            "48°51'24\"N 2°21'03\"E",
            "48°51′24″N, 2°21′03″E",
            "48 51 24 N 2 21 3 E",
            "N48°51.4' E2°21.05'",
            "N 48 51 24, E 2 21 3",
        ] {
            let p = dms_parse(text).unwrap_or_else(|| panic!("{text}"));
            assert!(near(p, paris, 15.0), "{text}: {p:?}");
        }
        let p = dms_parse("33°51'24\"S 151°12'55\"E").unwrap();
        assert!(p.0 < 0.0 && p.1 > 0.0);
        let p = dms_parse("40°26'46\"N 79°58'56\"O").unwrap();
        assert!(p.1 < 0.0, "O is west");
        assert_eq!(
            dms_parse("48.8566 2.3522"),
            None,
            "no letters: decimal, read elsewhere"
        );
        assert_eq!(dms_parse("2°21'03\"E 48°51'24\"E"), None, "two longitudes");
        assert_eq!(dms_parse("Calle Sol 5"), None);
        assert_eq!(dms_parse("95°00'00\"N 2°00'00\"E"), None);
        assert_eq!(
            dms_format(48.8566667, 2.3508333),
            "48°51'24.0\"N 2°21'03.0\"E"
        );
    }
}
