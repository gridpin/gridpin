//! What a query came to, for an agent or a person: an answer, a question back, or silence with its reason.
//!
//! A second layer over `query_cascade`, which itself does not change: the answers are byte for byte those of
//! before, and the question and the reason come beside them. A question is asked only where the engine already
//! says "confident but ambiguous" (`ambiguous_far`: the two best candidates score alike and lie more than 50 km
//! apart); the options are the distinct places among the candidates, and what is missing follows from them: the
//! town, or for two towns of one name the region or the postcode. The reason of a silence is read from the trace `--diagnose` keeps (`diagnosis`).

use serde::Serialize;

use crate::query::{query_cascade, Hit, Index};

/// One place to choose from: what to add to the query to get it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Choice {
    /// "Lexington, Kentucky" — the street's place as a person names it
    pub label: String,
    pub city: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Answer {
        results: Vec<Hit>,
    },
    /// `missing` is what the query lacks: "city", or "region" when the sheet names regions, or "postcode" for
    /// towns of one name on a sheet without regions
    Clarify {
        results: Vec<Hit>,
        missing: &'static str,
        options: Vec<Choice>,
    },
    /// no answer; `reason` is one of `REASONS`
    None {
        reason: &'static str,
    },
}

/// Why a query got no answer: the street is not on the sheet (not exactly, not within the fuzzy distance), the
/// street is there but no answer passed, the place named is unknown, the point is off the sheet, or the line could
/// not be read as an address at all.
pub const REASONS: [&str; 5] = [
    "no_street",
    "no_house",
    "city_unknown",
    "outside_sheet",
    "unparsed",
];

/// The answer of `query_cascade(addr, poi, q, k)` with, when there is one, a question or a reason beside it.
pub fn query_outcome(addr: &Index, poi: Option<&Index>, q: &str, k: usize) -> Outcome {
    let results = query_cascade(addr, poi, q, k);
    if results.is_empty() {
        return Outcome::None {
            reason: why_silent(addr, q),
        };
    }
    let top = &results[0];
    if !top.flags.contains(&"ambiguous_far") {
        return Outcome::Answer { results };
    }
    let options = choices(&query_cascade(addr, poi, q, 10));
    if options.len() < 2 {
        return Outcome::Answer { results };
    }
    // two towns of one name ("Lexington, Lexington") are told apart by region or postcode, others by town
    let same_name = options.iter().all(|c| c.city == options[0].city);
    let missing = match (same_name, options.iter().all(|c| c.region.is_some())) {
        (_, true) => "region",
        (true, false) => "postcode",
        (false, false) => "city",
    };
    Outcome::Clarify {
        results,
        missing,
        options,
    }
}

/// The distinct places of the candidates of the answer's tier, best first, at most five; two places closer than
/// 50 km are one place to a person.
fn choices(candidates: &[Hit]) -> Vec<Choice> {
    let Some(top) = candidates.first() else {
        return Vec::new();
    };
    let tier = |h: &Hit| match h.precision {
        "house" | "interp" | "near" => 0,
        "street" | "intersection" => 1,
        _ => 2,
    };
    let mut out: Vec<Choice> = Vec::new();
    for h in candidates.iter().filter(|h| tier(h) == tier(top)) {
        let far_from_all = out
            .iter()
            .all(|c| crate::codes::haversine_m((c.lat, c.lon), (h.lat, h.lon)) > 50_000.0);
        if !far_from_all {
            continue;
        }
        // a sheet without region polygons names the place by its postcode too: "Columbia" alone is no choice
        let label = match (&h.region, h.postcode.is_empty()) {
            (Some(region), _) if !region.is_empty() => format!("{}, {region}", h.commune),
            (_, false) => format!("{} {}", h.commune, h.postcode),
            _ => h.commune.clone(),
        };
        out.push(Choice {
            label,
            city: h.commune.clone(),
            region: h.region.clone().filter(|r| !r.is_empty()),
            lat: h.lat,
            lon: h.lon,
        });
        if out.len() == 5 {
            break;
        }
    }
    out
}

/// The reason of a silence: a coordinate the sheet has nothing near is off the sheet; otherwise the stage the
/// address path stopped at, as the trace of `--diagnose` records it.
fn why_silent(addr: &Index, q: &str) -> &'static str {
    if crate::forms::point(q).is_some() {
        return "outside_sheet";
    }
    if q.trim().is_empty() || !q.chars().any(char::is_alphabetic) {
        return "unparsed";
    }
    let trace = crate::diagnosis::Scope::new();
    let _ = addr.query(q, 1);
    let observed = trace.finish(0);
    match reason_of_stage(observed["stop_stage"].as_str().unwrap_or("")) {
        // a place is unknown only when the query names one apart from the street
        "city_unknown" if !q.contains(',') => "no_street",
        reason => reason,
    }
}

fn reason_of_stage(stage: &str) -> &'static str {
    match stage {
        "empty_parse" | "unobserved_shortcut" => "unparsed",
        "no_area_or_global_street" => "city_unknown",
        "no_street_candidate" => "no_street",
        // a street matched, and no answer on it passed
        "before_hit_construction"
        | "after_ranking_unobserved_guard"
        | "fallback_guard_rejected" => "no_house",
        _ => "no_street",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(commune: &str, region: Option<&str>, lat: f64, precision: &'static str) -> Hit {
        Hit {
            lat,
            lon: -84.5,
            precision,
            score: 1.0,
            confidence: 0.2,
            street: "Mill Street".into(),
            housenumber: None,
            commune: commune.into(),
            postcode: String::new(),
            flags: vec!["street_exact"],
            region: region.map(String::from),
            distance_m: None,
        }
    }

    #[test]
    fn choices_are_the_distinct_far_places_of_the_answer_tier() {
        let c = choices(&[
            hit("Lexington", Some("Kentucky"), 38.04, "street"),
            hit("Lexington", Some("Kentucky"), 38.05, "street"), // the same place, 1 km away
            hit("Lexington", Some("North Carolina"), 35.82, "street"),
            hit("Lexington", Some("Virginia"), 37.0, "city"), // another tier, 115 km away: still not an option
        ]);
        let labels: Vec<&str> = c.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Lexington, Kentucky", "Lexington, North Carolina"]);
    }

    #[test]
    fn every_stage_of_the_trace_has_one_of_the_five_reasons() {
        for stage in [
            "empty_parse",
            "unobserved_shortcut",
            "no_area_or_global_street",
            "no_street_candidate",
            "before_hit_construction",
            "after_ranking_unobserved_guard",
            "fallback_guard_rejected",
            "returned",
        ] {
            assert!(REASONS.contains(&reason_of_stage(stage)), "{stage}");
        }
        assert_eq!(reason_of_stage("no_area_or_global_street"), "city_unknown");
        assert_eq!(reason_of_stage("before_hit_construction"), "no_house");
    }
}
