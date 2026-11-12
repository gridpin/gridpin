//! `--aliases`: your own names for places, read before the sheet. "HQ", "depot 3", "store 117" -> an address.
//!
//! Not a sheet and not part of one: a small table beside the query. The address is geocoded as usual, so the
//! answer is the one a query for that address gets, plus the flag `alias` and the name that was asked. A
//! name matches the whole query after normalization; anything else takes the ordinary path untouched.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::norm::normalize;
use crate::query::Hit;

/// The flag every answer found through the table carries.
pub const ALIAS_FLAG: &str = "alias";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    pub name: String,
    pub address: String,
}

#[derive(Clone, Debug, Default)]
pub struct Aliases {
    by_key: HashMap<String, Alias>,
}

impl Aliases {
    /// A CSV file with the columns `name` and `address`.
    pub fn load(path: &Path) -> Result<Aliases> {
        let file =
            std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        Aliases::read(file).with_context(|| format!("in {}", path.display()))
    }

    /// Refuses what would answer wrongly later: a missing column, an empty cell, a name given twice.
    pub fn read(input: impl std::io::Read) -> Result<Aliases> {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_reader(input);
        let headers = reader.headers()?.clone();
        let column = |want: &str| {
            headers
                .iter()
                .position(|h| h.trim().eq_ignore_ascii_case(want))
                .ok_or_else(|| anyhow!("the aliases file needs a column {want:?}"))
        };
        let (name_at, address_at) = (column("name")?, column("address")?);
        let mut by_key = HashMap::new();
        for (i, record) in reader.records().enumerate() {
            let record = record?;
            let line = i + 2;
            let cell = |at: usize| record.get(at).map(str::trim).unwrap_or_default();
            let (name, address) = (cell(name_at), cell(address_at));
            let key = normalize(name);
            if key.is_empty() || address.is_empty() {
                bail!("line {line}: a name and an address are both required");
            }
            let entry = Alias {
                name: name.to_string(),
                address: address.to_string(),
            };
            if by_key.insert(key, entry).is_some() {
                bail!("line {line}: the name {name:?} is already in the file");
            }
        }
        Ok(Aliases { by_key })
    }

    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// The entry a whole query names.
    pub fn find(&self, query: &str) -> Option<&Alias> {
        self.by_key.get(&normalize(query))
    }
}

/// Answer a line through the table, when there is one: the alias's address goes to `geocode`, and every
/// answer is flagged. Returns the name that was asked, if the line was one.
pub fn answer(
    aliases: Option<&Aliases>,
    query: &str,
    geocode: impl FnOnce(&str) -> Vec<Hit>,
) -> (Vec<Hit>, Option<String>) {
    match aliases.and_then(|table| table.find(query)) {
        None => (geocode(query), None),
        Some(alias) => {
            let mut hits = geocode(&alias.address);
            for hit in &mut hits {
                hit.flags.push(ALIAS_FLAG);
            }
            (hits, Some(alias.name.clone()))
        }
    }
}

/// One hit as JSON, with `alias_name` after its other fields when the line was a name.
pub fn hit_json(hit: &Hit, name: Option<&str>) -> Result<Value> {
    let mut value = serde_json::to_value(hit)?;
    if let (Some(name), Some(fields)) = (name, value.as_object_mut()) {
        fields.insert("alias_name".into(), name.into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Result<Aliases> {
        Aliases::read(text.as_bytes())
    }

    fn hit() -> Hit {
        Hit {
            lat: 1.0,
            lon: 2.0,
            precision: "house",
            score: 0.0,
            confidence: 0.9,
            street: "Rue A".into(),
            housenumber: Some("1".into()),
            commune: "Ville".into(),
            postcode: String::new(),
            flags: vec!["street_exact"],
            region: None,
            distance_m: None,
        }
    }

    #[test]
    fn a_name_is_found_as_a_whole_query_whatever_its_case_and_accents() {
        let t = table("name,address\nHQ,\"rue a 1, ville\"\nDépôt 3,rue b 2\n").unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t.find("hq").unwrap().address, "rue a 1, ville");
        assert_eq!(t.find("  HQ ").unwrap().name, "HQ");
        assert_eq!(t.find("depot 3").unwrap().name, "Dépôt 3");
        assert!(t.find("hq paris").is_none(), "part of a query is no name");
        assert!(t.find("rue a 1, ville").is_none(), "an address is no name");
    }

    #[test]
    fn a_table_that_would_answer_wrongly_is_refused_with_its_line() {
        for (text, says) in [
            ("alias,address\nHQ,a\n", "\"name\""),
            ("name,where\nHQ,a\n", "\"address\""),
            ("name,address\n,a\n", "line 2"),
            ("name,address\nHQ,  \n", "line 2"),
            ("name,address\nHQ,a\nhq,b\n", "line 3"),
        ] {
            let err = format!("{:#}", table(text).unwrap_err());
            assert!(err.contains(says), "{text}: {err}");
        }
    }

    #[test]
    fn a_name_is_answered_by_its_address_and_flagged_and_other_lines_pass_untouched() {
        let t = table("name,address\nHQ,rue a 1\n").unwrap();
        let asked = std::cell::RefCell::new(String::new());
        let geocode = |q: &str| {
            *asked.borrow_mut() = q.to_string();
            vec![hit()]
        };
        let (hits, name) = answer(Some(&t), "hq", geocode);
        assert_eq!(asked.borrow().as_str(), "rue a 1");
        assert_eq!(name.as_deref(), Some("HQ"));
        assert_eq!(hits[0].flags, ["street_exact", ALIAS_FLAG]);
        let (hits, name) = answer(Some(&t), "rue b 2", geocode);
        assert_eq!(asked.borrow().as_str(), "rue b 2");
        assert_eq!(name, None);
        assert_eq!(hits[0].flags, ["street_exact"]);
        let (hits, name) = answer(None, "hq", geocode);
        assert_eq!(asked.borrow().as_str(), "hq");
        assert_eq!((hits[0].flags.len(), name), (1, None));
    }

    #[test]
    fn the_name_stands_in_the_json_only_when_there_is_one() {
        assert_eq!(hit_json(&hit(), Some("HQ")).unwrap()["alias_name"], "HQ");
        assert!(hit_json(&hit(), None).unwrap().get("alias_name").is_none());
    }
}
