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
}
