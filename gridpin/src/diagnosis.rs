//! Optional request-local observations. No lookup, scoring or decision is performed here.
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeSet;

#[derive(Default)]
struct Trace {
    parsed: Vec<Value>,
    boundaries: Vec<Value>,
    boundary_count: usize,
    guards: Vec<Value>,
    rejected_guards: usize,
    parse_attempts: usize,
    areas: BTreeSet<u32>,
    area_attempts: usize,
    area_examples: Vec<Value>,
    streets: BTreeSet<u32>,
    street_examples: Vec<Value>,
    houses: Vec<Value>,
    ranked_count: usize,
    house_attempts: usize,
    house_matches: usize,
    best_score: Option<f32>,
    parsed_nonempty: bool,
}
thread_local! { static TRACE: RefCell<Option<Trace>> = const { RefCell::new(None) }; }
pub(crate) fn enabled() -> bool {
    TRACE.with(|t| t.borrow().is_some())
}
fn observe(f: impl FnOnce(&mut Trace)) {
    TRACE.with(|cell| {
        if let Some(t) = cell.borrow_mut().as_mut() {
            f(t);
        }
    });
}
/// Restores the preceding scope even when the guarded query unwinds.
pub struct Scope {
    previous: Option<Trace>,
}
impl Scope {
    pub fn new() -> Self {
        Self {
            previous: TRACE.with(|t| t.replace(Some(Trace::default()))),
        }
    }
    pub fn finish(self, returned: usize) -> Value {
        let t = TRACE.with(|t| t.take()).unwrap_or_default();
        let stage = if returned > 0 {
            "returned"
        } else if t.ranked_count > 0 && t.rejected_guards > 0 {
            "fallback_guard_rejected"
        } else if t.ranked_count > 0 {
            "after_ranking_unobserved_guard"
        } else if !t.streets.is_empty() {
            "before_hit_construction"
        } else if t.parse_attempts > 0 && !t.parsed_nonempty {
            "empty_parse"
        } else if t.area_attempts > 0 && t.areas.is_empty() {
            "no_area_or_global_street"
        } else if t.parse_attempts > 0 {
            "no_street_candidate"
        } else {
            "unobserved_shortcut"
        };
        json!({
            "version": 1,
            "parsed": {"attempts": t.parse_attempts, "examples": t.parsed, "boundary_attempts": t.boundary_count, "boundary_examples": t.boundaries,
                "semantics": "Alternative parses, not a single accepted address; rest still includes the street/commune boundary. Examples capped at 8."},
            "area_candidates": {"count": t.areas.len(), "first_ids": t.areas.iter().take(8).collect::<Vec<_>>(),
                "lookup_attempts": t.area_attempts, "examples": t.area_examples},
            "street_candidates": {"count": t.streets.len(), "first_ids": t.streets.iter().take(8).collect::<Vec<_>>(),
                "examples": t.street_examples, "semantics": "Unique admitted street IDs across all hypotheses and retries, including cityless search; not restricted to one selected area."},
            "house_candidates": {"lookup_attempts": t.house_attempts, "resolved_count": t.house_matches, "examples": t.houses,
                "semantics": "House block size and actual make_hit lookup outcome per ranked street, first 8 calls. No separate house-candidate list exists; missing houses fall back to street centroids."},
            "before_threshold": {"count": t.ranked_count, "best_score": t.best_score,
                "semantics": "Count of make_hit calls across retries, not unique final hits. This pipeline has no common score rejection threshold; confidence is output metadata. Country-specific guards are not individually traced."},
            "fallback_guards": {"rejected_attempts": t.rejected_guards, "examples": t.guards},
            "stop_stage": stage,
            "returned_count": returned,
            "limitations": "Address candidate pipeline only; POI, city shortcuts and country-specific post-ranking guards have no detailed trace. Area lookup failure also occurs during cityless search and does not prove an intended city is absent. Counts include failed alternative parses. IDs are local to the loaded index."
        })
    }
}
impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        TRACE.with(|t| {
            t.replace(self.previous.take());
        });
    }
}

pub(crate) fn parsed(
    rest: &str,
    postcode: Option<u32>,
    number: Option<u32>,
    rep: u32,
    city: Option<&str>,
) {
    observe(|t| {
        t.parse_attempts += 1;
        t.parsed_nonempty |= !rest.trim().is_empty();
        if t.parsed.len() < 8 {
            t.parsed.push(json!({"street_or_rest": rest, "commune": city, "postcode": postcode, "house_number": number, "house_suffix_id": rep}));
        }
    });
}
pub(crate) fn area(name: &str, ids: &[u32], method: &str) {
    observe(|t| {
        t.area_attempts += 1;
        t.areas.extend(ids.iter().copied());
        // Keep successful examples preferentially; failed attempts are still counted.
        if t.area_examples.len() < 8 && !ids.is_empty() {
            t.area_examples.push(json!({"commune": name, "method": method, "count": ids.len(), "first_ids": ids.iter().take(8).collect::<Vec<_>>()}));
        }
    });
}
pub(crate) fn street(sid: u32, exact: bool, scoped: bool) {
    observe(|t| {
        if t.streets.insert(sid) && t.street_examples.len() < 8 {
            t.street_examples
                .push(json!({"id": sid, "exact": exact, "commune_scoped": scoped}));
        }
    });
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn ranked(
    sid: u32,
    cid: u32,
    house_count: u32,
    requested: Option<u32>,
    precision: &str,
    score: f32,
    street: &str,
    commune: &str,
) {
    observe(|t| {
        t.ranked_count += 1;
        t.house_attempts += usize::from(requested.is_some());
        t.house_matches += usize::from(requested.is_some() && precision != "street");
        if score.is_finite() && t.best_score.is_none_or(|s| score > s) {
            t.best_score = Some(score);
        }
        if t.houses.len() < 8 {
            t.houses.push(json!({"street_id": sid, "commune_id": cid, "street": street, "commune": commune, "block_house_count": house_count, "requested_number": requested, "outcome": precision, "score": score}));
        }
    });
}

pub(crate) fn boundary(
    street: &str,
    commune: Option<&str>,
    postcode: Option<u32>,
    number: Option<u32>,
) {
    observe(|t| {
        t.boundary_count += 1;
        if t.boundaries.len() < 8 {
            t.boundaries.push(json!({"street": street, "commune": commune, "postcode": postcode, "house_number": number}));
        }
    });
}
pub(crate) fn guard(stage: &str, count: usize, accepted: bool, evidence: Value) {
    observe(|t| {
        t.rejected_guards += usize::from(!accepted);
        if t.guards.len() < 8 {
            t.guards.push(
                json!({"stage": stage, "count": count, "accepted": accepted, "evidence": evidence}),
            );
        }
    });
}
