//! What happened to each field of the query in the answer: confirmed, corrected, inferred, replaced, unresolved.
//!
//! An answer can be right and still differ from what was asked: a street found one letter off, a postcode the
//! query did not have, the neighbour's house instead of the asked number. This reads the query as the engine read
//! it (the first parse hypothesis of the `--diagnose` trace) and compares it with the answer, field by field, from
//! the answer's own flags; nothing is looked up again. The verdicts are the explanation (free, `--with verdict`);
//! what to do with a row (accept, confirm with a person, fix) is decided in gridpin-pro.
//!
//! The town is read from the line itself (`named_town`): the engine may drop the town it did not find ("80331
//! München" on a sheet without Bavaria) and answer a house elsewhere; such a town is `replaced` or `unresolved`,
//! never `inferred` (ANSWERS §308, revision of 0.9.0).

use serde::Serialize;

use crate::norm::normalize;
use crate::query::{Hit, Index};

/// One of `VERDICTS` per field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FieldVerdicts {
    pub street: &'static str,
    pub number: &'static str,
    pub postcode: &'static str,
    pub city: &'static str,
}

/// confirmed: as asked; corrected: matched with a fix (spelling, the start of a name); inferred: not in the query,
/// filled by the answer; replaced: the query said one thing and the answer another; unresolved: asked and not found;
/// absent: neither in the query nor in the answer.
pub const VERDICTS: [&str; 6] = [
    "confirmed",
    "corrected",
    "inferred",
    "replaced",
    "unresolved",
    "absent",
];

/// An answer this far from where the named town alone answers is in another town (the radius the engine keeps a
/// named locality in, `query.rs`).
pub const TOWN_RADIUS_KM: f64 = 25.0;

/// A district of a city that is a commune of its own lies this close to the city's point (Berlin: Spandau and
/// Köpenick are 13–15 km from it).
pub const DISTRICT_RADIUS_KM: f64 = 15.0;

/// The town a line names (a segment after the first, other than a country or a region code).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NamedTown {
    /// normalized
    pub name: String,
    /// the sheet knows the name: a commune, the leading words of one, or a place the name alone answers
    pub on_sheet: bool,
    /// where the name alone answers as a town of the sheet
    pub at: Option<(f64, f64)>,
    /// the name is kept under other commune names: a group of districts ("Paris"), a town of the place table ("Niš"
    /// in the commune "Niš \"ćele Kula\"", "Beograd" in Zemun); any house within `TOWN_RADIUS_KM` of `at` is in it
    pub elsewhere: bool,
}

/// How the engine read the query: postcode and house number of its first hypothesis, and the town the line names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Asked {
    pub postcode: Option<u32>,
    pub number: Option<u32>,
    pub town: Option<NamedTown>,
}

/// Read `q` as the address path reads it (one traced query), and the town the line names.
pub fn asked(idx: &Index, q: &str) -> Asked {
    Asked {
        town: named_town(idx, q),
        ..reading(idx, q)
    }
}

/// The postcode and house number of `q` as the address path reads them, without the town (`town: None`): for a
/// caller that judges only the street and the number (the windows of `extract`), one query instead of two.
pub fn reading(idx: &Index, q: &str) -> Asked {
    let scope = crate::diagnosis::Scope::new();
    let _ = idx.query(q, 1);
    let trace = scope.finish(0);
    let first = &trace["parsed"]["examples"][0];
    let read = |k: &str| first[k].as_u64().and_then(|v| u32::try_from(v).ok());
    Asked {
        postcode: read("postcode"),
        number: read("house_number"),
        town: None,
    }
}

/// The name of a town segment: its words without digits ("80331 München" -> "München"), before a region code
/// ("Campo Grande - MS" -> "Campo Grande"); `None` for a region or state code ("FL 33040", "MN"), which is not a town.
pub fn town_words(segment: &str) -> Option<String> {
    let name = segment
        .split_whitespace()
        .filter(|w| !w.chars().any(|c| c.is_ascii_digit()))
        .collect::<Vec<_>>()
        .join(" ");
    let name = name
        .split(" - ")
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    (name.chars().filter(|c| c.is_alphabetic()).count() > 2).then_some(name)
}

/// Where `name` alone answers as a town of the sheet, and whether the answer's commune is another name (see
/// `NamedTown::elsewhere`).
fn town_point(idx: &Index, name: &str, region: &str) -> Option<((f64, f64), bool)> {
    // the rest of the line names the region of a town with namesakes ("St Paul, MN": not St. Paul, Missouri)
    let asked = if region.is_empty() {
        name.to_string()
    } else {
        format!("{name}, {region}")
    };
    let hits = crate::query::query_cascade(idx, None, &asked, 1);
    let h = hits.first().filter(|h| {
        h.precision == "city"
            && (h.flags.contains(&"place_exact") || h.flags.contains(&"place_prefix_group"))
    })?;
    let own_name =
        normalize(&h.commune) == normalize(name) && !h.flags.contains(&"place_prefix_group");
    Some(((h.lat, h.lon), !own_name))
}

/// The town the line `q` names: of the segments after the first (the first carries the street), from the end, the
/// first the sheet knows; when the sheet knows none, the last one, marked not on the sheet. A line without a comma
/// names no town this way.
///
/// The forms people write a town in, read on the owner's points of DE (revision of 0.9.0): a commune whose own name
/// holds a comma ("Sarstedt, Stadt": two segments are one name), a town with its district ("Köln-Nippes",
/// "Stuttgart Ost", "Frankfurt am Main-Innenstadt I", "Oldenburg (Oldb)": the leading words are the town when they
/// are a commune of that exact name; the start of a longer commune's name is not enough, "Bad" is not a town).
pub fn named_town(idx: &Index, q: &str) -> Option<NamedTown> {
    let segments: Vec<&str> = q
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let known = |name: String, point: Option<((f64, f64), bool)>| NamedTown {
        name,
        on_sheet: true,
        at: point.map(|p| p.0),
        elsewhere: point.is_some_and(|p| p.1),
    };
    // the words without digits of the segments after `i` ("MN 55106, USA" -> "MN, USA")
    let region_after = |i: usize| {
        segments[i + 1..]
            .iter()
            .map(|s| {
                s.split_whitespace()
                    .filter(|w| !w.chars().any(|c| c.is_ascii_digit()))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    };
    // a commune whose name holds a comma
    for i in (1..segments.len().saturating_sub(1)).rev() {
        let pair = normalize(&format!("{} {}", segments[i], segments[i + 1]));
        if crate::country_names::code_of(segments[i + 1]).is_none() && idx.has_commune(&pair) {
            return Some(known(
                pair.clone(),
                town_point(idx, &pair, &region_after(i + 1)),
            ));
        }
    }
    let mut unknown: Option<NamedTown> = None;
    for (i, s) in segments.iter().enumerate().skip(1).rev() {
        if crate::country_names::code_of(s).is_some() {
            continue;
        }
        let region = region_after(i);
        let Some(written) = town_words(s) else {
            continue;
        };
        let name = normalize(&written);
        if name.is_empty() {
            continue;
        }
        // a commune of this very name, or of its leading words ("koln nippes" -> "koln", "oldenburg oldb" ->
        // "oldenburg"): its houses carry that name
        let words: Vec<&str> = name.split(' ').collect();
        if let Some(n) = (1..=words.len())
            .rev()
            .find(|&n| idx.has_commune(&words[..n].join(" ")))
        {
            let lead = words[..n].join(" ");
            let point = town_point(idx, &lead, &region);
            return Some(known(lead, point));
        }
        // the start of communes' names ("paris" of "paris 2e arrondissement") or a town of the place table
        // ("beograd"): where the name answers as a town. Only for the whole name: the place table reads a longer
        // line loosely ("Oldenburg (Oldb)" answered in Vorpommern)
        let point = town_point(idx, &written, &region);
        if point.is_some() || idx.names_commune(&name) {
            return Some(known(name, point));
        }
        unknown.get_or_insert(NamedTown {
            name,
            on_sheet: false,
            at: None,
            elsewhere: false,
        });
    }
    unknown
}

fn leading_number(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn digits(s: &str) -> Option<u32> {
    let d: String = s.chars().filter(char::is_ascii_digit).collect();
    d.parse().ok()
}

fn dist_km((a, b): (f64, f64), (c, d): (f64, f64)) -> f64 {
    crate::codes::haversine_m((a, b), (c, d)) / 1000.0
}

/// Is the answer in the town the line names: the same commune (or a district named after it, "Paris 2e
/// Arrondissement"; the town before its district, "Magdeburg" of "Magdeburg-Altstadt", when the sheet does not know
/// the whole), a commune that carries the town's name within `TOWN_RADIUS_KM` ("Novi Beograd"), or any commune
/// within that radius of a town kept under other names (`NamedTown::elsewhere`) or with the line's own postcode. The answer's `commune_exact` flag
/// is not enough: "Partizanska 30, Niš" carries it in Žitkovac, 28 km away. A city that is a commune of its own and
/// holds its houses in districts ("Mitte" for "…, Berlin") needs the sheet: `field_verdicts_in`.
pub fn in_named_town(t: &NamedTown, h: &Hit) -> bool {
    let c = normalize(&h.commune);
    // the commune's own name, without what follows a comma or a bracket ("Magdeburg, Landeshauptstadt")
    let core = normalize(h.commune.split([',', '(']).next().unwrap_or_default());
    let near = |radius: f64| t.at.is_some_and(|at| dist_km(at, (h.lat, h.lon)) <= radius);
    let carries_name = format!(" {c} ").contains(&format!(" {} ", t.name));
    c == t.name
        || c.starts_with(&format!("{} ", t.name))
        || (!t.on_sheet && !core.is_empty() && t.name.starts_with(&format!("{core} ")))
        // the line's own postcode at the house, near the town: a postal town over its municipalities ("Brooklyn, NY
        // 11238" in Park Slope, "El Paso" in "City Of El Paso"); far away the postcode is another town's ("13100
        // Lille" in Aix-en-Provence). When the sheet gives no point for the town ("St Paul, MN" answers as a street),
        // the postcode alone decides
        || ((carries_name || h.flags.contains(&"pc_exact"))
            && (near(TOWN_RADIUS_KM) || (t.at.is_none() && h.flags.contains(&"pc_exact"))))
        || (t.elsewhere && near(TOWN_RADIUS_KM))
}

/// The named town is the answer's street ("10178 Berlin, Spreeufer 4" read backwards): not a town.
fn town_is_the_street(t: &NamedTown, h: &Hit) -> bool {
    let street = normalize(&h.street);
    let words: Vec<&str> = street.split(' ').collect();
    !t.on_sheet && t.name.split(' ').all(|w| words.contains(&w))
}

/// The verdicts of one answer against what was asked.
pub fn field_verdicts(asked: Asked, h: &Hit) -> FieldVerdicts {
    field_verdicts_of(&asked, h)
}

/// `field_verdicts` with the sheet at hand: a house of a district of the named city ("Spreeufer 4, 10178 Berlin" in
/// Mitte, ANSWERS §284 p.5) is in the city, not `replaced`. Every surface of the free engine (`exists`, `--with`,
/// `format --out iso20022`, `explain`) reads this one.
pub fn field_verdicts_in(idx: &Index, q: &str, asked: &Asked, h: &Hit) -> FieldVerdicts {
    let mut v = field_verdicts_of(asked, h);
    // the districts of a city are close to it: the engine's own test reaches 25 km and takes Gornji Milanovac for
    // Čačak and Bačka Topola for Subotica (revision of 0.9.0)
    let close = asked.town.as_ref().is_none_or(|t| {
        t.at.is_none_or(|at| dist_km(at, (h.lat, h.lon)) <= DISTRICT_RADIUS_KM)
    });
    if v.city == "replaced" && close && idx.hit_in_named_city_of_districts(q, h) == Some(true) {
        v.city = "confirmed";
    }
    v
}

/// `field_verdicts` without taking the reading.
pub fn field_verdicts_of(asked: &Asked, h: &Hit) -> FieldVerdicts {
    let has = |f: &str| h.flags.contains(&f);
    let street = if h.street.is_empty() {
        "unresolved"
    } else if has("street_exact") || has("intersection") {
        "confirmed"
    } else {
        "corrected"
    };
    let answered_number = h.housenumber.as_deref().and_then(leading_number);
    let number = match (asked.number, answered_number) {
        (None, None) => "absent",
        (None, Some(_)) => "inferred",
        (Some(_), None) => "unresolved",
        (Some(a), Some(b)) if a == b && h.precision != "near" => "confirmed",
        (Some(_), Some(_)) => "replaced",
    };
    let answered_code = (!h.postcode.is_empty())
        .then(|| digits(&h.postcode))
        .flatten();
    let postcode = match (asked.postcode, answered_code) {
        (None, None) => "absent",
        (None, Some(_)) => "inferred",
        (Some(_), None) => "unresolved",
        (Some(_), Some(_)) if has("pc_exact") => "confirmed",
        // the sheet keeps a short form of some codes (five digits of a Brazilian CEP): compare the start
        (Some(a), Some(b))
            if a.to_string().starts_with(&b.to_string())
                || b.to_string().starts_with(&a.to_string()) =>
        {
            "confirmed"
        }
        (Some(_), Some(_)) => "replaced",
    };
    let by_flags = if has("commune_exact") || has("place_exact") {
        "confirmed"
    } else if has("commune_prefix") || has("place_prefix_group") {
        "corrected"
    } else if h.commune.is_empty() {
        "absent"
    } else {
        "inferred"
    };
    let city = match &asked.town {
        Some(t) if !town_is_the_street(t, h) => {
            if in_named_town(t, h) {
                if by_flags == "corrected" {
                    "corrected"
                } else {
                    "confirmed"
                }
            } else if t.on_sheet {
                "replaced"
            } else {
                "unresolved"
            }
        }
        _ => by_flags,
    };
    FieldVerdicts {
        street,
        number,
        postcode,
        city,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(
        precision: &'static str,
        number: Option<&str>,
        postcode: &str,
        flags: Vec<&'static str>,
    ) -> Hit {
        Hit {
            lat: 0.0,
            lon: 0.0,
            precision,
            score: 1.0,
            confidence: 0.9,
            street: "Main Street".into(),
            housenumber: number.map(String::from),
            commune: "Springfield".into(),
            postcode: postcode.into(),
            flags,
            region: None,
            distance_m: None,
        }
    }

    const ASKED: Asked = Asked {
        postcode: Some(62701),
        number: Some(12),
        town: None,
    };

    #[test]
    fn an_answer_as_asked_is_confirmed_field_by_field() {
        let v = field_verdicts(
            ASKED,
            &hit(
                "house",
                Some("12"),
                "62701",
                vec!["street_exact", "commune_exact", "pc_exact"],
            ),
        );
        assert_eq!(
            (v.street, v.number, v.postcode, v.city),
            ("confirmed", "confirmed", "confirmed", "confirmed")
        );
    }

    #[test]
    fn each_kind_of_difference_has_its_word() {
        let fuzzy = field_verdicts(
            ASKED,
            &hit(
                "house",
                Some("12"),
                "62701",
                vec!["street_fuzzy", "commune_prefix"],
            ),
        );
        assert_eq!((fuzzy.street, fuzzy.city), ("corrected", "corrected"));
        let neighbour = field_verdicts(
            ASKED,
            &hit("near", Some("14"), "62702", vec!["street_exact"]),
        );
        assert_eq!(
            (neighbour.number, neighbour.postcode, neighbour.city),
            ("replaced", "replaced", "inferred")
        );
        let same_number_near = field_verdicts(
            ASKED,
            &hit("near", Some("12"), "62701", vec!["street_exact"]),
        );
        assert_eq!(
            same_number_near.number, "replaced",
            "near is the neighbour's point even with the same digits"
        );
        let street_only = field_verdicts(ASKED, &hit("street", None, "", vec!["street_exact"]));
        assert_eq!(
            (street_only.number, street_only.postcode),
            ("unresolved", "unresolved")
        );
        let nothing_asked = field_verdicts(
            Asked::default(),
            &hit("house", Some("5"), "62701", vec!["street_exact"]),
        );
        assert_eq!(
            (nothing_asked.number, nothing_asked.postcode),
            ("inferred", "inferred")
        );
        let mut town = hit("city", None, "", vec!["place_exact"]);
        town.street = String::new();
        let v = field_verdicts(Asked::default(), &town);
        assert_eq!(
            (v.street, v.number, v.postcode, v.city),
            ("unresolved", "absent", "absent", "confirmed")
        );
        for v in [fuzzy, neighbour, street_only, nothing_asked] {
            for word in [v.street, v.number, v.postcode, v.city] {
                assert!(VERDICTS.contains(&word), "{word}");
            }
        }
    }

    fn town(name: &str, on_sheet: bool, at: Option<(f64, f64)>) -> Asked {
        Asked {
            town: Some(NamedTown {
                name: name.into(),
                on_sheet,
                at,
                elsewhere: at.is_some(),
            }),
            ..ASKED
        }
    }

    #[test]
    fn a_named_town_is_confirmed_only_in_that_town() {
        let house = |commune: &str, lat: f64, flags: Vec<&'static str>| {
            let mut h = hit("house", Some("12"), "62701", flags);
            h.commune = commune.into();
            h.lat = lat;
            h.lon = 21.92;
            h
        };
        // the same commune, or a district named after it
        let springfield = town("springfield", true, None);
        let v = field_verdicts(
            springfield.clone(),
            &house("Springfield", 0.0, vec!["street_exact"]),
        );
        assert_eq!(v.city, "confirmed");
        let v = field_verdicts(
            town("paris", true, None),
            &house("Paris 2e Arrondissement", 0.0, vec!["street_exact"]),
        );
        assert_eq!(v.city, "confirmed");
        // the near miss: a commune whose name starts with the town's letters but is another place ("Rostocker Heide")
        let v = field_verdicts(
            town("rostock", true, None),
            &house(
                "Rostocker Heide",
                0.0,
                vec!["street_exact", "commune_prefix"],
            ),
        );
        assert_eq!(v.city, "replaced");
        // another commune: replaced, and the engine's commune_exact on it does not make it the named town
        let v = field_verdicts(
            springfield.clone(),
            &house("Shelbyville", 0.0, vec!["street_exact", "commune_exact"]),
        );
        assert_eq!(v.city, "replaced");
        // a town of the place table: within the radius of its point it is the town, beyond it another one
        let nis = town("nis", true, Some((43.31, 21.92)));
        assert_eq!(
            field_verdicts(nis.clone(), &house("Medijana", 43.32, vec!["street_exact"])).city,
            "confirmed"
        );
        // the edge: 0.22° north is 24.5 km, inside; 0.24° is 26.7 km, outside
        assert_eq!(
            field_verdicts(nis.clone(), &house("Zitkovac", 43.53, vec!["street_exact"])).city,
            "confirmed"
        );
        assert_eq!(
            field_verdicts(nis, &house("Zitkovac", 43.55, vec!["street_exact"])).city,
            "replaced"
        );
        // the line's postcode at the house: a postal town over its municipalities, near the town only
        let springfield_at = Asked {
            town: Some(NamedTown {
                name: "springfield".into(),
                on_sheet: true,
                at: Some((43.31, 21.92)),
                elsewhere: false,
            }),
            ..ASKED
        };
        assert_eq!(
            field_verdicts(
                springfield_at.clone(),
                &house("Alamo Heights", 43.35, vec!["street_exact", "pc_exact"])
            )
            .city,
            "confirmed"
        );
        assert_eq!(
            field_verdicts(
                springfield_at.clone(),
                &house("Alamo Heights", 43.35, vec!["street_exact"])
            )
            .city,
            "replaced",
            "near the town without the line's postcode: another town"
        );
        assert_eq!(
            field_verdicts(
                springfield_at,
                &house("Aix", 44.31, vec!["street_exact", "pc_exact"])
            )
            .city,
            "replaced",
            "the line's postcode far from the town it names"
        );
        // a town the sheet does not know whose leading words are the answer's commune without its suffix
        // ("Magdeburg-Altstadt" in "Magdeburg, Landeshauptstadt")
        assert_eq!(
            field_verdicts(
                town("grande ville altstadt", false, None),
                &house("Grande Ville, Stadt", 0.0, vec!["street_exact"])
            )
            .city,
            "confirmed"
        );
        assert_eq!(
            field_verdicts(
                town("grande ville altstadt", true, None),
                &house("Grande Ville, Stadt", 0.0, vec!["street_exact"])
            )
            .city,
            "replaced",
            "a town of the sheet is matched by its own name, not by the start of the line's"
        );
        // a town the sheet does not know: unresolved, never inferred
        let v = field_verdicts(
            town("munchen", false, None),
            &house("Petershagen", 0.0, vec!["street_exact", "dropped_suffix"]),
        );
        assert_eq!(v.city, "unresolved");
        // ... unless the "town" is the answer's own street (a line written town first)
        let mut street = house("Mitte", 0.0, vec!["street_exact"]);
        street.street = "Spreeufer".into();
        let v = field_verdicts(town("spreeufer", false, None), &street);
        assert_eq!(v.city, "inferred");
        // no town named: as before, from the flags
        let v = field_verdicts(ASKED, &house("Shelbyville", 0.0, vec!["street_exact"]));
        assert_eq!(v.city, "inferred");
        // a prefix match in the named town stays corrected
        let v = field_verdicts(
            springfield,
            &house("Springfield", 0.0, vec!["street_exact", "commune_prefix"]),
        );
        assert_eq!(v.city, "corrected");
    }

    #[test]
    fn a_town_segment_is_its_words_without_digits_and_a_region_code_is_none() {
        assert_eq!(town_words("80331 München").as_deref(), Some("München"));
        assert_eq!(
            town_words("Campo Grande - MS").as_deref(),
            Some("Campo Grande")
        );
        assert_eq!(town_words("FL 33040"), None);
        assert_eq!(town_words("MN"), None);
        assert_eq!(
            town_words("Ulm").as_deref(),
            Some("Ulm"),
            "three letters are a town"
        );
    }

    #[test]
    fn a_short_stored_code_is_the_asked_code() {
        let br = Asked {
            postcode: Some(79075224),
            number: Some(5),
            town: None,
        };
        let v = field_verdicts(br, &hit("house", Some("5"), "79075", vec!["street_exact"]));
        assert_eq!(v.postcode, "confirmed");
    }
}
