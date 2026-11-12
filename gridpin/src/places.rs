//! `gridpin build --places places.csv own.bin`: a sheet of one's own places from a friendly table.
//!
//! Input columns: `name, lat, lon` and, optionally, `group` (a campus, a site; it plays the town), `code` (a
//! building code, answered like the name) and `aliases` (other names, separated by `|`). Each place becomes a
//! street-level record of the ordinary builder: the name is the street, the group is the commune, there is no
//! house number and no postcode. The sheet is a POI layer (`layer = poi`, `country = own`): it opens as the
//! second sheet of a query (`--also`, before the country's sheet) or as `--poi`. No free sheet is touched.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::norm::normalize;

/// One row of the friendly table.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub line: usize,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub group: String,
    pub code: String,
    pub aliases: Vec<String>,
}

/// Read the friendly table; every problem names its line.
pub fn read(input: &Path) -> Result<Vec<Place>> {
    let delimiter = if input
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("tsv"))
    {
        b'\t'
    } else {
        b','
    };
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(input)
        .with_context(|| format!("cannot open {}", input.display()))?;
    let headers = rdr.headers()?.clone();
    let col = |name: &str| {
        headers
            .iter()
            .position(|h| h.trim().eq_ignore_ascii_case(name))
    };
    let (Some(c_name), Some(c_lat), Some(c_lon)) = (col("name"), col("lat"), col("lon")) else {
        bail!("--places: the table needs columns name, lat, lon (optional: group, code, aliases); it has {headers:?}");
    };
    let (c_group, c_code, c_aliases) = (col("group"), col("code"), col("aliases"));
    let mut out = Vec::new();
    for (i, rec) in rdr.records().enumerate() {
        let line = i + 2;
        let rec = rec.with_context(|| format!("--places: line {line}"))?;
        let cell = |c: Option<usize>| c.and_then(|c| rec.get(c)).unwrap_or("").trim().to_string();
        let name = cell(Some(c_name));
        if name.is_empty() {
            bail!("--places: line {line} has no name");
        }
        let number = |c: usize, what: &str, max: f64| -> Result<f64> {
            let raw = rec.get(c).unwrap_or("").trim();
            match raw.parse::<f64>() {
                Ok(v) if v.is_finite() && v.abs() <= max => Ok(v),
                _ => bail!("--places: line {line}: {what} {raw:?} is not a number within ±{max}"),
            }
        };
        let (lat, lon) = (number(c_lat, "lat", 90.0)?, number(c_lon, "lon", 180.0)?);
        let aliases = cell(c_aliases)
            .split('|')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(String::from)
            .collect();
        out.push(Place {
            line,
            name,
            lat,
            lon,
            group: cell(c_group),
            code: cell(c_code),
            aliases,
        });
    }
    if out.is_empty() {
        bail!("--places: the table has no places");
    }
    Ok(out)
}

/// The builder's rows for `places`: (nom_voie_norm, code_insee, nom_commune_norm, nom_voie, nom_commune, lat, lon),
/// sorted as the builder needs. A name, code or alias that two places of one group share is an error: the sheet
/// would answer the middle of the two.
pub fn rows(places: &[Place], title: &str) -> Result<Vec<[String; 7]>> {
    rows_keeping(places, title, false).map(|(r, _)| r)
}

/// `rows`, and with `first_wins` a name already taken in its group is left out instead of refused (for tables made
/// from an outside source, where homonyms of one group are common); returns the rows and how many names were left out.
pub fn rows_keeping(
    places: &[Place],
    title: &str,
    first_wins: bool,
) -> Result<(Vec<[String; 7]>, usize)> {
    let mut dropped = 0;
    let mut groups: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen: BTreeMap<(String, usize), usize> = BTreeMap::new();
    let mut rows = Vec::new();
    for p in places {
        let group = if p.group.is_empty() {
            title
        } else {
            p.group.as_str()
        };
        let next = groups.len() + 1;
        let gid = *groups.entry(group.to_string()).or_insert(next);
        let mut keys: Vec<&str> = vec![p.name.as_str()];
        if !p.code.is_empty() {
            keys.push(p.code.as_str());
        }
        keys.extend(p.aliases.iter().map(String::as_str));
        let mut own: Vec<String> = Vec::new();
        for k in keys {
            let norm = normalize(k);
            if norm.is_empty() || own.contains(&norm) {
                continue;
            }
            if let Some(first) = seen.get(&(norm.clone(), gid)).copied() {
                if first_wins {
                    dropped += 1;
                    continue;
                }
                bail!(
                    "--places: {k:?} on line {} is also a name on line {first} in the same group; give one of them \
                     another name or group",
                    p.line
                );
            }
            seen.insert((norm.clone(), gid), p.line);
            own.push(norm.clone());
            rows.push([
                norm,
                format!("{gid:08}"),
                normalize(group),
                p.name.clone(),
                group.to_string(),
                format!("{:.7}", p.lat),
                format!("{:.7}", p.lon),
            ]);
        }
    }
    // the order of the street key the builder writes: name, separator, group code
    let key =
        |r: &[String; 7]| [r[0].as_bytes(), &[crate::index::KEY_SEP], r[1].as_bytes()].concat();
    rows.sort_by_key(key);
    Ok((rows, dropped))
}

/// Build `out` from the friendly table `input`; returns the number of places.
pub fn build(input: &Path, out: &Path, license: Option<&str>) -> Result<usize> {
    build_keeping(input, out, license, false).map(|(n, _)| n)
}

/// `build` with `first_wins` (see `rows_keeping`); returns the places and the names left out.
/// `a` and `b` name one existing file: the same path after links, or two hard links of one inode.
fn same_file(a: &Path, b: &Path) -> bool {
    if let (Ok(x), Ok(y)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        if x == y {
            return true;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return x.dev() == y.dev() && x.ino() == y.ino();
        }
    }
    false
}

pub fn build_keeping(
    input: &Path,
    out: &Path,
    license: Option<&str>,
    first_wins: bool,
) -> Result<(usize, usize)> {
    // the table is the user's only copy: a sheet written over it (or a helper file of the build) would destroy it
    for (path, what) in [
        (out.to_path_buf(), "the output"),
        (
            out.with_extension("places-input.csv"),
            "the build's helper file",
        ),
        (
            out.with_extension("places-meta.json"),
            "the build's helper file",
        ),
    ] {
        if same_file(input, &path) {
            bail!(
                "--places: {} is {what} too: writing it would destroy the table; give the sheet another name",
                input.display()
            );
        }
    }
    let places = read(input)?;
    let title = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("places")
        .to_string();
    let (rows, dropped) = rows_keeping(&places, &title, first_wins)?;
    let csv_path = out.with_extension("places-input.csv");
    let meta_path = out.with_extension("places-meta.json");
    let result = (|| -> Result<()> {
        let mut w = csv::Writer::from_path(&csv_path)?;
        w.write_record([
            "nom_voie_norm",
            "code_insee",
            "nom_commune_norm",
            "code_postal",
            "numero",
            "rep",
            "lon",
            "lat",
            "nom_voie",
            "nom_commune",
        ])?;
        for r in &rows {
            w.write_record([&r[0], &r[1], &r[2], "0", "", "", &r[6], &r[5], &r[3], &r[4]])?;
        }
        w.flush()?;
        let meta = serde_json::json!({
            "country": "own",
            "layer": "poi",
            "license": license.unwrap_or("the owner's own data"),
            "source_release": format!("{} ({} places)", input.file_name().and_then(|s| s.to_str()).unwrap_or(""), places.len()),
            "places_title": title,
        });
        std::fs::File::create(&meta_path)?.write_all(meta.to_string().as_bytes())?;
        crate::builder::build(&csv_path, out, None, None, None, None, Some(&meta_path))
    })();
    // the two files are this build's own scaffolding; they go whatever happened
    let _ = std::fs::remove_file(&csv_path);
    let _ = std::fs::remove_file(&meta_path);
    result?;
    Ok((places.len(), dropped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(line: usize, name: &str, group: &str, code: &str, aliases: &[&str]) -> Place {
        Place {
            line,
            name: name.into(),
            lat: 1.0,
            lon: 2.0,
            group: group.into(),
            code: code.into(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
        }
    }

    #[test]
    fn a_code_and_aliases_are_more_names_of_the_same_point() {
        let r = rows(
            &[place(2, "Main Library", "", "B12", &["Old Library"])],
            "campus",
        )
        .unwrap();
        let keys: Vec<&str> = r.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(
            keys,
            ["b12", "main library", "old library"],
            "sorted by the key"
        );
        assert!(
            r.iter().all(|r| r[3] == "Main Library" && r[4] == "campus"),
            "the answer is the name"
        );
    }

    #[test]
    fn one_name_twice_in_a_group_is_an_error_but_not_across_groups() {
        let err = rows(
            &[
                place(2, "Library", "North", "", &[]),
                place(3, "library", "North", "", &[]),
            ],
            "t",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("line 3") && err.to_string().contains("line 2"),
            "{err}"
        );
        assert!(rows(
            &[
                place(2, "Library", "North", "", &[]),
                place(3, "Library", "South", "", &[])
            ],
            "t"
        )
        .is_ok());
        let alias = rows(
            &[
                place(2, "Hall", "", "", &["Library"]),
                place(3, "Library", "", "", &[]),
            ],
            "t",
        );
        assert!(alias.is_err(), "an alias collides with a name too");
    }
}
