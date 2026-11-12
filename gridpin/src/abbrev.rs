//! `--abbrev words.tsv`: your own abbreviations, written out before the query reaches the engine.
//!
//! A sheet knows the abbreviations of its country ("St" for "Street", "Av." for "Avenida"); a trade or a
//! company has its own ("Ind. Est." for "Industrial Estate", "Bldg" for "Building"). The file has two columns,
//! the short form and the full one, separated by a tab; a line starting with `#` is a comment. A word of the
//! query is replaced when it equals a short form, without regard to case and with or without a closing dot.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

#[derive(Clone, Debug, Default)]
pub struct Abbrev {
    full: HashMap<String, String>,
}

fn key(word: &str) -> String {
    word.trim_end_matches('.').to_lowercase()
}

impl Abbrev {
    pub fn load(path: &Path) -> Result<Abbrev> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        Abbrev::read(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn read(text: &str) -> Result<Abbrev> {
        let mut full = HashMap::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let Some((short, long)) = line.split_once('\t') else {
                bail!(
                    "line {}: two columns separated by a tab: short form, full form",
                    n + 1
                );
            };
            let (short, long) = (short.trim(), long.trim());
            if short.is_empty() || long.is_empty() || short.contains(' ') {
                bail!(
                    "line {}: the short form is one word and neither column is empty",
                    n + 1
                );
            }
            if full.insert(key(short), long.to_string()).is_some() {
                bail!("line {}: {short:?} appears twice", n + 1);
            }
        }
        if full.is_empty() {
            bail!("no abbreviations");
        }
        Ok(Abbrev { full })
    }

    /// The query with every listed short form written out; commas and spacing between words are kept.
    pub fn apply(&self, q: &str) -> String {
        let mut out = String::with_capacity(q.len() + 16);
        let mut word = String::new();
        let flush = |word: &mut String, out: &mut String| {
            if !word.is_empty() {
                match self.full.get(&key(word)) {
                    Some(long) => out.push_str(long),
                    None => out.push_str(word),
                }
                word.clear();
            }
        };
        for c in q.chars() {
            if c.is_whitespace() || c == ',' || c == ';' {
                flush(&mut word, &mut out);
                out.push(c);
            } else {
                word.push(c);
            }
        }
        flush(&mut word, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_forms_are_written_out_with_or_without_a_dot_and_case() {
        let a =
            Abbrev::read("# trade words\nInd.\tIndustrial\nEst\tEstate\nbldg\tBuilding\n").unwrap();
        assert_eq!(
            a.apply("Unit 4, Fenton Ind. Est., Stoke"),
            "Unit 4, Fenton Industrial Estate, Stoke"
        );
        assert_eq!(a.apply("BLDG 7 Main St"), "Building 7 Main St");
        assert_eq!(
            a.apply("Industrial Road 5"),
            "Industrial Road 5",
            "only whole words"
        );
        assert_eq!(a.apply("Estonia Street"), "Estonia Street");
    }

    #[test]
    fn a_bad_file_says_which_line() {
        for bad in [
            "Ind. Industrial\n",
            "\tIndustrial\n",
            "a b\tc\n",
            "x\ty\nx\tz\n",
            "# only a comment\n",
        ] {
            assert!(Abbrev::read(bad).is_err(), "{bad:?}");
        }
    }
}
