//! A table joined by a code (plan T1.1, `gridpin attach`): statistics a publisher gives by census tract, statistical
//! area or grid cell, attached to rows that carry that code (a layer column of `gridpin-pro batch --append`) or a point
//! the code is computed from (`h3:<n>`, `geohash:<n>`, `cell:<name>`).
//!
//! A table comes with a passport (`<name>.table.json` beside the file): the key column, its kind and the year of the
//! boundaries the codes belong to (`vintage`), the columns to take and the values the publisher uses for "suppressed",
//! the edition, publisher, licence and a caveat. Three things are refused rather than warned about, because each gives
//! an answer that looks right and is not:
//! 1. codes of one year of boundaries joined to a table of another (`--key-vintage`, or the layer's passport);
//! 2. a share of codes the table does not hold above `max_absent_share` when the table says it is complete — the
//!    content check of the same mistake when nobody named the year (`--allow-absent` turns it into a warning);
//! 3. a value of a small area given to a point that does not fix the area: a town centre gets no tract (the key basis).
//!
//! A value of a table is a value of the area the address lies in, not of the household or the person there.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

/// How fine an area is: a tract, a block, a small statistical area or a grid cell is `fine`, a region, a county or a
/// NUTS area is `coarse`. A fine area needs a point that fixes it (`Basis`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grain {
    Fine,
    Coarse,
}

impl Grain {
    pub fn parse(s: &str) -> Result<Grain> {
        match s {
            "fine" => Ok(Grain::Fine),
            "coarse" => Ok(Grain::Coarse),
            other => bail!("grain {other:?}: \"fine\" or \"coarse\""),
        }
    }
}

/// A layer of the registry `layers.json`: the old form is a file name, the new one a passport object
/// `{"file", "kind", "vintage", "grain", "source", "licence", "built", "simplify_m"}`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerPassport {
    pub file: String,
    pub kind: Option<String>,
    /// the year the boundaries were drawn (not the year of the file); `None` when unknown
    pub vintage: Option<String>,
    pub grain: Option<Grain>,
    pub source: Option<String>,
    pub licence: Option<String>,
    /// layers of one family are editions of the same boundaries over time (plan T2.2: congressional districts per
    /// Congress); `--append regions:<family> --as-of` takes the one whose days hold the date
    pub family: Option<String>,
    /// the first day the boundaries apply (YYYY-MM-DD)
    pub valid_from: Option<String>,
    /// the first day they no longer apply (YYYY-MM-DD); `None`: still in force
    pub valid_to: Option<String>,
}

impl LayerPassport {
    /// Do the boundaries apply on `day` (YYYY-MM-DD)? A layer without days applies on none.
    pub fn applies_on(&self, day: &str) -> bool {
        self.valid_from.as_deref().is_some_and(|f| f <= day)
            && self.valid_to.as_deref().is_none_or(|t| day < t)
    }
}

/// Is `s` a day written YYYY-MM-DD (the form string comparison orders)?
pub fn is_day(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn year_text(v: Option<&Value>, what: &str) -> Result<Option<String>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.trim().to_string())),
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(other) => bail!("{what}: a year as text or null, not {other}"),
    }
}

fn day(v: Option<&Value>, name: &str, what: &str) -> Result<Option<String>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if is_day(s) => Ok(Some(s.clone())),
        Some(other) => {
            bail!("layers.json: the layer {name:?} {what}: a day as YYYY-MM-DD, not {other}")
        }
    }
}

/// Read one entry of `regions` in `layers.json`; `None` when the layer is not named there.
pub fn layer_entry(name: &str, v: &Value) -> Result<Option<LayerPassport>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(LayerPassport {
            file: s.clone(),
            ..LayerPassport::default()
        })),
        Value::Object(o) => {
            let file = o.get("file").and_then(Value::as_str).ok_or_else(|| {
                anyhow!("layers.json: the layer {name:?} is an object without a \"file\"")
            })?;
            let text = |k: &str| o.get(k).and_then(Value::as_str).map(String::from);
            let grain = text("grain")
                .map(|g| Grain::parse(&g))
                .transpose()
                .with_context(|| format!("layers.json: the layer {name:?}"))?;
            Ok(Some(LayerPassport {
                file: file.to_string(),
                kind: text("kind"),
                vintage: year_text(o.get("vintage"), &format!("layers.json: {name:?} vintage"))?,
                grain,
                source: text("source"),
                licence: text("licence"),
                family: text("family"),
                valid_from: day(o.get("valid_from"), name, "valid_from")?,
                valid_to: day(o.get("valid_to"), name, "valid_to")?,
            }))
        }
        _ => bail!("layers.json: the layer {name:?} is neither a file name nor an object"),
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeySpec {
    /// the code column of the table file
    pub column: String,
    /// what the code is: `us_tract`, `au_sa2`, … or a key computed from a point: `h3:<n>`, `geohash:<n>`, `cell:<name>`
    pub kind: String,
    /// the year of the boundaries the codes belong to; `null` for a computed key or a code without boundaries
    #[serde(default)]
    pub vintage: Option<String>,
    /// codes of only digits shorter than this are padded with zeros on the left (a spreadsheet eats them)
    #[serde(default)]
    pub width: Option<usize>,
    /// `fine` (the default) or `coarse`: what point may carry the key (`Basis`)
    #[serde(default)]
    pub grain: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Take {
    #[serde(rename = "as")]
    pub name: String,
    #[serde(default)]
    pub unit: Option<String>,
    /// the column of the value's margin of error
    #[serde(default)]
    pub moe: Option<String>,
    /// the publisher's marks of a value that is not given (suppressed, not available)
    #[serde(default)]
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Valid {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub from_column: Option<String>,
    #[serde(default)]
    pub to_column: Option<String>,
}

impl Valid {
    fn any(&self) -> bool {
        self.from.is_some() || self.to.is_some() || self.by_rows()
    }
    fn by_rows(&self) -> bool {
        self.from_column.is_some() || self.to_column.is_some()
    }
}

/// The passport of a table, `<name>.table.json`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TablePassport {
    pub table: String,
    /// the table file, relative to the passport: CSV, TSV, optionally `.gz`
    pub file: String,
    /// the output columns are `gp_<prefix>_<as>`
    pub prefix: String,
    pub key: KeySpec,
    /// source column → what to call it; written in the order of the source column names
    pub take: BTreeMap<String, Take>,
    #[serde(default)]
    pub valid: Option<Valid>,
    /// a field separator other than the one of the file's extension (Zensus files use `;`)
    #[serde(default)]
    pub delimiter: Option<String>,
    pub edition: String,
    pub publisher: String,
    #[serde(default)]
    pub source_url: Option<String>,
    /// the day the publisher's terms were read
    #[serde(default)]
    pub read: Option<String>,
    pub licence: String,
    #[serde(default)]
    pub attribution: Option<String>,
    /// `complete` when every code of its kind is in the table, `partial` otherwise
    #[serde(default)]
    pub coverage: Option<String>,
    #[serde(default)]
    pub max_absent_share: Option<f64>,
    pub caveat: String,
}

/// The share of codes absent from a complete table above which `attach` stops, unless the passport names its own.
pub const DEFAULT_MAX_ABSENT_SHARE: f64 = 0.02;

/// A key computed from a point, not read from a column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Computed {
    H3(u8),
    Geohash(usize),
    Cell(&'static str),
}

impl Computed {
    pub fn parse(kind: &str) -> Result<Option<Computed>> {
        let Some((name, arg)) = kind.split_once(':') else {
            return Ok(None);
        };
        match name {
            "h3" => match arg.parse::<u8>() {
                Ok(n) if n <= 15 => Ok(Some(Computed::H3(n))),
                _ => bail!("key kind {kind:?}: h3:<0..15>"),
            },
            "geohash" => match arg.parse::<usize>() {
                Ok(n) if (1..=12).contains(&n) => Ok(Some(Computed::Geohash(n))),
                _ => bail!("key kind {kind:?}: geohash:<1..12>"),
            },
            "cell" => crate::cells::KINDS
                .iter()
                .find(|(n, _)| *n == arg)
                .map(|(n, _)| Some(Computed::Cell(n)))
                .ok_or_else(|| {
                    let names: Vec<&str> = crate::cells::KINDS.iter().map(|k| k.0).collect();
                    anyhow!("key kind {kind:?}: the cells are {}", names.join(", "))
                }),
            _ => Ok(None),
        }
    }

    pub fn code(&self, lat: f64, lon: f64) -> Option<String> {
        match self {
            Computed::H3(r) => {
                let res = h3o::Resolution::try_from(*r).ok()?;
                Some(h3o::LatLng::new(lat, lon).ok()?.to_cell(res).to_string())
            }
            Computed::Geohash(n) => Some(crate::codes::geohash_encode(lat, lon, *n)),
            Computed::Cell(name) => crate::cells::code(name, lat, lon),
        }
    }

    /// A grid cell is a small area; H3 from level 7 (about 5 km²) and a geohash from 6 characters (about 1 km) too.
    pub fn grain(&self) -> Grain {
        match self {
            Computed::H3(r) if *r < 7 => Grain::Coarse,
            Computed::Geohash(n) if *n < 6 => Grain::Coarse,
            _ => Grain::Fine,
        }
    }
}

/// What the point of a row says about the area it names (plan T1.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    /// a house point: the stored point, a point on a range of numbers, between known neighbours
    Point,
    /// a point on the street: its centre, a neighbour's house, a crossing
    Approximate,
    /// coordinates the input gave, not an answer of the engine
    Given,
    /// an answer's point (`gp_lat`, `gp_lon`) without its precision or method: what it fixes is not known
    Unknown,
    /// the centre of a town, a postcode or a place: it does not fix a small area
    TooCoarse,
    /// the engine called the answer weak
    Weak,
    /// no point at all
    None,
}

impl Basis {
    /// The `point_method` of an answer (`extras::point_method`) as a basis.
    pub fn of_method(method: &str) -> Basis {
        match method {
            "registry_point" | "range_interpolated" | "between_neighbours" => Basis::Point,
            "street_centre" | "nearest_house" | "crossing_of_streets" => Basis::Approximate,
            "" => Basis::None,
            _ => Basis::TooCoarse,
        }
    }

    /// The method a bare `gp_precision` implies when the row has no `gp_point_method` (a range answer is a house too).
    pub fn of_precision(precision: &str) -> Basis {
        Basis::of_method(match precision {
            "house" | "approximate" => "registry_point",
            "interp" => "between_neighbours",
            "near" => "nearest_house",
            "street" => "street_centre",
            "intersection" => "crossing_of_streets",
            "" => "",
            _ => "commune_centre",
        })
    }

    /// May a key of this grain be read from such a point.
    pub fn carries(self, grain: Grain) -> bool {
        match self {
            Basis::Point | Basis::Approximate | Basis::Given => true,
            Basis::TooCoarse | Basis::Unknown => grain == Grain::Coarse,
            Basis::Weak | Basis::None => false,
        }
    }

    /// The cell of `gp_key_basis`.
    pub fn cell(self) -> &'static str {
        match self {
            Basis::Point => "point",
            Basis::Approximate => "approximate",
            Basis::Given => "given",
            Basis::TooCoarse => "point_too_coarse",
            Basis::Weak => "weak_answer",
            Basis::Unknown => "basis_unknown",
            Basis::None => "",
        }
    }
}

/// The status of a row against a table.
pub const STATUSES: [&str; 5] = [
    "matched",
    "key_absent",
    "no_key",
    "suppressed",
    "out_of_dates",
];

/// The column every run adds once per row.
pub const BASIS_COLUMN: &str = "gp_key_basis";

struct Entry {
    from: Option<String>,
    to: Option<String>,
    /// per taken column: the value, then the margin when the column has one
    cells: Vec<String>,
}

/// A loaded table: only its key, the taken columns and the dates are kept.
pub struct Table {
    pub passport: TablePassport,
    pub path: PathBuf,
    computed: Option<Computed>,
    grain: Grain,
    rows: HashMap<String, Vec<Entry>>,
    /// codes of the table file padded with zeros
    pub padded: usize,
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// A code as the table and the rows are compared: trimmed, and a code of only digits shorter than `width` padded
/// with zeros on the left. The flag says whether it was padded.
pub fn norm_key(raw: &str, width: Option<usize>) -> (String, bool) {
    let t = raw.trim();
    match width {
        Some(w) if t.len() < w && !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) => {
            (format!("{t:0>w$}"), true)
        }
        _ => (t.to_string(), false),
    }
}

/// The code of a layer cell: a layer whose names are `code|label` (`SA2_CODE21 || '|' || SA2_NAME21`) gives the
/// part before the first `|`. A point on the border of two areas is named by both (`a;b`): it fixes neither, and
/// has no code.
pub fn layer_code(cell: &str) -> &str {
    if cell.contains(';') {
        return "";
    }
    cell.split('|').next().unwrap_or("")
}

fn open_text(path: &Path) -> Result<Box<dyn Read>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    Ok(if path.extension().is_some_and(|e| e == "gz") {
        Box::new(flate2::read::MultiGzDecoder::new(file))
    } else {
        Box::new(file)
    })
}

fn column_at(headers: &csv::StringRecord, name: &str, what: &str, path: &Path) -> Result<usize> {
    headers
        .iter()
        .position(|h| h.trim() == name)
        .ok_or_else(|| {
            let have: Vec<&str> = headers.iter().collect();
            anyhow!(
                "{}: no column {name:?} ({what}); the columns are: {}",
                path.display(),
                have.join(", ")
            )
        })
}

impl Table {
    /// Read a passport and its table.
    pub fn open(passport: &Path) -> Result<Table> {
        let text = std::fs::read_to_string(passport)
            .with_context(|| format!("cannot read {}", passport.display()))?;
        let p: TablePassport = serde_json::from_str(&text)
            .with_context(|| format!("{}: not a table passport", passport.display()))?;
        let dir = passport.parent().unwrap_or(Path::new("."));
        let path = dir.join(&p.file);
        Table::load(p, path)
    }

    pub fn load(p: TablePassport, path: PathBuf) -> Result<Table> {
        let computed = Computed::parse(&p.key.kind)?;
        if computed.is_some() && p.key.vintage.is_some() {
            bail!(
                "{}: the key {:?} is computed from the point and has no year of boundaries; set \"vintage\": null",
                p.table,
                p.key.kind
            );
        }
        let grain = match (&computed, &p.key.grain) {
            (Some(c), None) => c.grain(),
            (_, Some(g)) => Grain::parse(g).with_context(|| format!("{}: key grain", p.table))?,
            (None, None) => Grain::Fine,
        };
        match p.coverage.as_deref() {
            None | Some("complete") | Some("partial") => {}
            Some(other) => bail!(
                "{}: coverage {other:?}: \"complete\" or \"partial\"",
                p.table
            ),
        }
        if p.take.is_empty() {
            bail!("{}: \"take\" names no column", p.table);
        }
        let valid = p.valid.clone().unwrap_or_default();
        for d in [&valid.from, &valid.to].into_iter().flatten() {
            if !is_date(d) {
                bail!("{}: the date {d:?} is not YYYY-MM-DD", p.table);
            }
        }
        let name = path.to_string_lossy();
        let delimiter = match p.delimiter.as_deref() {
            Some(d) if d.len() == 1 => d.as_bytes()[0],
            Some("\\t") => b'\t',
            Some(d) => bail!("{}: delimiter {d:?} is one character", p.table),
            None if name.ends_with(".tsv") || name.ends_with(".tsv.gz") => b'\t',
            None => b',',
        };
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .from_reader(open_text(&path)?);
        let headers = reader.headers()?.clone();
        let key_at = column_at(&headers, &p.key.column, "the key", &path)?;
        let mut take_at = Vec::new();
        for (col, t) in &p.take {
            take_at.push(column_at(&headers, col, "in \"take\"", &path)?);
            if let Some(m) = &t.moe {
                take_at.push(column_at(&headers, m, "a margin of error", &path)?);
            }
        }
        let from_at = valid
            .from_column
            .as_deref()
            .map(|c| column_at(&headers, c, "the start of validity", &path))
            .transpose()?;
        let to_at = valid
            .to_column
            .as_deref()
            .map(|c| column_at(&headers, c, "the end of validity", &path))
            .transpose()?;
        let mut rows: HashMap<String, Vec<Entry>> = HashMap::new();
        let mut padded = 0;
        for (line, rec) in reader.records().enumerate() {
            let rec = rec.with_context(|| format!("{} row {}", path.display(), line + 2))?;
            let (key, pad) = norm_key(rec.get(key_at).unwrap_or(""), p.key.width);
            if key.is_empty() {
                continue;
            }
            padded += usize::from(pad);
            let date = |at: Option<usize>| -> Result<Option<String>> {
                match at.map(|i| rec.get(i).unwrap_or("").trim()) {
                    None | Some("") => Ok(None),
                    Some(d) if is_date(d) => Ok(Some(d.to_string())),
                    Some(d) => bail!(
                        "{} row {}: the date {d:?} is not YYYY-MM-DD",
                        path.display(),
                        line + 2
                    ),
                }
            };
            let entry = Entry {
                from: date(from_at)?,
                to: date(to_at)?,
                cells: take_at
                    .iter()
                    .map(|&i| rec.get(i).unwrap_or("").trim().to_string())
                    .collect(),
            };
            rows.entry(key).or_default().push(entry);
        }
        if rows.is_empty() {
            bail!("{}: the table has no rows", path.display());
        }
        let mut repeated: Vec<&String> = Vec::new();
        for (k, v) in &rows {
            if v.len() < 2 {
                continue;
            }
            if !valid.by_rows() {
                repeated.push(k);
                continue;
            }
            let mut spans: Vec<(&str, &str)> = v
                .iter()
                .map(|e| {
                    (
                        e.from.as_deref().unwrap_or(""),
                        e.to.as_deref().unwrap_or("9999-99-99"),
                    )
                })
                .collect();
            spans.sort();
            if spans.windows(2).any(|w| w[1].0 <= w[0].1) {
                bail!(
                    "{}: the key {k:?} has rows whose dates overlap",
                    path.display()
                );
            }
        }
        if !repeated.is_empty() {
            repeated.sort();
            let first: Vec<&str> = repeated.iter().take(5).map(|s| s.as_str()).collect();
            bail!(
                "{}: {} keys repeat in a table without dates, e.g. {}",
                path.display(),
                repeated.len(),
                first.join(", ")
            );
        }
        Ok(Table {
            passport: p,
            path,
            computed,
            grain,
            rows,
            padded,
        })
    }

    pub fn computed(&self) -> Option<&Computed> {
        self.computed.as_ref()
    }

    pub fn grain(&self) -> Grain {
        self.grain
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The output columns: the taken values (and margins), the status and the edition.
    pub fn columns(&self) -> Vec<String> {
        let p = &self.passport;
        let mut c = Vec::new();
        for t in p.take.values() {
            c.push(format!("gp_{}_{}", p.prefix, t.name));
            if t.moe.is_some() {
                c.push(format!("gp_{}_{}_moe", p.prefix, t.name));
            }
        }
        c.push(format!("gp_{}_status", p.prefix));
        c.push(format!("gp_{}_edition", p.prefix));
        c
    }

    /// The edition and the year of the boundaries, one cell.
    pub fn edition(&self) -> String {
        let p = &self.passport;
        match &p.key.vintage {
            Some(v) => format!("{}; {} boundaries {v}", p.edition, p.key.kind),
            None => format!("{}; key {}", p.edition, p.key.kind),
        }
    }

    /// Does the year of the codes a caller has fit the table (check 1 and 2 of the module).
    pub fn check_vintage(
        &self,
        kind: Option<&str>,
        vintage: Option<&str>,
        whose: &str,
    ) -> Result<()> {
        let p = &self.passport;
        if self.computed.is_some() {
            return Ok(());
        }
        if let Some(k) = kind {
            if k != p.key.kind {
                bail!(
                    "codes of kind {k:?} ({whose}) against the table {} ({}), keyed by {:?}: refused",
                    p.table,
                    self.path.display(),
                    p.key.kind
                );
            }
        }
        match (&p.key.vintage, vintage) {
            (None, _) => Ok(()),
            (Some(t), Some(v)) if t == v => Ok(()),
            (Some(t), Some(v)) => bail!(
                "codes of the {v} boundaries ({whose}) against the table {} ({}), keyed by the {t} boundaries: \
                 a code of one year names another area in the other, refused",
                p.table,
                self.path.display()
            ),
            (Some(t), None) => bail!(
                "the table {} ({}) is keyed by codes of the {t} boundaries, and no year is given for the codes of \
                 {whose}: give --key-vintage {t} if they are of {t} (a code of another year names another area)",
                p.table,
                self.path.display()
            ),
        }
    }

    /// The cells of one row and its status. `key` is the row's code (already read or computed), `basis` what the
    /// point of the row says, `as_of` the day the dates are read at.
    pub fn cells(
        &self,
        key: Option<&str>,
        basis: Basis,
        as_of: Option<&str>,
    ) -> (Vec<String>, &'static str) {
        let p = &self.passport;
        let width: usize = p
            .take
            .values()
            .map(|t| 1 + usize::from(t.moe.is_some()))
            .sum();
        let empty = |status: &'static str| {
            let mut c = vec![String::new(); width];
            c.push(status.to_string());
            c.push(self.edition());
            (c, status)
        };
        let key = match key.map(|k| norm_key(k, p.key.width).0) {
            Some(k) if !k.is_empty() && basis.carries(self.grain) => k,
            _ => return empty("no_key"),
        };
        let Some(entries) = self.rows.get(&key) else {
            return empty("key_absent");
        };
        let within = |from: Option<&str>, to: Option<&str>| match as_of {
            None => true,
            Some(d) => from.is_none_or(|f| f <= d) && to.is_none_or(|t| d <= t),
        };
        let valid = p.valid.clone().unwrap_or_default();
        if !within(valid.from.as_deref(), valid.to.as_deref()) {
            return empty("out_of_dates");
        }
        let Some(entry) = entries
            .iter()
            .find(|e| within(e.from.as_deref(), e.to.as_deref()))
        else {
            return empty("out_of_dates");
        };
        let mut cells = Vec::with_capacity(width + 2);
        let (mut given, mut hidden) = (0, 0);
        let mut at = 0;
        for t in p.take.values() {
            let parts = 1 + usize::from(t.moe.is_some());
            for (i, raw) in entry.cells[at..at + parts].iter().enumerate() {
                if t.missing.iter().any(|m| m == raw) || raw.is_empty() {
                    cells.push(String::new());
                    if i == 0 {
                        hidden += 1;
                    }
                } else {
                    cells.push(raw.clone());
                    if i == 0 {
                        given += 1;
                    }
                }
            }
            at += parts;
        }
        let status = if given == 0 && hidden > 0 {
            "suppressed"
        } else {
            "matched"
        };
        cells.push(status.to_string());
        cells.push(self.edition());
        (cells, status)
    }
}

/// What a run tells besides the rows: counts by status and by basis, padded codes, the first absent codes.
#[derive(Debug, Default)]
pub struct Summary {
    pub rows: usize,
    pub status: BTreeMap<&'static str, usize>,
    pub basis: BTreeMap<&'static str, usize>,
    pub padded_rows: usize,
    pub absent: Vec<String>,
}

impl Summary {
    pub fn count(&mut self, status: &'static str, basis: Basis, key: Option<&str>) {
        self.rows += 1;
        *self.status.entry(status).or_default() += 1;
        *self.basis.entry(basis.cell()).or_default() += 1;
        if status == "key_absent" && self.absent.len() < 10 {
            if let Some(k) = key {
                if !self.absent.iter().any(|a| a == k) {
                    self.absent.push(k.to_string());
                }
            }
        }
    }

    /// The share of rows with a code the table does not hold, among the rows that looked a code up.
    pub fn absent_share(&self) -> f64 {
        let looked = self.rows - self.status.get("no_key").copied().unwrap_or(0);
        if looked == 0 {
            return 0.0;
        }
        self.status.get("key_absent").copied().unwrap_or(0) as f64 / looked as f64
    }

    /// Check 3: a complete table that misses many codes was given codes of another year or kind.
    pub fn check_absent(&self, table: &Table, allow: bool) -> Result<()> {
        let p = &table.passport;
        if p.coverage.as_deref() != Some("complete") {
            return Ok(());
        }
        let limit = p.max_absent_share.unwrap_or(DEFAULT_MAX_ABSENT_SHARE);
        let share = self.absent_share();
        if share <= limit {
            return Ok(());
        }
        let msg = format!(
            "{:.1} % of the codes are not in the table {} ({}), which says it is complete (allowed {:.1} %): the codes \
             are probably of another year of boundaries or another kind than {:?}; the first absent: {}",
            share * 100.0,
            p.table,
            table.path.display(),
            limit * 100.0,
            p.key.kind,
            self.absent.join(", ")
        );
        if allow {
            eprintln!("warning: {msg}");
            Ok(())
        } else {
            bail!("{msg} (--allow-absent writes the rows anyway)")
        }
    }

    /// The lines written to standard error after a run.
    pub fn text(&self, table: &Table) -> String {
        let p = &table.passport;
        let counts: Vec<String> = STATUSES
            .iter()
            .map(|s| format!("{s} {}", self.status.get(s).copied().unwrap_or(0)))
            .collect();
        let basis: Vec<String> = self
            .basis
            .iter()
            .map(|(b, n)| format!("{} {n}", if b.is_empty() { "no point" } else { b }))
            .collect();
        let mut out = format!(
            "{}: {} rows: {}\nkey basis: {}\n",
            p.table,
            self.rows,
            counts.join(", "),
            basis.join(", ")
        );
        if self.basis.contains_key("basis_unknown") {
            out.push_str(
                "basis_unknown: the rows carry gp_lat and gp_lon without gp_precision or gp_point_method, so a small \
                 area is not attached to them; join in one pass (gridpin-pro batch --table) or keep gp_precision\n",
            );
        }
        if let Some(w) = p.key.width {
            if self.padded_rows + table.padded > 0 {
                out.push_str(&format!(
                    "{} keys of the rows and {} of the table padded to {w} digits\n",
                    self.padded_rows, table.padded
                ));
            }
        }
        out.push_str(&format!(
            "{}, {}; licence: {}\n",
            p.edition, p.publisher, p.licence
        ));
        if let Some(a) = &p.attribution {
            out.push_str(&format!("attribution: {a}\n"));
        }
        out.push_str(&format!("note: {}\n", p.caveat));
        out
    }

    pub fn json(&self, table: &Table) -> Value {
        let p = &table.passport;
        json!({
            "table": p.table,
            "edition": table.edition(),
            "publisher": p.publisher,
            "licence": p.licence,
            "attribution": p.attribution,
            "source_url": p.source_url,
            "read": p.read,
            "caveat": p.caveat,
            "rows": self.rows,
            "status": self.status,
            "key_basis": self.basis,
            "padded_rows": self.padded_rows,
            "padded_table": table.padded,
            "absent_share": self.absent_share(),
            "first_absent": self.absent,
        })
    }
}

/// How `attach` reads the rows.
#[derive(Debug, Default)]
pub struct Options<'a> {
    /// the code column of the rows (not needed for a computed key)
    pub on: Option<&'a str>,
    /// the year of the boundaries of the rows' codes
    pub key_vintage: Option<&'a str>,
    /// the day validity is read at; required by a table with dates
    pub as_of: Option<&'a str>,
    pub allow_absent: bool,
}

/// The column of the first header named like one of `names` (case-insensitive).
fn header_at(headers: &csv::StringRecord, names: &[&str]) -> Option<usize> {
    headers
        .iter()
        .position(|h| names.iter().any(|n| h.trim().eq_ignore_ascii_case(n)))
}

/// The basis of one row of a table: `gp_weak`, then `gp_point_method`, then `gp_precision`. Without any of them a
/// point of `lat`, `lon` is the input's own; a point of `gp_lat`, `gp_lon` is an answer whose precision is unknown.
pub struct BasisColumns {
    weak: Option<usize>,
    method: Option<usize>,
    precision: Option<usize>,
}

impl BasisColumns {
    pub fn of(headers: &csv::StringRecord) -> BasisColumns {
        BasisColumns {
            weak: header_at(headers, &["gp_weak"]),
            method: header_at(headers, &["gp_point_method"]),
            precision: header_at(headers, &["gp_precision"]),
        }
    }

    /// `answer`: the row's point is an answer's `gp_lat`, `gp_lon`; `given`: the row carries its own point or code.
    pub fn basis(&self, rec: &csv::StringRecord, answer: bool, given: bool) -> Basis {
        let get = |at: Option<usize>| at.map(|i| rec.get(i).unwrap_or("").trim());
        if get(self.weak).is_some_and(|w| w.eq_ignore_ascii_case("true")) {
            return Basis::Weak;
        }
        if let Some(m) = get(self.method).filter(|m| !m.is_empty()) {
            return Basis::of_method(m);
        }
        if let Some(p) = get(self.precision) {
            return Basis::of_precision(p);
        }
        if self.method.is_some() {
            return Basis::None;
        }
        if answer {
            Basis::Unknown
        } else if given {
            Basis::Given
        } else {
            Basis::None
        }
    }
}

/// `gridpin attach`: the rows of `input` with the columns of `table` and `gp_key_basis`, written to `out` only when
/// every check passes (nothing is written on a refusal).
pub fn attach(input: &Path, table: &Table, opts: &Options, out: &mut dyn Write) -> Result<Summary> {
    let p = &table.passport;
    if p.valid.as_ref().is_some_and(Valid::any) {
        match opts.as_of {
            Some(d) if is_date(d) => {}
            Some(d) => bail!("--as-of {d:?}: a day as YYYY-MM-DD"),
            None => bail!(
                "the table {} holds dates of validity: give --as-of YYYY-MM-DD, so the answer does not depend on the \
                 day it is run",
                p.table
            ),
        }
    }
    if table.computed().is_none() {
        table.check_vintage(None, opts.key_vintage, "the rows")?;
    }
    let delimiter = if crate::tabular::Format::of_input(None, input)? == crate::tabular::Format::Tsv
    {
        b'\t'
    } else {
        b','
    };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_reader(crate::tabular::open_table(input)?);
    let headers = reader.headers()?.clone();
    let added = table.columns();
    if let Some(taken) = headers.iter().find(|h| {
        added.iter().any(|a| h.eq_ignore_ascii_case(a)) || h.eq_ignore_ascii_case(BASIS_COLUMN)
    }) {
        bail!(
            "{}: the rows already have a {taken:?} column",
            input.display()
        );
    }
    let on_at = match (table.computed(), opts.on) {
        (Some(_), Some(_)) => bail!("--on: the key {:?} is computed from each row's point", p.key.kind),
        (Some(_), None) => None,
        (None, Some(c)) => Some(column_at(&headers, c, "--on", input)?),
        (None, None) => bail!(
            "--on: name the column of {:?} codes in {} (e.g. gp_tract from gridpin-pro batch --append regions:tract)",
            p.key.kind,
            input.display()
        ),
    };
    let answer_at = header_at(&headers, &["gp_lat"]).zip(header_at(&headers, &["gp_lon"]));
    let point_at = answer_at.or_else(|| {
        crate::tabular::coordinate_column(&headers, &crate::tabular::LAT_NAMES).zip(
            crate::tabular::coordinate_column(&headers, &crate::tabular::LON_NAMES),
        )
    });
    if table.computed().is_some() && point_at.is_none() {
        bail!(
            "{}: the key {:?} is computed from a point, and the rows have no gp_lat and gp_lon (or lat and lon) columns",
            input.display(),
            p.key.kind
        );
    }
    let basis_cols = BasisColumns::of(&headers);
    let mut summary = Summary::default();
    let mut lines: Vec<Vec<String>> = Vec::new();
    for rec in reader.records() {
        let rec = rec?;
        let point = point_at.and_then(|(la, lo)| {
            let lat: f64 = rec.get(la)?.trim().parse().ok()?;
            let lon: f64 = rec.get(lo)?.trim().parse().ok()?;
            (lat.abs() <= 90.0 && lon.abs() <= 180.0).then_some((lat, lon))
        });
        let answer = point.is_some() && answer_at.is_some();
        let basis = basis_cols.basis(&rec, answer, point.is_some() || on_at.is_some());
        let key: Option<String> = match (table.computed(), on_at) {
            (Some(c), _) => point.and_then(|(lat, lon)| c.code(lat, lon)),
            (None, Some(i)) => {
                let (k, pad) = norm_key(layer_code(rec.get(i).unwrap_or("")), p.key.width);
                summary.padded_rows += usize::from(pad);
                (!k.is_empty()).then_some(k)
            }
            (None, None) => None,
        };
        let (cells, status) = table.cells(key.as_deref(), basis, opts.as_of);
        summary.count(status, basis, key.as_deref());
        let mut line: Vec<String> = rec.iter().map(String::from).collect();
        line.extend(cells);
        // a row without a code says why only when its point is the reason
        let said = key.is_some() || !matches!(basis, Basis::Given | Basis::None);
        line.push(if said { basis.cell() } else { "" }.to_string());
        lines.push(line);
    }
    summary.check_absent(table, opts.allow_absent)?;
    let mut w = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(out);
    let mut head: Vec<String> = headers.iter().map(String::from).collect();
    head.extend(added);
    head.push(BASIS_COLUMN.to_string());
    w.write_record(&head)?;
    for l in &lines {
        w.write_record(l)?;
    }
    w.flush()?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_day_is_written_year_month_day_and_a_layer_reads_its_days() {
        assert!(is_day("2025-01-03"));
        for bad in [
            "2025-1-03",
            "2025/01/03",
            "20250103",
            "2025-01-033",
            "202a-01-03",
        ] {
            assert!(!is_day(bad), "{bad}");
        }
        let v: Value = serde_json::json!({"file": "cd119.bin", "family": "cd", "valid_from": "2025-01-03", "valid_to": "2027-01-03"});
        let p = layer_entry("cd119", &v).unwrap().unwrap();
        assert_eq!(p.family.as_deref(), Some("cd"));
        assert!(p.applies_on("2025-01-03") && p.applies_on("2027-01-02"));
        assert!(!p.applies_on("2027-01-03") && !p.applies_on("2025-01-02"));
        let bad: Value = serde_json::json!({"file": "x.bin", "valid_from": "3 January 2025"});
        assert!(layer_entry("x", &bad).is_err());
        assert!(
            !LayerPassport::default().applies_on("2025-01-03"),
            "a layer without days applies on none"
        );
    }

    use super::*;

    #[test]
    fn a_layer_entry_is_a_file_name_or_a_passport() {
        let old = layer_entry("nuts", &json!("de/de_nuts.bin"))
            .unwrap()
            .unwrap();
        assert_eq!(old.file, "de/de_nuts.bin");
        assert_eq!((old.kind, old.vintage, old.grain), (None, None, None));
        let new = layer_entry(
            "tract",
            &json!({"file": "us/us_tract.bin", "kind": "us_tract", "vintage": 2020, "grain": "fine"}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(new.vintage.as_deref(), Some("2020"));
        assert_eq!(new.grain, Some(Grain::Fine));
        assert!(layer_entry("x", &Value::Null).unwrap().is_none());
        let e = layer_entry("tract", &json!({"kind": "us_tract"}))
            .unwrap_err()
            .to_string();
        assert!(e.contains("\"tract\"") && e.contains("file"), "{e}");
        assert!(layer_entry("x", &json!({"file": "a", "grain": "small"})).is_err());
        assert!(layer_entry("x", &json!(7)).is_err());
    }

    #[test]
    fn only_a_code_of_digits_shorter_than_the_width_is_padded() {
        assert_eq!(
            norm_key("1001020100", Some(11)),
            ("01001020100".into(), true)
        );
        assert_eq!(
            norm_key(" 01001020100 ", Some(11)),
            ("01001020100".into(), false)
        );
        assert_eq!(
            norm_key("100102010A", Some(11)),
            ("100102010A".into(), false)
        );
        assert_eq!(norm_key("1001020100", None), ("1001020100".into(), false));
        assert_eq!(norm_key("", Some(11)), ("".into(), false));
    }

    #[test]
    fn every_point_method_has_its_basis() {
        let cases = [
            ("registry_point", Basis::Point),
            ("range_interpolated", Basis::Point),
            ("between_neighbours", Basis::Point),
            ("street_centre", Basis::Approximate),
            ("nearest_house", Basis::Approximate),
            ("crossing_of_streets", Basis::Approximate),
            ("place_table", Basis::TooCoarse),
            ("commune_centre", Basis::TooCoarse),
            ("postcode_centre", Basis::TooCoarse),
            ("postcode_layer", Basis::TooCoarse),
            ("place_layer", Basis::TooCoarse),
            ("own_place", Basis::TooCoarse),
        ];
        for (m, b) in cases {
            assert_eq!(Basis::of_method(m), b, "{m}");
        }
        for (p, b) in [
            ("house", Basis::Point),
            ("interp", Basis::Point),
            ("near", Basis::Approximate),
            ("street", Basis::Approximate),
            ("city", Basis::TooCoarse),
            ("postcode", Basis::TooCoarse),
            ("", Basis::None),
        ] {
            assert_eq!(Basis::of_precision(p), b, "{p}");
        }
        assert!(Basis::TooCoarse.carries(Grain::Coarse) && !Basis::TooCoarse.carries(Grain::Fine));
        assert!(!Basis::Weak.carries(Grain::Coarse));
        assert!(Basis::Approximate.carries(Grain::Fine) && Basis::Given.carries(Grain::Fine));
    }

    #[test]
    fn computed_keys_and_their_grain() {
        assert_eq!(Computed::parse("us_tract").unwrap(), None);
        assert_eq!(Computed::parse("h3:9").unwrap(), Some(Computed::H3(9)));
        assert_eq!(Computed::H3(6).grain(), Grain::Coarse);
        assert_eq!(Computed::H3(7).grain(), Grain::Fine);
        assert_eq!(Computed::Geohash(5).grain(), Grain::Coarse);
        assert_eq!(Computed::Geohash(6).grain(), Grain::Fine);
        assert!(Computed::parse("h3:16").is_err() && Computed::parse("cell:eu5km").is_err());
        assert_eq!(
            Computed::parse("cell:jp3")
                .unwrap()
                .unwrap()
                .code(35.681236, 139.767125)
                .unwrap(),
            "53394611"
        );
    }
}
