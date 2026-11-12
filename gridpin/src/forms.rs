//! Forms of a query the address path does not know, read before it: a coordinate (decimal, degrees-minutes-
//! seconds, UTM, MGRS, Plus Code, geohash), a special address (post office box, military post, "care of",
//! milepost, motorway exit) and a street intersection.
//!
//! Every reader fires only on an unambiguous form of the WHOLE query and says `None` otherwise, so a query
//! that is not such a form goes the old way untouched. Each one has a table of "form / not a form" in its tests
//! and a reverse mutation that switches it off.

use crate::codes;

/// What a coordinate was written as; it becomes the flag `input_<kind>` of the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointKind {
    Decimal,
    Dms,
    Utm,
    Mgrs,
    PlusCode,
    Geohash,
}

impl PointKind {
    pub fn flag(self) -> &'static str {
        match self {
            PointKind::Decimal => "input_decimal",
            PointKind::Dms => "input_dms",
            PointKind::Utm => "input_utm",
            PointKind::Mgrs => "input_mgrs",
            PointKind::PlusCode => "input_plus_code",
            PointKind::Geohash => "input_geohash",
        }
    }
}

/// A query that is a point. `None` for anything else, including a short Plus Code (it needs a reference point).
pub fn point(q: &str) -> Option<(f64, f64, PointKind)> {
    let t = q.trim();
    if t.is_empty() || t.len() > 64 {
        return None;
    }
    if let Some((lat, lon)) = decimal_pair(t) {
        return Some((lat, lon, PointKind::Decimal));
    }
    if let Some((lat, lon)) = codes::dms_parse(t) {
        return Some((lat, lon, PointKind::Dms));
    }
    if let Some((lat, lon)) = codes::utm_parse(t) {
        return Some((lat, lon, PointKind::Utm));
    }
    if looks_like_mgrs(t) {
        if let Some((lat, lon)) = codes::mgrs_decode(t) {
            return Some((lat, lon, PointKind::Mgrs));
        }
    }
    if t.contains('+') && !t.contains(' ') {
        if let Some((lat, lon)) = codes::plus_code_decode(t) {
            return Some((lat, lon, PointKind::PlusCode));
        }
    }
    if let Some(code) = geohash_text(t) {
        if let Some((lat, lon)) = codes::geohash_decode(code) {
            return Some((lat, lon, PointKind::Geohash));
        }
    }
    None
}

/// "48.8566, 2.3522", "48.8566 2.3522", "48.8566N 2.3522E", "-33.85 151.21": two numbers, each with a decimal
/// point (so "12 34" or a five-digit ZIP never reads as a point), in range, with optional hemisphere letters.
fn decimal_pair(t: &str) -> Option<(f64, f64)> {
    let cleaned: String = t
        .chars()
        .map(|c| if c == ',' || c == ';' { ' ' } else { c })
        .collect();
    let parts: Vec<&str> = cleaned.split_whitespace().collect();
    let (a, b) = match parts.as_slice() {
        [a, b] => (*a, *b),
        // "48.85 N 2.35 E"
        [a, ha, b, hb] if ha.len() == 1 && hb.len() == 1 => {
            return decimal_pair(&format!("{a}{ha} {b}{hb}"));
        }
        _ => return None,
    };
    let read = |s: &str| -> Option<(f64, Option<char>)> {
        let (num, hemi) = match s.chars().last()? {
            c @ ('N' | 'S' | 'E' | 'W' | 'n' | 's' | 'e' | 'w') => {
                (&s[..s.len() - 1], Some(c.to_ascii_uppercase()))
            }
            _ => (s, None),
        };
        let (num, hemi) = match (hemi, num.chars().next()?) {
            (None, c @ ('N' | 'S' | 'E' | 'W')) => (&num[1..], Some(c)),
            (h, _) => (num, h),
        };
        let digits = num.trim_start_matches(['-', '+']);
        let (int, frac) = digits.split_once('.')?;
        if int.is_empty()
            || int.len() > 3
            || frac.is_empty()
            || !int.bytes().all(|b| b.is_ascii_digit())
            || !frac.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        Some((num.parse::<f64>().ok()?, hemi))
    };
    let (x, hx) = read(a)?;
    let (y, hy) = read(b)?;
    let sign = |v: f64, h: Option<char>| match h {
        Some('S') | Some('W') => -v.abs(),
        Some(_) => v.abs(),
        None => v,
    };
    let (lat, lon) = match (hx, hy) {
        (Some('E') | Some('W'), Some('N') | Some('S')) => (sign(y, hy), sign(x, hx)),
        (None | Some('N') | Some('S'), None | Some('E') | Some('W')) => (sign(x, hx), sign(y, hy)),
        _ => return None,
    };
    ((-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
        && !(lat == 0.0 && lon == 0.0))
        .then_some((lat, lon))
}

/// zone (1-2 digits), band letter, two square letters, then an even number of digits up to ten
fn looks_like_mgrs(t: &str) -> bool {
    let s: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    let b = s.as_bytes();
    let zone = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if !(1..=2).contains(&zone) || b.len() < zone + 3 {
        return false;
    }
    let letters = &b[zone..zone + 3];
    let digits = &b[zone + 3..];
    letters.iter().all(u8::is_ascii_alphabetic)
        && digits.iter().all(u8::is_ascii_digit)
        && digits.len().is_multiple_of(2)
        && digits.len() >= 2
        && digits.len() <= 10
}

/// "geohash:u4pruydqqvj", or a bare code of 9 to 12 characters from the geohash alphabet with a digit and at
/// least three letters ("1012XJ", a Dutch postcode, is too short; words have vowels the alphabet lacks: a, i, o).
fn geohash_text(t: &str) -> Option<&str> {
    if let Some(code) = t
        .strip_prefix("geohash:")
        .or_else(|| t.strip_prefix("GEOHASH:"))
    {
        return Some(code.trim());
    }
    let alphabet = "0123456789bcdefghjkmnpqrstuvwxyz";
    let ok = (9..=12).contains(&t.len())
        && t.bytes().all(|c| alphabet.as_bytes().contains(&c))
        && t.bytes().any(|c| c.is_ascii_digit())
        && t.bytes().filter(u8::is_ascii_alphabetic).count() >= 3;
    ok.then_some(t)
}

/// A special address: what it is, and the part of the query that still names a place (or, for "care of",
/// the whole address after the name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Special {
    pub kind: SpecialKind,
    pub rest: String,
    /// the first word of the form as written ("apartado", "postfach", "exit"): a street of the sheet may bear it
    pub lead: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecialKind {
    PoBox,
    Military,
    CareOf,
    Milepost,
    Exit,
}

impl SpecialKind {
    pub fn flag(self) -> &'static str {
        match self {
            SpecialKind::PoBox => "po_box",
            SpecialKind::Military => "military",
            SpecialKind::CareOf => "care_of",
            SpecialKind::Milepost => "milepost",
            SpecialKind::Exit => "exit",
        }
    }
}

const BOX_WORDS: [&[&str]; 8] = [
    &["p", "o", "box"],
    &["po", "box"],
    &["pob"],
    &["post", "office", "box"],
    &["apartado", "de", "correos"],
    &["apartado"],
    &["caixa", "postal"],
    &["postfach"],
];

fn words(q: &str) -> Vec<String> {
    q.split(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | ';' | ':'))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// The part of `q` after the first `skip` words (as `words` cuts them), keeping the original text.
fn after_words(q: &str, skip: usize) -> String {
    let mut seen = 0;
    let mut in_word = false;
    for (i, c) in q.char_indices() {
        let sep = c.is_whitespace() || matches!(c, ',' | '.' | ';' | ':');
        if !sep && !in_word {
            if seen == skip {
                return q[i..]
                    .trim()
                    .trim_start_matches([',', ';'])
                    .trim()
                    .to_string();
            }
            seen += 1;
        }
        in_word = !sep;
    }
    String::new()
}

/// A special address at the start of the query. A box needs its number right after the box words
/// ("Postfachstraße 3" is a street), a military post needs APO/FPO/DPO with AA/AE/AP, a milepost "mile N"
/// or "km N" with a road, an exit "exit N".
pub fn special(q: &str) -> Option<Special> {
    let w = words(q);
    let is_num = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.len() <= 7;
    for box_words in BOX_WORDS {
        let n = box_words.len();
        if w.len() > n
            && w[..n]
                .iter()
                .map(String::as_str)
                .eq(box_words.iter().copied())
            && is_num(&w[n])
        {
            return Some(Special {
                kind: SpecialKind::PoBox,
                rest: after_words(q, n + 1),
                lead: box_words[0],
            });
        }
    }
    for (i, word) in w.iter().enumerate() {
        if matches!(word.as_str(), "apo" | "fpo" | "dpo")
            && w.get(i + 1)
                .is_some_and(|x| matches!(x.as_str(), "aa" | "ae" | "ap"))
        {
            return Some(Special {
                kind: SpecialKind::Military,
                rest: after_words(q, i + 2),
                lead: "apo",
            });
        }
    }
    if w.len() > 2
        && (w[0] == "c/o"
            || (w[0] == "c" && w[1] == "o" && q.trim_start().to_lowercase().starts_with("c/o")))
    {
        // "c/o Name, the address": the address is what follows the first comma
        let (_, address) = q.split_once(',')?;
        let address = address.trim();
        return (!address.is_empty()).then(|| Special {
            kind: SpecialKind::CareOf,
            rest: address.to_string(),
            lead: "c/o",
        });
    }
    if w.len() >= 3
        && matches!(w[0].as_str(), "mile" | "milepost" | "mm" | "km" | "pk")
        && is_num(&w[1])
        && is_road(&w[2])
    {
        return Some(Special {
            kind: SpecialKind::Milepost,
            rest: after_words(q, 3),
            lead: "mile",
        });
    }
    if w.len() >= 2
        && matches!(w[0].as_str(), "exit" | "salida" | "sortie" | "ausfahrt")
        && is_num(&w[1])
    {
        return Some(Special {
            kind: SpecialKind::Exit,
            rest: after_words(q, 2),
            lead: "exit",
        });
    }
    None
}

/// "I-80", "us-101", "A-2", "N-VI", "SR 99" style road names.
fn is_road(w: &str) -> bool {
    let w = w.to_ascii_lowercase();
    let (head, num) = match w.split_once('-') {
        Some(p) => p,
        None => {
            let split = w.find(|c: char| c.is_ascii_digit()).unwrap_or(w.len());
            (&w[..split], &w[split..])
        }
    };
    matches!(
        head,
        "i" | "us" | "sr" | "a" | "n" | "ap" | "m" | "e" | "br" | "hwy" | "ca" | "tx" | "ny"
    ) && !num.is_empty()
        && num.len() <= 4
        && num.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// "1200 block of Main St, Springfield" (police and court records hide the house this way): the middle of the
/// hundred, "1250 Main St, Springfield". `None` for anything else; the block number must end in 00.
pub fn block(q: &str) -> Option<String> {
    let t = q.trim();
    let (num, rest) = t.split_once(char::is_whitespace)?;
    let n: u32 = num.parse().ok()?;
    if n < 100 || !n.is_multiple_of(100) || num.len() > 6 {
        return None;
    }
    let rest = rest.trim_start();
    let lower = rest.to_lowercase();
    let after = lower
        .strip_prefix("block of ")
        .or_else(|| lower.strip_prefix("blk of "))
        .or_else(|| lower.strip_prefix("block "))
        .or_else(|| lower.strip_prefix("blk "))?;
    let street = &rest[rest.len() - after.len()..];
    let street = street.trim();
    (!street.is_empty() && !street.eq_ignore_ascii_case("of"))
        .then(|| format!("{} {street}", n + 50))
}

/// Two streets and the place they are in: "Main St & 2nd Ave, Springfield". The connector is one of a
/// fixed set that is the same for every sheet (no branch by country, ANSWERS §60); the reader does not
/// decide alone whether the query IS an intersection — the caller first lets the whole line be a street,
/// and asks for the pair only when it is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crossing {
    pub first: String,
    pub second: String,
    /// the place after the first comma ("Springfield, IL"), empty when there is none
    pub place: String,
}

/// Connectors written between two street names. "and", "y", "e", "et", "i", "en", "und" are only taken
/// with spaces around them; the symbols also without.
const CONNECTORS: [&str; 17] = [
    " & ",
    " and ",
    " at ",
    " y ",
    " con ",
    " esquina ",
    " esq ",
    " e ",
    " et ",
    " angle ",
    " angolo ",
    " en ",
    " hoek ",
    " und ",
    " ecke ",
    " i ",
    " ugao ",
];

pub fn crossing(q: &str) -> Option<Crossing> {
    let (streets, place) = match q.split_once(',') {
        Some((s, p)) => (s.trim(), p.trim()),
        None => (q.trim(), ""),
    };
    // a house number anywhere in the street part means an address, not a crossing ("5th" is a name)
    let has_number = streets
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()));
    if has_number || streets.is_empty() {
        return None;
    }
    let lower = format!(
        " {} ",
        streets.to_lowercase().replace(['&', '/', '@'], " & ")
    );
    let lower = lower.replace("corner of ", "").replace(" esq. ", " esq ");
    let lower = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    let padded = format!(" {lower} ");
    let mut found: Option<(usize, &str)> = None;
    for c in CONNECTORS {
        if let Some(at) = padded.find(c) {
            if padded[at + c.len()..].contains(c) {
                return None; // the same connector twice: not one crossing
            }
            if found.is_none_or(|(f, _)| at < f) {
                found = Some((at, c));
            }
        }
    }
    let (at, c) = found?;
    let first = padded[..at].trim().to_string();
    let second = padded[at + c.len()..].trim().to_string();
    let enough = |s: &str| s.chars().filter(|c| c.is_alphabetic()).count() >= 2;
    (enough(&first) && enough(&second)).then(|| Crossing {
        first,
        second,
        place: place.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_are_read_in_every_written_form() {
        let paris = (48.8566, 2.3522);
        let near = |p: (f64, f64, PointKind), want: (f64, f64), m: f64| {
            codes::haversine_m((p.0, p.1), want) <= m
        };
        for (q, kind) in [
            ("48.8566, 2.3522", PointKind::Decimal),
            ("48.8566 2.3522", PointKind::Decimal),
            ("48.8566N 2.3522E", PointKind::Decimal),
            ("N48.8566 E2.3522", PointKind::Decimal),
            ("2.3522E 48.8566N", PointKind::Decimal),
            ("48.8566 N, 2.3522 E", PointKind::Decimal),
            ("48°51'24\"N 2°21'08\"E", PointKind::Dms),
        ] {
            let p = point(q).unwrap_or_else(|| panic!("{q}"));
            assert_eq!(p.2, kind, "{q}");
            assert!(near(p, paris, 30.0), "{q}: {p:?}");
        }
        let eiffel = (48.8584, 2.2945);
        assert_eq!(point("31U 448252 5411955").unwrap().2, PointKind::Utm);
        assert!(near(point("31U 448252 5411955").unwrap(), eiffel, 2.0));
        assert_eq!(point("31UDQ4825211954").unwrap().2, PointKind::Mgrs);
        assert_eq!(point("31U DQ 48252 11954").unwrap().2, PointKind::Mgrs);
        assert_eq!(point("8FVC9G8F+6X").unwrap().2, PointKind::PlusCode);
        assert_eq!(point("geohash:u4pruydqqvj").unwrap().2, PointKind::Geohash);
        assert_eq!(point("u4pruydqqvj").unwrap().2, PointKind::Geohash);
        let sydney = point("-33.8568, 151.2153").unwrap();
        assert!(sydney.0 < 0.0 && sydney.1 > 0.0);
    }

    #[test]
    fn addresses_and_codes_that_look_like_numbers_are_not_points() {
        for q in [
            "11631 Moonmist Dr, Houston, TX 77072", // five-digit numbers (2.8-twelfth)
            "12 34",
            "77072",
            "1012XJ", // Dutch postcode
            "1012 XJ Amsterdam",
            "9G8F+6X", // short Plus Code: needs a reference point
            "Calle Sol 5",
            "48 51",
            "0.0, 0.0",
            "48. 2.", // a dot without decimals: a list number, not a coordinate
            "95.1, 2.0",
            "bergstrasse", // a word in the geohash letters but without digits
            "Main St & 2nd Ave",
            "3.5 Mile Road",
            "Postfach 12 34",
        ] {
            assert_eq!(point(q), None, "{q}");
        }
    }

    #[test]
    fn special_addresses_are_named_and_keep_their_place() {
        let s = special("PO Box 123, Springfield, IL 62701").unwrap();
        assert_eq!(s.kind, SpecialKind::PoBox);
        assert_eq!(s.rest, "Springfield, IL 62701");
        assert_eq!(
            special("P.O. Box 42 Austin TX").unwrap().kind,
            SpecialKind::PoBox
        );
        assert_eq!(
            special("Postfach 1234, 10117 Berlin").unwrap().rest,
            "10117 Berlin"
        );
        assert_eq!(
            special("Apartado de Correos 5, Madrid").unwrap().kind,
            SpecialKind::PoBox
        );
        assert_eq!(
            special("Caixa Postal 100, São Paulo").unwrap().kind,
            SpecialKind::PoBox
        );
        let m = special("Unit 2050 Box 4190, APO AP 96278").unwrap();
        assert_eq!(m.kind, SpecialKind::Military);
        let c = special("c/o Jane Doe, 1600 Pennsylvania Ave NW, Washington DC").unwrap();
        assert_eq!(
            (c.kind, c.rest.as_str()),
            (
                SpecialKind::CareOf,
                "1600 Pennsylvania Ave NW, Washington DC"
            )
        );
        assert_eq!(
            special("mile 47 I-80, Wyoming").unwrap().kind,
            SpecialKind::Milepost
        );
        assert_eq!(special("km 12 A-2, Madrid").unwrap().rest, "Madrid");
        assert_eq!(
            special("Exit 12, Newark NJ").unwrap().kind,
            SpecialKind::Exit
        );
    }

    #[test]
    fn streets_that_merely_start_like_a_special_form_are_not_one() {
        for q in [
            "Postfachstraße 3, Berlin",
            "Box Hill Road 5, Melbourne",
            "Mile End Road 12, London",
            "Exit Street 4",
            "Calle Apartado 3",
            "12 Coolidge Ave",
            "km 12, Madrid", // no road named
            "Apostle Street 4",
        ] {
            assert_eq!(special(q), None, "{q}");
        }
    }

    #[test]
    fn a_block_is_the_middle_of_its_hundred() {
        assert_eq!(
            block("1200 block of Main St, Springfield").as_deref(),
            Some("1250 Main St, Springfield")
        );
        assert_eq!(block("300 Block Elm Ave").as_deref(), Some("350 Elm Ave"));
        assert_eq!(
            block("4500 blk of N Broadway, Chicago, IL").as_deref(),
            Some("4550 N Broadway, Chicago, IL")
        );
        for q in [
            "1250 block of Main St",
            "12 block of Main St",
            "1200 Main St",
            "1200 block of",
            "Block 1200 Main St",
            "1200 Blockhouse Road",
        ] {
            assert_eq!(block(q), None, "{q}");
        }
    }

    #[test]
    fn a_crossing_needs_two_named_streets_and_no_house_number() {
        let c = crossing("Main St & 2nd Ave, Springfield, IL").unwrap();
        assert_eq!(
            (c.first.as_str(), c.second.as_str(), c.place.as_str()),
            ("main st", "2nd ave", "Springfield, IL")
        );
        assert_eq!(
            crossing("Broadway and W 42nd St, New York").unwrap().second,
            "w 42nd st"
        );
        assert_eq!(
            crossing("Calle Mayor esq. Calle Toledo, Madrid")
                .unwrap()
                .second,
            "calle toledo"
        );
        assert_eq!(crossing("Main/Elm").unwrap().first, "main");
        assert_eq!(
            crossing("corner of King St and Queen St, Toronto")
                .unwrap()
                .first,
            "king st"
        );
        for q in [
            "1600 Pennsylvania Ave NW, Washington",
            "Main St 5 & Elm St, Springfield",
            "Main St, Springfield",
            "A & B",
            "x & y & z",
        ] {
            assert_eq!(crossing(q), None, "{q}");
        }
    }
}
