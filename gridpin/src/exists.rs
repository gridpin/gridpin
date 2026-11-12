//! Does an address exist in the sheet? `yes`, `maybe` or `no`, with the reason, read from the answer and the
//! verdicts of its fields (`verdict.rs`); nothing is looked up beyond the address cascade itself.
//!
//! `yes`: the house, its street found as written or with a fix, the asked number, the town the line names (when it
//! names one) and the postcode not replaced, and an answer the engine itself does not doubt. `maybe`: the number
//! falls between known houses (`number_interpolated`), the query has no number (`no_number`), the postcode differs
//! (`postcode_differs`), the line names a town the sheet does not know and the postcode does not place the house in
//! it (`town_not_on_sheet`: "80331 München" on a sheet without Bavaria), or the answer is weak or of a confidence
//! below `MIN_CONFIDENCE` (`weak_answer`). `no`: `street_unknown`, `number_not_on_street` (the street is there, the
//! number is not), `wrong_city` (the house is in another town than the one named, and not within the named town's
//! districts), `empty` (no answer at all).

use serde::Serialize;

use crate::query::{hit_is_weak, query_cascade, Hit, Index};
use crate::verdict::{asked, field_verdicts_in, Asked, FieldVerdicts};

/// An answer below this confidence does not confirm an address (revision of 0.9.0: on the owner's points of DE
/// it takes ten far answers out of `yes` for one right one).
pub const MIN_CONFIDENCE: f32 = 0.3;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Existence {
    /// yes, maybe, no
    pub exists: &'static str,
    pub reason: &'static str,
}

/// The judgement of one answer to `q` (`None`: no answer).
pub fn judge_hit(idx: &Index, q: &str, a: Asked, h: Option<&Hit>) -> Existence {
    judge_answer(idx, q, &a, h)
}

/// `judge_hit` without taking the reading.
pub fn judge_answer(idx: &Index, q: &str, a: &Asked, h: Option<&Hit>) -> Existence {
    judge_verdicts(h, |h| field_verdicts_in(idx, q, a, h))
}

fn judge_verdicts(h: Option<&Hit>, verdicts: impl Fn(&Hit) -> FieldVerdicts) -> Existence {
    let e = |exists, reason| Existence { exists, reason };
    let Some(h) = h else { return e("no", "empty") };
    let v = verdicts(h);
    if h.precision == "city" || h.street.is_empty() || v.street == "unresolved" {
        return e("no", "street_unknown");
    }
    if v.city == "replaced" {
        return e("no", "wrong_city");
    }
    let found = match (h.precision, v.number) {
        ("house", "confirmed") if v.postcode == "replaced" => e("maybe", "postcode_differs"),
        ("house", "confirmed") => e("yes", "house_found"),
        ("interp", _) => e("maybe", "number_interpolated"),
        (_, "absent") => e("maybe", "no_number"),
        _ => e("no", "number_not_on_street"),
    };
    if found.exists != "yes" {
        return found;
    }
    // the line's postcode at the house already confirms a town the sheet does not have (`verdict::in_named_town`)
    if v.city == "unresolved" {
        return e("maybe", "town_not_on_sheet");
    }
    // a low confidence of a house in the town the line names is the engine's doubt between namesakes far away
    // ("Friedrichstr. 100, Berlin": 0.2 for the other Friedrichstraßen): the named town has settled it
    let town_settles = a_town_confirms(&v);
    if hit_is_weak(h) || (h.confidence < MIN_CONFIDENCE && !town_settles) {
        return e("maybe", "weak_answer");
    }
    found
}

/// The house is in the town the line names, as written or with a fix.
fn a_town_confirms(v: &FieldVerdicts) -> bool {
    matches!(v.city, "confirmed" | "corrected")
}

/// `judge_answer` from verdicts already read (the city verdict without the districts the sheet would add).
#[cfg(test)]
fn judge_reading(a: &Asked, h: Option<&Hit>) -> Existence {
    judge_verdicts(h, |h| crate::verdict::field_verdicts_of(a, h))
}

/// Does `q` exist in the sheet?
pub fn judge(idx: &Index, q: &str) -> Existence {
    let hits = query_cascade(idx, None, q, 1);
    judge_answer(idx, q, &asked(idx, q), hits.first())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::NamedTown;

    fn hit(confidence: f32, flags: Vec<&'static str>, commune: &str) -> Hit {
        Hit {
            lat: 0.0,
            lon: 0.0,
            precision: "house",
            score: 1.0,
            confidence,
            street: "Main Street".into(),
            housenumber: Some("12".into()),
            commune: commune.into(),
            postcode: "62701".into(),
            flags,
            region: None,
            distance_m: None,
        }
    }

    fn asked(town: Option<(&str, bool)>) -> Asked {
        Asked {
            postcode: None,
            number: Some(12),
            town: town.map(|(name, on_sheet)| NamedTown {
                name: name.into(),
                on_sheet,
                at: None,
                elsewhere: false,
            }),
        }
    }

    fn judge(a: &Asked, h: &Hit) -> (&'static str, &'static str) {
        // the sheet is read only for districts of a replaced town; these answers are never replaced
        let e = judge_reading(a, Some(h));
        (e.exists, e.reason)
    }

    #[test]
    fn yes_needs_the_named_town_and_an_answer_the_engine_does_not_doubt() {
        let exact = vec!["street_exact"];
        assert_eq!(
            judge(
                &asked(Some(("springfield", true))),
                &hit(0.9, exact.clone(), "Springfield")
            ),
            ("yes", "house_found")
        );
        assert_eq!(
            judge(&asked(None), &hit(0.9, exact.clone(), "Springfield")),
            ("yes", "house_found"),
            "no town named: the house is enough"
        );
        // a town the sheet does not know, the postcode does not place the house in it
        assert_eq!(
            judge(
                &asked(Some(("munchen", false))),
                &hit(0.6, exact.clone(), "Petershagen")
            ),
            ("maybe", "town_not_on_sheet")
        );
        // ... and the postcode does: the house is where the line's code says
        assert_eq!(
            judge(
                &asked(Some(("ortsteil", false))),
                &hit(0.9, vec!["street_exact", "pc_exact"], "Stadt")
            ),
            ("yes", "house_found")
        );
        // the boundary of the confidence: 0.30 confirms, 0.29 does not
        assert_eq!(
            judge(&asked(None), &hit(0.30, exact.clone(), "Springfield")),
            ("yes", "house_found")
        );
        assert_eq!(
            judge(&asked(None), &hit(0.29, exact.clone(), "Springfield")),
            ("maybe", "weak_answer")
        );
        // the town the line names settles a low confidence ("Friedrichstr. 100, Berlin": 0.2 for far namesakes)
        assert_eq!(
            judge(
                &asked(Some(("springfield", true))),
                &hit(0.2, exact.clone(), "Springfield")
            ),
            ("yes", "house_found")
        );
        // a weak answer (a street found with a fix, confidence under 0.60) is not yes either
        assert_eq!(
            judge(&asked(None), &hit(0.4, vec!["street_fuzzy"], "Springfield")),
            ("maybe", "weak_answer")
        );
    }

    #[test]
    fn a_house_in_another_town_than_the_named_one_is_no() {
        assert_eq!(
            judge(
                &asked(Some(("springfield", true))),
                &hit(0.9, vec!["street_exact", "commune_exact"], "Shelbyville")
            ),
            ("no", "wrong_city")
        );
    }
}
