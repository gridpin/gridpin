//! `parse` and `same_address`: how the engine reads an address line, and whether two lines name one place.
//!
//! No new parsing rules live here. `read` reports two things the engine already does: the first reading
//! of the line (the words, postcode and house number of the first parse hypothesis, the same record
//! `batch --diagnose` shows) and where the geocoder ended up (its top answer). The geocoder tries many
//! street/commune boundaries and keeps the one that answers best, so the answer is the reading it chose.
//!
//! `same_address` compares the two top answers. "house" requires the same street, number and commune
//! AND that neither answer is weak (ANSWERS §195): on the published sets that keeps false merges at
//! or below 0.14 % in every country, where unfiltered answers merged up to 1.6 % of different
//! addresses (France). When either line has no answer the level is "unknown", and `text` says how the
//! two readings compare.

use crate::norm::normalize;
use crate::query::{hit_is_weak, query_cascade, Hit, Index};
use serde::Serialize;

#[derive(Serialize, Clone)]
pub struct Reading {
    pub query: String,
    /// words of the first parse hypothesis, normalized: street and commune words not yet split
    pub words: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postcode: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub house_number: Option<u32>,
    /// the geocoder's top answer; `None` when it finds nothing
    pub answer: Option<Hit>,
}

/// One reading per line: the first hypothesis and the top answer of the address cascade.
pub fn read(addr: &Index, poi: Option<&Index>, query: &str) -> Reading {
    let scope = crate::diagnosis::Scope::new();
    let answer = query_cascade(addr, poi, query, 1).into_iter().next();
    let trace = scope.finish(usize::from(answer.is_some()));
    let first = &trace["parsed"]["examples"][0];
    let words = first["street_or_rest"]
        .as_str()
        .map(normalize)
        .unwrap_or_else(|| normalize(query));
    Reading {
        query: query.to_string(),
        words,
        postcode: first["postcode"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok()),
        house_number: first["house_number"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok()),
        answer,
    }
}

#[derive(Serialize)]
pub struct Same {
    /// house | street | commune | different | unknown
    pub level: &'static str,
    pub reason: String,
    /// only when a line has no answer: same_text | different_number | different_text
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<&'static str>,
    pub a: Reading,
    pub b: Reading,
}

fn same_key(x: &str, y: &str) -> bool {
    let x = normalize(x);
    !x.is_empty() && x == normalize(y)
}

/// The level two answers share, before the weak rule.
pub fn answer_level(a: &Hit, b: &Hit) -> &'static str {
    let commune = same_key(&a.commune, &b.commune);
    let street = commune && same_key(&a.street, &b.street);
    match (&a.housenumber, &b.housenumber) {
        (Some(x), Some(y)) if street && normalize(x) == normalize(y) => "house",
        _ if street => "street",
        _ if commune => "commune",
        _ => "different",
    }
}

fn text_level(a: &Reading, b: &Reading) -> &'static str {
    let postcodes_agree = a.postcode.is_none() || b.postcode.is_none() || a.postcode == b.postcode;
    if a.words.is_empty() || a.words != b.words || !postcodes_agree {
        "different_text"
    } else if a.house_number == b.house_number {
        "same_text"
    } else {
        "different_number"
    }
}

pub fn decide(a: Reading, b: Reading) -> Same {
    let (level, reason, text) = match (&a.answer, &b.answer) {
        (Some(x), Some(y)) => {
            let weak = hit_is_weak(x) || hit_is_weak(y);
            match answer_level(x, y) {
                "house" if weak => (
                    "street",
                    "same street and number, but an answer is weak: not confirmed as one house".to_string(),
                    None,
                ),
                "house" => ("house", "same street, number and commune; neither answer is weak".to_string(), None),
                "different" if weak => (
                    "unknown",
                    "the answers differ and an answer is weak: the difference may be a wrong answer".to_string(),
                    None,
                ),
                "street" => ("street", "same street and commune, different or missing house number".to_string(), None),
                "commune" => ("commune", "same commune, different street".to_string(), None),
                _ => ("different", "the answers are in different communes".to_string(), None),
            }
        }
        _ => (
            "unknown",
            "a line has no answer in this sheet; `text` compares how the two lines read"
                .to_string(),
            Some(text_level(&a, &b)),
        ),
    };
    Same {
        level,
        reason,
        text,
        a,
        b,
    }
}

pub fn same_address(addr: &Index, poi: Option<&Index>, a: &str, b: &str) -> Same {
    decide(read(addr, poi, a), read(addr, poi, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(
        street: &str,
        number: Option<&str>,
        commune: &str,
        precision: &'static str,
        confidence: f32,
    ) -> Hit {
        Hit {
            lat: 0.0,
            lon: 0.0,
            precision,
            score: 0.0,
            confidence,
            street: street.into(),
            housenumber: number.map(Into::into),
            commune: commune.into(),
            postcode: String::new(),
            flags: vec!["street_exact"],
            region: None,
            distance_m: None,
        }
    }

    fn reading(
        answer: Option<Hit>,
        words: &str,
        number: Option<u32>,
        postcode: Option<u32>,
    ) -> Reading {
        Reading {
            query: words.into(),
            words: words.into(),
            postcode,
            house_number: number,
            answer,
        }
    }

    fn level(a: Option<Hit>, b: Option<Hit>) -> &'static str {
        decide(reading(a, "x", None, None), reading(b, "x", None, None)).level
    }

    #[test]
    fn house_needs_the_same_street_number_and_commune_and_no_weak_answer() {
        let strong = |street, number, commune| Some(hit(street, number, commune, "house", 0.9));
        assert_eq!(
            level(
                strong("Main St", Some("12"), "Ames"),
                strong("MAIN ST", Some("12"), "ames")
            ),
            "house"
        );
        // near misses of each field: number, street, commune
        assert_eq!(
            level(
                strong("Main St", Some("12"), "Ames"),
                strong("Main St", Some("14"), "Ames")
            ),
            "street"
        );
        assert_eq!(
            level(
                strong("Main St", Some("12"), "Ames"),
                strong("Main St", None, "Ames")
            ),
            "street"
        );
        assert_eq!(
            level(
                strong("Main St", Some("12"), "Ames"),
                strong("Oak St", Some("12"), "Ames")
            ),
            "commune"
        );
        assert_eq!(
            level(
                strong("Main St", Some("12"), "Ames"),
                strong("Main St", Some("12"), "Boone")
            ),
            "different"
        );
        // the same house through a weak answer on either side is only the same street
        let mut fuzzy = hit("Main St", Some("12"), "Ames", "house", 0.2);
        fuzzy.flags = vec!["street_fuzzy"];
        let weak = Some(fuzzy);
        assert_eq!(
            level(strong("Main St", Some("12"), "Ames"), weak.clone()),
            "street"
        );
        assert_eq!(level(weak, strong("Main St", Some("12"), "Ames")), "street");
        // an exact house on an exact street is never weak, whatever its confidence
        let exact_low = Some(hit("Main St", Some("12"), "Ames", "house", 0.1));
        assert_eq!(
            level(exact_low, strong("Main St", Some("12"), "Ames")),
            "house"
        );
    }

    #[test]
    fn a_difference_through_a_weak_answer_is_unknown_not_different() {
        let city = Some(hit("", None, "Boone", "city", 0.05));
        let strong = Some(hit("Main St", Some("12"), "Ames", "house", 0.9));
        assert_eq!(level(strong.clone(), city), "unknown");
        let other = Some(hit("Oak St", Some("1"), "Boone", "house", 0.9));
        assert_eq!(level(strong, other), "different");
    }

    #[test]
    fn without_an_answer_the_level_is_unknown_and_text_compares_the_readings() {
        let a = reading(None, "main st ames", Some(12), Some(50010));
        let same = decide(a.clone(), reading(None, "main st ames", Some(12), None));
        assert_eq!((same.level, same.text), ("unknown", Some("same_text")));
        let number = decide(
            a.clone(),
            reading(None, "main st ames", Some(14), Some(50010)),
        );
        assert_eq!(number.text, Some("different_number"));
        let postcode = decide(
            a.clone(),
            reading(None, "main st ames", Some(12), Some(50011)),
        );
        assert_eq!(postcode.text, Some("different_text"));
        let words = decide(a, reading(None, "oak st ames", Some(12), Some(50010)));
        assert_eq!(words.text, Some("different_text"));
        let answered = decide(
            reading(
                Some(hit("Main St", Some("12"), "Ames", "house", 0.9)),
                "x",
                None,
                None,
            ),
            reading(
                Some(hit("Main St", Some("12"), "Ames", "house", 0.9)),
                "x",
                None,
                None,
            ),
        );
        assert_eq!(answered.text, None);
    }
}
