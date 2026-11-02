//! CSV and TSV for `gridpin batch`: the input file comes back with its own columns plus `gp_*` answer columns.
//!
//! JSONL stays the default and is handled in `main.rs` unchanged; this module only takes over when the input
//! is a `.csv` / `.tsv` / `.tab` file or `--format csv|tsv` is given. Rows are read in windows (the same
//! bound on memory as the JSONL path) and written in input order; a row without an address keeps its place
//! with empty answer columns, so N rows in always gives N rows out (one per candidate with `-k` > 1).

use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

use crate::query::{hit_is_weak, Hit};

/// Answer columns appended to every row; the same names `gp_geocode_df` adds in the R package.
pub const GP_COLUMNS: [&str; 10] = [
    "gp_lat",
    "gp_lon",
    "gp_precision",
    "gp_confidence",
    "gp_weak",
    "gp_street",
    "gp_housenumber",
    "gp_postcode",
    "gp_commune",
    "gp_region",
];

/// Header names taken as the address column when `--column` is not given (case-insensitive).
const ADDRESS_NAMES: [&str; 3] = ["address", "query", "q"];

/// Rows per window, as on the JSONL path.
const WINDOW: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jsonl,
    Csv,
    Tsv,
}

impl Format {
    /// `--format` value, or the input extension: `.csv` -> CSV, `.tsv`/`.tab` -> TSV, anything else -> JSONL
    /// (every input that worked before this module keeps working as JSONL).
    pub fn of_input(explicit: Option<&str>, path: &Path) -> Result<Format> {
        match explicit {
            Some(name) => Format::parse(name),
            None => Ok(
                match path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref()
                {
                    Some("csv") => Format::Csv,
                    Some("tsv") | Some("tab") => Format::Tsv,
                    _ => Format::Jsonl,
                },
            ),
        }
    }

    pub fn parse(name: &str) -> Result<Format> {
        match name.to_ascii_lowercase().as_str() {
            "jsonl" => Ok(Format::Jsonl),
            "csv" => Ok(Format::Csv),
            "tsv" => Ok(Format::Tsv),
            other => bail!("unknown format {other:?}: use jsonl, csv or tsv"),
        }
    }

    fn default_delimiter(self) -> u8 {
        if self == Format::Tsv {
            b'\t'
        } else {
            b','
        }
    }
}

/// Everything the table path needs from the command line.
pub struct Options<'a> {
    pub input_format: Format,
    pub output_format: Format,
    pub columns: Option<&'a str>,
    pub delimiter: Option<&'a str>,
    pub k: usize,
}

/// What the run did, for the closing line on stderr.
pub struct Stats {
    pub rows: u64,
    pub without_address: u64,
}

/// `--delimiter`: one ASCII character, or `tab` / `\t`.
pub fn parse_delimiter(raw: &str) -> Result<u8> {
    match raw {
        "tab" | "\\t" | "\t" => Ok(b'\t'),
        s if s.len() == 1 && s.is_ascii() => Ok(s.as_bytes()[0]),
        s => bail!("--delimiter must be one ASCII character or `tab`, got {s:?}"),
    }
}

/// Which header columns make the query. With `--column`, every listed name (comma-separated) must be a
/// header, compared without regard to case but otherwise exactly; without it, exactly one header must be
/// one of `address`, `query`, `q`.
pub fn query_columns(headers: &csv::StringRecord, columns: Option<&str>) -> Result<Vec<usize>> {
    let names: Vec<&str> = headers.iter().collect();
    let find = |want: &str| -> Result<usize> {
        let hits: Vec<usize> = names
            .iter()
            .enumerate()
            .filter(|(_, h)| h.eq_ignore_ascii_case(want))
            .map(|(i, _)| i)
            .collect();
        match hits.as_slice() {
            [one] => Ok(*one),
            [] => Err(anyhow!(
                "no column {want:?}; the file has: {}",
                list(&names)
            )),
            _ => Err(anyhow!(
                "column {want:?} appears more than once in the header"
            )),
        }
    };
    match columns {
        Some(spec) => {
            let wanted: Vec<&str> = spec
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if wanted.is_empty() {
                bail!("--column is empty; the file has: {}", list(&names));
            }
            wanted.into_iter().map(find).collect()
        }
        None => {
            let found: Vec<usize> = names
                .iter()
                .enumerate()
                .filter(|(_, h)| ADDRESS_NAMES.iter().any(|a| h.eq_ignore_ascii_case(a)))
                .map(|(i, _)| i)
                .collect();
            match found.as_slice() {
                [one] => Ok(vec![*one]),
                [] => bail!(
                    "no address column: name it with --column (several columns are joined with \", \"); \
                     the file has: {}",
                    list(&names)
                ),
                _ => bail!(
                    "several address-like columns ({}): choose one with --column",
                    found.iter().map(|&i| names[i]).collect::<Vec<_>>().join(", ")
                ),
            }
        }
    }
}

fn list(names: &[&str]) -> String {
    names
        .iter()
        .map(|n| format!("{n:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The query of one row: the chosen cells, trimmed, empty ones skipped, joined with ", ".
pub fn query_text(record: &csv::StringRecord, cols: &[usize]) -> String {
    cols.iter()
        .filter_map(|&i| record.get(i))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The answer columns of one hit, in the order of `GP_COLUMNS`.
pub fn hit_cells(h: &Hit) -> [String; 10] {
    [
        h.lat.to_string(),
        h.lon.to_string(),
        h.precision.to_string(),
        h.confidence.to_string(),
        hit_is_weak(h).to_string(),
        h.street.clone(),
        h.housenumber.clone().unwrap_or_default(),
        h.postcode.clone(),
        h.commune.clone(),
        h.region.clone().unwrap_or_default(),
    ]
}

/// The CSV reader drops a UTF-8 byte-order mark itself; anything that is not UTF-8 stops with a hint.
fn utf8_hint(err: csv::Error, input: &Path) -> anyhow::Error {
    if matches!(err.kind(), csv::ErrorKind::Utf8 { .. }) {
        anyhow!(
            "{} is not UTF-8 ({err}); save it as \"CSV UTF-8\" (Excel: File > Save As) or convert it, \
             e.g. iconv -f WINDOWS-1252 -t UTF-8",
            input.display()
        )
    } else {
        anyhow::Error::new(err).context(format!("cannot read {}", input.display()))
    }
}

/// Read the table, answer each row with `geocode`, write the table with `gp_*` columns to `out`.
/// `geocode` runs on the rayon pool the caller installs; rows keep their input order.
pub fn run<W, G>(
    input: &Path,
    out: W,
    opts: &Options,
    pool: &rayon::ThreadPool,
    geocode: G,
) -> Result<Stats>
where
    W: Write,
    G: Fn(&str) -> Vec<Hit> + Sync,
{
    use rayon::prelude::*;
    let in_delim = match opts.delimiter {
        Some(raw) => parse_delimiter(raw)?,
        None => opts.input_format.default_delimiter(),
    };
    let file =
        std::fs::File::open(input).with_context(|| format!("cannot open {}", input.display()))?;
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(in_delim)
        .has_headers(true)
        .flexible(false)
        .from_reader(file);
    let headers = reader.headers().map_err(|e| utf8_hint(e, input))?.clone();
    if let Some(taken) = headers
        .iter()
        .find(|h| GP_COLUMNS.iter().any(|g| h.eq_ignore_ascii_case(g)))
    {
        bail!("the input already has a {taken:?} column (output of an earlier run?); rename or drop it");
    }
    let cols = query_columns(&headers, opts.columns)?;
    let ranked = opts.k > 1;

    let mut writer = csv::WriterBuilder::new()
        .delimiter(opts.output_format.default_delimiter())
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(out);
    let mut head: Vec<&str> = headers.iter().collect();
    head.extend(GP_COLUMNS);
    if ranked {
        head.push("gp_rank");
    }
    writer.write_record(&head)?;

    let width = headers.len();
    let empty: Vec<String> = vec![String::new(); GP_COLUMNS.len() + usize::from(ranked)];
    let mut stats = Stats {
        rows: 0,
        without_address: 0,
    };
    let mut records = reader.into_records();
    loop {
        let mut window: Vec<(csv::StringRecord, String)> = Vec::with_capacity(WINDOW.min(4096));
        for record in records.by_ref() {
            let record = record.map_err(|e| utf8_hint(e, input))?;
            let q = query_text(&record, &cols);
            window.push((record, q));
            if window.len() == WINDOW {
                break;
            }
        }
        if window.is_empty() {
            break;
        }
        stats.rows += window.len() as u64;
        stats.without_address += window.iter().filter(|(_, q)| q.is_empty()).count() as u64;
        let answers: Vec<Vec<Hit>> = pool.install(|| {
            window
                .par_iter()
                .map(|(_, q)| if q.is_empty() { Vec::new() } else { geocode(q) })
                .collect()
        });
        for ((record, _), hits) in window.iter().zip(answers) {
            debug_assert_eq!(record.len(), width);
            if hits.is_empty() {
                writer.write_record(record.iter().chain(empty.iter().map(String::as_str)))?;
                continue;
            }
            for (rank, hit) in hits.iter().take(opts.k).enumerate() {
                let cells = hit_cells(hit);
                let rank_cell = (rank + 1).to_string();
                let tail = cells
                    .iter()
                    .map(String::as_str)
                    .chain(ranked.then_some(rank_cell.as_str()));
                writer.write_record(record.iter().chain(tail))?;
            }
        }
    }
    writer.flush()?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(names: &[&str]) -> csv::StringRecord {
        csv::StringRecord::from(names.to_vec())
    }

    #[test]
    fn input_format_follows_the_extension_and_unknown_ones_stay_jsonl() {
        assert_eq!(
            Format::of_input(None, Path::new("a.CSV")).unwrap(),
            Format::Csv
        );
        assert_eq!(
            Format::of_input(None, Path::new("a.tab")).unwrap(),
            Format::Tsv
        );
        assert_eq!(
            Format::of_input(None, Path::new("a.txt")).unwrap(),
            Format::Jsonl
        );
        assert_eq!(
            Format::of_input(None, Path::new("queries")).unwrap(),
            Format::Jsonl
        );
        assert_eq!(
            Format::of_input(Some("tsv"), Path::new("a.csv")).unwrap(),
            Format::Tsv
        );
        assert!(Format::of_input(Some("xlsx"), Path::new("a.csv")).is_err());
    }

    #[test]
    fn the_address_column_is_found_by_name_without_case_but_not_with_spaces() {
        assert_eq!(
            query_columns(&header(&["id", "ADDRESS"]), None).unwrap(),
            vec![1]
        );
        let err = query_columns(&header(&["id", "Address "]), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("\"Address \""), "{err}");
        assert!(query_columns(&header(&["address", "q"]), None).is_err());
        assert_eq!(
            query_columns(&header(&["Street", "City", "zip"]), Some("street, city")).unwrap(),
            vec![0, 1]
        );
        assert!(query_columns(&header(&["street"]), Some("street,town")).is_err());
        assert!(query_columns(&header(&["street", "STREET"]), Some("street")).is_err());
    }

    #[test]
    fn a_query_joins_the_non_empty_cells_with_comma_space() {
        let rec = csv::StringRecord::from(vec![" 10 Main St ", "", "Springfield"]);
        assert_eq!(query_text(&rec, &[0, 1, 2]), "10 Main St, Springfield");
        assert_eq!(query_text(&rec, &[1]), "");
    }

    #[test]
    fn delimiters_are_one_ascii_character_or_tab() {
        assert_eq!(parse_delimiter(";").unwrap(), b';');
        assert_eq!(parse_delimiter("tab").unwrap(), b'\t');
        assert!(parse_delimiter(";;").is_err());
        assert!(parse_delimiter("§").is_err());
    }
}
