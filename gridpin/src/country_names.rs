//! Country names -> ISO 3166-1 alpha-2 (`country_names.tsv`): which country the end of a query names, for every
//! sheet alike. Data, no country branch: a server of several sheets picks the sheet a query names by it
//! (09.11.2026: "…, Serbia" and "…, España" did not pick their sheet, because only the US, CA, MX and AU rule
//! packs carry country words).

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::norm::normalize;

const TABLE: &str = include_str!("country_names.tsv");
/// Names in Cyrillic, kept here as data: the public text files of the project carry no Cyrillic.
const CYRILLIC: &[(&str, &str)] = &[("RU", "Россия"), ("RS", "Србија"), ("UA", "Україна")];

fn table() -> &'static HashMap<String, &'static str> {
    static MAP: OnceLock<HashMap<String, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        TABLE
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split_once('\t'))
            .chain(CYRILLIC.iter().copied())
            .map(|(cc, name)| (normalize(name), cc))
            .filter(|(k, _)| !k.is_empty())
            .collect()
    })
}

/// The ISO alpha-2 code (lower case) of the country a text names, written in any form of the table.
pub fn code_of(text: &str) -> Option<String> {
    table()
        .get(&normalize(text))
        .map(|cc| cc.to_ascii_lowercase())
}

/// The English short name of a country (the first name of its code in the table, ISO 3166-1), for `address.country`
/// of a Nominatim answer. `None` for a code the table does not have.
pub fn name_of(code: &str) -> Option<&'static str> {
    if code.len() != 2 {
        return None;
    }
    TABLE
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .find(|(cc, _)| cc.eq_ignore_ascii_case(code))
        .map(|(_, name)| name)
}

/// The country the last comma segment of a query names, if it names one.
pub fn named_at_end(q: &str) -> Option<String> {
    let (_, tail) = q.rsplit_once(',')?;
    code_of(tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_in_english_in_their_own_language_and_codes_find_the_country() {
        for (text, cc) in [
            ("Serbia", "rs"),
            ("Србија", "rs"),
            ("España", "es"),
            ("SPAIN", "es"),
            ("USA", "us"),
            ("the Netherlands", "nl"),
            ("Deutschland", "de"),
            ("BRA", "br"),
            ("México", "mx"),
        ] {
            assert_eq!(code_of(text).as_deref(), Some(cc), "{text}");
        }
        assert_eq!(code_of("Madrid"), None);
        assert_eq!(
            named_at_end("Knez Mihailova 10, Beograd, Serbia").as_deref(),
            Some("rs")
        );
        assert_eq!(named_at_end("Knez Mihailova 10, Beograd"), None);
        assert_eq!(
            named_at_end("Serbia"),
            None,
            "a query that is only a country names no sheet by its end"
        );
    }

    #[test]
    fn a_code_is_named_by_its_english_short_name_and_nothing_else_is() {
        assert_eq!(name_of("us"), Some("United States"));
        assert_eq!(name_of("RS"), Some("Serbia"));
        assert_eq!(name_of("de"), Some("Germany"));
        assert_eq!(name_of("zz"), None);
        // an alpha-3 code or a name is not a code
        assert_eq!(name_of("usa"), None);
        assert_eq!(name_of(""), None);
    }
}
