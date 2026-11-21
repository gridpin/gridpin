//! The parts of an address, one model for every structured format the engine writes (ISO 20022, FHIR, CLDXF, …).
//!
//! There is one reading of a line into parts: `iso20022::build`, which writes structure only from an answer that
//! agrees with the line field for field (a house the sheet confirms as written, the town the line names; a weak answer
//! or a house in another town is treated as no answer — the lesson of the 0.9.0 revision). `of` turns its outcome into
//! `Parts`, so a format built on them can never be more structured than ISO 20022 for the same line, and ISO 20022
//! itself is unchanged by construction.
//!
//! On a sheet whose street type comes last (`city_tail street_type_last`: the United States, Canada, Australia) the
//! street is split into a direction before, the name, the type and a direction after — by the sheet's own street-type
//! words, never leaving the name empty ("North Street" stays a name and a type). Elsewhere the street stays whole in
//! `street_name` and the split fields are absent: a split is not invented.

use serde::Serialize;

use crate::iso20022::{self, Outcome};
use crate::norm::normalize;
use crate::query::{Hit, Index};

/// How much of the address a format may write as structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// every part confirmed by the sheet
    Structured,
    /// the town (and country, a confirmed postcode) only; the rest is the line as written
    AsWritten,
    /// nothing confirmed: the line as written and the reasons
    None,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Structured => "structured",
            Mode::AsWritten => "as_written",
            Mode::None => "none",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Parts {
    pub number: Option<String>,
    pub number_suffix: Option<String>,
    pub pre_direction: Option<String>,
    pub street_name: Option<String>,
    pub street_type: Option<String>,
    pub post_direction: Option<String>,
    pub unit_designator: Option<String>,
    pub unit_id: Option<String>,
    pub floor: Option<String>,
    pub po_box: Option<String>,
    pub locality: Option<String>,
    pub district: Option<String>,
    pub region: Option<String>,
    pub postcode: Option<String>,
    pub country: Option<String>,
}

/// The parts of a line, how much of them is confirmed, the reasons (those of ISO 20022), and the line as written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reading {
    pub mode: Mode,
    pub parts: Parts,
    pub reasons: Vec<String>,
    pub text: String,
}

const DIRECTIONS: [&str; 8] = [
    "north",
    "south",
    "east",
    "west",
    "northeast",
    "northwest",
    "southeast",
    "southwest",
];

fn some(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// "12bis" -> ("12", "bis"); "5" -> ("5", none); a number that does not start with a digit, or holds a second
/// number, stays whole.
pub fn split_number(n: &str) -> (Option<String>, Option<String>) {
    let n = n.trim();
    let digits: String = n.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return (some(n), None);
    }
    let rest = n[digits.len()..].trim_start_matches([' ', '-', '/']);
    // "1-9", "5/7": a range or a second number is not a suffix (the 0.9.0 revision turned "1-9" into "1")
    if rest.chars().any(|c| c.is_ascii_digit()) {
        return (some(n), None);
    }
    (Some(digits), some(rest))
}

/// (pre direction, name, type, post direction) of a street on a sheet whose type comes last; `None` elsewhere.
pub fn split_street(idx: &Index, street: &str) -> Option<[Option<String>; 4]> {
    let type_last = idx
        .rule_set()
        .city_tail
        .get("street_type_last")
        .is_some_and(|v| v == "1");
    type_last.then(|| split_words(street, |w| idx.is_street_type(&normalize(w))))
}

/// The split itself: a direction last, a street type last, a direction first, each only when a word stays after it,
/// so the name is never left empty ("North Street": name "North", type "Street"; "Broadway East": name "Broadway").
pub fn split_words(street: &str, is_type: impl Fn(&str) -> bool) -> [Option<String>; 4] {
    let mut words: Vec<&str> = street.split_whitespace().collect();
    let is_dir = |w: &str| DIRECTIONS.contains(&normalize(w).as_str());
    let mut post = None;
    if words.len() > 1 && words.last().is_some_and(|w| is_dir(w)) {
        post = words.pop().map(str::to_string);
    }
    let mut kind = None;
    if words.len() > 1 && words.last().is_some_and(|w| is_type(w)) {
        kind = words.pop().map(str::to_string);
    }
    let mut pre = None;
    if words.len() > 1 && is_dir(words[0]) {
        pre = Some(words.remove(0).to_string());
    }
    [pre, some(&words.join(" ")), kind, post]
}

/// The district a hit names when the town written is the whole city ("Paris 2e Arrondissement" for "Paris").
fn district_of(h: Option<&Hit>, locality: &str) -> Option<String> {
    let c = h.map(|h| h.commune.as_str()).unwrap_or_default();
    (!c.is_empty() && !locality.is_empty() && normalize(c) != normalize(locality))
        .then(|| c.to_string())
}

/// The parts of `q` from the outcome of ISO 20022 and the hit it judged.
pub fn from_outcome(idx: &Index, q: &str, o: &Outcome, h: Option<&Hit>) -> Reading {
    let a = &o.adr;
    let mode = match o.mode {
        iso20022::Mode::Structured => Mode::Structured,
        iso20022::Mode::Hybrid => Mode::AsWritten,
        iso20022::Mode::None => Mode::None,
    };
    let mut p = Parts {
        locality: some(&a.twn_nm),
        region: some(&a.ctry_sub_dvsn),
        postcode: some(&a.pst_cd),
        country: some(&a.ctry),
        po_box: some(&a.pst_bx),
        floor: some(&a.flr),
        ..Parts::default()
    };
    if mode == Mode::Structured {
        (p.number, p.number_suffix) = split_number(&a.bldg_nb);
        match split_street(idx, &a.strt_nm) {
            Some([pre, name, kind, post]) => {
                (
                    p.pre_direction,
                    p.street_name,
                    p.street_type,
                    p.post_direction,
                ) = (pre, name, kind, post)
            }
            None => p.street_name = some(&a.strt_nm),
        }
        if !a.room.is_empty() {
            p.unit_id = some(&a.room);
            p.unit_designator = crate::extras::unit_of(q).map(|(_, kind)| kind.to_string());
        }
        p.district = district_of(h, &a.twn_nm);
    }
    Reading {
        mode,
        parts: p,
        reasons: o.checks.clone(),
        text: q.to_string(),
    }
}

/// The parts of the line `q` and the sheet's answer `h`.
pub fn of(idx: &Index, q: &str, h: Option<&Hit>) -> Reading {
    from_outcome(idx, q, &iso20022::build(idx, q, h), h)
}

/// The geolocation extension of FHIR (StructureDefinition `geolocation` 5.4.0, read 17.11.2026).
pub const FHIR_GEOLOCATION: &str = "http://hl7.org/fhir/StructureDefinition/geolocation";

/// The first address line as the country writes it: number first or after the street, as the postal layout of the
/// sheet does (`Index::number_first` or the country's layout, the rule of `extras::place`).
pub fn street_line(idx: &Index, p: &Parts) -> Option<String> {
    let street: Vec<&str> = [
        &p.pre_direction,
        &p.street_name,
        &p.street_type,
        &p.post_direction,
    ]
    .into_iter()
    .filter_map(|x| x.as_deref())
    .collect();
    let number = [&p.number, &p.number_suffix]
        .into_iter()
        .filter_map(|x| x.as_deref())
        .collect::<Vec<_>>()
        .join("");
    let street = street.join(" ");
    let line = match (street.is_empty(), number.is_empty()) {
        (true, _) => return p.po_box.as_ref().map(|b| format!("PO Box {b}")),
        (false, true) => street,
        (false, false) if idx.number_first() || crate::extras::number_written_first(idx) => {
            format!("{number} {street}")
        }
        (false, false) => format!("{street} {number}"),
    };
    Some(line)
}

/// HL7 FHIR `Address` ( 5.0.0, unchanged from ): the structure only for a `structured` reading, with the point as
/// the `geolocation` extension; `as_written` and `none` give `text` alone, the line as written.
///
/// The standard: when both `text` and parts are present, no content is included in the text that is not found in a
/// part (datatypes.html#Address, read 17.11.2026). The line as written carries what the parts do not (a country
/// word, a street the sheet corrected, a typo), so a structured address writes `text` from its own parts, and the
/// line as written stays with the caller (the input column of a batch).
pub fn fhir(idx: &Index, r: &Reading, h: Option<&Hit>) -> serde_json::Value {
    use serde_json::{json, Map, Value};
    let mut a = Map::new();
    if r.mode != Mode::Structured {
        a.insert("text".into(), json!(r.text));
        return Value::Object(a);
    }
    let p = &r.parts;
    a.insert(
        "type".into(),
        json!(if p.po_box.is_some() {
            "postal"
        } else {
            "physical"
        }),
    );
    let mut lines: Vec<String> = street_line(idx, p).into_iter().collect();
    let unit: Vec<&str> = [&p.floor, &p.unit_id]
        .into_iter()
        .filter_map(|x| x.as_deref())
        .collect();
    if !unit.is_empty() {
        lines.push(unit.join(" "));
    }
    // the town lines in the order of the sheet's postal layout ("Washington 20500", "75002 Paris")
    let layout = crate::extras::postal_layout_of(crate::extras::SHEET_LAYOUT, idx);
    let field = |k: &str| -> &str {
        match k {
            "postcode" => p.postcode.as_deref(),
            "city" => p.locality.as_deref(),
            "region" => p.region.as_deref(),
            _ => None,
        }
        .unwrap_or("")
    };
    let town = crate::extras::postal_lines(crate::extras::postal_template(Some(layout)), field);
    let text: Vec<&str> = lines.iter().chain(&town).map(String::as_str).collect();
    a.insert("text".into(), json!(text.join(", ")));
    if !lines.is_empty() {
        a.insert("line".into(), json!(lines));
    }
    for (k, v) in [
        ("city", &p.locality),
        ("district", &p.district),
        ("state", &p.region),
        ("postalCode", &p.postcode),
        ("country", &p.country),
    ] {
        if let Some(v) = v {
            a.insert(k.into(), json!(v));
        }
    }
    // the point only for a house the reading confirmed: a post office box structured from the written town has no
    // answer of its own. A weak answer is never a point: `iso20022::build` reads a weak hit as no answer, so a house
    // it structures always comes from a hit that is not weak (PRINCIPLES 2.16 third source)
    let confirmed_house = p.street_name.is_some() && p.number.is_some();
    if let (Some(h), true) = (h, confirmed_house) {
        a.insert(
            "extension".into(),
            json!([{"url": FHIR_GEOLOCATION, "extension": [
                {"url": "latitude", "valueDecimal": h.lat},
                {"url": "longitude", "valueDecimal": h.lon}]}]),
        );
    }
    Value::Object(a)
}

/// `format --out fhir` and `--with fhir`: the FHIR `Address` of `q`, its mode and the reasons.
pub fn fhir_json(idx: &Index, q: &str, h: Option<&Hit>) -> serde_json::Value {
    let r = of(idx, q, h);
    serde_json::json!({"address": fhir(idx, &r, h), "mode": r.mode, "reasons": r.reasons})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(w: &str) -> bool {
        matches!(
            normalize(w).as_str(),
            "street" | "avenue" | "road" | "boulevard"
        )
    }

    fn split(s: &str) -> [Option<String>; 4] {
        split_words(s, types)
    }

    fn o(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn a_street_splits_into_direction_name_type_direction() {
        assert_eq!(
            split("PENNSYLVANIA Avenue Northwest"),
            [None, o("PENNSYLVANIA"), o("Avenue"), o("Northwest")]
        );
        assert_eq!(
            split("North Main Street"),
            [o("North"), o("Main"), o("Street"), None]
        );
        assert_eq!(
            split("North Street"),
            [None, o("North"), o("Street"), None],
            "the name is never empty"
        );
        assert_eq!(
            split("Avenue of the Americas"),
            [None, o("Avenue of the Americas"), None, None]
        );
        assert_eq!(
            split("West Avenue North"),
            [None, o("West"), o("Avenue"), o("North")]
        );
        assert_eq!(split("Broadway"), [None, o("Broadway"), None, None]);
        assert_eq!(
            split("Avenue"),
            [None, o("Avenue"), None, None],
            "a type alone is the name"
        );
        assert_eq!(
            split("North"),
            [None, o("North"), None, None],
            "a direction alone is the name"
        );
        assert_eq!(
            split("Broadway East"),
            [None, o("Broadway"), None, o("East")]
        );
    }

    #[test]
    fn a_number_and_its_suffix() {
        assert_eq!(
            split_number("12bis"),
            (Some("12".into()), Some("bis".into()))
        );
        assert_eq!(split_number("5"), (Some("5".into()), None));
        assert_eq!(split_number("1-9"), (Some("1-9".into()), None));
        assert_eq!(split_number("5 v"), (Some("5".into()), Some("v".into())));
        assert_eq!(split_number("B12"), (Some("B12".into()), None));
        assert_eq!(split_number(""), (None, None));
    }
}
