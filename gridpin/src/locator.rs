//! A number for an answer, for clients that will not take an answer without an object number (`gridpin serve --ids`):
//! TeslaMate stores `osm_id` under a unique index, Dawarich merges places by it, Atlas builds `type:id` from it.
//!
//! The answers of a sheet are not OpenStreetMap objects, so the number is made from the answer itself: two bits of kind
//! (address, street, place, place of interest), three bits of the street and house number, 24 bits of latitude and 24
//! of longitude, 53 bits in all, so every JSON reader holds it exactly. The grid is about 1.2 m north to south and 2.4 m
//! east to west at the equator (1.7 m at 45°). The three bits part two addresses that share a point (a house with two
//! numbers, two entrances): measured on 1 000 addresses per sheet on 14.11.2026, a grid of 0.6 m alone gave one such
//! pair the same number in the United States and two in France. The number does not depend on the sheet's release
//! while the point and the address stay as they are. It goes out negative: in OpenStreetMap a negative id means "no such object in the database", so
//! it can never be read as, or merged with, a real object, and a link "open in OSM" shows "not found" rather than
//! someone else's house. `decode` turns it back into the kind and the point; `/lookup` answers the nearest address
//! there only when that answer's own number is the one asked (ANSWERS §324, fork 1, variant B).

use crate::query::Hit;

const LAT_BITS: u32 = 24;
const LON_BITS: u32 = 24;
const TAG_BITS: u32 = 3;

/// The kind of an answer, the two top bits of its number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Address = 0,
    Street = 1,
    Place = 2,
    Poi = 3,
}

pub fn kind_of(h: &Hit) -> Kind {
    if h.flags.contains(&"poi_layer") {
        return Kind::Poi;
    }
    match h.precision {
        "street" | "intersection" => Kind::Street,
        "city" => Kind::Place,
        _ => Kind::Address,
    }
}

fn cell(v: f64, lo: f64, span: f64, bits: u32) -> u64 {
    let n = ((v - lo) / span * (1u64 << bits) as f64).floor();
    n.clamp(0.0, ((1u64 << bits) - 1) as f64) as u64
}

/// Three bits of the street and the house number (FNV-1a), to part two addresses at one point.
fn tag(h: &Hit) -> u64 {
    let mut x: u64 = 0xcbf29ce484222325;
    for b in h
        .street
        .bytes()
        .chain(*b"|")
        .chain(h.housenumber.as_deref().unwrap_or("").bytes())
    {
        x ^= u64::from(b);
        x = x.wrapping_mul(0x100000001b3);
    }
    x & ((1 << TAG_BITS) - 1)
}

/// The number of an answer, negative.
pub fn encode(h: &Hit) -> i64 {
    let lat = cell(h.lat, -90.0, 180.0, LAT_BITS);
    let lon = cell(h.lon, -180.0, 360.0, LON_BITS);
    let n = ((kind_of(h) as u64) << (TAG_BITS + LAT_BITS + LON_BITS))
        | (tag(h) << (LAT_BITS + LON_BITS))
        | (lat << LON_BITS)
        | lon;
    -(n as i64)
}

/// Whether an answer lies in the grid cell of a number (its latitude and longitude bits, kind and address apart).
pub fn in_cell(id: i64, h: &Hit) -> bool {
    let mask = (1u64 << (LAT_BITS + LON_BITS)) - 1;
    id.unsigned_abs() & mask == encode(h).unsigned_abs() & mask
}

/// The kind and the centre of the grid cell of a number made by `encode`; `None` for a positive number (an
/// OpenStreetMap object) or one beyond 53 bits.
pub fn decode(id: i64) -> Option<(Kind, f64, f64)> {
    if id >= 0 {
        return None;
    }
    let n = id.unsigned_abs();
    if n >> (TAG_BITS + LAT_BITS + LON_BITS + 2) != 0 {
        return None;
    }
    let kind = match n >> (TAG_BITS + LAT_BITS + LON_BITS) {
        0 => Kind::Address,
        1 => Kind::Street,
        2 => Kind::Place,
        _ => Kind::Poi,
    };
    let lat = (n >> LON_BITS) & ((1 << LAT_BITS) - 1);
    let lon = n & ((1 << LON_BITS) - 1);
    let centre = |c: u64, lo: f64, span: f64, bits: u32| {
        lo + (c as f64 + 0.5) * span / (1u64 << bits) as f64
    };
    Some((
        kind,
        centre(lat, -90.0, 180.0, LAT_BITS),
        centre(lon, -180.0, 360.0, LON_BITS),
    ))
}

/// `N-123` (Nominatim's `osm_ids`, also `n-123`, `node/-123` is not read): the number, if it is one of ours.
pub fn parse_osm_id(s: &str) -> Option<i64> {
    let s = s.trim();
    let rest = s.strip_prefix('N').or_else(|| s.strip_prefix('n'))?;
    rest.parse::<i64>().ok().filter(|n| *n < 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(lat: f64, lon: f64, precision: &'static str, flags: Vec<&'static str>) -> Hit {
        Hit {
            lat,
            lon,
            precision,
            score: 1.0,
            confidence: 0.9,
            street: "Rue A".into(),
            housenumber: Some("1".into()),
            commune: "Ville".into(),
            postcode: "10000".into(),
            flags,
            region: None,
            distance_m: None,
        }
    }

    #[test]
    fn a_number_is_negative_within_53_bits_and_comes_back_to_its_kind_and_point() {
        for (lat, lon) in [
            (43.73, 7.42),
            (-33.8688, 151.2093),
            (89.999, -179.999),
            (-90.0, 180.0),
            (0.0, 0.0),
        ] {
            for (precision, flags, kind) in [
                ("house", vec![], Kind::Address),
                ("interp", vec![], Kind::Address),
                ("street", vec![], Kind::Street),
                ("city", vec![], Kind::Place),
                ("house", vec!["poi_layer"], Kind::Poi),
            ] {
                let id = encode(&hit(lat, lon, precision, flags));
                assert!(id < 0 && id.unsigned_abs() < 1 << 53, "{id}");
                let (k, la, lo) = decode(id).unwrap();
                assert_eq!(k, kind);
                // the centre of a cell of about 1.2 m by 2.4 m
                assert!(
                    (la - lat).abs() < 1.1e-5 && (lo - lon).abs() < 2.2e-5,
                    "{lat},{lon} -> {la},{lo}"
                );
            }
        }
    }

    #[test]
    fn two_points_three_metres_apart_two_kinds_and_two_addresses_at_one_point_have_different_numbers(
    ) {
        let a = encode(&hit(43.73, 7.42, "house", vec![]));
        assert_ne!(
            a,
            encode(&hit(43.73003, 7.42, "house", vec![])),
            "about 3.3 m north"
        );
        assert_ne!(
            a,
            encode(&hit(43.73, 7.42004, "house", vec![])),
            "about 3.2 m east"
        );
        // a house with two numbers at one point: the street and number bits part them
        let mut two = hit(43.73, 7.42, "house", vec![]);
        two.housenumber = Some("3".into());
        assert_ne!(tag(&hit(43.73, 7.42, "house", vec![])), tag(&two));
        assert_ne!(a, encode(&two));
        assert_ne!(a, encode(&hit(43.73, 7.42, "street", vec![])));
        assert_eq!(
            a,
            encode(&hit(43.73, 7.42, "near", vec![])),
            "the neighbour's door is an address too"
        );
    }

    #[test]
    fn only_our_negative_numbers_are_read() {
        let id = encode(&hit(43.73, 7.42, "house", vec![]));
        assert_eq!(parse_osm_id(&format!("N{id}")), Some(id));
        assert_eq!(parse_osm_id(&format!("n{id}")), Some(id));
        for bad in ["N123", "W-5", "R-5", "N", "Nabc", "-5", ""] {
            assert_eq!(parse_osm_id(bad), None, "{bad}");
        }
        assert_eq!(
            decode(5),
            None,
            "a positive number is an OpenStreetMap object"
        );
        assert_eq!(decode(-(1i64 << 54)), None, "beyond 53 bits");
    }
}
