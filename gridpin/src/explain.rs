//! `gridpin explain <sheet> "query"`: why the engine answered as it did, in words and as JSON for agents.
//!
//! Read from three things the engine already makes: the trace of `--diagnose` (how the line was read: street,
//! town, house number, postcode of the first readings; how many towns and streets matched; where it stopped), the
//! verdicts of the answer's fields (`verdict.rs`), and the outcome (an answer, a question to ask, or a silence with
//! its reason, `outcome.rs`). The top three candidates are listed with what each lost on. Nothing is looked up
//! again, and the answers are those of `gridpin query -k 3`.

use serde_json::{json, Value};

use crate::exists;
use crate::outcome::{query_outcome, Outcome};
use crate::query::{query_cascade, Hit, Index};
use crate::verdict::{asked, field_verdicts_in};

/// The reading of the line that gave the answer: among the street/town readings the address path tried (the
/// `--diagnose` trace), the one whose town and street agree with the top answer; the first reading only when no
/// answer came. Revision 0.9.0: always taking the first reading said "town not named" next to "city: confirmed".
/// `answered_by` says which: `answer` (a reading agrees with the answer), `first` (no answer), `none` (the answer
/// came by another path — a place by its name, a POI — and no reading agrees with it; the fields stay empty).
fn reading(trace: &Value, top: Option<&Hit>) -> Value {
    let examples = trace["parsed"]["boundary_examples"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let (chosen, answered_by) = match top {
        None => (examples.first().cloned(), "first"),
        Some(h) => {
            // the reading that agrees best; of equals, the first tried
            let mut best: Option<(u32, &Value)> = None;
            for e in &examples {
                if let Some(a) = agreement(e, h) {
                    if best.is_none_or(|(b, _)| a > b) {
                        best = Some((a, e));
                    }
                }
            }
            match best {
                Some((_, e)) => (Some(e.clone()), "answer"),
                None => (None, "none"),
            }
        }
    };
    let field = |k: &str| chosen.as_ref().map(|e| e[k].clone()).unwrap_or(Value::Null);
    json!({
        "street": field("street"),
        "town": field("commune"),
        "house_number": field("house_number"),
        "postcode": field("postcode"),
        "readings_tried": trace["parsed"]["attempts"],
        "answered_by": answered_by,
        // the town the answer matched by a name of the query (commune_exact, commune_prefix), when the reading named
        // has none: the readings kept by the trace are the first eight, and the one with that town may be later
        "town_matched": top
            .filter(|_| chosen.as_ref().is_none_or(|e| e["commune"].is_null()))
            .filter(|h| h.flags.contains(&"commune_exact") || h.flags.contains(&"commune_prefix"))
            .map(|h| h.commune.clone()),
    })
}

/// How well a reading agrees with an answer: `None` when its town contradicts it, else the weight of the fields
/// that agree (town 2, street 1, house number 1). Names are compared normalized; a reading's town agrees
/// when it is the answer's commune or a word-prefix of it ("beograd" / "Beograd (Vračar)").
fn agreement(e: &Value, h: &Hit) -> Option<u32> {
    use crate::norm::normalize;
    let close = |read: &str, answer: &str| {
        let (r, a) = (normalize(read), normalize(answer));
        !r.is_empty() && (a == r || a.starts_with(&format!("{r} ")))
    };
    let mut score = 0;
    if let Some(town) = e["commune"].as_str().filter(|t| !t.trim().is_empty()) {
        // the reading's town agrees when each of its words is a word of the answer's commune or region (Beograd
        // written by districts: commune Stari Grad, region Grad Beograd)
        let words = |t: &str| {
            normalize(t)
                .split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>()
        };
        let mut place = words(&h.commune);
        place.extend(h.region.as_deref().map(words).unwrap_or_default());
        let read = words(town);
        let agrees = !read.is_empty() && read.iter().all(|w| place.contains(w));
        if !agrees {
            return None;
        }
        score += 2;
    }
    if let Some(street) = e["street"].as_str().filter(|t| !t.trim().is_empty()) {
        // a street read differently (an abbreviation, a fixed spelling) does not contradict the answer
        if !h.street.is_empty() && (close(street, &h.street) || close(&h.street, street)) {
            score += 1;
        }
    }
    if let (Some(n), Some(hn)) = (e["house_number"].as_u64(), h.housenumber.as_deref()) {
        let digits: String = hn.chars().take_while(char::is_ascii_digit).collect();
        if digits.parse::<u64>().ok() == Some(n) {
            score += 1;
        }
    }
    Some(score)
}

fn label(h: &Hit) -> String {
    let number = h
        .housenumber
        .as_deref()
        .map(|n| format!(" {n}"))
        .unwrap_or_default();
    let street = if h.street.is_empty() {
        String::new()
    } else {
        format!("{}{number}, ", h.street)
    };
    format!("{street}{} ({})", h.commune, h.precision)
}

/// What each candidate after the first lost on: a lower score, a weaker level, a fixed street.
fn lost_on(top: &Hit, h: &Hit) -> String {
    let mut why = Vec::new();
    if h.precision != top.precision {
        why.push(format!(
            "level {} instead of {}",
            h.precision, top.precision
        ));
    }
    if h.flags.contains(&"street_fuzzy") && top.flags.contains(&"street_exact") {
        why.push("street found with a fix, not as written".to_string());
    }
    if h.score < top.score {
        why.push(format!("score {:.2} below {:.2}", h.score, top.score));
    } else if (h.score - top.score).abs() < 1e-6 {
        why.push(
            "the same score: ordered by the sheet's tie rules (town size, distance)".to_string(),
        );
    }
    if why.is_empty() {
        why.push("ranked after it".to_string());
    }
    why.join("; ")
}

/// The explanation as JSON: reading, outcome, verdicts, existence and candidates.
pub fn explain(idx: &Index, q: &str) -> Value {
    let scope = crate::diagnosis::Scope::new();
    let hits = query_cascade(idx, None, q, 3);
    let trace = scope.finish(hits.len());
    let outcome = query_outcome(idx, None, q, 3);
    let (kind, reason, missing) = match &outcome {
        Outcome::Answer { .. } => ("answer", None, None),
        Outcome::Clarify { missing, .. } => ("clarify", None, Some(*missing)),
        Outcome::None { reason } => ("none", Some(*reason), None),
    };
    let a = asked(idx, q);
    let top = hits.first();
    let candidates: Vec<Value> = hits
        .iter()
        .enumerate()
        .map(|(i, h)| {
            json!({
                "rank": i + 1,
                "answer": label(h),
                "score": h.score,
                "confidence": h.confidence,
                "flags": h.flags,
                "lost_on": if i == 0 { Value::Null } else { json!(lost_on(&hits[0], h)) },
            })
        })
        .collect();
    json!({
        "query": q,
        "reading": reading(&trace, top),
        "outcome": kind,
        "silence_reason": reason,
        "missing": missing,
        "verdicts": top.map(|h| json!(field_verdicts_in(idx, q, &a, h))),
        "exists": exists::judge_answer(idx, q, &a, top),
        "stopped_at": trace["stop_stage"],
        "candidates": candidates,
    })
}

/// The same in words.
pub fn text(e: &Value) -> String {
    let r = &e["reading"];
    let s = |v: &Value| {
        v.as_str()
            .map(String::from)
            .unwrap_or_else(|| v.to_string())
    };
    let mut out = if r["answered_by"] == "none" {
        format!(
            "The answer did not come from a street and town reading of the line ({} readings tried): it came by \
             another path (a place by its name, a point of interest, a postcode).\n",
            s(&r["readings_tried"]),
        )
    } else {
        format!(
            "The line was read as street {}, town {}, house number {}, postcode {} ({} readings tried).\n",
            if r["street"].is_null() { "none".to_string() } else { format!("\"{}\"", s(&r["street"])) },
            match (r["town"].as_str(), r["town_matched"].as_str()) {
                (Some(t), _) => format!("\"{t}\""),
                (None, Some(m)) => format!("matched as \"{m}\""),
                (None, None) => "not named".to_string(),
            },
            if r["house_number"].is_null() { "none".to_string() } else { s(&r["house_number"]) },
            if r["postcode"].is_null() { "none".to_string() } else { s(&r["postcode"]) },
            s(&r["readings_tried"]),
        )
    };
    match e["outcome"].as_str() {
        Some("none") => out += &format!("No answer: {}.\n", s(&e["silence_reason"])),
        Some("clarify") => {
            out += &format!(
                "An answer, but the query needs a {} to choose among places.\n",
                s(&e["missing"])
            )
        }
        _ => {}
    }
    if let Some(v) = e["verdicts"].as_object() {
        let parts: Vec<String> = v.iter().map(|(k, x)| format!("{k}: {}", s(x))).collect();
        out += &format!("Fields: {}.\n", parts.join(", "));
    }
    out += &format!(
        "Exists: {} ({}).\n",
        s(&e["exists"]["exists"]),
        s(&e["exists"]["reason"])
    );
    for c in e["candidates"].as_array().into_iter().flatten() {
        out += &format!(
            "{}. {} — score {:.2}",
            c["rank"],
            s(&c["answer"]),
            c["score"].as_f64().unwrap_or(0.0)
        );
        if let Some(l) = c["lost_on"].as_str() {
            out += &format!(" — lost on: {l}");
        }
        out += "\n";
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(street: &str, commune: &str, region: Option<&str>, number: Option<&str>) -> Hit {
        Hit {
            lat: 0.0,
            lon: 0.0,
            precision: "house",
            score: 1.0,
            confidence: 1.0,
            street: street.into(),
            housenumber: number.map(String::from),
            commune: commune.into(),
            postcode: String::new(),
            flags: vec!["street_exact", "commune_exact"],
            region: region.map(String::from),
            distance_m: None,
        }
    }

    #[test]
    fn a_reading_whose_town_is_not_the_answers_place_does_not_explain_it() {
        let h = hit("Hilandarska", "Stari Grad", Some("Grad Beograd"), Some("8"));
        let r = |street: &str, town: Option<&str>| json!({"street": street, "commune": town, "house_number": 8, "postcode": null});
        // the town written by its districts: every word of the reading's town is a word of the commune or region
        assert_eq!(agreement(&r("hilandarska", Some("beograd")), &h), Some(4));
        // the street read as the town, or another town: no agreement at all
        assert_eq!(agreement(&r("beograd", Some("hilandarska")), &h), None);
        assert_eq!(agreement(&r("hilandarska", Some("novi sad")), &h), None);
        // a reading without a town agrees on what it has
        assert_eq!(agreement(&r("hilandarska beograd", None), &h), Some(2));
        // the reading chosen is the one with the town, though it comes second
        let trace = json!({"parsed": {"attempts": 2, "boundary_examples": [
            r("hilandarska beograd", None), r("beograd", Some("hilandarska")), r("hilandarska", Some("beograd"))]}});
        let got = reading(&trace, Some(&h));
        assert_eq!(got["town"], "beograd", "{got}");
        assert_eq!(got["answered_by"], "answer");
        // no reading agrees on the town and none is without one: the answer came by another path
        let other = json!({"parsed": {"attempts": 1, "boundary_examples": [r("beograd", Some("hilandarska"))]}});
        assert_eq!(reading(&other, Some(&h))["answered_by"], "none");
    }
}
