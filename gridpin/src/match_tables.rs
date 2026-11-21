//! `gridpin match A B OUT`: two tables joined by the address their rows name.
//!
//! Each row is answered by the sheet, and its key is the key `gridpin same` compares (`compare::answer_level`):
//! the normalized commune, street and, at the house level, house number of the top answer. Two rows pair when
//! their keys are equal, so a pair at the house level is a pair `same` calls "house" (neither answer weak), and
//! at the street level one it calls "house" or "street". The join is a hash join on the key, not a comparison of
//! every row with every other.
//!
//! A row gets no key, and so no pair, when the sheet does not answer it (`no_answer`), when the answer is weak
//! (`weak`, unless `lenient`: a weak answer is a guess the sheet does not stand behind), when the answer lacks a
//! part the level needs (`no_commune`, `no_street`, `no_number`), or at the house level when it is the nearest door
//! (`near`: its number is a neighbour's). One row of A with the same key as three rows of B
//! gives three pairs, each carrying the count of rows of the other side it pairs with.

use std::collections::HashMap;

use crate::norm::normalize;
use crate::query::{hit_is_weak, Hit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    House,
    Street,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "house" => Some(Level::House),
            "street" => Some(Level::Street),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Level::House => "house",
            Level::Street => "street",
        }
    }
}

/// The key of an answer at `level`, or why it has none.
pub fn key_of(h: Option<&Hit>, level: Level, lenient: bool) -> Result<String, &'static str> {
    let h = h.ok_or("no_answer")?;
    if hit_is_weak(h) && !lenient {
        return Err("weak");
    }
    let commune = normalize(&h.commune);
    let street = normalize(&h.street);
    if commune.is_empty() {
        return Err("no_commune");
    }
    if street.is_empty() {
        return Err("no_street");
    }
    match level {
        Level::Street => Ok(format!("{commune}\u{1f}{street}")),
        Level::House => {
            // the nearest door carries a neighbour's number, not the one written: "… 21" and "… 31" would join as one
            // house when the sheet lacks 31 (19.11.2026, plan M M7); the row still joins at the street level
            if h.precision == "near" {
                return Err("near");
            }
            let number = h.housenumber.as_deref().map(normalize).unwrap_or_default();
            if number.is_empty() {
                return Err("no_number");
            }
            Ok(format!("{commune}\u{1f}{street}\u{1f}{number}"))
        }
    }
}

/// A row of A and a row of B with one key; `a_matches` is how many rows of B the row of A pairs with, and
/// `b_matches` how many rows of A the row of B pairs with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    pub a: usize,
    pub b: usize,
    pub a_matches: usize,
    pub b_matches: usize,
}

/// The pairs, then the rows of each side without a pair and why (`no_pair` when the row has a key the other side
/// lacks). Rows are indexes into the two key lists, in their order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Joined {
    pub pairs: Vec<Pair>,
    pub only_a: Vec<(usize, &'static str)>,
    pub only_b: Vec<(usize, &'static str)>,
}

fn index<'a>(side: &'a [Result<String, &'static str>]) -> HashMap<&'a str, Vec<usize>> {
    let mut m: HashMap<&'a str, Vec<usize>> = HashMap::new();
    for (i, k) in side.iter().enumerate() {
        if let Ok(k) = k {
            m.entry(k.as_str()).or_default().push(i);
        }
    }
    m
}

pub fn join(a: &[Result<String, &'static str>], b: &[Result<String, &'static str>]) -> Joined {
    let (in_a, in_b) = (index(a), index(b));
    let mut out = Joined::default();
    for (i, k) in a.iter().enumerate() {
        match k {
            Err(why) => out.only_a.push((i, why)),
            Ok(k) => match in_b.get(k.as_str()) {
                None => out.only_a.push((i, "no_pair")),
                Some(bs) => out.pairs.extend(bs.iter().map(|&j| Pair {
                    a: i,
                    b: j,
                    a_matches: bs.len(),
                    b_matches: in_a[k.as_str()].len(),
                })),
            },
        }
    }
    for (j, k) in b.iter().enumerate() {
        match k {
            Err(why) => out.only_b.push((j, why)),
            Ok(k) if !in_a.contains_key(k.as_str()) => out.only_b.push((j, "no_pair")),
            Ok(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Result<String, &'static str> {
        Ok(s.to_string())
    }

    #[test]
    fn rows_pair_by_key_and_one_to_many_is_kept_with_its_counts() {
        let a = [k("x"), k("y"), Err("weak"), k("x")];
        let b = [k("x"), k("z"), Err("no_answer")];
        let j = join(&a, &b);
        assert_eq!(
            j.pairs,
            vec![
                Pair {
                    a: 0,
                    b: 0,
                    a_matches: 1,
                    b_matches: 2
                },
                Pair {
                    a: 3,
                    b: 0,
                    a_matches: 1,
                    b_matches: 2
                },
            ]
        );
        assert_eq!(j.only_a, vec![(1, "no_pair"), (2, "weak")]);
        assert_eq!(j.only_b, vec![(1, "no_pair"), (2, "no_answer")]);
    }

    fn hit(precision: &'static str, number: &str) -> Hit {
        Hit {
            lat: 0.0,
            lon: 0.0,
            precision,
            score: 0.0,
            confidence: 0.9,
            street: "Main St".into(),
            housenumber: Some(number.into()),
            commune: "Ames".into(),
            postcode: String::new(),
            flags: vec!["street_exact", "commune_exact"],
            region: None,
            distance_m: None,
        }
    }

    #[test]
    fn the_nearest_door_has_no_house_key_but_keeps_its_street() {
        // "Main St 31" the sheet lacks answers the neighbour 21: at the house level it must not join "Main St 21"
        assert_eq!(
            key_of(Some(&hit("near", "21")), Level::House, false),
            Err("near")
        );
        assert_eq!(
            key_of(Some(&hit("near", "21")), Level::Street, false),
            key_of(Some(&hit("house", "21")), Level::Street, false)
        );
        // near misses: a registry house and an interpolated number keep their house key
        assert!(key_of(Some(&hit("house", "21")), Level::House, false).is_ok());
        assert!(key_of(Some(&hit("interp", "21")), Level::House, false).is_ok());
    }

    #[test]
    fn a_key_on_one_side_only_or_no_rows_gives_no_pair() {
        assert_eq!(join(&[], &[k("x")]).only_b, vec![(0, "no_pair")]);
        assert!(join(&[k("x")], &[k("y")]).pairs.is_empty());
    }
}
