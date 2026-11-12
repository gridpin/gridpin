//! `gridpin cite <sheet>`: a citation of the sheet a study geocoded with, in BibTeX, APA or CFF.
//!
//! A sheet is a fixed file: its SHA-256 names it exactly, and the same engine version gives the same answers from it.
//! The citation carries both, with the data release and the licence the sheet records about itself; the release of
//! GridPin and the DOI come from the registry of this engine (`sheets.json`) when the hash is one of its sheets.

use std::path::Path;

use anyhow::{bail, Result};

use crate::query::Index;

pub const FORMATS: [&str; 3] = ["bibtex", "apa", "cff"];

/// What a citation says.
#[derive(Clone, Debug, PartialEq)]
pub struct Citation {
    pub title: String,
    pub key: String,
    /// GridPin release of the sheet (`v0.8.0`), or None for a sheet that is not in this engine's registry
    pub release: Option<String>,
    pub sha256: String,
    pub data_release: String,
    pub license: String,
    pub sources: String,
    pub engine: String,
    pub doi: Option<String>,
    pub year: String,
}

pub fn of(idx: &Index, path: &Path) -> Result<Citation> {
    let sha256 = crate::sheets::sha256_file(path)?;
    let meta = |k: &str| {
        idx.meta()
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let registry = crate::sheets::registry();
    let listed = registry.sheets.iter().find(|s| s.sha256 == sha256);
    let country = meta("country");
    let name = match (listed, meta("places_title").as_str()) {
        (Some(s), _) => s.name.clone(),
        (None, title) if !title.is_empty() => title.to_string(),
        _ if !country.is_empty() => country.to_uppercase(),
        _ => "unnamed sheet".to_string(),
    };
    let data_release = meta("source_release");
    let year = data_release
        .get(..4)
        .filter(|y| y.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or("n.d.")
        .to_string();
    let release = listed.map(|_| registry.release.clone());
    let key = format!(
        "gridpin_{}_{}",
        if country.is_empty() {
            "sheet"
        } else {
            &country
        },
        release.as_deref().unwrap_or(&sha256[..8]).replace('.', "_")
    );
    Ok(Citation {
        title: format!("GridPin address sheet: {name}"),
        key,
        release,
        sha256,
        data_release,
        license: meta("license"),
        sources: {
            let s = meta("sources");
            if s.is_empty() {
                meta("attribution")
            } else {
                s
            }
        },
        engine: env!("CARGO_PKG_VERSION").to_string(),
        doi: listed.map(|s| s.doi.clone()).filter(|d| !d.is_empty()),
        year,
    })
}

fn version(c: &Citation) -> String {
    c.release
        .clone()
        .unwrap_or_else(|| format!("unreleased build sha256:{}", &c.sha256[..12]))
}

fn note(c: &Citation) -> String {
    let mut parts = vec![format!("SHA-256 {}", c.sha256)];
    if !c.data_release.is_empty() {
        parts.push(format!("data release {}", c.data_release));
    }
    if !c.license.is_empty() {
        parts.push(format!("licence: {}", c.license));
    }
    parts.push(format!("geocoded with gridpin {}", c.engine));
    if !c.sources.is_empty() {
        parts.push(format!("sources: {}", c.sources));
    }
    parts.join("; ")
}

fn bib(s: &str) -> String {
    s.replace('\\', "\\textbackslash{}")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('&', "\\&")
        .replace('%', "\\%")
}

fn yaml(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn render(c: &Citation, format: &str) -> Result<String> {
    Ok(match format {
        "bibtex" => {
            let mut out = format!(
                "@misc{{{},\n  author = {{{{GridPin}}}},\n  title = {{{{{}}}}},\n  year = {{{}}},\n  version = {{{}}},\n  howpublished = {{\\url{{https://gridpin.dev}}}},\n",
                c.key, bib(&c.title), c.year, bib(&version(c))
            );
            if let Some(doi) = &c.doi {
                out += &format!("  doi = {{{}}},\n", bib(doi));
            }
            out + &format!("  note = {{{}}}\n}}\n", bib(&note(c)))
        }
        "apa" => {
            let link = c
                .doi
                .as_ref()
                .map_or("https://gridpin.dev".to_string(), |d| {
                    format!("https://doi.org/{d}")
                });
            format!(
                "GridPin. ({}). {} (Version {}) [Data set]. {}. {}.\n",
                c.year,
                c.title,
                version(c),
                link,
                note(c)
            )
        }
        "cff" => {
            let mut out = format!(
                "cff-version: 1.2.0\nmessage: \"If you geocoded with this sheet, please cite it as below.\"\ntype: dataset\ntitle: {}\nauthors:\n  - name: GridPin\nversion: {}\nurl: \"https://gridpin.dev\"\n",
                yaml(&c.title),
                yaml(&version(c))
            );
            if let Some(doi) = &c.doi {
                out += &format!("doi: {}\n", yaml(doi));
            }
            out += &format!(
                "identifiers:\n  - type: other\n    value: {}\n    description: \"SHA-256 of the sheet file\"\nabstract: {}\n",
                yaml(&format!("sha256:{}", c.sha256)),
                yaml(&note(c))
            );
            out
        }
        other => bail!("--format {other:?}: one of {}", FORMATS.join(", ")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(doi: Option<&str>, release: Option<&str>) -> Citation {
        Citation {
            title: "GridPin address sheet: Serbia".into(),
            key: "gridpin_rs_v0_8_0".into(),
            release: release.map(String::from),
            sha256: "ab".repeat(32),
            data_release: "2026-06-17.0".into(),
            license: "data.gov.rs Terms of use".into(),
            sources: "Overture Maps & {RGZ}".into(),
            engine: "0.9.0".into(),
            doi: doi.map(String::from),
            year: "2026".into(),
        }
    }

    #[test]
    fn each_format_names_the_hash_the_release_and_the_engine() {
        for f in FORMATS {
            let text = render(&c(None, Some("v0.8.0")), f).unwrap();
            assert!(
                text.contains(&"ab".repeat(32))
                    && text.contains("v0.8.0")
                    && text.contains("0.9.0"),
                "{f}: {text}"
            );
            assert!(!text.contains("doi"), "{f}: no DOI is invented");
        }
        let bib = render(&c(Some("10.5281/zenodo.1"), Some("v0.8.0")), "bibtex").unwrap();
        assert!(
            bib.contains("doi = {10.5281/zenodo.1}") && bib.contains("Overture Maps \\& \\{RGZ\\}"),
            "{bib}"
        );
        assert!(render(&c(Some("10.5281/zenodo.1"), None), "apa")
            .unwrap()
            .contains("https://doi.org/10.5281/zenodo.1"));
        assert!(render(&c(None, None), "cff")
            .unwrap()
            .contains("unreleased build sha256:abababababab"));
        assert!(render(&c(None, None), "ris").is_err());
    }
}
