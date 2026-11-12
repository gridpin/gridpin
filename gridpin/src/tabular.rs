//! CSV and TSV for `gridpin batch`: the input file comes back with its own columns plus `gp_*` answer columns.
//!
//! JSONL stays the default and is handled in `main.rs` unchanged; this module only takes over when the input
//! is a `.csv` / `.tsv` / `.tab` file or `--format csv|tsv` is given. Rows are read in windows (the same
//! bound on memory as the JSONL path) and written in input order; a row without an address keeps its place
//! with empty answer columns, so N rows in always gives N rows out (one per candidate with `-k` > 1).

use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

use crate::aliases::{self, Aliases};
use crate::query::{hit_is_weak, match_level, Hit};

/// Answer columns appended to every row; the same names `gp_geocode_df` adds in the R package.
pub const GP_COLUMNS: [&str; 12] = [
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
    "gp_match_level",
    "gp_match_status",
];

/// `gp_match_status` of a row without an answer; every other answer column stays empty there.
const UNMATCHED: &str = "U";

/// Header names taken as the address column when `--column` is not given (case-insensitive).
const ADDRESS_NAMES: [&str; 3] = ["address", "query", "q"];

/// Headers taken as coordinates (case-insensitive), here and in `gridpin nearest` and `join`.
/// The lines `gridpin list`, `export`, `sample` and `stats` write above the header of a CSV: the licence travels with
/// the addresses. A table read back skips exactly these at its top and nothing else.
pub const LICENCE_LINES: [&str; 2] = ["# license: ", "# attribution: "];

/// Open a table for reading, past the licence lines GridPin writes above the header, so the output of `sample`,
/// `list` or `export` is the input of `batch`, `nearest`, `join` and `bench` as it is.
pub fn open_table(path: &Path) -> Result<std::io::BufReader<std::fs::File>> {
    use std::io::BufRead;
    let file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    loop {
        let head = reader.fill_buf()?;
        if !LICENCE_LINES.iter().any(|p| head.starts_with(p.as_bytes())) {
            return Ok(reader);
        }
        reader.read_until(b'\n', &mut Vec::new())?;
    }
}

pub const LAT_NAMES: [&str; 2] = ["lat", "latitude"];
pub const LON_NAMES: [&str; 3] = ["lon", "lng", "longitude"];

/// The first header with one of these names.
pub fn coordinate_column(headers: &csv::StringRecord, names: &[&str]) -> Option<usize> {
    headers
        .iter()
        .position(|h| names.iter().any(|n| h.trim().eq_ignore_ascii_case(n)))
}

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
    /// `--columns street=Street,number=No,...`: the cells go to the engine as fields, not as one line
    pub fields: Option<&'a str>,
    /// `--aliases`: names read before the sheet; adds the column `gp_alias_name`
    pub aliases: Option<&'a Aliases>,
    /// `--reverse`: the coordinate columns; adds `gp_distance_m`
    pub reverse: Option<(Option<&'a str>, Option<&'a str>)>,
    pub delimiter: Option<&'a str>,
    pub k: usize,
    /// `--with`: extra answer fields as `gp_*` columns after the others (before `gp_rank`)
    pub extras: Option<(&'a crate::extras::Extras, &'a crate::query::Index)>,
    /// `--aggregate`: counts by cell instead of rows, and the bar under which cells merge (`--min-count`)
    pub aggregate: Option<(&'a crate::publish::Cells, u64)>,
    /// `--mask`: every point moved; adds `gp_masked_m` (and `gp_mask_k` with k), blanks street and number
    pub mask: Option<(&'a crate::publish::Mask, &'a crate::query::Index)>,
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

/// Which header column holds each field of `--columns`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldColumns {
    pub street: usize,
    pub number: Option<usize>,
    pub city: Option<usize>,
    pub region: Option<usize>,
    pub postcode: Option<usize>,
}

/// What one row asks the engine.
#[derive(Clone, Debug, PartialEq)]
pub enum RowQuery {
    /// the address as one line (empty: the row has no address)
    Line(String),
    /// the address as fields, nothing left to guess about their boundaries
    Fields {
        street: String,
        number: Option<String>,
        city: String,
        region: Option<String>,
        postcode: Option<String>,
    },
    /// a point to read the address of
    Point(f64, f64),
}

impl RowQuery {
    fn is_empty(&self) -> bool {
        matches!(self, RowQuery::Line(q) if q.is_empty())
    }
}

/// `--columns street=Street,number=No,city=Town,region=State,postcode=ZIP`. A bare field name means a column of that
/// name. `street` is required; every other field may be left out. Columns are found as in `--column`.
pub fn field_columns(headers: &csv::StringRecord, spec: &str) -> Result<FieldColumns> {
    let (mut street, mut number, mut city, mut region, mut postcode) =
        (None, None, None, None, None);
    for pair in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (field, column) = pair.split_once('=').unwrap_or((pair, pair));
        let at = *query_columns(headers, Some(column.trim()))?
            .first()
            .ok_or_else(|| anyhow!("--columns: {pair:?} names no column"))?;
        let slot = match field.trim().to_ascii_lowercase().as_str() {
            "street" => &mut street,
            "number" => &mut number,
            "city" => &mut city,
            "region" => &mut region,
            "postcode" => &mut postcode,
            other => bail!(
                "--columns: unknown field {other:?}; use street, number, city, region, postcode"
            ),
        };
        if slot.replace(at).is_some() {
            bail!("--columns: field {:?} is given twice", field.trim());
        }
    }
    Ok(FieldColumns {
        street: street.ok_or_else(|| anyhow!("--columns needs street=<column>"))?,
        number,
        city,
        region,
        postcode,
    })
}

fn cell(record: &csv::StringRecord, at: Option<usize>) -> Option<String> {
    at.and_then(|i| record.get(i))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// The query of one row under `--reverse`: a cell that is no coordinate leaves the row without a query.
pub fn point_query(record: &csv::StringRecord, lat_at: usize, lon_at: usize) -> RowQuery {
    let number = |at: usize| record.get(at)?.trim().parse::<f64>().ok();
    match (number(lat_at), number(lon_at)) {
        (Some(lat), Some(lon)) if lat.abs() <= 90.0 && lon.abs() <= 180.0 => {
            RowQuery::Point(lat, lon)
        }
        _ => RowQuery::Line(String::new()),
    }
}

/// The query of one row under `--columns`. A row whose street cell is empty still has whatever the other
/// cells say ("Springfield", "62704"), read as one line.
pub fn field_query(record: &csv::StringRecord, cols: &FieldColumns) -> RowQuery {
    match cell(record, Some(cols.street)) {
        Some(street) => RowQuery::Fields {
            street,
            number: cell(record, cols.number),
            city: cell(record, cols.city).unwrap_or_default(),
            region: cell(record, cols.region),
            postcode: cell(record, cols.postcode),
        },
        None => RowQuery::Line(
            [cols.number, cols.postcode, cols.city, cols.region]
                .into_iter()
                .filter_map(|at| cell(record, at))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
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

/// A TabSeparated field as ClickHouse writes it: backslash, tab and newline escaped.
pub fn tsv_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// The text of a TabSeparated field (`\\`, `\t`, `\n`, `\r`; any other escaped character stands for itself).
pub fn tsv_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// The answer columns of one hit, in the order of `GP_COLUMNS`.
pub fn hit_cells(h: &Hit) -> [String; 12] {
    let (level, status) = match_level(h);
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
        level.to_string(),
        status.to_string(),
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
    G: Fn(&RowQuery) -> Vec<Hit> + Sync,
{
    use rayon::prelude::*;
    let in_delim = match opts.delimiter {
        Some(raw) => parse_delimiter(raw)?,
        None => opts.input_format.default_delimiter(),
    };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(in_delim)
        .has_headers(true)
        .flexible(false)
        .from_reader(open_table(input)?);
    let headers = reader.headers().map_err(|e| utf8_hint(e, input))?.clone();
    let mut extra: Vec<&str> = Vec::new();
    let mut extra_taken: Vec<&str> = Vec::new();
    if opts.reverse.is_some() {
        extra.push("gp_distance_m");
    }
    if opts.aliases.is_some() {
        extra.push("gp_alias_name");
    }
    if let Some((e, _)) = opts.extras {
        extra.extend(e.with.columns());
    }
    if opts.aggregate.is_some() || opts.mask.is_some() {
        let what = if opts.aggregate.is_some() {
            "--aggregate"
        } else {
            "--mask"
        };
        if opts.aggregate.is_some() && opts.mask.is_some() {
            bail!("--aggregate and --mask together: an aggregate has no points left to move");
        }
        if opts.extras.is_some() || opts.reverse.is_some() || opts.k > 1 {
            bail!("{what} works on one answer per row of addresses: drop --with, --reverse and -k");
        }
    }
    if let Some((m, _)) = opts.mask {
        extra.push("gp_masked_m");
        if m.k.is_some() {
            extra.push("gp_mask_k");
        }
    }
    if opts.k > 1 {
        extra_taken.push("gp_rank");
    }
    if let Some(taken) = headers.iter().find(|h| {
        GP_COLUMNS
            .iter()
            .chain(extra.iter())
            .chain(extra_taken.iter())
            .any(|g| h.eq_ignore_ascii_case(g))
    }) {
        bail!("the input already has a {taken:?} column (output of an earlier run?); rename or drop it");
    }
    // exactly one reading of the row: fields under --columns, one joined line otherwise
    enum Reading {
        Line(Vec<usize>),
        Fields(FieldColumns),
        Point(usize, usize),
    }
    let reading = match (opts.reverse, opts.fields) {
        (Some((lat, lon)), _) => {
            let found = |name: Option<&str>, names: &[&str], what: &str| -> Result<usize> {
                match name {
                    Some(name) => Ok(*query_columns(&headers, Some(name))?
                        .first()
                        .ok_or_else(|| anyhow!("--reverse names no column"))?),
                    None => coordinate_column(&headers, names)
                        .ok_or_else(|| anyhow!("no {what} column: name it with --{what}-column")),
                }
            };
            Reading::Point(
                found(lat, &LAT_NAMES, "lat")?,
                found(lon, &LON_NAMES, "lon")?,
            )
        }
        (None, Some(spec)) => Reading::Fields(field_columns(&headers, spec)?),
        (None, None) => Reading::Line(query_columns(&headers, opts.columns)?),
    };
    let ranked = opts.k > 1;

    let mut writer = csv::WriterBuilder::new()
        .delimiter(opts.output_format.default_delimiter())
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(out);
    let mut head: Vec<&str> = headers.iter().collect();
    head.extend(GP_COLUMNS);
    head.extend(extra.iter().copied());
    if ranked {
        head.push("gp_rank");
    }
    if opts.aggregate.is_none() {
        writer.write_record(&head)?;
    }
    let mut aggregate = crate::publish::Aggregate::default();

    let width = headers.len();
    let mut empty: Vec<String> =
        vec![String::new(); GP_COLUMNS.len() + extra.len() + usize::from(ranked)];
    empty[GP_COLUMNS.len() - 1] = UNMATCHED.to_string();
    let mut stats = Stats {
        rows: 0,
        without_address: 0,
    };
    let mut records = reader.into_records();
    loop {
        let mut window: Vec<(csv::StringRecord, RowQuery)> = Vec::with_capacity(WINDOW.min(4096));
        for record in records.by_ref() {
            let record = record.map_err(|e| utf8_hint(e, input))?;
            let q = match &reading {
                Reading::Line(cols) => RowQuery::Line(query_text(&record, cols)),
                Reading::Fields(cols) => field_query(&record, cols),
                Reading::Point(lat, lon) => point_query(&record, *lat, *lon),
            };
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
        let answers: Vec<(Vec<Hit>, Option<String>)> = pool.install(|| {
            window
                .par_iter()
                .map(|(_, q)| match q {
                    q if q.is_empty() => (Vec::new(), None),
                    RowQuery::Line(line) => aliases::answer(opts.aliases, line, |text| {
                        geocode(&RowQuery::Line(text.to_string()))
                    }),
                    q => (geocode(q), None),
                })
                .collect()
        });
        let first_row = stats.rows - window.len() as u64;
        if let Some((cells, _)) = opts.aggregate {
            // counted and dropped: no point of this window is written anywhere
            for (hits, _) in &answers {
                aggregate.add(cells, hits.first());
            }
            continue;
        }
        // under --mask each answer moves; the move and the k count go to their own columns
        let mut moves: Vec<Option<(f64, Option<usize>)>> = vec![None; answers.len()];
        let answers: Vec<(Vec<Hit>, Option<String>)> = match opts.mask {
            None => answers,
            Some((m, idx)) => answers
                .into_iter()
                .enumerate()
                .map(|(i, (mut hits, alias))| {
                    if let Some(h) = hits.first_mut() {
                        let (lat, lon, moved, within) =
                            m.apply(idx, first_row + i as u64, h.lat, h.lon);
                        (h.lat, h.lon) = (lat, lon);
                        h.street.clear();
                        h.housenumber = None;
                        moves[i] = Some((moved, within));
                    }
                    hits.truncate(1);
                    (hits, alias)
                })
                .collect(),
        };
        for (row, ((record, asked), (hits, alias))) in window.iter().zip(answers).enumerate() {
            let asked_text = match asked {
                RowQuery::Line(text) => Some(text.clone()),
                RowQuery::Fields {
                    street,
                    number,
                    city,
                    region,
                    postcode,
                } => Some(
                    [
                        Some(street.as_str()),
                        number.as_deref(),
                        Some(city.as_str()),
                        region.as_deref(),
                        postcode.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(", "),
                ),
                RowQuery::Point(..) => None,
            };
            debug_assert_eq!(record.len(), width);
            if hits.is_empty() {
                let iso = opts
                    .extras
                    .zip(asked_text.as_deref())
                    .and_then(|((e, idx), q)| {
                        let at = head.iter().position(|c| *c == "gp_iso_mode")? - width;
                        Some((at, e.iso_cells_without_answer(q, idx)?))
                    });
                match iso {
                    Some((at, cells)) => {
                        let mut row = empty.clone();
                        row[at..at + cells.len()].clone_from_slice(&cells);
                        writer.write_record(record.iter().chain(row.iter().map(String::as_str)))?;
                    }
                    None => writer
                        .write_record(record.iter().chain(empty.iter().map(String::as_str)))?,
                }
                continue;
            }
            for (rank, hit) in hits.iter().take(opts.k).enumerate() {
                let cells = hit_cells(hit);
                let rank_cell = (rank + 1).to_string();
                let distance = hit.distance_m.map(|d| d.to_string()).unwrap_or_default();
                let masked = opts.mask.map(|(m, _)| {
                    let (moved, within) = moves[row].unwrap_or((0.0, None));
                    let mut c = vec![format!("{moved:.0}")];
                    if m.k.is_some() {
                        c.push(within.map(|n| n.to_string()).unwrap_or_default());
                    }
                    c
                });
                let extra_cells = opts
                    .extras
                    .map(|(e, idx)| match &asked_text {
                        Some(q) => crate::extras::for_query(q, || e.cells(hit, idx)),
                        None => e.cells(hit, idx),
                    })
                    .unwrap_or_default();
                let tail = cells
                    .iter()
                    .map(String::as_str)
                    .chain(opts.reverse.is_some().then_some(distance.as_str()))
                    .chain(
                        opts.aliases
                            .is_some()
                            .then_some(alias.as_deref().unwrap_or("")),
                    )
                    .chain(extra_cells.iter().map(String::as_str))
                    .chain(masked.iter().flatten().map(String::as_str))
                    .chain(ranked.then_some(rank_cell.as_str()));
                writer.write_record(record.iter().chain(tail))?;
            }
        }
    }
    writer.flush()?;
    if let Some((_, min_count)) = opts.aggregate {
        let mut out = writer.into_inner().map_err(|e| anyhow!("{e}"))?;
        aggregate.write(min_count, opts.output_format.default_delimiter(), &mut out)?;
        out.flush()?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(names: &[&str]) -> csv::StringRecord {
        csv::StringRecord::from(names.to_vec())
    }

    /// Only the two licence lines GridPin writes are skipped, and only at the top: a header that starts with "#",
    /// another comment and a licence line below the header stay in the table.
    #[test]
    fn a_table_opens_past_the_licence_lines_and_nothing_else() {
        use std::io::Read;
        let dir = std::env::temp_dir().join(format!("gp-open-table-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let read = |name: &str, text: &str| {
            let path = dir.join(name);
            std::fs::write(&path, text).unwrap();
            let mut out = String::new();
            open_table(&path).unwrap().read_to_string(&mut out).unwrap();
            out
        };
        let body = "address\n# license: in a cell\n";
        assert_eq!(
            read(
                "both.csv",
                &format!("# license: CC BY 4.0\n# attribution: X\n{body}")
            ),
            body
        );
        assert_eq!(read("one.csv", &format!("# attribution: X\n{body}")), body);
        for kept in [
            "#id,address\n1,a\n",
            "# a note\naddress\n",
            "#license: x\naddress\n",
            body,
        ] {
            assert_eq!(read("kept.csv", kept), kept);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_tab_separated_field_comes_back_as_it_went() {
        for s in [
            "plain",
            "a\tb",
            "line\nbreak",
            "back\\slash",
            "{\"q\": \"x\\\"y\"}",
            "cr\r",
        ] {
            assert_eq!(tsv_unescape(&tsv_escape(s)), s, "{s:?}");
            assert!(!tsv_escape(s).contains(['\t', '\n', '\r']), "{s:?}");
        }
        assert_eq!(tsv_unescape("a\\'b\\0"), "a'b\0");
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
    fn field_columns_map_fields_to_headers_and_refuse_what_they_cannot_read() {
        let head = header(&["Id", "Street", "No", "Town", "ZIP"]);
        assert_eq!(
            field_columns(&head, "street=Street, number=no,city=TOWN,postcode=ZIP").unwrap(),
            FieldColumns {
                street: 1,
                number: Some(2),
                city: Some(3),
                region: None,
                postcode: Some(4)
            }
        );
        assert_eq!(
            field_columns(&head, "street").unwrap(),
            FieldColumns {
                street: 1,
                number: None,
                city: None,
                region: None,
                postcode: None
            }
        );
        for (spec, says) in [
            ("city=Town", "needs street"),
            ("street=Street,zip=ZIP", "unknown field"),
            ("street=Street,street=Town", "twice"),
            ("street=Nope", "no column"),
        ] {
            let err = field_columns(&head, spec).unwrap_err().to_string();
            assert!(err.contains(says), "{spec}: {err}");
        }
    }

    #[test]
    fn a_row_goes_to_the_engine_as_its_fields_with_empty_cells_left_out() {
        let cols = FieldColumns {
            street: 0,
            number: Some(1),
            city: Some(2),
            region: None,
            postcode: Some(3),
        };
        let rec = csv::StringRecord::from(vec![" Rue A ", " ", "Ville", "10000"]);
        assert_eq!(
            field_query(&rec, &cols),
            RowQuery::Fields {
                street: "Rue A".into(),
                number: None,
                city: "Ville".into(),
                region: None,
                postcode: Some("10000".into())
            }
        );
        let no_street = csv::StringRecord::from(vec!["", "", "Ville", "10000"]);
        assert_eq!(
            field_query(&no_street, &cols),
            RowQuery::Line("10000, Ville".into())
        );
        let blank = csv::StringRecord::from(vec!["", "", "", ""]);
        assert!(field_query(&blank, &cols).is_empty());
    }

    #[test]
    fn a_point_is_read_only_from_cells_that_are_coordinates() {
        let rec = csv::StringRecord::from(vec![" 43.7 ", "7.4", "x", "91", "181"]);
        assert_eq!(point_query(&rec, 0, 1), RowQuery::Point(43.7, 7.4));
        for (lat, lon) in [(2, 1), (0, 2), (3, 1), (0, 4), (0, 9)] {
            assert!(point_query(&rec, lat, lon).is_empty(), "{lat} {lon}");
        }
    }

    #[test]
    fn delimiters_are_one_ascii_character_or_tab() {
        assert_eq!(parse_delimiter(";").unwrap(), b';');
        assert_eq!(parse_delimiter("tab").unwrap(), b'\t');
        assert!(parse_delimiter(";;").is_err());
        assert!(parse_delimiter("§").is_err());
    }
}
