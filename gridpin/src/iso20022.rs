//! A structured ISO 20022 postal address (`PstlAdr`) from a free address line, confirmed against the sheet.
//!
//! Banks moving to structured addresses (Swift CBPR+, the EPC SEPA schemes) need `PstlAdr` either `structured` (no
//! `AdrLine`, town and country set) or `hybrid` (town and country set, at most two `AdrLine` of 70 characters). From a
//! free line and the sheet's answer this builds one of the two, or says why neither is possible (`none`), and keeps
//! the existence judgement (`exists`) apart: "passes the form check" and "the address exists" are two answers.
//!
//! Field lengths: `PostalAddress24` of `pacs.008.001.08` (Max16Text / Max35Text / Max70Text), read from the schema
//! model of moov-io/rtp20022 and the CBPR+ address guidance of 09.2025, 10.11.2026; not yet checked against the CBPR+
//! usage guideline itself. Lengths are counted in characters, not bytes. A field over its length is never cut: the
//! address falls back to `hybrid` (the text goes to `AdrLine`) or to `none`.

use serde_json::{json, Map, Value};

use crate::exists::{judge_answer, Existence};
use crate::norm::normalize;
use crate::query::{query_cascade, Hit, Index};
use crate::verdict::{asked, field_verdicts_in};

const PO_BOX_WORDS: &str = include_str!("po_box_words.tsv");

/// (element, maximum length in characters), in the order ISO 20022 writes them.
pub const FIELDS: [(&str, usize); 11] = [
    ("StrtNm", 70),
    ("BldgNb", 16),
    ("Flr", 70),
    ("PstBx", 16),
    ("Room", 70),
    ("PstCd", 16),
    ("TwnNm", 35),
    ("CtrySubDvsn", 35),
    ("Ctry", 2),
    ("AdrLine", 70),
    ("AdrLine", 70),
];
/// At most two `AdrLine` in a hybrid address.
pub const ADR_LINES: usize = 2;
pub const ADR_LINE_LEN: usize = 70;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PstlAdr {
    pub strt_nm: String,
    pub bldg_nb: String,
    pub flr: String,
    pub pst_bx: String,
    pub room: String,
    pub pst_cd: String,
    pub twn_nm: String,
    pub ctry_sub_dvsn: String,
    pub ctry: String,
    pub adr_line: Vec<String>,
}

impl PstlAdr {
    /// The elements in ISO order, empty ones left out.
    pub fn elements(&self) -> Vec<(&'static str, &str)> {
        let mut out: Vec<(&'static str, &str)> = [
            ("StrtNm", self.strt_nm.as_str()),
            ("BldgNb", self.bldg_nb.as_str()),
            ("Flr", self.flr.as_str()),
            ("PstBx", self.pst_bx.as_str()),
            ("Room", self.room.as_str()),
            ("PstCd", self.pst_cd.as_str()),
            ("TwnNm", self.twn_nm.as_str()),
            ("CtrySubDvsn", self.ctry_sub_dvsn.as_str()),
            ("Ctry", self.ctry.as_str()),
        ]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect();
        out.extend(self.adr_line.iter().map(|l| ("AdrLine", l.as_str())));
        out
    }

    /// The first element longer than its limit.
    fn too_long(&self) -> Option<&'static str> {
        self.elements().into_iter().find_map(|(name, value)| {
            let limit = FIELDS.iter().find(|(n, _)| *n == name).map_or(70, |f| f.1);
            (value.chars().count() > limit).then_some(name)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Structured,
    Hybrid,
    None,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Structured => "structured",
            Mode::Hybrid => "hybrid",
            Mode::None => "none",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub mode: Mode,
    pub adr: PstlAdr,
    /// the reasons of `none` and the warnings of the other modes
    pub checks: Vec<String>,
    pub exists: Existence,
}

/// The country a segment of the line names, looking from the end ("…, Paris, France" and "…, France, Paris").
fn named_country(q: &str) -> Option<String> {
    let segments: Vec<&str> = q
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }
    segments
        .iter()
        .rev()
        .take(2)
        .find_map(|s| crate::country_names::code_of(s))
}

/// The sheet's country through the one read the owner allowed (ANSWERS §278): the postal layout of the sheet.
pub fn sheet_country(idx: &Index) -> Option<String> {
    let layout = crate::extras::postal_layout_of(crate::extras::SHEET_LAYOUT, idx);
    (layout != "default").then(|| layout.to_ascii_uppercase())
}

fn po_box_words() -> impl Iterator<Item = &'static str> {
    PO_BOX_WORDS
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(str::trim)
}

/// `TwnNm` of an answer: the sheet's commune, or the town as the line writes it when the commune is a district of it
/// ("…, Paris" answered in "Paris 2e Arrondissement", a house of a city the sheet records by districts): a payment
/// names the town, not its district.
fn town_of(idx: &Index, q: &str, h: &Hit) -> String {
    let c = normalize(&h.commune);
    // the town is a segment after the first one (the first carries the street): a line of one segment is read below
    let segments: Vec<&str> = q
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let written = segments
        .iter()
        .skip(1)
        .rev()
        .filter(|s| crate::country_names::code_of(s).is_none())
        .map(|s| {
            s.split_whitespace()
                .filter(|w| !w.chars().any(|c| c.is_ascii_digit()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        // a region code ("NY 10118", "São Paulo - SP" has a name too) is not the town
        .find(|s| !s.is_empty() && s.chars().filter(|c| c.is_alphabetic()).count() > 2);
    if let Some(written) = written {
        let w = normalize(&written);
        let district = c != w
            && (c.starts_with(&format!("{w} "))
                || idx.hit_in_named_city_of_districts(q, h) == Some(true));
        if district {
            return written;
        }
    }
    // a line without commas ("3 rue de la paix 75002 paris"): the town is the district's name before its number,
    // when the line ends with it
    let base: Vec<&str> = c
        .split(' ')
        .take_while(|t| !t.chars().any(|ch| ch.is_ascii_digit()))
        .collect();
    let words: Vec<&str> = q
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .filter(|t| !t.is_empty() && !t.chars().any(|ch| ch.is_ascii_digit()))
        .collect();
    if !base.is_empty() && base.len() < c.split(' ').count() && words.len() > base.len() {
        let tail = &words[words.len() - base.len()..];
        if normalize(&tail.join(" ")) == base.join(" ") {
            return tail.join(" ");
        }
    }
    h.commune.clone()
}

/// A town the sheet keeps by its districts ("Lyon" over "Lyon 1er Arrondissement" …): the name alone answers as the
/// town itself or as the group of districts that start with it.
fn town_by_districts(idx: &Index, name: &str) -> bool {
    query_cascade(idx, None, name, 1).first().is_some_and(|h| {
        h.precision == "city"
            && (h.flags.contains(&"place_exact") || h.flags.contains(&"place_prefix_group"))
    })
}

/// The town a line names as written ("BP 12, 75002 Paris" -> "Paris", postcode "75002"): the last segment, other than
/// a country, whose words without digits are a commune of the sheet. For a post office box, which has no street for
/// the sheet to answer.
fn written_town(idx: &Index, q: &str) -> Option<(String, Option<String>)> {
    q.split(',')
        .map(str::trim)
        .rev()
        .filter(|s| !s.is_empty() && crate::country_names::code_of(s).is_none())
        .find_map(|s| {
            let (code, name): (Vec<&str>, Vec<&str>) = s
                .split_whitespace()
                .partition(|w| w.chars().any(|c| c.is_ascii_digit()));
            let name = name.join(" ");
            // "MN 55082": a state code is not a town, whatever commune it spells the start of (ANSWERS §308)
            if name.chars().filter(|c| c.is_alphabetic()).count() <= 2 {
                return None;
            }
            // "Campo Grande - MS": the town before its region code
            let short = name
                .split(" - ")
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            let known = |n: &str| {
                !n.is_empty() && (idx.has_commune(&normalize(n)) || town_by_districts(idx, n))
            };
            let code = code.first().map(|c| c.to_string());
            if known(&name) {
                Some((name, code))
            } else if short != name && known(&short) {
                Some((short, code))
            } else {
                None
            }
        })
}

/// The post office box a line names: its number after a box word ("P.O. Box 123" -> "123").
pub fn po_box_of(q: &str) -> Option<String> {
    let words: Vec<String> = q
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect();
    let norm: Vec<String> = words
        .iter()
        .map(|w| normalize(&w.replace('.', " ")))
        .collect();
    for i in 0..words.len() {
        for take in [3usize, 2, 1] {
            if i + take >= words.len() {
                continue;
            }
            let phrase = norm[i..i + take].join(" ");
            if po_box_words().any(|w| w == phrase) {
                let number = words[i + take].trim_matches(|c: char| !c.is_alphanumeric());
                if !number.is_empty() && number.chars().any(|c| c.is_ascii_digit()) {
                    return Some(number.to_string());
                }
            }
        }
    }
    None
}

/// The free line without the segments the structured elements already carry: the town (with or without its
/// postcode), the postcode written in `PstCd` and the country; packed into at most two lines of 70 characters, cut
/// only between segments. `None` when it does not fit. A segment of digits that is not that postcode stays: it is
/// the house number of "Rua X, 302, Bairro" or a code the answer did not confirm.
fn adr_lines(q: &str, town: &str, pst_cd: &str) -> Option<Vec<String>> {
    let town = normalize(town);
    let digits = |t: &str| t.chars().filter(char::is_ascii_digit).collect::<String>();
    let code = digits(pst_cd);
    let kept: Vec<String> = q
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && crate::country_names::code_of(s).is_none())
        .filter_map(|s| {
            let (code_words, name_words): (Vec<&str>, Vec<&str>) = s
                .split_whitespace()
                .partition(|w| w.chars().any(|c| c.is_ascii_digit()));
            // the segment's words without the postcode ("28013 Madrid" -> "madrid")
            let name = normalize(&name_words.join(" "));
            let codes = code_words.join(" ");
            let written_in_pstcd = !code.is_empty() && digits(&codes) == code;
            if name.is_empty() {
                // a segment of digits: dropped only when it is the code written in PstCd
                return (!written_in_pstcd).then(|| s.to_string());
            }
            if name == town {
                // the town segment: TwnNm carries the name; a code the sheet did not confirm stays in the line
                return (!codes.is_empty() && !written_in_pstcd).then_some(codes);
            }
            if written_in_pstcd {
                // "FL 33040" with PstCd 33040: the code is written once, the state stays in the line
                return Some(name_words.join(" "));
            }
            Some(s.to_string())
        })
        .collect();
    let mut lines: Vec<String> = Vec::new();
    for seg in &kept {
        if seg.chars().count() > ADR_LINE_LEN {
            return None;
        }
        match lines.last_mut() {
            Some(last) if last.chars().count() + 2 + seg.chars().count() <= ADR_LINE_LEN => {
                last.push_str(", ");
                last.push_str(seg);
            }
            _ => lines.push(seg.clone()),
        }
    }
    (lines.len() <= ADR_LINES).then_some(lines)
}

/// The postcode for `PstCd`: the sheet's code, or the query's own code when the sheet keeps only its start (five
/// digits of a Brazilian CEP for "04075-000"): a payment carries the code as the payer wrote it, not a shortened one.
fn postcode_of(q: &str, sheet_code: &str) -> String {
    let short: String = sheet_code.chars().filter(char::is_ascii_digit).collect();
    if short.is_empty() || short.len() != sheet_code.len() {
        return sheet_code.to_string();
    }
    q.split(|c: char| c == ',' || c.is_whitespace())
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .find(|t| {
            let d: String = t.chars().filter(char::is_ascii_digit).collect();
            !t.is_empty()
                && t.chars().all(|c| c.is_ascii_digit() || c == '-')
                && d.len() > short.len()
                && d.starts_with(&short)
        })
        .map_or_else(|| sheet_code.to_string(), str::to_string)
}

fn non_latin(adr: &PstlAdr) -> bool {
    adr.elements()
        .iter()
        .any(|(_, v)| v.chars().any(|c| c as u32 > 0xFF))
}

/// `PstlAdr` for the line `q` and the sheet's answer `h`.
pub fn build(idx: &Index, q: &str, h: Option<&Hit>) -> Outcome {
    let a = asked(idx, q);
    let exists = judge_answer(idx, q, &a, h);
    let mut checks: Vec<String> = Vec::new();
    let none = |checks: Vec<String>, exists: Existence| Outcome {
        mode: Mode::None,
        adr: PstlAdr::default(),
        checks,
        exists,
    };
    let named = named_country(q);
    let sheet = sheet_country(idx);
    if let (Some(n), Some(s)) = (&named, &sheet) {
        if !n.eq_ignore_ascii_case(s) {
            return none(vec!["country_mismatch".into()], exists);
        }
    }
    let ctry = named.or(sheet).map(|c| c.to_ascii_uppercase());
    // a weak answer does not confirm a payment address ("350 Fifth Avenue, New York" answered in York, Alabama): the
    // line is treated as one the sheet did not answer
    let weak = h.is_some_and(crate::query::hit_is_weak);
    let h = h.filter(|_| !weak);
    // a house in another town than the one the line names ("Sparkassenstraße 5, 80331 München" answered in
    // Petershagen): the answer is not the payer's address, the line is treated as one the sheet did not answer
    let off_town = h.is_some_and(|h| {
        matches!(
            field_verdicts_in(idx, q, &a, h).city,
            "replaced" | "unresolved"
        ) && exists.exists != "yes"
    });
    let h = h.filter(|_| !off_town);
    let Some(h) = h else {
        // no answer of the sheet, but the town is a commune of the sheet: a post office box is structured (it has no
        // street to answer), anything else is hybrid, the town and the country with the rest in the address lines
        let (Some(ctry), Some((twn_nm, code))) = (&ctry, written_town(idx, q)) else {
            let why = if weak {
                vec!["weak_answer".into(), "town_unconfirmed".into()]
            } else if off_town {
                vec![exists.reason.into(), "town_unconfirmed".into()]
            } else {
                vec!["empty".into()]
            };
            return none(why, exists);
        };
        let written_code = code.unwrap_or_default();
        let (mode, adr, mut checks) = if let Some(pst_bx) = po_box_of(q) {
            // a post box has no address line: the code goes as written, unconfirmed
            let pst_cd = written_code;
            let adr = PstlAdr {
                pst_bx,
                pst_cd,
                twn_nm,
                ctry: ctry.clone(),
                ..PstlAdr::default()
            };
            let mut checks = vec!["po_box".to_string()];
            if !adr.pst_cd.is_empty() {
                checks.push("postcode_unconfirmed".into());
            }
            (Mode::Structured, adr, checks)
        } else {
            // the sheet did not answer: the code stays in the address line, not confirmed as PstCd
            let pst_cd = String::new();
            let Some(adr_line) = adr_lines(q, &twn_nm, &pst_cd) else {
                return none(
                    vec!["street_unknown".into(), "adr_line_overflow".into()],
                    exists,
                );
            };
            let adr = PstlAdr {
                pst_cd,
                twn_nm,
                ctry: ctry.clone(),
                adr_line,
                ..PstlAdr::default()
            };
            (Mode::Hybrid, adr, vec!["street_unknown".to_string()])
        };
        if weak {
            checks.insert(0, "weak_answer".into());
        } else if off_town {
            checks.insert(0, exists.reason.into());
        }
        if let Some(field) = adr.too_long() {
            checks.push(format!("too_long:{field}"));
            return none(checks, exists);
        }
        if mode == Mode::Structured && adr.pst_cd.is_empty() {
            checks.push("postcode_missing".into());
        }
        return Outcome {
            mode,
            adr,
            checks,
            exists,
        };
    };
    let Some(ctry) = ctry else {
        return none(vec!["country_unknown".into()], exists);
    };
    let v = field_verdicts_in(idx, q, &a, h);
    let unit = crate::extras::unit_of(q);
    let po_box = po_box_of(q);
    if po_box.is_some() {
        checks.push("po_box".into());
    }
    if exists.exists != "yes" && !checks.contains(&exists.reason.to_string()) {
        checks.push(exists.reason.into());
    }
    let city_known =
        matches!(v.city, "confirmed" | "corrected" | "inferred") && !h.commune.is_empty();
    // structured only for a house the sheet confirms as written (`exists` yes): a house between known ones, another
    // postcode, a town the sheet does not know or a doubtful answer stay hybrid (ANSWERS §308)
    let house = exists.exists == "yes"
        && h.precision == "house"
        && matches!(v.street, "confirmed" | "corrected")
        && v.number == "confirmed";
    let (flr, room) = match &unit {
        Some((n, "floor")) => (n.clone(), String::new()),
        Some((n, _)) => (String::new(), n.clone()),
        None => (String::new(), String::new()),
    };
    let po_box_in_town = po_box.is_some() && matches!(v.city, "confirmed" | "corrected");
    if (house && city_known) || po_box_in_town {
        let adr = PstlAdr {
            strt_nm: if house {
                h.street.clone()
            } else {
                String::new()
            },
            bldg_nb: if house {
                h.housenumber.clone().unwrap_or_default()
            } else {
                String::new()
            },
            flr: flr.clone(),
            pst_bx: po_box.clone().unwrap_or_default(),
            room: room.clone(),
            pst_cd: postcode_of(q, &h.postcode),
            twn_nm: town_of(idx, q, h),
            ctry_sub_dvsn: h.region.clone().unwrap_or_default(),
            ctry: ctry.clone(),
            adr_line: Vec::new(),
        };
        match adr.too_long() {
            None => {
                if v.street == "corrected" && !adr.strt_nm.is_empty() {
                    // the sheet's street differs from the line's ("Rue A" -> "Rue Antoine Bourdelle"): a person confirms
                    checks.push("street_corrected".into());
                }
                if adr.pst_cd.is_empty() {
                    checks.push("postcode_missing".into());
                }
                if non_latin(&adr) {
                    checks.push("non_latin".into());
                }
                return Outcome {
                    mode: Mode::Structured,
                    adr,
                    checks,
                    exists,
                };
            }
            Some(field) => checks.push(format!("too_long:{field}")),
        }
    }
    if !matches!(v.city, "confirmed" | "corrected") || h.commune.is_empty() {
        checks.push("town_unconfirmed".into());
        return none(checks, exists);
    }
    let pst_cd = if h.flags.contains(&"pc_exact") {
        postcode_of(q, &h.postcode)
    } else {
        String::new()
    };
    let twn_nm = town_of(idx, q, h);
    let Some(lines) = adr_lines(q, &twn_nm, &pst_cd) else {
        checks.push("adr_line_overflow".into());
        return none(checks, exists);
    };
    let adr = PstlAdr {
        pst_cd,
        twn_nm,
        ctry,
        adr_line: lines,
        ..PstlAdr::default()
    };
    if let Some(field) = adr.too_long() {
        checks.push(format!("too_long:{field}"));
        return none(checks, exists);
    }
    if non_latin(&adr) {
        checks.push("non_latin".into());
    }
    Outcome {
        mode: Mode::Hybrid,
        adr,
        checks,
        exists,
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `<PstlAdr>…</PstlAdr>` in ISO order, without a namespace (the message it goes into sets it).
pub fn xml(adr: &PstlAdr) -> String {
    let mut out = String::from("<PstlAdr>");
    for (name, value) in adr.elements() {
        out.push_str(&format!("<{name}>{}</{name}>", escape(value)));
    }
    out.push_str("</PstlAdr>");
    out
}

/// The outcome as JSON: mode, the elements in ISO order, checks and, with `with_exists`, the existence judgement.
pub fn json(o: &Outcome, with_exists: bool) -> Value {
    let mut adr = Map::new();
    for (name, value) in o.adr.elements() {
        if name == "AdrLine" {
            adr.insert("AdrLine".into(), json!(o.adr.adr_line));
        } else {
            adr.insert(name.into(), json!(value));
        }
    }
    let mut v = json!({"mode": o.mode.as_str(), "PstlAdr": Value::Object(adr), "checks": o.checks});
    if with_exists {
        v["exists"] = json!({"exists": o.exists.exists, "reason": o.exists.reason});
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_house_number_segment_stays_in_the_address_line_and_only_the_written_postcode_goes() {
        let q = "Rua X, 302, Bairro, Campinas, 13000-100, Brasil";
        assert_eq!(
            adr_lines(q, "Campinas", "").unwrap(),
            ["Rua X, 302, Bairro, 13000-100"]
        );
        assert_eq!(
            adr_lines(q, "Campinas", "13000-100").unwrap(),
            ["Rua X, 302, Bairro"]
        );
        assert_eq!(
            adr_lines("Rua X, 302, Campinas", "Campinas", "302").unwrap(),
            ["Rua X"]
        );
        assert_eq!(
            adr_lines(
                "1110 Georgia St, Key West, FL 33040, USA",
                "Key West",
                "33040"
            )
            .unwrap(),
            ["1110 Georgia St, FL"],
            "the postcode of PstCd is not written again beside the state"
        );
    }

    #[test]
    fn a_shortened_sheet_code_gives_way_to_the_code_of_the_query() {
        assert_eq!(
            postcode_of("Alameda Irae, 302, São Paulo - SP, 04075000", "04075"),
            "04075000"
        );
        assert_eq!(
            postcode_of("R Domingos, 5, Campo Grande - MS, 79075-224", "79075"),
            "79075-224"
        );
        // same length or other code, no code at all, a code with letters: the sheet's code stays
        assert_eq!(postcode_of("Main St 5, 04076", "04075"), "04075");
        assert_eq!(postcode_of("Rua A, 302, São Paulo", "04075"), "04075");
        assert_eq!(postcode_of("1012 AB Amsterdam", "1012 AB"), "1012 AB");
    }

    #[test]
    fn a_po_box_is_read_with_its_number() {
        assert_eq!(
            po_box_of("P.O. Box 123, Springfield").as_deref(),
            Some("123")
        );
        assert_eq!(
            po_box_of("Postfach 1234, 10117 Berlin").as_deref(),
            Some("1234")
        );
        assert_eq!(
            po_box_of("Caixa Postal 55, São Paulo").as_deref(),
            Some("55")
        );
        assert_eq!(po_box_of("Boxhagener Straße 12, Berlin"), None);
        assert_eq!(po_box_of("Rue de la Paix 10, Paris"), None);
    }

    #[test]
    fn xml_keeps_iso_order_escapes_and_leaves_empty_elements_out() {
        let adr = PstlAdr {
            twn_nm: "Paris".into(),
            strt_nm: "Rue A & B".into(),
            bldg_nb: "10".into(),
            ctry: "FR".into(),
            ..PstlAdr::default()
        };
        assert_eq!(
            xml(&adr),
            "<PstlAdr><StrtNm>Rue A &amp; B</StrtNm><BldgNb>10</BldgNb><TwnNm>Paris</TwnNm><Ctry>FR</Ctry></PstlAdr>"
        );
    }

    #[test]
    fn a_length_is_counted_in_characters() {
        let adr = PstlAdr {
            strt_nm: "é".repeat(70),
            ..PstlAdr::default()
        };
        assert_eq!(
            adr.too_long(),
            None,
            "70 accented letters are 140 bytes and fit"
        );
        let adr = PstlAdr {
            strt_nm: "é".repeat(71),
            ..PstlAdr::default()
        };
        assert_eq!(adr.too_long(), Some("StrtNm"));
    }
}
