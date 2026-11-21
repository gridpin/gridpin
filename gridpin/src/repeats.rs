//! Rows that repeat an earlier row of the same `batch` run are answered once.
//!
//! The key is the question exactly as the run asks it: the whole JSONL line, or the cells a CSV row is read from
//! (one joined line, the `--columns` fields, or the point of `--reverse`). Nothing is normalised: "10 Main St" and
//! "10  main st" are two questions, because flags such as `--with verdict` read how the address is written. The
//! cache lives for one run, in memory, and holds at most `cap` answers; when a window would pass the cap, the
//! answers kept so far are dropped and the cache starts again from that window.
//!
//! Repeats are found before a window goes to the thread pool, so the same rows are answered once at any number
//! of threads and the count printed at the end is the same on every run of the same input.
use std::collections::HashMap;
use std::hash::Hash;

/// Answers kept when `GRIDPIN_REPEAT_CACHE` is not set. A kept answer costs its question and its JSON answer, about
/// half a kilobyte for an answer with a house: some 50 MB at this cap.
pub const DEFAULT_CAP: usize = 100_000;

/// `GRIDPIN_REPEAT_CACHE`: the number of answers kept; `0` turns the cache off (every row is answered).
pub fn cap_from_env() -> usize {
    std::env::var("GRIDPIN_REPEAT_CACHE")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_CAP)
}

pub struct Repeats<K, V> {
    kept: HashMap<K, V>,
    cap: usize,
    /// rows answered from an earlier row with the same key
    pub repeated: u64,
}

impl<K: Hash + Eq + Clone, V: Clone> Repeats<K, V> {
    pub fn new(cap: usize) -> Self {
        Repeats {
            kept: HashMap::new(),
            cap,
            repeated: 0,
        }
    }

    /// Answers kept right now (never more than the cap).
    pub fn len(&self) -> usize {
        self.kept.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }

    /// The answers of one window, in the order of `keys`. A row without a key (`None`) is always asked and never
    /// counted. `ask` gets the rows to answer, by their position in `keys`, and returns their answers in that order.
    pub fn answer<F>(&mut self, keys: &[Option<K>], ask: F) -> Vec<V>
    where
        F: FnOnce(&[usize]) -> Vec<V>,
    {
        if self.cap == 0 {
            let all: Vec<usize> = (0..keys.len()).collect();
            return ask(&all);
        }
        // the first row of each new key, and the rows without a key, are asked; the other rows copy an answer
        let mut first_of: HashMap<&K, usize> = HashMap::new();
        let mut asked: Vec<usize> = Vec::new();
        for (at, key) in keys.iter().enumerate() {
            match key {
                None => asked.push(at),
                Some(k) if self.kept.contains_key(k) || first_of.contains_key(k) => {
                    self.repeated += 1
                }
                Some(k) => {
                    first_of.insert(k, at);
                    asked.push(at);
                }
            }
        }
        let answers = ask(&asked);
        assert_eq!(answers.len(), asked.len(), "one answer per asked row");
        let mut by_row: Vec<Option<V>> = vec![None; keys.len()];
        for (at, v) in asked.into_iter().zip(answers) {
            by_row[at] = Some(v);
        }
        let out: Vec<V> = keys
            .iter()
            .enumerate()
            .map(|(at, key)| match (&by_row[at], key) {
                (Some(v), _) => v.clone(),
                (None, Some(k)) => match self.kept.get(k) {
                    Some(v) => v.clone(),
                    None => by_row[first_of[k]]
                        .clone()
                        .expect("first row of a key is asked"),
                },
                (None, None) => unreachable!("a row without a key is asked"),
            })
            .collect();
        let new: Vec<(K, usize)> = first_of
            .into_iter()
            .map(|(k, at)| (k.clone(), at))
            .collect();
        if self.kept.len() + new.len() > self.cap {
            self.kept.clear();
        }
        let mut new = new;
        new.sort_by_key(|(_, at)| *at);
        for (k, at) in new.into_iter().take(self.cap) {
            if let Some(v) = by_row[at].take() {
                self.kept.insert(k, v);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn keys(words: &[&str]) -> Vec<Option<String>> {
        words
            .iter()
            .map(|w| (!w.is_empty()).then(|| w.to_string()))
            .collect()
    }

    /// answers "<key>#<how many times this key was asked so far>", so a copied answer is told from an asked one
    fn run(
        r: &mut Repeats<String, String>,
        window: &[&str],
        log: &RefCell<Vec<String>>,
    ) -> Vec<String> {
        let k = keys(window);
        r.answer(&k, |rows| {
            rows.iter()
                .map(|&at| {
                    let name = window[at].to_string();
                    log.borrow_mut().push(name.clone());
                    let n = log.borrow().iter().filter(|x| **x == name).count();
                    format!("{name}#{n}")
                })
                .collect()
        })
    }

    #[test]
    fn a_repeated_row_is_asked_once_and_gets_the_same_answer() {
        let log = RefCell::new(Vec::new());
        let mut r = Repeats::new(10);
        let out = run(&mut r, &["a", "b", "a", "a"], &log);
        assert_eq!(out, ["a#1", "b#1", "a#1", "a#1"]);
        assert_eq!(*log.borrow(), ["a", "b"]);
        assert_eq!(r.repeated, 2);
        // the next window reuses what the first one kept
        let out = run(&mut r, &["b", "c"], &log);
        assert_eq!(out, ["b#1", "c#1"]);
        assert_eq!(r.repeated, 3);
    }

    #[test]
    fn rows_without_a_key_are_always_asked_and_never_counted() {
        let log = RefCell::new(Vec::new());
        let mut r = Repeats::new(10);
        let out = run(&mut r, &["", "", "a"], &log);
        assert_eq!(out, ["#1", "#2", "a#1"]);
        assert_eq!(r.repeated, 0);
    }

    #[test]
    fn keys_that_differ_in_case_or_spacing_are_different_questions() {
        let log = RefCell::new(Vec::new());
        let mut r = Repeats::new(10);
        run(
            &mut r,
            &["10 Main St", "10 main st", "10  Main St", "10 Main St "],
            &log,
        );
        assert_eq!(r.repeated, 0);
    }

    #[test]
    fn the_cache_never_holds_more_than_the_cap() {
        let log = RefCell::new(Vec::new());
        let mut r = Repeats::new(3);
        run(&mut r, &["a", "b"], &log);
        assert_eq!(r.len(), 2);
        run(&mut r, &["c", "d"], &log);
        assert!(r.len() <= 3, "{}", r.len());
        run(&mut r, &["e", "f", "g", "h", "i"], &log);
        assert_eq!(r.len(), 3);
        // what was dropped is asked again, and gets the same answer the engine gives
        let out = run(&mut r, &["a"], &log);
        assert_eq!(out, ["a#2"]);
    }

    #[test]
    fn cap_zero_asks_every_row() {
        let log = RefCell::new(Vec::new());
        let mut r = Repeats::new(0);
        let out = run(&mut r, &["a", "a", "a"], &log);
        assert_eq!(out, ["a#1", "a#2", "a#3"]);
        assert_eq!(r.repeated, 0);
        assert!(r.is_empty());
    }
}
