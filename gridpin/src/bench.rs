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
    /// matched at an address (`MATCHED_LEVELS`) over all rows
    pub match_rate: f64,
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

/// The summary of one run: for each row its answer (if any) and its true point.
pub fn summarize(rows: &[(Option<&Hit>, (f64, f64))]) -> Summary {
    let mut dists: Vec<f64> = Vec::new();
    let mut by_level = std::collections::BTreeMap::new();
    let mut matched = 0;
    for (hit, truth) in rows {
        match hit {
            Some(h) => {
                dists.push(crate::codes::haversine_m(*truth, (h.lat, h.lon)));
                let level = crate::query::match_level(h).0;
                matched += usize::from(MATCHED_LEVELS.contains(&level));
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
        match_rate: matched as f64 / n,
        by_match_level: by_level,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
