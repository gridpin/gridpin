//! `gridpin bench`: geocode a table whose true points are known and say how close the answers came.
//!
//! The figures are those of the project's own benchmark tool (`eval/wikidata4_regress.py`): distance on the
//! sphere of 6 371 000 m, a hit within 150 m, p95 linear and inclusive over the rows that got an answer. Besides,
//! the match rate of crime-analysis practice: the share of rows matched at an address (`match_level` PointAddress,
//! StreetAddress or StreetInt) — Ratcliffe's 85 % is the usual bar.

use serde::Serialize;

use crate::query::Hit;

/// Levels that count as matched for the match rate.
pub const MATCHED_LEVELS: [&str; 3] = ["PointAddress", "StreetAddress", "StreetInt"];

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Summary {
    pub rows: usize,
    pub answered: usize,
    pub within_150m: usize,
    /// within 150 m over all rows
    pub share_within_150m: f64,
    pub median_m: Option<f64>,
    pub p95_m: Option<f64>,
    /// matched at an address (`MATCHED_LEVELS`) over all rows; absent for answers read from a file (`--answers`),
    /// which carry no level
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_rate: Option<f64>,
    /// rows by `match_level`; "U" for rows without an answer
    pub by_match_level: std::collections::BTreeMap<String, usize>,
}

/// Linear inclusive quantile of sorted values (numpy's default), as the benchmark tool computes p95.
fn quantile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (sorted.len() - 1) as f64 * q;
    let (lo, hi) = (rank.floor() as usize, rank.ceil() as usize);
    Some(sorted[lo] + (rank - lo as f64) * (sorted[hi] - sorted[lo]))
}

/// One row for the summary: the answer's point and level (None: no answer) and the true point.
pub type Row = (Option<((f64, f64), &'static str)>, (f64, f64));

/// The summary of one run: for each row its answer (if any) and its true point.
pub fn summarize(rows: &[(Option<&Hit>, (f64, f64))]) -> Summary {
    let rows: Vec<Row> = rows
        .iter()
        .map(|(h, t)| {
            (
                h.map(|h| ((h.lat, h.lon), crate::query::match_level(h).0)),
                *t,
            )
        })
        .collect();
    summarize_rows(&rows, true)
}

/// An answer read from a file (None: no answer) and the true point.
pub type PointRow = (Option<(f64, f64)>, (f64, f64));

/// The summary of answers read from a file (`bench --answers`): points without a level, so no match rate.
pub fn summarize_points(rows: &[PointRow]) -> Summary {
    let rows: Vec<Row> = rows
        .iter()
        .map(|(p, t)| (p.map(|p| (p, "answered")), *t))
        .collect();
    summarize_rows(&rows, false)
}

/// The summary of rows; `levels` false leaves the match rate out.
pub fn summarize_rows(rows: &[Row], levels: bool) -> Summary {
    let mut dists: Vec<f64> = Vec::new();
    let mut by_level = std::collections::BTreeMap::new();
    let mut matched = 0;
    for (answer, truth) in rows {
        match answer {
            Some((point, level)) => {
                dists.push(crate::codes::haversine_m(*truth, *point));
                matched += usize::from(MATCHED_LEVELS.contains(level));
                *by_level.entry(level.to_string()).or_insert(0) += 1;
            }
            None => *by_level.entry("U".to_string()).or_insert(0) += 1,
        }
    }
    let within = dists.iter().filter(|d| **d <= 150.0).count();
    dists.sort_by(f64::total_cmp);
    let n = rows.len().max(1) as f64;
    Summary {
        rows: rows.len(),
        answered: dists.len(),
        within_150m: within,
        share_within_150m: within as f64 / n,
        median_m: quantile(&dists, 0.5).map(|v| (v * 100.0).round() / 100.0),
        p95_m: quantile(&dists, 0.95).map(|v| (v * 100.0).round() / 100.0),
        match_rate: levels.then(|| matched as f64 / n),
        by_match_level: by_level,
    }
}

/// Rows below this many are a probe, not a result (PRINCIPLES 2.4): a slice that small says nothing about a difference.
pub const PROBE_ROWS: usize = 300;

/// The summary of each value of a column (`bench --by`), with `probe` true for a slice under `PROBE_ROWS` rows.
/// An empty value is its own slice, named "(empty)".
pub fn by_slices(keys: &[String], rows: &[Row], levels: bool) -> serde_json::Value {
    let mut groups: std::collections::BTreeMap<&str, Vec<Row>> = std::collections::BTreeMap::new();
    for (k, r) in keys.iter().zip(rows) {
        let k = if k.trim().is_empty() {
            "(empty)"
        } else {
            k.as_str()
        };
        groups.entry(k).or_default().push(*r);
    }
    let mut out = serde_json::Map::new();
    for (k, g) in groups {
        let mut v = serde_json::to_value(summarize_rows(&g, levels)).expect("a summary is JSON");
        v["probe"] = serde_json::Value::Bool(g.len() < PROBE_ROWS);
        out.insert(k.to_string(), v);
    }
    serde_json::Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_add_up_to_the_whole_and_small_ones_are_probes() {
        let near = (Some(((0.0, 0.0), "PointAddress")), (0.0, 0.0001));
        let far = (Some(((0.0, 0.0), "StreetName")), (0.01, 0.0));
        let none = (None, (0.0, 0.0));
        let rows = vec![near, far, none, near];
        let keys: Vec<String> = ["a", "a", "b", ""].iter().map(|s| s.to_string()).collect();
        let v = by_slices(&keys, &rows, true);
        let whole = summarize_rows(&rows, true);
        let sum = |f: &str| {
            ["a", "b", "(empty)"]
                .iter()
                .map(|k| v[k][f].as_u64().unwrap())
                .sum::<u64>()
        };
        assert_eq!(sum("rows"), whole.rows as u64);
        assert_eq!(sum("within_150m"), whole.within_150m as u64);
        assert_eq!(sum("answered"), whole.answered as u64);
        assert_eq!(v["a"]["within_150m"], 1);
        assert_eq!(v["a"]["probe"], true);
        assert_eq!(v["(empty)"]["rows"], 1, "an empty value is its own slice");
    }

    #[test]
    fn answers_from_a_file_have_no_match_rate() {
        let s = summarize_points(&[(Some((0.0, 0.0)), (0.0, 0.0)), (None, (1.0, 1.0))]);
        assert_eq!((s.rows, s.answered, s.within_150m), (2, 1, 1));
        assert!(s.match_rate.is_none());
        assert!(!serde_json::to_string(&s).unwrap().contains("match_rate"));
    }

    #[test]
    fn the_quantile_is_the_linear_inclusive_one_of_the_benchmark_tool() {
        // numpy.percentile([0, 10, 20, 30], 95, method="linear") = 28.5
        let close = |got: Option<f64>, want: f64| got.is_some_and(|g| (g - want).abs() < 1e-9);
        assert!(close(quantile(&[0.0, 10.0, 20.0, 30.0], 0.95), 28.5));
        assert_eq!(quantile(&[5.0], 0.95), Some(5.0));
        assert_eq!(quantile(&[], 0.5), None);
        assert!(close(quantile(&[0.0, 10.0, 20.0, 30.0], 0.5), 15.0));
    }
}
