use std::io::{BufRead, BufWriter, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use gridpin::{builder, query};

fn parse_near(raw: &str) -> Result<(f64, f64), String> {
    let Some((lat_raw, lon_raw)) = raw.split_once(',') else {
        return Err("expected LAT,LON with exactly one comma".to_string());
    };
    if lat_raw.is_empty() || lon_raw.is_empty() || lon_raw.contains(',') {
        return Err("expected LAT,LON with exactly one comma".to_string());
    }
    let lat = lat_raw
        .parse::<f64>()
        .map_err(|_| "latitude must be a number".to_string())?;
    let lon = lon_raw
        .parse::<f64>()
        .map_err(|_| "longitude must be a number".to_string())?;
    query::validate_query_near(lat, lon)?;
    Ok((lat, lon))
}

/// gridpin — offline geocoder: country-in-a-file data sheets, forward and reverse.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

// A guard of the docs holds every visible command to a section there; a new command stays `hide = true`
// only until its section is written.
#[derive(Subcommand)]
enum Cmd {
    /// Show or verify the watermark of an index file
    #[cfg(feature = "watermark")]
    Mark {
        /// index file (.bin)
        index: PathBuf,
        /// claimed mark to verify
        #[arg(long)]
        verify: Option<String>,
    },
    /// Stamp a finished index file with a watermark (no rebuild)
    #[cfg(feature = "watermark")]
    #[command(hide = true)]
    Stamp {
        /// input index file (.bin)
        input: PathBuf,
        /// output index file (may equal the input to stamp in place)
        output: PathBuf,
        /// mark string to embed
        #[arg(long)]
        mark: String,
    },
    /// Build an index from a sorted CSV stream (prep/export_build.py)
    Build {
        input: PathBuf,
        out: PathBuf,
        /// optional parser model (ml/parser_v0.bin)
        #[arg(long)]
        model: Option<PathBuf>,
        /// optional ranking weights (ml/rank_v0.bin)
        #[arg(long)]
        rank: Option<PathBuf>,
        /// optional rules-in-data directory (TSV files)
        #[arg(long)]
        rules: Option<PathBuf>,
        /// optional watermark string to embed in the index (no-op without the watermark feature)
        // hidden from --help in public builds: advertising a do-nothing flag reads
        // as a broken promise; still accepted for script compat
        #[arg(long, hide = !cfg!(feature = "watermark"))]
        mark: Option<String>,
        /// build manifest (JSON): provenance + country/layer identity -> SEC_META (v6+).
        /// Distributed sheets MUST carry one; omitting it prints a loud warning.
        #[arg(long)]
        meta: Option<PathBuf>,
        /// the input is a table of one's own places (name, lat, lon[, group][, code][, aliases]), not the
        /// builder's CSV; the sheet answers as `--also` (before the country's sheet) or as `--poi`
        #[arg(long, conflicts_with_all = ["model", "rank", "rules", "meta"])]
        places: bool,
        /// with --places: the license line the sheet carries (default "the owner's own data")
        #[arg(long, requires = "places")]
        license: Option<String>,
        /// with --places: a name already taken in its group is left out (counted) instead of refused
        #[arg(long, requires = "places")]
        first_wins: bool,
    },
    /// Reverse geocoding: coordinates -> nearest indexed addresses (approximate; see precision/distance_m)
    // allow_negative_numbers: otherwise clap treats a western longitude like "-1.5536"
    // as a flag, making addresses west of 0° unreachable for reverse
    #[command(allow_negative_numbers = true)]
    Reverse {
        index: PathBuf,
        lat: f64,
        lon: f64,
        #[arg(short, default_value_t = 3)]
        k: usize,
        /// extra answer fields: h3[:res], geohash[:len], placekey, codes, street_point, links, provenance, place[:level],
        /// verdict, postal[:cc], crs:EPSG:<code>, exists, area, unit
        #[arg(long, value_name = "FIELDS")]
        with: Option<String>,
    },
    /// Single forward query
    Query {
        index: PathBuf,
        q: String,
        #[arg(short, default_value_t = 10)]
        k: usize,
        /// optional WGS84 focus; widens cityless homonyms and breaks equal-quality ties
        #[arg(
            long,
            value_name = "LAT,LON",
            value_parser = parse_near,
            allow_hyphen_values = true
        )]
        near: Option<(f64, f64)>,
        /// optional POI layer (second .bin): cascades when the address result is weak
        #[arg(long)]
        poi: Option<PathBuf>,
        /// a sheet of one's own places (`gridpin build --places`), asked before the address sheet
        #[arg(long)]
        also: Option<PathBuf>,
        /// a sheet of places asked only when the address sheet has no answer or only a weak town (the world
        /// sheet of towns: `prep/overture_world_places.py`)
        #[arg(long)]
        also_after: Option<PathBuf>,
        /// your own names for places: a CSV with the columns name,address, read before the sheet
        #[arg(long)]
        aliases: Option<PathBuf>,
        /// extra answer fields: h3[:res], geohash[:len], placekey, codes, street_point, links, provenance, place[:level],
        /// verdict, postal[:cc], crs:EPSG:<code>, exists, area, unit
        #[arg(long, value_name = "FIELDS")]
        with: Option<String>,
        /// keep only answers that match the query as written (no fuzzy street, interpolation or neighbour)
        #[arg(long, conflicts_with = "lenient")]
        strict: bool,
        /// let a street name of 9-11 letters be two edits away instead of one
        #[arg(long)]
        lenient: bool,
        /// your own abbreviations, written out before the query: a TSV of short form and full form
        #[arg(long, value_name = "TSV")]
        abbrev: Option<PathBuf>,
    },
    /// Batch mode: JSONL {"q": ...} in -> JSONL {"results": [...]} out, or a CSV/TSV file in -> the same
    /// file with gp_* answer columns out; order preserved
    Batch {
        index: PathBuf,
        input: PathBuf,
        output: PathBuf,
        /// candidates per query (default 10 for JSONL, 1 for CSV/TSV)
        #[arg(short)]
        k: Option<usize>,
        /// input format: jsonl, csv or tsv (default: from the extension; .csv, .tsv/.tab, anything else jsonl);
        /// a .geojson or .geojsonl file is read as GeoJSON features
        #[arg(long)]
        format: Option<String>,
        /// output format for a CSV/TSV input: csv or tsv (default: the input format)
        #[arg(long)]
        output_format: Option<String>,
        /// CSV/TSV: the address column, or several joined with ", " ("street,city,postcode");
        /// default: the one column named address, query or q
        #[arg(long)]
        column: Option<String>,
        /// CSV/TSV: the address already split into fields, sent to the engine as such:
        /// street=Street,number=No,city=Town,region=State,postcode=ZIP (street required, the rest optional)
        #[arg(long, conflicts_with = "column")]
        columns: Option<String>,
        /// CSV/TSV: each row is a point, answered by its nearest address; adds gp_distance_m
        #[arg(long, conflicts_with_all = ["column", "columns", "aliases"])]
        reverse: bool,
        /// with --reverse: the latitude column (default: lat or latitude)
        #[arg(long, requires = "reverse")]
        lat_column: Option<String>,
        /// with --reverse: the longitude column (default: lon, lng or longitude)
        #[arg(long, requires = "reverse")]
        lon_column: Option<String>,
        /// your own names for places: a CSV with the columns name,address, read before the sheet
        #[arg(long)]
        aliases: Option<PathBuf>,
        /// CSV/TSV: field delimiter of the input (one character or `tab`; default , for CSV and tab for TSV)
        #[arg(long)]
        delimiter: Option<String>,
        /// dump candidate features (rank training)
        #[arg(long, default_value_t = false)]
        dump: bool,
        /// observe address parsing and candidate stages without changing results
        #[arg(long, default_value_t = false, conflicts_with = "dump")]
        diagnose: bool,
        /// optional POI layer (second .bin): cascades when the address result is weak
        #[arg(long)]
        poi: Option<PathBuf>,
        /// a sheet of one's own places (`gridpin build --places`), asked before the address sheet
        #[arg(long)]
        also: Option<PathBuf>,
        /// a sheet of places asked only when the address sheet has no answer or only a weak town (the world
        /// sheet of towns: `prep/overture_world_places.py`)
        #[arg(long)]
        also_after: Option<PathBuf>,
        /// extra answer fields: h3[:res], geohash[:len], placekey, codes, street_point, links, provenance, place[:level],
        /// verdict, postal[:cc], crs:EPSG:<code>, exists, area, unit
        #[arg(long, value_name = "FIELDS")]
        with: Option<String>,
        /// keep only answers that match the query as written (no fuzzy street, interpolation or neighbour)
        #[arg(long, conflicts_with = "lenient")]
        strict: bool,
        /// let a street name of 9-11 letters be two edits away instead of one
        #[arg(long)]
        lenient: bool,
        /// your own abbreviations, written out before the query: a TSV of short form and full form
        #[arg(long, value_name = "TSV")]
        abbrev: Option<PathBuf>,
        /// write counts by cell instead of rows, no point anywhere: h3:<level>, geohash:<length> or zones.geojson
        #[arg(long, value_name = "CELLS", conflicts_with = "mask")]
        aggregate: Option<String>,
        /// with --aggregate: cells with fewer rows go into the line "other"
        #[arg(long, default_value_t = 5, requires = "aggregate")]
        min_count: u64,
        /// move every point at random in a ring, metres: donut:200-500, and optionally k:5 (at least 5 addresses
        /// of the sheet within the move); street and number are left out
        #[arg(long, value_name = "SPEC")]
        mask: Option<String>,
        /// with --mask: the seed of the draw, so a run can be repeated (default: from the clock, printed)
        #[arg(long, requires = "mask")]
        seed: Option<u64>,
        /// read one address per line on stdin and write one line per address on stdout, flushed at once: the
        /// top answer as JSON or an empty line (TabSeparated escapes); input and output are `-`. For a ClickHouse
        /// executable function or any pipe
        #[arg(long, conflicts_with_all = ["aggregate", "mask", "dump", "diagnose", "reverse", "columns"])]
        lines: bool,
        /// write OUTPUT.receipt.json beside the output: engine version, SHA-256 of the sheet, the input and the
        /// output, the command line and the time — the method of the step, in a form one can check
        #[arg(long, conflicts_with = "lines")]
        receipt: bool,
        /// GeoJSON input: put the answer's point in place of each Feature's geometry
        #[arg(long)]
        replace_geometry: bool,
        /// JSONL input: continue a run that was cut off — keep the rows in OUTPUT.partial (or in OUTPUT) and answer the rest
        #[arg(long, conflicts_with_all = ["lines", "dump"])]
        resume: bool,
        /// no progress line on stderr
        #[arg(long)]
        quiet: bool,
    },
    /// Serve one sheet over local HTTP: /v1/geocode, /v1/reverse and Nominatim-compatible /search,
    /// /reverse, /status. Prints `listening on http://HOST:PORT` first.
    Serve {
        /// the address sheet (.bin); not needed with --probe
        #[arg(required_unless_present = "probe")]
        index: Option<PathBuf>,
        /// optional POI layer (second .bin): cascades when the address result is weak
        #[arg(long)]
        poi: Option<PathBuf>,
        /// a sheet of one's own places (`gridpin build --places`), asked before the address sheet
        #[arg(long)]
        also: Option<PathBuf>,
        /// a sheet of places asked only when the address sheet has no answer or only a weak town (the world
        /// sheet of towns: `prep/overture_world_places.py`)
        #[arg(long)]
        also_after: Option<PathBuf>,
        /// more country sheets in the same server: a query is asked of the sheet `country=` names, or of the one whose
        /// country closes the query (a shared table of country names and ISO codes), otherwise of the first sheet;
        /// answers carry `country_<cc>`
        #[arg(long, value_delimiter = ',')]
        sheets: Vec<PathBuf>,
        /// address to listen on; 127.0.0.1 keeps the server on this machine
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// port; 0 picks a free one (printed on the first line)
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// connections handled at once (default: cores minus two, at least 1)
        #[arg(long)]
        threads: Option<usize>,
        /// your own names for places: a CSV with the columns name,address, read before the sheet
        #[arg(long)]
        aliases: Option<PathBuf>,
        /// only GET this http:// URL and exit 0 on status 200 (container health checks)
        #[arg(long, conflicts_with_all = ["poi"])]
        probe: Option<String>,
    },
    /// Addresses in free text (a letter, a delivery note, an answer of a model), each confirmed by the sheet
    Extract {
        index: PathBuf,
        /// the text file to read (`-`: standard input)
        #[arg(long, conflicts_with = "input")]
        text: Option<PathBuf>,
        /// a CSV whose column --column holds one text per row
        #[arg(long, requires = "column")]
        input: Option<PathBuf>,
        #[arg(long)]
        column: Option<String>,
        /// also report the addresses with a number the sheet does not have, with a reason
        #[arg(long)]
        verify: bool,
    },
    /// Delivery zones and rounds: k-means of the points of a CSV (lat, lon columns), sizes kept under --max-size
    Cluster {
        input: PathBuf,
        #[arg(long)]
        k: usize,
        #[arg(long)]
        max_size: Option<usize>,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(short = 'o', long)]
        out: PathBuf,
        /// also the convex hull of each zone (GeoJSON)
        #[arg(long)]
        geojson: Option<PathBuf>,
    },
    /// The top answer laid out as the post of the sheet's country writes an address
    Format {
        index: PathBuf,
        q: String,
        /// the postal layout: a country code of `postal_formats.tsv` (de, fr, us, br, ...), `default` (street,
        /// number, postcode, town) or `sheet`: the layout of the sheet's country
        #[arg(long, default_value = "sheet")]
        layout: String,
        /// what to print: `postal` (the layout above) or `iso20022` (the ISO 20022 `PstlAdr` as JSON: mode structured,
        /// hybrid or none, the elements, the checks and whether the address exists)
        #[arg(long, default_value = "postal")]
        out: String,
        /// with `--out iso20022`: the `<PstlAdr>` element as XML instead of JSON
        #[arg(long)]
        xml: bool,
    },
    /// The address grammar the sheet's rules declare, in the open schema docs-public/address-grammar.schema.json
    Grammar { index: PathBuf },
    /// An outline of each postcode, drawn from the houses' H3 cells (GeoJSON)
    Postcodes {
        index: PathBuf,
        /// H3 resolution of the cells (9: about 0.1 km²)
        #[arg(long, default_value_t = 9)]
        res: u8,
        #[arg(short = 'o', long)]
        out: PathBuf,
    },
    /// Why the engine answered as it did: the reading, the verdicts, the candidates and what they lost on
    Explain {
        index: PathBuf,
        q: String,
        /// text (default) or json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Does the address exist in the sheet: yes, maybe or no, and why
    Exists { index: PathBuf, q: String },
    /// How the engine reads an address line: first-hypothesis words, postcode, house number, top answer
    Parse {
        index: PathBuf,
        q: String,
        /// optional POI layer (second .bin): cascades when the address result is weak
        #[arg(long)]
        poi: Option<PathBuf>,
    },
    /// Do two address lines name the same house, street or commune? (`--pairs`: a JSONL file of {"a","b"})
    Same {
        index: PathBuf,
        /// the two lines; or with --pairs, the input and output JSONL files
        #[arg(num_args = 2)]
        lines: Vec<String>,
        /// treat the two arguments as an input file of {"a": ..., "b": ...} lines and an output file
        #[arg(long)]
        pairs: bool,
        /// optional POI layer (second .bin): cascades when the address result is weak
        #[arg(long)]
        poi: Option<PathBuf>,
    },
    /// Country sheets of this release: list, where they are kept, verify, download
    Sheets {
        #[command(subcommand)]
        cmd: SheetsCmd,
    },
    /// For each row of the first file, the nearest rows of the second by straight-line distance. A file with
    /// lat and lon columns is read as points; other files are geocoded on --sheet
    Nearest {
        rows: PathBuf,
        list: PathBuf,
        /// the sheet to geocode addresses on, when a file has no coordinates
        #[arg(long)]
        sheet: Option<PathBuf>,
        #[arg(long, default_value_t = 1)]
        k: usize,
        /// leave out anything farther than this
        #[arg(long, allow_hyphen_values = true)]
        max_km: Option<f64>,
        /// the column of the second file that names a row (default: id, else its line number)
        #[arg(long)]
        id_column: Option<String>,
        /// the address column of the first file
        #[arg(long)]
        rows_column: Option<String>,
        /// the address column of the second file
        #[arg(long)]
        list_column: Option<String>,
        /// output file (default: standard output)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
    /// Give each row the properties of the zone it falls in, from a GeoJSON file of polygons. A file with lat
    /// and lon columns is read as points; another is geocoded on --sheet
    Join {
        rows: PathBuf,
        zones: PathBuf,
        /// the sheet to geocode addresses on, when the file has no coordinates
        #[arg(long)]
        sheet: Option<PathBuf>,
        /// the address column
        #[arg(long)]
        column: Option<String>,
        /// a property to copy; repeat for several (default: every property)
        #[arg(long)]
        property: Vec<String>,
        /// output file (default: standard output)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
    /// Addresses of a street, a postcode, a circle, a box or a polygon, as a table
    List {
        index: PathBuf,
        #[command(flatten)]
        area: AreaArgs,
        /// csv, jsonl, geojsonl, geojson or gpx (default: from the -o extension, else csv)
        #[arg(long)]
        format: Option<String>,
        /// stop after this many addresses
        #[arg(long)]
        limit: Option<u64>,
        /// output file (default: standard output)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
    /// Copy addresses of a region, a box, a polygon or the whole sheet to a file, with the sheet's licence.
    /// For Parquet: DuckDB reads the CSV (see the documentation).
    Export {
        index: PathBuf,
        #[command(flatten)]
        area: AreaArgs,
        /// the whole sheet
        #[arg(long)]
        all: bool,
        /// csv, jsonl, geojsonl, geojson or gpx (default: from the -o extension, else csv)
        #[arg(long)]
        format: Option<String>,
        #[arg(short = 'o', long)]
        output: PathBuf,
    },
    /// A smaller sheet cut from a sheet by a box, a region, a postcode or a polygon (the source is not changed)
    Slice {
        index: PathBuf,
        #[command(flatten)]
        area: AreaArgs,
        #[arg(short = 'o', long)]
        out: PathBuf,
    },
    /// A random sample of real addresses, for test data; the same --seed gives the same addresses
    Sample {
        index: PathBuf,
        #[command(flatten)]
        area: AreaArgs,
        /// how many addresses
        #[arg(long, default_value_t = 1000)]
        n: usize,
        /// default: the clock
        #[arg(long)]
        seed: Option<u64>,
        /// csv, jsonl, geojsonl, geojson or gpx (default: from the -o extension, else csv)
        #[arg(long)]
        format: Option<String>,
        /// output file (default: standard output)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
    /// How many addresses each grid cell of 0.01 degree holds: where a sheet is dense, thin or empty
    Stats {
        index: PathBuf,
        #[command(flatten)]
        area: AreaArgs,
        /// csv or geojson (default: from the -o extension, else csv)
        #[arg(long)]
        format: Option<String>,
        /// output file (default: standard output)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
    /// Print the provenance/identity record of a sheet (SEC_META, v6+)
    Meta {
        index: PathBuf,
        /// machine-readable JSON object (safe against values with embedded newlines)
        #[arg(long)]
        json: bool,
    },
    /// Replace SEC_META in a current-format sheet without rebuilding data sections.
    /// Older v5/v6 sheets must be rebuilt because v7 changes house_blocks.
    Repack {
        input: PathBuf,
        output: PathBuf,
        /// build manifest (JSON): provenance + country/layer identity
        #[arg(long)]
        meta: PathBuf,
    },
    /// A citation of the sheet a study geocoded with: release, data date, licence, SHA-256, engine version
    Cite {
        sheet: PathBuf,
        /// bibtex, apa or cff (CITATION.cff)
        #[arg(long, default_value = "bibtex")]
        format: String,
    },
    /// How close the answers come to points you know: a CSV with an address and lat, lon of the truth.
    /// Prints rows, hits within 150 m, median, p95 and the match rate as JSON.
    Bench {
        index: PathBuf,
        input: PathBuf,
        /// the address column, or several joined with ", " (default: the one named address, query or q)
        #[arg(long)]
        column: Option<String>,
        /// the latitude column of the truth (default: lat or latitude)
        #[arg(long)]
        lat_column: Option<String>,
        /// the longitude column of the truth (default: lon, lng or longitude)
        #[arg(long)]
        lon_column: Option<String>,
        /// fail (exit 1) when fewer rows than this share are matched at an address (e.g. 0.85)
        #[arg(long)]
        min_match_rate: Option<f64>,
        /// also write each row with its answer and its distance to the truth (CSV)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
}

/// Which addresses of a sheet a command works on; every given option must hold.
#[derive(clap::Args, Default)]
struct AreaArgs {
    /// a street as you would write it; needs --city or --postcode
    #[arg(long)]
    street: Option<String>,
    /// a commune by name
    #[arg(long)]
    city: Option<String>,
    #[arg(long)]
    postcode: Option<String>,
    /// a circle: LAT,LON,METERS
    #[arg(long, value_name = "LAT,LON,METERS", value_parser = query::parse_circle, allow_hyphen_values = true)]
    near: Option<(f64, f64, f64)>,
    /// a box: MINLON,MINLAT,MAXLON,MAXLAT
    #[arg(long, value_name = "MINLON,MINLAT,MAXLON,MAXLAT", value_parser = query::BBox::parse, allow_hyphen_values = true)]
    bbox: Option<query::BBox>,
    /// a GeoJSON file with a Polygon or MultiPolygon
    #[arg(long, value_name = "FILE")]
    polygon: Option<PathBuf>,
    /// a region by name, from the polygon file <sheet>_admin.bin beside the sheet
    #[arg(long, conflicts_with = "polygon")]
    region: Option<String>,
}

impl AreaArgs {
    fn area(self) -> anyhow::Result<query::Area> {
        let polygons = self
            .polygon
            .map(|path| {
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
                query::PolygonSet::from_geojson(&text)
            })
            .transpose()?;
        Ok(query::Area {
            street: self.street,
            city: self.city,
            postcode: self.postcode,
            circle: self.near,
            bbox: self.bbox,
            polygons,
            region: self.region,
        })
    }
}

/// The top answers of one sheet, one query at a time; a panic on a corrupt sheet is an empty answer.
fn sheet_geocoder(idx: &query::Index) -> impl Fn(&str) -> Vec<query::Hit> + Sync + '_ {
    move |q| panic_safe(|| query::query_cascade(idx, None, q, 1))
}

/// Open the sheet and turn the options into a filter; says on stderr what a street was read as.
fn open_area(
    index: &std::path::Path,
    args: AreaArgs,
) -> anyhow::Result<(query::Index, query::HouseFilter, bool)> {
    let idx = query::Index::open_address(index)?;
    let area = args.area()?;
    let empty = area.is_empty();
    let (filter, read_as) = idx.house_filter(&area)?;
    if let Some(read) = read_as {
        eprintln!("street read as: {read}");
    }
    Ok((idx, filter, empty))
}

/// Write to the file through a sibling temp and a rename, as `batch` does; or to standard output.
fn write_rows(
    output: Option<&std::path::Path>,
    write: impl FnOnce(&mut dyn Write) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let Some(path) = output else {
        let mut out = BufWriter::new(std::io::stdout().lock());
        return match write(&mut out).and_then(|()| out.flush().map_err(Into::into)) {
            // `gridpin list ... | head` is a normal way to look at a table
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe) =>
            {
                Ok(())
            }
            other => other,
        };
    };
    let tmp = gridpin::builder::unique_tmp_path(path);
    let mut out = BufWriter::new(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?,
    );
    let written = write(&mut out)
        .and_then(|()| out.flush().map_err(anyhow::Error::from))
        .and_then(|()| out.get_ref().sync_all().map_err(anyhow::Error::from))
        .and_then(|()| gridpin::builder::finalize_replace(&tmp, path).map_err(anyhow::Error::from));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Walk the filter and write each kept address.
fn write_walk(
    idx: &query::Index,
    filter: &query::HouseFilter,
    format: gridpin::export::Format,
    limit: Option<u64>,
    out: &mut dyn Write,
) -> anyhow::Result<query::WalkStats> {
    use std::ops::ControlFlow;
    let provenance = gridpin::export::Provenance::of(idx);
    let mut writer = gridpin::export::Writer::new(out, format, &provenance)?;
    let mut failed = None;
    let mut written = 0u64;
    let stats = idx.walk_houses(filter, &mut |h| {
        if let Err(e) = writer.write(&idx.house_row(h)) {
            failed = Some(e);
            return ControlFlow::Break(());
        }
        written += 1;
        if limit.is_some_and(|n| written >= n) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    if let Some(e) = failed {
        return Err(e);
    }
    writer.finish()?;
    Ok(stats)
}

fn report_walk(stats: query::WalkStats) {
    eprintln!("{} addresses", stats.kept);
    if stats.range_points > 0 {
        eprintln!(
            "note: {} points sampled from address ranges were left out: they are not addresses",
            stats.range_points
        );
    }
}

/// `gridpin stats`: one row per grid cell, south-west to north-east.
fn write_stats(
    idx: &query::Index,
    filter: &query::HouseFilter,
    geojson: bool,
    out: &mut dyn Write,
) -> anyhow::Result<query::WalkStats> {
    let provenance = gridpin::export::Provenance::of(idx);
    let (cells, stats) = idx.house_counts(filter);
    let mut rows: Vec<(u32, u64)> = cells.into_iter().collect();
    rows.sort_unstable();
    if geojson {
        let features: Vec<serde_json::Value> = rows
            .iter()
            .map(|&(cell, houses)| {
                let (lat, lon) = query::cell_corner(cell);
                let (n, e) = (lat + 0.01, lon + 0.01);
                serde_json::json!({
                    "type": "Feature",
                    "properties": { "houses": houses },
                    "geometry": { "type": "Polygon",
                        "coordinates": [[[lon, lat], [e, lat], [e, n], [lon, n], [lon, lat]]] },
                })
            })
            .collect();
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "type": "FeatureCollection",
                "license": provenance.license,
                "attribution": provenance.attribution,
                "features": features,
            })
        )?;
    } else {
        writeln!(out, "# license: {}", provenance.license)?;
        writeln!(out, "# attribution: {}", provenance.attribution)?;
        writeln!(out, "lat,lon,houses")?;
        for (cell, houses) in rows {
            let (lat, lon) = query::cell_corner(cell);
            writeln!(out, "{lat:.2},{lon:.2},{houses}")?;
        }
    }
    Ok(stats)
}

#[derive(Subcommand)]
enum SheetsCmd {
    /// Every sheet of the release: code, country, size, and whether it is downloaded
    List {
        /// one JSON array, with file, SHA-256, address and local path
        #[arg(long)]
        json: bool,
    },
    /// The folder of downloaded sheets, or with a country the path of its sheet
    Where { country: Option<String> },
    /// Check size and SHA-256 against the signed release (no argument: every downloaded sheet)
    Verify {
        /// country codes or names, or paths to .bin files
        targets: Vec<String>,
    },
    /// Download a sheet with the system curl and check it before it is used
    Fetch {
        country: String,
        /// never use the network (also GRIDPIN_OFFLINE=1): print the command to run elsewhere
        #[arg(long)]
        offline: bool,
    },
}

fn sheets_command(cmd: SheetsCmd) -> anyhow::Result<()> {
    use gridpin::sheets;
    let registry = sheets::registry();
    match cmd {
        SheetsCmd::List { json } => {
            if json {
                let rows = registry
                    .sheets
                    .iter()
                    .map(|s| {
                        Ok(serde_json::json!({
                            "code": s.code, "name": s.name, "file": s.file, "size": s.size,
                            "sha256": s.sha256, "url": s.url(),
                            "path": sheets::local_path(s)?, "downloaded": sheets::is_downloaded(s)?,
                        }))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                println!("{}", serde_json::to_string(&rows)?);
            } else {
                println!(
                    "release {}, folder {}",
                    registry.release,
                    sheets::home()?.join(&registry.release).display()
                );
                for s in &registry.sheets {
                    let state = if sheets::is_downloaded(s)? {
                        "downloaded"
                    } else {
                        "-"
                    };
                    println!(
                        "{:<4}{:<16}{:>9.0} MB  {}",
                        s.code,
                        s.name,
                        s.size as f64 / 1e6,
                        state
                    );
                }
            }
        }
        SheetsCmd::Where { country } => match country {
            None => println!("{}", sheets::home()?.join(&registry.release).display()),
            Some(country) => {
                let s = sheets::resolve(&country)?;
                let path = sheets::local_path(s)?;
                if !sheets::is_downloaded(s)? {
                    anyhow::bail!(
                        "the {} sheet is not downloaded; run `gridpin sheets fetch {}`",
                        s.name,
                        s.code
                    );
                }
                println!("{}", path.display());
            }
        },
        SheetsCmd::Verify { targets } => {
            let mut checks: Vec<(PathBuf, &gridpin::sheets::Sheet)> = Vec::new();
            if targets.is_empty() {
                for s in &registry.sheets {
                    if sheets::is_downloaded(s)? {
                        checks.push((sheets::local_path(s)?, s));
                    }
                }
                if checks.is_empty() {
                    println!("no sheet of {} is downloaded", registry.release);
                }
            }
            for target in &targets {
                let path = PathBuf::from(target);
                if path.extension().is_some_and(|e| e == "bin") || path.is_file() {
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default();
                    let s = registry
                        .sheets
                        .iter()
                        .find(|s| s.file == name)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "{target}: not a sheet file name of {} (expected e.g. {})",
                                registry.release,
                                registry.sheets[0].file
                            )
                        })?;
                    checks.push((path, s));
                } else {
                    let s = sheets::resolve(target)?;
                    checks.push((sheets::local_path(s)?, s));
                }
            }
            let mut bad = 0;
            for (path, s) in checks {
                let verdict = sheets::verify(&path, s)?;
                let line = match &verdict {
                    sheets::Verdict::Ok => "ok".to_string(),
                    sheets::Verdict::Missing => "missing".to_string(),
                    sheets::Verdict::WrongSize(n) => {
                        format!("WRONG SIZE: {n} bytes, expected {}", s.size)
                    }
                    sheets::Verdict::WrongDigest(d) => {
                        format!("WRONG SHA-256: {d}, expected {}", s.sha256)
                    }
                };
                bad += usize::from(verdict != sheets::Verdict::Ok);
                println!("{}  {}  {line}", s.code, path.display());
            }
            if bad > 0 {
                anyhow::bail!("{bad} sheet(s) failed the check");
            }
        }
        SheetsCmd::Fetch { country, offline } => {
            let s = sheets::resolve(&country)?;
            let path = sheets::fetch(s, offline || sheets::offline_from_env())?;
            println!("{}", path.display());
        }
    }
    Ok(())
}

/// Run a query that reads mmap'd sections, but never let a panic INSIDE the fst crate on a
/// corrupt-but-openable sheet abort the CLI: the Python/DuckDB bindings have their
/// own panic boundary; the CLI query/reverse/batch paths need one too. A caught panic yields the
/// type's default (empty results) so the process exits cleanly instead of aborting the host.
/// Like `panic_safe` for answers without a neutral value: a caught panic becomes an error.
fn guarded<T>(f: impl FnOnce() -> T) -> anyhow::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .map_err(|_| anyhow::anyhow!("the engine failed on this line (corrupt sheet or input)"))
}

fn panic_safe<T: Default>(f: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_default()
}

/// The question or the reason of an outcome, for a person at the terminal.
fn tell(outcome: &gridpin::outcome::Outcome) {
    use gridpin::outcome::Outcome;
    match outcome {
        Outcome::Clarify {
            missing, options, ..
        } => {
            let places: Vec<&str> = options.iter().map(|c| c.label.as_str()).collect();
            eprintln!(
                "gridpin: {} places match; add the {missing} to choose: {}",
                places.len(),
                places.join(" | ")
            );
        }
        Outcome::None { reason } => eprintln!("gridpin: no answer ({reason})"),
        Outcome::Answer { .. } => {}
    }
}

/// The address sheet with the sheet of one's own places, when `--also` names one.
fn open_address_also(
    index: &std::path::Path,
    also: Option<&std::path::Path>,
    also_after: Option<&std::path::Path>,
) -> anyhow::Result<query::Index> {
    let mut idx = query::Index::open_address(index)?;
    if let Some(a) = also {
        idx.set_also(query::Index::open_also(a)?);
    }
    if let Some(a) = also_after {
        idx.set_also_after(query::Index::open_also(a)?);
    }
    Ok(idx)
}

fn tolerance_of(strict: bool, lenient: bool) -> query::Tolerance {
    match (strict, lenient) {
        (true, _) => query::Tolerance::Strict,
        (_, true) => query::Tolerance::Lenient,
        _ => query::Tolerance::Default,
    }
}

/// One JSONL output line with `extras` on every hit (and `diagnosis` when asked).
fn with_extras(
    hits: &[query::Hit],
    extras: &gridpin::extras::Extras,
    idx: &query::Index,
    alias: Option<&str>,
    diagnosis: Option<serde_json::Value>,
) -> String {
    let rows = gridpin::extras::hits_text(hits, Some(extras), idx, alias)
        .unwrap_or_else(|_| "[]".to_string());
    match diagnosis {
        Some(d) => format!("{{\"results\":{rows},\"diagnosis\":{d}}}"),
        None => format!("{{\"results\":{rows}}}"),
    }
}

/// The command line. GRIDPIN_HELP_ALL=1 also shows the commands and flags hidden until their release in `--help`:
/// the site's command reference (`tools/cli_reference.py`) documents them with the code, marked as new.
fn parse_cli() -> Cli {
    use clap::{CommandFactory, FromArgMatches};
    if std::env::var_os("GRIDPIN_HELP_ALL").is_none() {
        return Cli::parse();
    }
    fn unhide(c: clap::Command) -> clap::Command {
        c.hide(false)
            .mut_args(|a| a.hide(false))
            .mut_subcommands(unhide)
    }
    let matches = unhide(Cli::command()).get_matches();
    Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())
}

fn main() -> anyhow::Result<()> {
    // A corrupt or hostile sheet can drive a deep dependency (e.g. the fst reader) to panic on
    // bytes our own accessors can't pre-validate. For a shipped CLI that must be a controlled
    // exit(1), never a 101/SIGABRT that takes down the host shell or pipeline.
    // The hook is SILENT (not exit) so the inner `panic_safe` boundaries can actually CATCH a
    // query-time panic — e.g. one corrupt line in a batch yields empty and the batch continues,
    // instead of the whole process dying. Uncaught panics hit the top-level catch below -> exit(1).
    // GRIDPIN_PANIC_TRACE=1 names each caught panic on stderr (a diagnostic for engine work: a
    // silent catch turns a defect into an empty answer that looks like honest silence).
    let trace = std::env::var_os("GRIDPIN_PANIC_TRACE").is_some();
    std::panic::set_hook(Box::new(move |info| {
        if trace {
            eprintln!(
                "gridpin: panic caught: {info}\n{}",
                std::backtrace::Backtrace::force_capture()
            );
        }
    }));
    // The whole CLI runs on a thread with an explicit stack. The main thread gets 8 MiB on Linux
    // and macOS but 1 MiB on Windows, and a debug build overflowed it in argument parsing alone
    // (the recursive `unhide` walk moves clap `Command` values by value): every CLI test failed
    // on Windows CI for v0.9.0. One explicit size gives every platform the same headroom.
    std::thread::Builder::new()
        .name("gridpin".into())
        .stack_size(MAIN_STACK_BYTES)
        .spawn(|| {
            let cli = parse_cli();
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(cli))) {
                Ok(r) => r,
                Err(_) => {
                    eprintln!("gridpin: fatal: corrupt or unsupported input");
                    std::process::exit(1);
                }
            }
        })?
        .join()
        .unwrap_or_else(|_| {
            eprintln!("gridpin: fatal: corrupt or unsupported input");
            std::process::exit(1);
        })
}

/// Stack of the thread that runs the CLI (see `main`).
const MAIN_STACK_BYTES: usize = 16 * 1024 * 1024;

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.cmd {
        #[cfg(feature = "watermark")]
        Cmd::Mark { index, verify } => gridpin::mark::cli_show(&index, verify.as_deref())?,
        #[cfg(feature = "watermark")]
        Cmd::Stamp {
            input,
            output,
            mark,
        } => gridpin::mark::cli_stamp(&input, &output, &mark)?,
        Cmd::Build {
            input,
            out,
            model,
            rank,
            rules,
            mark,
            meta,
            places: true,
            license,
            first_wins,
        } => {
            let _ = (model, rank, rules, mark, meta);
            let (n, dropped) =
                gridpin::places::build_keeping(&input, &out, license.as_deref(), first_wins)?;
            eprintln!("{n} places -> {}", out.display());
            if dropped > 0 {
                eprintln!("{dropped} names left out: already taken in their group (--first-wins)");
            }
        }
        Cmd::Build {
            input,
            out,
            model,
            rank,
            rules,
            mark,
            meta,
            ..
        } => builder::build(
            &input,
            &out,
            model.as_deref(),
            rank.as_deref(),
            rules.as_deref(),
            mark.as_deref(),
            meta.as_deref(),
        )?,
        Cmd::Reverse {
            index,
            lat,
            lon,
            k,
            with,
        } => {
            // reverse on a POI-only sheet would mark a place as a `house` — refuse it as primary
            // too, consistent with query/batch; POI belongs to the cascade only.
            let idx = query::Index::open_address(&index)?;
            // try_reverse is the single strict entry point: a bad coordinate is an error
            // here just as in py/DuckDB, not a silent empty result.
            let extras = gridpin::extras::Extras::new(with.as_deref(), &idx)?;
            let hits = idx
                .try_reverse(lat, lon, k)
                .map_err(|e| anyhow::anyhow!(e))?;
            for h in hits {
                match &extras {
                    None => println!("{}", serde_json::to_string(&h)?),
                    Some(e) => println!("{}", e.hit_json(&h, &idx, None)?),
                }
            }
        }
        Cmd::Meta { index, json } => {
            let idx = query::Index::open(&index)?;
            if json {
                // a value can contain arbitrary bytes incl. newlines; JSON escapes them, so a
                // downstream parser cannot be spoofed by an injected `key: value` line.
                let map: std::collections::BTreeMap<&str, &str> = idx
                    .meta()
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                println!("{}", serde_json::to_string(&map)?);
            } else {
                if idx.meta().is_empty() {
                    println!("(no SEC_META: pre-v6 data or built without --meta)");
                }
                for (k, v) in idx.meta() {
                    println!("{k}: {v}");
                }
            }
        }
        Cmd::Repack {
            input,
            output,
            meta,
        } => {
            use gridpin::index;
            let data = std::fs::read(&input)?;
            // Repack is byte-for-byte for all data sections and is therefore safe only within v7.
            // v5/v6 house blocks do not carry v7's conditional per-house postcode ids; relabeling
            // them as v7 would make the reader interpret unrelated bytes as a dictionary.
            let secs = index::parse_sections_for_repack(&data)?;
            let meta_sec = index::encode_meta(&builder::meta_from_manifest(&meta)?);
            // copy every section verbatim, drop any pre-existing SEC_META, append ours
            let mut sections: Vec<(u8, Vec<u8>)> = secs
                .iter()
                .filter(|(id, _, _)| *id as usize != index::SEC_META)
                .map(|(id, off, len)| (*id, data[*off as usize..(*off + *len) as usize].to_vec()))
                .collect();
            sections.push((index::SEC_META as u8, meta_sec));
            let mut toc: Vec<(u8, u64, u64)> = Vec::new();
            // header size follows the ACTUAL entry count: a sheet built before some
            // optional section existed has fewer TOC entries than N_SECTIONS, and
            // assuming the maximum would shift every offset (caught by self-check)
            let mut off = (6 + sections.len() * 17) as u64;
            for (id, sec) in &sections {
                toc.push((*id, off, sec.len() as u64));
                off += sec.len() as u64;
            }
            let mut header = Vec::new();
            index::write_header(&mut header, &toc);
            // Self-check BEFORE publishing: assembling the bytes in
            // memory lets us structurally parse them and read back the identity we
            // just wrote; only a verified result reaches the target path. Publishing
            // first once destroyed a source sheet when the check failed after rename.
            let mut bytes = header;
            for (_, s) in &sections {
                bytes.extend_from_slice(s);
            }
            let secs_check = index::parse_sections(&bytes)?;
            let (moff, mlen) = secs_check[index::SEC_META];
            let meta_ok = index::decode_meta(&bytes[moff as usize..(moff + mlen) as usize])
                .is_some_and(|m| !m.is_empty());
            anyhow::ensure!(
                meta_ok,
                "repack self-check failed: SEC_META unreadable — output not written"
            );
            // Stronger self-check: actually OPEN + query the produced bytes before
            // publishing, so a structurally-valid-but-semantically-corrupt sheet (e.g. a byte-flip
            // inside an FST) is caught here — not handed to a buyer as a sheet that won't open.
            let tmp = builder::unique_tmp_path(&output);
            {
                // fsync the file DATA before the self-check + durable rename (adversarial finding):
                // repack used std::fs::write (no fsync), so finalize_replace made only the directory
                // entry durable — a crash after rename could publish a final-named sheet with
                // unflushed/zeroed data blocks. Match write_atomic / the batch path.
                use std::io::Write as _;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&tmp)?;
                f.write_all(&bytes)?;
                f.sync_all()?;
            }
            let verified =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<()> {
                    let idx = gridpin::query::Index::open(&tmp)?; // full open = semantic validation
                    let _ = idx.query("a", 1); // exercise the FST/postings read paths
                    let _ = idx.reverse(0.0, 0.0, 1);
                    Ok(())
                }));
            if !matches!(verified, Ok(Ok(()))) {
                let _ = std::fs::remove_file(&tmp);
                anyhow::bail!("repack self-check failed: produced sheet did not open/query — output not written");
            }
            builder::finalize_replace(&tmp, &output)?;
            println!(
                "repacked {} -> {} (v{}, {} sections + meta)",
                input.display(),
                output.display(),
                index::VERSION,
                sections.len()
            );
        }
        Cmd::Query {
            index,
            q,
            k,
            near,
            poi,
            also,
            also_after,
            aliases,
            with,
            strict,
            lenient,
            abbrev,
        } => {
            let idx = open_address_also(&index, also.as_deref(), also_after.as_deref())?;
            let mut extras = gridpin::extras::Extras::new(with.as_deref(), &idx)?;
            let tolerance = tolerance_of(strict, lenient);
            let abbrev = abbrev
                .as_deref()
                .map(gridpin::abbrev::Abbrev::load)
                .transpose()?;
            let q = abbrev.as_ref().map_or(q.clone(), |a| a.apply(&q));
            let poi_idx = poi.map(|p| query::Index::open(&p)).transpose()?;
            if let (Some(e), Some(pi)) = (extras.as_mut(), &poi_idx) {
                e.set_poi(pi);
            }
            if let Some(pi) = &poi_idx {
                query::check_pair(&idx, pi).map_err(|e| anyhow::anyhow!(e))?;
            }
            let aliases = aliases
                .as_deref()
                .map(gridpin::aliases::Aliases::load)
                .transpose()?;
            // a question or the reason of a silence goes to stderr; stdout stays the answers of before
            if near.is_none() && aliases.is_none() && tolerance == query::Tolerance::Default {
                tell(&gridpin::outcome::query_outcome(
                    &idx,
                    poi_idx.as_ref(),
                    &q,
                    k,
                ));
            }
            let (hits, name) = gridpin::aliases::answer(aliases.as_ref(), &q, |text| {
                query::with_tolerance(tolerance, || match near {
                    None => panic_safe(|| query::query_cascade(&idx, poi_idx.as_ref(), text, k)),
                    Some((lat, lon)) => panic_safe(|| {
                        query::query_cascade_near(&idx, poi_idx.as_ref(), text, k, lat, lon)
                            .unwrap_or_default()
                    }),
                })
            });
            gridpin::extras::for_query(&q, || -> anyhow::Result<()> {
                for h in hits {
                    match (&name, &extras) {
                        (None, None) => println!("{}", serde_json::to_string(&h)?),
                        (Some(name), None) => {
                            println!("{}", gridpin::aliases::hit_json(&h, Some(name))?)
                        }
                        (name, extras) => println!(
                            "{}",
                            gridpin::extras::hit_text(&h, extras.as_ref(), &idx, name.as_deref())?
                        ),
                    }
                }
                Ok(())
            })?;
        }
        Cmd::Batch {
            index,
            input,
            output,
            k,
            format,
            output_format,
            column,
            columns,
            reverse,
            lat_column,
            lon_column,
            aliases,
            delimiter,
            dump,
            diagnose,
            poi,
            also,
            also_after,
            with,
            strict,
            lenient,
            abbrev,
            aggregate,
            min_count,
            mask,
            seed,
            lines,
            receipt,
            replace_geometry,
            resume,
            quiet,
        } => {
            use rayon::prelude::*;
            let started = std::time::SystemTime::now();
            let tolerance = tolerance_of(strict, lenient);
            let abbrev = abbrev
                .as_deref()
                .map(gridpin::abbrev::Abbrev::load)
                .transpose()?;
            let written =
                |q: &str| -> String { abbrev.as_ref().map_or(q.to_string(), |a| a.apply(q)) };
            if gridpin::geojson_io::kind_of(&input).is_some() {
                if resume {
                    anyhow::bail!("batch --resume reads JSONL input only");
                }
                let idx = open_address_also(&index, also.as_deref(), also_after.as_deref())?;
                let poi_idx = poi.map(|p| query::Index::open(&p)).transpose()?;
                let k = 1;
                let opts = gridpin::geojson_io::Options {
                    column: column.as_deref(),
                    reverse,
                    replace_geometry,
                };
                let mut buf: Vec<u8> = Vec::new();
                let n = gridpin::geojson_io::run(&input, &mut buf, &opts, |ask| {
                    use gridpin::geojson_io::Ask;
                    query::with_tolerance(tolerance, || {
                        panic_safe(|| match ask {
                            Ask::Line(q) => {
                                query::query_cascade(&idx, poi_idx.as_ref(), &written(q), k)
                            }
                            Ask::Point(lat, lon) => {
                                idx.try_reverse(*lat, *lon, k).unwrap_or_default()
                            }
                            Ask::Nothing => Vec::new(),
                        })
                    })
                    .into_iter()
                    .next()
                })?;
                std::fs::write(&output, buf)?;
                eprintln!("{n} features -> {}", output.display());
                return Ok(());
            }
            let input_format = gridpin::tabular::Format::of_input(format.as_deref(), &input)?;
            if resume && input_format != gridpin::tabular::Format::Jsonl {
                anyhow::bail!(
                    "batch --resume reads JSONL input only (a CSV run is answered in one pass)"
                );
            }
            let table = input_format != gridpin::tabular::Format::Jsonl;
            let output_format = match output_format.as_deref() {
                None => input_format,
                Some(name) => gridpin::tabular::Format::parse(name)?,
            };
            if table && (dump || diagnose) {
                anyhow::bail!("batch: --dump and --diagnose read JSONL only");
            }
            if table == (output_format == gridpin::tabular::Format::Jsonl) {
                anyhow::bail!(
                    "batch: CSV/TSV output needs a CSV/TSV input, and JSONL output a JSONL input"
                );
            }
            if !table && (column.is_some() || columns.is_some() || delimiter.is_some() || reverse) {
                anyhow::bail!(
                    "batch: --column, --columns, --reverse and --delimiter apply to CSV/TSV input only"
                );
            }
            if !table && (aggregate.is_some() || mask.is_some()) {
                anyhow::bail!("batch: --aggregate and --mask apply to CSV/TSV input only");
            }
            let cells = aggregate
                .as_deref()
                .map(gridpin::publish::Cells::parse)
                .transpose()?;
            let mask = match mask.as_deref() {
                None => None,
                Some(spec) => {
                    let seed = seed.unwrap_or_else(|| {
                        let s = gridpin::publish::fresh_seed();
                        eprintln!("--mask: seed {s} (pass --seed {s} to repeat this run)");
                        s
                    });
                    Some(gridpin::publish::Mask::parse(spec, seed)?)
                }
            };
            let aliases = aliases
                .as_deref()
                .map(gridpin::aliases::Aliases::load)
                .transpose()?;
            let k = k.unwrap_or(if table { 1 } else { 10 });
            let t0 = std::time::Instant::now();
            let idx = open_address_also(&index, also.as_deref(), also_after.as_deref())?;
            let poi_idx = poi.map(|p| query::Index::open(&p)).transpose()?;
            if let Some(pi) = &poi_idx {
                query::check_pair(&idx, pi).map_err(|e| anyhow::anyhow!(e))?;
            }
            let mut extras = gridpin::extras::Extras::new(with.as_deref(), &idx)?;
            if let (Some(e), Some(pi)) = (extras.as_mut(), &poi_idx) {
                e.set_poi(pi);
            }
            if extras.is_some() && dump {
                anyhow::bail!("batch: --with does not apply to --dump");
            }
            let open_s = t0.elapsed().as_secs_f64();
            // gentle pool: cores minus two (override with GRIDPIN_THREADS; clamped to >=1
            // so GRIDPIN_THREADS=0 does not fail the build)
            let n_threads = std::env::var("GRIDPIN_THREADS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .map(|v| v.max(1))
                .unwrap_or_else(|| {
                    std::thread::available_parallelism()
                        .map(|c| c.get().saturating_sub(2).max(1))
                        .unwrap_or(2)
                });
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(n_threads)
                .build()?;
            if lines {
                if input.as_os_str() != "-" || output.as_os_str() != "-" {
                    anyhow::bail!("batch --lines reads stdin and writes stdout: pass - - as the input and the output");
                }
                let mut out = std::io::stdout().lock();
                for line in std::io::stdin().lock().lines() {
                    let q = gridpin::tabular::tsv_unescape(&line?);
                    let hits = query::with_tolerance(tolerance, || {
                        panic_safe(|| query::query_cascade(&idx, poi_idx.as_ref(), &written(&q), 1))
                    });
                    let text = hits
                        .first()
                        .map(serde_json::to_string)
                        .transpose()?
                        .unwrap_or_default();
                    writeln!(out, "{}", gridpin::tabular::tsv_escape(&text))?;
                    out.flush()?;
                }
                return Ok(());
            }
            let mut inp = std::io::BufReader::new(std::fs::File::open(&input)?).lines();
            // input == output would destroy the input. canonicalize catches
            // symlinks and the same literal path; but two HARDLINKS have different paths yet
            // the SAME inode, so also compare file identity (dev+ino) on unix.
            if let (Ok(a), Ok(b)) = (
                std::fs::canonicalize(&input),
                std::fs::canonicalize(&output),
            ) {
                if a == b {
                    anyhow::bail!(
                        "batch: input and output are the same file — this would destroy the input"
                    );
                }
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if let (Ok(mi), Ok(mo)) = (std::fs::metadata(&input), std::fs::metadata(&output)) {
                    if mi.dev() == mo.dev() && mi.ino() == mo.ino() {
                        anyhow::bail!(
                            "batch: input and output are the same file (hardlink/alias) — refusing to overwrite the input"
                        );
                    }
                }
            }
            // Stream into a sibling temp, then atomically rename on full success (
            // ): a read/decode failure mid-stream leaves the PREVIOUS output intact
            // instead of a truncated partial, and writing a fresh inode keeps a hardlinked
            // input untouched. The temp name is UNIQUE (pid + counter) and opened with
            // create_new, so it can never collide with — and truncate — an input that happens
            // to be named like the temp: create_new
            // fails on an existing path instead of destroying it. One shared unique-temp writer
            // with the build/repack path.
            // A JSONL run writes into "<output>.partial" and flushes it after every window, so a run cut by
            // Ctrl-C or a crash keeps what it wrote and `--resume` continues from there (09.11.2026: the
            // unique temp vanished with the process and --resume after an interrupt started from zero).
            // A CSV/TSV run keeps the unique temp of  / .
            let partial = (!table).then(|| partial_path(&output));
            let resumed_rows = match &partial {
                Some(p) if resume && p.exists() => Some(keep_whole_lines(p)?),
                Some(p) if p.exists() => anyhow::bail!(
                    "{} is left from an interrupted run: add --resume to continue it, or delete it",
                    p.display()
                ),
                _ => None,
            };
            let tmp_out = partial
                .clone()
                .unwrap_or_else(|| gridpin::builder::unique_tmp_path(&output));
            let mut out = BufWriter::new(if resumed_rows.is_some() {
                std::fs::OpenOptions::new().append(true).open(&tmp_out)?
            } else {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&tmp_out)?
            });
            // Remove the temp on ANY early return (a mid-stream `?` error) so a failed batch never
            // leaves a ~MB partial temp behind. Disarmed only after a clean publish.
            struct TempCleanup {
                path: Option<std::path::PathBuf>,
            }
            impl TempCleanup {
                fn disarm(&mut self) {
                    self.path = None;
                }
                fn cleanup(&mut self) {
                    if let Some(p) = self.path.take() {
                        let _ = std::fs::remove_file(p);
                    }
                }
            }
            impl Drop for TempCleanup {
                fn drop(&mut self) {
                    self.cleanup();
                }
            }
            // the partial file of a JSONL run is kept on an error: it is what --resume continues
            let mut tmp_guard = TempCleanup {
                path: partial.is_none().then(|| tmp_out.clone()),
            };
            let mut nq = 0u64;
            let tq = std::time::Instant::now();
            let mut without_address = 0u64;
            if table {
                let opts = gridpin::tabular::Options {
                    input_format,
                    output_format,
                    columns: column.as_deref(),
                    fields: columns.as_deref(),
                    aliases: aliases.as_ref(),
                    reverse: reverse.then_some((lat_column.as_deref(), lon_column.as_deref())),
                    delimiter: delimiter.as_deref(),
                    k,
                    extras: extras.as_ref().map(|e| (e, &idx)),
                    aggregate: cells.as_ref().map(|c| (c, min_count)),
                    mask: mask.as_ref().map(|m| (m, &idx)),
                };
                let stats = gridpin::tabular::run(&input, &mut out, &opts, &pool, |row| {
                    use gridpin::tabular::RowQuery;
                    query::with_tolerance(tolerance, || {
                        panic_safe(|| match row {
                            RowQuery::Line(q) => {
                                query::query_cascade(&idx, poi_idx.as_ref(), &written(q), k)
                            }
                            RowQuery::Point(lat, lon) => {
                                idx.try_reverse(*lat, *lon, k).unwrap_or_default()
                            }
                            RowQuery::Fields {
                                street,
                                number,
                                city,
                                region,
                                postcode,
                            } => query::query_fields_cascade(
                                &idx,
                                poi_idx.as_ref(),
                                query::AddressFields {
                                    street,
                                    number: number.as_deref(),
                                    city,
                                    region: region.as_deref(),
                                    postcode: postcode.as_deref(),
                                },
                                k,
                            ),
                        })
                    })
                })?; // on Err, Drop cleans the temp
                nq = stats.rows;
                without_address = stats.without_address;
            }
            // input is JSONL: {"q": "..."} per line. Lines without a usable "q" would
            // silently yield empty results, so they are counted and reported.
            let malformed = std::sync::atomic::AtomicU64::new(0);
            // the row number (0-based, over the whole run) of the first line without a usable "q": the partial file is
            // cut there and kept, so a fixed input continues with --resume instead of starting from zero
            let first_bad = std::sync::atomic::AtomicU64::new(u64::MAX);
            // --resume: the rows an interrupted run already wrote are kept as they are, and that many input lines
            // are skipped; the output is still written through the temp and renamed at the end
            if resume && !table {
                let from = if resumed_rows.is_some() {
                    partial.clone().unwrap_or_default()
                } else {
                    output.clone()
                };
                if let Ok(done) = std::fs::read_to_string(&from) {
                    let kept: Vec<&str> = done.lines().collect();
                    if resumed_rows.is_none() {
                        for l in &kept {
                            writeln!(out, "{l}")?;
                        }
                    }
                    for done_rows in 0..kept.len() {
                        if inp.next().transpose()?.is_none() {
                            // more rows kept than the input has: another input, or another output (revision 0.9.0)
                            anyhow::bail!(
                                "--resume: {} rows are kept in {} but {} has only {done_rows} lines: \
                                 is it the input of that run?",
                                kept.len(),
                                from.display(),
                                input.display()
                            );
                        }
                    }
                    nq = kept.len() as u64;
                    eprintln!("--resume: {} rows kept from {}", kept.len(), from.display());
                }
            }
            let progress = !quiet && std::io::IsTerminal::is_terminal(&std::io::stderr());
            let mut last_progress = std::time::Instant::now();
            // Windowed streaming: holding the whole input AND the whole
            // output in memory used to OOM on tens of millions of rows. A window keeps
            // par_iter's order guarantee within itself, and windows are written in
            // order, so the output is byte-identical to the old all-in-memory path.
            const WINDOW: usize = 65_536;
            loop {
                if table {
                    break; // a CSV/TSV input was answered above by gridpin::tabular::run
                }
                let mut lines: Vec<String> = Vec::with_capacity(WINDOW);
                for l in inp.by_ref() {
                    // Push EVERY input line — do NOT silently drop a blank one: that
                    // broke input↔output cardinality (N lines in, fewer out). A blank line has no
                    // usable "q", so it is counted as malformed below (fail-closed: no publish),
                    // while a well-formed JSONL (one object per line) never contains a blank line.
                    lines.push(l?);
                    if lines.len() == WINDOW {
                        break;
                    }
                }
                if lines.is_empty() {
                    break;
                }
                let window_start = nq;
                nq += lines.len() as u64;
                if progress && last_progress.elapsed().as_secs_f64() >= 2.0 {
                    let rate = nq as f64 / tq.elapsed().as_secs_f64().max(1e-9);
                    eprint!("\rrows {nq} · {rate:.0} rows/s   ");
                    last_progress = std::time::Instant::now();
                }
                let results: Vec<String> = pool.install(|| {
                lines
                    .par_iter()
                    .enumerate()
                    .map(|(at, line)| {
                        let v: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
                        let q = v.get("q").and_then(|x| x.as_str()).unwrap_or("").trim();
                        // field as string OR number ("housenumber":5 / postcode:75001); a blank or
                        // whitespace-only field is treated as ABSENT so it can't hijack a valid `q`
                        // and return an empty structured result.
                        let fld = |key: &str| -> Option<String> {
                            v.get(key)
                                .and_then(|x| x.as_str().map(String::from).or_else(|| x.as_u64().map(|n| n.to_string())))
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                        };
                        // Dispatch: dump > structured (non-blank "street", address-index only) >
                        // free-form ("q", full address+POI cascade). A line is usable if it has a
                        // free-form "q" OR a structured "street"; only genuinely empty lines are malformed.
                        if q.is_empty() && fld("street").is_none() && !dump {
                            malformed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            first_bad.fetch_min(window_start + at as u64, std::sync::atomic::Ordering::Relaxed);
                        }
                        // each per-line query is panic-guarded: a corrupt-but-openable
                        // sheet must not abort the whole batch — a panicking line yields empty.
                        let diagnosis = diagnose.then(gridpin::diagnosis::Scope::new);
                        if dump {
                            let cands: Vec<serde_json::Value> = panic_safe(|| idx.query_feats(q, k))
                                .into_iter()
                                .map(|(h, f)| serde_json::json!({"f": f, "lat": h.lat, "lon": h.lon}))
                                .collect();
                            serde_json::json!({ "cands": cands }).to_string()
                        } else if let Some(street) = fld("street") {
                            // structured input: pre-parsed fields, no field-boundary guessing. Uses
                            // the SAME POI cascade as free-form, so a structured POI query
                            // is not silently unanswerable just because the caller pre-split fields.
                            let city = fld("city").unwrap_or_default();
                            let number = fld("housenumber").or_else(|| fld("number"));
                            let postcode = fld("postcode");
                            let hits = query::with_tolerance(tolerance, || panic_safe(|| {
                                query::query_structured_cascade(
                                    &idx,
                                    poi_idx.as_ref(),
                                    &street,
                                    number.as_deref(),
                                    &city,
                                    postcode.as_deref(),
                                    k,
                                )
                            }));
                            if let Some(e) = &extras {
                                let asked = [Some(street.as_str()), number.as_deref(), Some(city.as_str()), postcode.as_deref()]
                                    .into_iter()
                                    .flatten()
                                    .filter(|s| !s.is_empty())
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                let d = diagnosis.map(|d| d.finish(hits.len()));
                                gridpin::extras::for_query(&asked, || with_extras(&hits, e, &idx, None, d))
                            } else if let Some(trace) = diagnosis {
                                let observed = trace.finish(hits.len());
                                serde_json::json!({ "results": hits, "diagnosis": observed }).to_string()
                            } else {
                                serde_json::json!({ "results": hits }).to_string()
                            }
                        } else {
                            let q = written(q);
                            let (hits, name) = gridpin::aliases::answer(aliases.as_ref(), &q, |text| {
                                query::with_tolerance(tolerance, || {
                                    panic_safe(|| query::query_cascade(&idx, poi_idx.as_ref(), text, k))
                                })
                            });
                            if let Some(e) = &extras {
                                let d = diagnosis.map(|d| d.finish(hits.len()));
                                gridpin::extras::for_query(&q, || with_extras(&hits, e, &idx, name.as_deref(), d))
                            } else if let Some(name) = name {
                                let rows: Vec<serde_json::Value> = hits
                                    .iter()
                                    .filter_map(|h| gridpin::aliases::hit_json(h, Some(&name)).ok())
                                    .collect();
                                serde_json::json!({ "results": rows }).to_string()
                            } else if let Some(trace) = diagnosis {
                                let observed = trace.finish(hits.len());
                                serde_json::json!({ "results": hits, "diagnosis": observed }).to_string()
                            } else {
                                serde_json::json!({ "results": hits }).to_string()
                            }
                        }
                    })
                    .collect()
            });
                for r in &results {
                    out.write_all(r.as_bytes())?;
                    out.write_all(b"\n")?;
                }
                out.flush()?; // a window on disk survives an interrupt (the partial file of --resume)
            }
            out.flush()?; // Drop cleans the temp on an error return
            out.into_inner()?.sync_all()?; // ditto
            let dt = tq.elapsed().as_secs_f64();
            eprintln!(
                "open {:.2}s · {} queries in {:.2}s · {:.0} req/s",
                open_s,
                nq,
                dt,
                nq as f64 / dt
            );
            if without_address > 0 {
                // a blank address cell is data, not a format error: the row is kept with empty gp_* columns
                eprintln!(
                    "note: {without_address} of {nq} rows had no address and were left unanswered"
                );
            }
            let bad = malformed.load(std::sync::atomic::Ordering::Relaxed);
            if bad > 0 {
                // Semantic FAILURE: never publish, keep the previous output intact.
                // exit() skips Drop, so remove the temp explicitly here.
                tmp_guard.cleanup();
                eprintln!(
                    "warning: {bad} of {nq} input lines had no usable \"q\" field and returned \
                     nothing — batch input is JSONL, one {{\"q\": \"...\"}} object per line; \
                     output NOT written"
                );
                // the rows before the first bad line are answers: the partial file keeps them, cut at that line, so a
                // run of hours is not lost to one blank line at the end (revision 0.9.0)
                if partial.is_some() {
                    let first = first_bad.load(std::sync::atomic::Ordering::Relaxed);
                    match keep_first_lines(&tmp_out, first) {
                        Ok(0) | Err(_) => {
                            let _ = std::fs::remove_file(&tmp_out);
                        }
                        Ok(n) => eprintln!(
                            "{n} rows before line {} are kept in {}: fix that line and run again with --resume",
                            first + 1,
                            tmp_out.display()
                        ),
                    }
                }
                std::process::exit(1);
            }
            // success: atomic publish + parent-dir fsync so the rename is crash-durable, same as the
            // build/repack path.
            gridpin::builder::finalize_replace(&tmp_out, &output)?; // on Err, Drop cleans the temp
            tmp_guard.disarm(); // published; do not remove the renamed file
            if receipt {
                let path = gridpin::receipt::write(&index, &input, &output, nq, started)?;
                eprintln!("receipt: {}", path.display());
            }
        }
        Cmd::Cite { sheet, format } => {
            let idx = query::Index::open(&sheet)?;
            print!(
                "{}",
                gridpin::cite::render(&gridpin::cite::of(&idx, &sheet)?, &format)?
            );
        }
        Cmd::Serve {
            index,
            poi,
            also,
            also_after,
            sheets,
            host,
            port,
            threads,
            aliases,
            probe,
        } => {
            if let Some(url) = probe {
                return gridpin::serve::probe(&url);
            }
            let index = index.expect("clap requires the index without --probe");
            let mut engine = gridpin::serve::Engine::open(&index, poi.as_deref())?;
            for extra in &sheets {
                engine.add_sheet(extra)?;
            }
            if let Some(a) = &also {
                engine.addr.set_also(query::Index::open_also(a)?);
            }
            if let Some(a) = &also_after {
                engine.addr.set_also_after(query::Index::open_also(a)?);
            }
            engine.aliases = aliases
                .as_deref()
                .map(gridpin::aliases::Aliases::load)
                .transpose()?;
            let threads = threads.unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|c| c.get().saturating_sub(2).max(1))
                    .unwrap_or(2)
            });
            gridpin::serve::run(engine, index, &host, port, threads)?;
        }
        Cmd::Sheets { cmd } => sheets_command(cmd)?,
        Cmd::Nearest {
            rows,
            list,
            sheet,
            k,
            max_km,
            id_column,
            rows_column,
            list_column,
            output,
        } => {
            use gridpin::points;
            let idx = sheet
                .as_deref()
                .map(query::Index::open_address)
                .transpose()?;
            let geocoder = idx.as_ref().map(sheet_geocoder);
            let geocode = geocoder.as_ref().map(|f| f as &gridpin::points::Geocode);
            let table = points::read_points(&rows, rows_column.as_deref(), geocode)?;
            let targets = points::read_points(&list, list_column.as_deref(), geocode)?;
            let options = points::NearestOptions {
                k,
                max_km,
                id_column: id_column.as_deref(),
            };
            let mut counts = None;
            write_rows(output.as_deref(), |out| {
                counts = Some(points::write_nearest(&table, &targets, &options, out)?);
                Ok(())
            })?;
            if let Some(counts) = counts {
                if counts.list_without_point > 0 {
                    eprintln!(
                        "note: {} of {} rows of {} have no point and are not in the list",
                        counts.list_without_point,
                        counts.list,
                        list.display()
                    );
                }
                eprintln!(
                    "{} of {} rows have no answer ({} without a point)",
                    counts.rows_without_answer, counts.rows, counts.rows_without_point
                );
            }
        }
        Cmd::Join {
            rows,
            zones,
            sheet,
            column,
            property,
            output,
        } => {
            use gridpin::points;
            let idx = sheet
                .as_deref()
                .map(query::Index::open_address)
                .transpose()?;
            let geocoder = idx.as_ref().map(sheet_geocoder);
            let geocode = geocoder.as_ref().map(|f| f as &gridpin::points::Geocode);
            let table = points::read_points(&rows, column.as_deref(), geocode)?;
            let zones = points::read_zones(&std::fs::read_to_string(&zones)?)?;
            let wanted = (!property.is_empty()).then_some(property.as_slice());
            let mut counts = None;
            write_rows(output.as_deref(), |out| {
                counts = Some(points::write_join(&table, &zones, wanted, out)?);
                Ok(())
            })?;
            if let Some(counts) = counts {
                eprintln!(
                    "{} of {} rows in no zone ({} without a point)",
                    counts.rows_without_answer, counts.rows, counts.rows_without_point
                );
            }
        }
        Cmd::List {
            index,
            area,
            format,
            limit,
            output,
        } => {
            let format = gridpin::export::Format::choose(format.as_deref(), output.as_deref())?;
            let (idx, filter, empty) = open_area(&index, area)?;
            if empty {
                anyhow::bail!(
                    "list: name a street, a postcode, a commune or an area; for the whole sheet use export --all"
                );
            }
            let mut stats = None;
            write_rows(output.as_deref(), |out| {
                stats = Some(write_walk(&idx, &filter, format, limit, out)?);
                Ok(())
            })?;
            // nothing to report after `| head`: the walk did not finish
            if let Some(stats) = stats {
                report_walk(stats);
            }
        }
        Cmd::Slice { index, area, out } => {
            let mut parts = Vec::new();
            if let Some(b) = area.bbox {
                parts.push(format!(
                    "bbox {},{},{},{}",
                    b.min_lon, b.min_lat, b.max_lon, b.max_lat
                ));
            }
            if let Some((la, lo, m)) = area.near {
                parts.push(format!("near {la},{lo},{m}"));
            }
            for (name, v) in [
                ("street", &area.street),
                ("city", &area.city),
                ("postcode", &area.postcode),
                ("region", &area.region),
            ] {
                if let Some(v) = v {
                    parts.push(format!("{name} {v}"));
                }
            }
            if area.polygon.is_some() {
                parts.push("polygon".to_string());
            }
            let rule = parts.join("; ");
            let (_idx, filter, empty) = open_area(&index, area)?;
            if empty {
                anyhow::bail!("slice: name the area: --bbox, --region, --postcode, --city, --near or --polygon");
            }
            let n = gridpin::slice::slice(&index, &filter, &rule, &out)?;
            eprintln!("{n} houses -> {}", out.display());
        }
        Cmd::Export {
            index,
            area,
            all,
            format,
            output,
        } => {
            let format = gridpin::export::Format::choose(format.as_deref(), Some(&output))?;
            let (idx, filter, empty) = open_area(&index, area)?;
            if empty != all {
                anyhow::bail!("export: name a region, a box or a polygon, or say --all, not both");
            }
            let mut stats = None;
            write_rows(Some(&output), |out| {
                stats = Some(write_walk(&idx, &filter, format, None, out)?);
                Ok(())
            })?;
            // nothing to report after `| head`: the walk did not finish
            if let Some(stats) = stats {
                report_walk(stats);
            }
        }
        Cmd::Sample {
            index,
            area,
            n,
            seed,
            format,
            output,
        } => {
            let format = gridpin::export::Format::choose(format.as_deref(), output.as_deref())?;
            let (idx, filter, _) = open_area(&index, area)?;
            let seed = seed.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |t| t.as_nanos() as u64)
            });
            let (rows, stats) = idx.sample_houses(&filter, n, seed);
            write_rows(output.as_deref(), |out| {
                let provenance = gridpin::export::Provenance::of(&idx);
                let mut writer = gridpin::export::Writer::new(out, format, &provenance)?;
                for row in &rows {
                    writer.write(row)?;
                }
                writer.finish()?;
                Ok(())
            })?;
            eprintln!("{} addresses out of {}", rows.len(), stats.kept);
        }
        Cmd::Stats {
            index,
            area,
            format,
            output,
        } => {
            let geojson =
                match gridpin::export::Format::choose(format.as_deref(), output.as_deref())? {
                    gridpin::export::Format::Csv => false,
                    gridpin::export::Format::GeoJson => true,
                    _ => anyhow::bail!("stats: use csv or geojson"),
                };
            let (idx, filter, _) = open_area(&index, area)?;
            let mut stats = None;
            write_rows(output.as_deref(), |out| {
                stats = Some(write_stats(&idx, &filter, geojson, out)?);
                Ok(())
            })?;
            // nothing to report after `| head`: the walk did not finish
            if let Some(stats) = stats {
                report_walk(stats);
            }
        }
        Cmd::Extract {
            index,
            text,
            input,
            column,
            verify,
        } => {
            let idx = query::Index::open_address(&index)?;
            let mut out = std::io::BufWriter::new(std::io::stdout().lock());
            let mut emit = |row: Option<usize>, doc: &str| -> anyhow::Result<()> {
                let (found, stats) = guarded(|| gridpin::extract::extract(&idx, doc, verify))?;
                if stats.capped {
                    eprintln!(
                        "extract: {} windows asked, the rest of the text skipped (cap)",
                        gridpin::extract::MAX_WINDOWS
                    );
                }
                for f in found {
                    let mut v = serde_json::to_value(&f)?;
                    if let Some(r) = row {
                        v["row"] = serde_json::json!(r);
                    }
                    writeln!(out, "{v}")?;
                }
                Ok(())
            };
            match (text, input) {
                (Some(path), None) => {
                    let doc = if path.as_os_str() == "-" {
                        let mut s = String::new();
                        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
                        s
                    } else {
                        std::fs::read_to_string(&path)
                            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?
                    };
                    emit(None, &doc)?;
                }
                (None, Some(path)) => {
                    let mut reader = csv::ReaderBuilder::new()
                        .flexible(false)
                        .from_reader(gridpin::tabular::open_table(&path)?);
                    let headers = reader.headers()?.clone();
                    let at = *gridpin::tabular::query_columns(&headers, column.as_deref())?
                        .first()
                        .ok_or_else(|| anyhow::anyhow!("no text column"))?;
                    for (row, rec) in reader.records().enumerate() {
                        emit(Some(row), rec?.get(at).unwrap_or(""))?;
                    }
                }
                _ => anyhow::bail!("give --text FILE or --input CSV --column NAME"),
            }
            out.flush()?;
        }
        Cmd::Cluster {
            input,
            k,
            max_size,
            seed,
            out,
            geojson,
        } => {
            let mut reader =
                csv::ReaderBuilder::new().from_reader(gridpin::tabular::open_table(&input)?);
            let headers = reader.headers()?.clone();
            // the user's own lat, lon first; else the answer columns of `gridpin batch` (gp_lat, gp_lon), so the
            // documented `batch -> cluster` runs on batch's output as it is (revision 0.9.0)
            let column = |own: &[&str], answer: &str| {
                gridpin::tabular::coordinate_column(&headers, own)
                    .or_else(|| gridpin::tabular::coordinate_column(&headers, &[answer]))
            };
            let lat_at = column(&["lat", "latitude"], "gp_lat").ok_or_else(|| {
                anyhow::anyhow!("cluster: no lat column (lat, latitude or gp_lat)")
            })?;
            let lon_at = column(&["lon", "lng", "longitude"], "gp_lon").ok_or_else(|| {
                anyhow::anyhow!("cluster: no lon column (lon, lng, longitude or gp_lon)")
            })?;
            let records: Vec<csv::StringRecord> = reader.records().collect::<Result<_, _>>()?;
            // a row without a point (an address batch did not answer) stays in the output with empty zone cells
            let point_of = |r: &csv::StringRecord| {
                let n = |at: usize| {
                    r.get(at)
                        .and_then(|v| v.trim().parse::<f64>().ok())
                        .filter(|v| v.is_finite())
                };
                n(lat_at)
                    .zip(n(lon_at))
                    .filter(|(lat, lon)| lat.abs() <= 90.0 && lon.abs() <= 180.0)
            };
            let row_points: Vec<Option<(f64, f64)>> = records.iter().map(point_of).collect();
            let points: Vec<(f64, f64)> = row_points.iter().flatten().copied().collect();
            let without = records.len() - points.len();
            if points.is_empty() {
                anyhow::bail!("cluster: no row has a point ({} rows read)", records.len());
            }
            let z = gridpin::cluster::cluster(&points, k, max_size, seed)?;
            let mut w = csv::Writer::from_path(&out)?;
            let mut head: Vec<&str> = headers.iter().collect();
            head.extend([
                "gp_cluster",
                "gp_cluster_lat",
                "gp_cluster_lon",
                "gp_cluster_size",
            ]);
            w.write_record(&head)?;
            let mut zone = z.of.iter();
            for (r, p) in records.iter().zip(&row_points) {
                let mut row: Vec<String> = r.iter().map(String::from).collect();
                match p.and_then(|_| zone.next()) {
                    Some(c) => {
                        row.push((c + 1).to_string());
                        row.push(format!("{:.7}", z.centres[*c].0));
                        row.push(format!("{:.7}", z.centres[*c].1));
                        row.push(z.sizes[*c].to_string());
                    }
                    None => row.extend(std::iter::repeat_n(String::new(), 4)),
                }
                w.write_record(&row)?;
            }
            w.flush()?;
            if let Some(path) = geojson {
                std::fs::write(&path, gridpin::cluster::hulls(&points, &z).to_string())?;
            }
            if without > 0 {
                eprintln!("{without} rows without a point were left out of the zones (their zone cells are empty)");
            }
            eprintln!(
                "{} points in {} zones",
                points.len(),
                z.sizes.iter().filter(|s| **s > 0).count()
            );
        }
        Cmd::Format {
            index, q, out, xml, ..
        } if out == "iso20022" => {
            let idx = query::Index::open_address(&index)?;
            let hits = guarded(|| query::query_cascade(&idx, None, &q, 1))?;
            let o = gridpin::iso20022::build(&idx, &q, hits.first());
            if xml {
                println!("{}", gridpin::iso20022::xml(&o.adr));
            } else {
                println!("{}", gridpin::iso20022::json(&o, true));
            }
        }
        Cmd::Format { out, .. } if out != "postal" => {
            anyhow::bail!("--out {out}: postal or iso20022")
        }
        Cmd::Format { xml: true, .. } => anyhow::bail!("--xml goes with --out iso20022"),
        Cmd::Format {
            index, q, layout, ..
        } => {
            let layout = gridpin::extras::postal_layout_key(&layout).ok_or_else(|| {
                anyhow::anyhow!(
                    "--layout {layout}: unknown; known: {}",
                    gridpin::extras::postal_layouts().join(", ")
                )
            })?;
            let idx = query::Index::open_address(&index)?;
            let hits = guarded(|| query::query_cascade(&idx, None, &q, 1))?;
            match hits.first() {
                // a weak answer is a guess ("350 Fifth Avenue, New York" -> York, Alabama): printed as an address it
                // would go on a label with nothing to say so (ANSWERS §298)
                Some(h) if query::hit_is_weak(h) => anyhow::bail!(
                    "no confirmed address for {q:?} in this sheet: the best answer is a guess ({} {}, {}); \
                     `gridpin query` shows it",
                    h.housenumber.as_deref().unwrap_or(""),
                    h.street,
                    h.commune
                ),
                Some(h) => println!(
                    "{}",
                    gridpin::extras::postal(
                        h,
                        Some(gridpin::extras::postal_layout_of(layout, &idx))
                    )
                ),
                None => anyhow::bail!("no answer for {q:?} in this sheet"),
            }
        }
        Cmd::Grammar { index } => {
            let idx = query::Index::open_address(&index)?;
            let name = index
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("sheet");
            println!(
                "{}",
                serde_json::to_string_pretty(&gridpin::grammar::declared(&idx, name)?)?
            );
        }
        Cmd::Postcodes { index, res, out } => {
            let idx = query::Index::open_address(&index)?;
            let fc = gridpin::postcodes::postcodes(&idx, res)?;
            std::fs::write(&out, serde_json::to_string(&fc)?)?;
            eprintln!(
                "{} postcodes",
                fc["features"].as_array().map_or(0, Vec::len)
            );
        }
        Cmd::Explain { index, q, format } => {
            let idx = query::Index::open_address(&index)?;
            let e = guarded(|| gridpin::explain::explain(&idx, &q))?;
            match format.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(&e)?),
                "text" => print!("{}", gridpin::explain::text(&e)),
                other => anyhow::bail!("--format {other}: text or json"),
            }
        }
        Cmd::Exists { index, q } => {
            let idx = query::Index::open_address(&index)?;
            let e = guarded(|| gridpin::exists::judge(&idx, &q))?;
            println!("{}", serde_json::to_string(&e)?);
        }
        Cmd::Parse { index, q, poi } => {
            let idx = query::Index::open_address(&index)?;
            let poi_idx = poi.map(|p| query::Index::open(&p)).transpose()?;
            let reading = guarded(|| gridpin::compare::read(&idx, poi_idx.as_ref(), &q))?;
            println!("{}", serde_json::to_string(&reading)?);
        }
        Cmd::Same {
            index,
            lines,
            pairs,
            poi,
        } => {
            let idx = query::Index::open_address(&index)?;
            let poi_idx = poi.map(|p| query::Index::open(&p)).transpose()?;
            if !pairs {
                let same = guarded(|| {
                    gridpin::compare::same_address(&idx, poi_idx.as_ref(), &lines[0], &lines[1])
                })?;
                println!("{}", serde_json::to_string(&same)?);
            } else {
                use rayon::prelude::*;
                let input = std::fs::read_to_string(&lines[0])?;
                let rows: Vec<&str> = input.lines().collect();
                let out: Vec<String> = rows
                    .par_iter()
                    .map(|line| {
                        let pair: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
                        let (Some(a), Some(b)) = (pair["a"].as_str(), pair["b"].as_str()) else {
                            return serde_json::json!({"error": "expected {\"a\": ..., \"b\": ...}"}).to_string();
                        };
                        match guarded(|| gridpin::compare::same_address(&idx, poi_idx.as_ref(), a, b)) {
                            Ok(same) => serde_json::to_string(&same).unwrap_or_default(),
                            Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
                        }
                    })
                    .collect();
                std::fs::write(&lines[1], out.join("\n") + "\n")?;
            }
        }
        Cmd::Bench {
            index,
            input,
            column,
            lat_column,
            lon_column,
            min_match_rate,
            output,
        } => {
            use rayon::prelude::*;
            let idx = query::Index::open_address(&index)?;
            let mut reader = csv::ReaderBuilder::new()
                .flexible(false)
                .from_reader(gridpin::tabular::open_table(&input)?);
            let headers = reader.headers()?.clone();
            let cols = gridpin::tabular::query_columns(&headers, column.as_deref())?;
            let at = |name: Option<&str>, names: &[&str], what: &str| -> anyhow::Result<usize> {
                match name {
                    Some(n) => Ok(*gridpin::tabular::query_columns(&headers, Some(n))?
                        .first()
                        .ok_or_else(|| anyhow::anyhow!("no {what} column"))?),
                    None => gridpin::tabular::coordinate_column(&headers, names).ok_or_else(|| {
                        anyhow::anyhow!("no {what} column: name it with --{what}-column")
                    }),
                }
            };
            let (lat_at, lon_at) = (
                at(lat_column.as_deref(), &gridpin::tabular::LAT_NAMES, "lat")?,
                at(lon_column.as_deref(), &gridpin::tabular::LON_NAMES, "lon")?,
            );
            let mut rows = Vec::new();
            for (n, record) in reader.records().enumerate() {
                let record = record?;
                let num = |i: usize| record.get(i).and_then(|s| s.trim().parse::<f64>().ok());
                let truth = match (num(lat_at), num(lon_at)) {
                    (Some(lat), Some(lon))
                        if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) =>
                    {
                        (lat, lon)
                    }
                    _ => anyhow::bail!(
                        "row {}: the true lat, lon are not two numbers in range",
                        n + 2
                    ),
                };
                rows.push((
                    record.clone(),
                    gridpin::tabular::query_text(&record, &cols),
                    truth,
                ));
            }
            let answers: Vec<Option<query::Hit>> = rows
                .par_iter()
                .map(|(_, q, _)| {
                    if q.is_empty() {
                        None
                    } else {
                        panic_safe(|| query::query_cascade(&idx, None, q, 1))
                            .into_iter()
                            .next()
                    }
                })
                .collect();
            let pairs: Vec<(Option<&query::Hit>, (f64, f64))> = answers
                .iter()
                .zip(&rows)
                .map(|(a, (_, _, t))| (a.as_ref(), *t))
                .collect();
            let summary = gridpin::bench::summarize(&pairs);
            if let Some(path) = output {
                let mut w = csv::Writer::from_path(&path)?;
                let mut head: Vec<&str> = headers.iter().collect();
                head.extend(gridpin::tabular::GP_COLUMNS);
                head.push("gp_error_m");
                w.write_record(&head)?;
                for ((record, _, truth), hit) in rows.iter().zip(&answers) {
                    let (cells, err) = match hit {
                        Some(h) => (
                            gridpin::tabular::hit_cells(h).to_vec(),
                            format!("{:.1}", gridpin::codes::haversine_m(*truth, (h.lat, h.lon))),
                        ),
                        None => {
                            let mut c = vec![String::new(); gridpin::tabular::GP_COLUMNS.len()];
                            *c.last_mut().expect("columns") = "U".into();
                            (c, String::new())
                        }
                    };
                    w.write_record(
                        record
                            .iter()
                            .chain(cells.iter().map(String::as_str))
                            .chain([err.as_str()]),
                    )?;
                }
                w.flush()?;
            }
            println!("{}", serde_json::to_string_pretty(&summary)?);
            if let Some(min) = min_match_rate {
                if summary.match_rate < min {
                    eprintln!(
                        "bench: match rate {:.3} is below --min-match-rate {min}",
                        summary.match_rate
                    );
                    std::process::exit(1);
                }
            }
        }
    }
    Ok(())
}

/// "<output>.partial": where a JSONL batch writes until it finishes.
fn partial_path(output: &std::path::Path) -> std::path::PathBuf {
    let mut name = output.as_os_str().to_os_string();
    name.push(".partial");
    std::path::PathBuf::from(name)
}

/// Cut a partial file back to its last whole line (an interrupt can stop in the middle of one) and count its lines.
fn keep_whole_lines(path: &std::path::Path) -> anyhow::Result<u64> {
    let bytes = std::fs::read(path)?;
    let whole = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    if whole < bytes.len() {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)?
            .set_len(whole as u64)?;
    }
    Ok(bytes[..whole].iter().filter(|&&b| b == b'\n').count() as u64)
}

/// Cut a partial file to its first `n` lines and count what is left (fewer when the file is shorter).
fn keep_first_lines(path: &std::path::Path, n: u64) -> anyhow::Result<u64> {
    let bytes = std::fs::read(path)?;
    let mut left = 0u64;
    let mut end = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if left == n {
            break;
        }
        if *b == b'\n' {
            left += 1;
            end = i + 1;
        }
    }
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_len(end as u64)?;
    Ok(left)
}

#[cfg(test)]
mod tests {
    use super::{panic_safe, Cli, Cmd};
    use clap::Parser;

    #[test]
    fn panic_safe_returns_default_on_panic_never_aborts() {
        // the CLI query/reverse/batch paths wrap the fst read in panic_safe, so a
        // corrupt-but-openable sheet that panics INSIDE the fst crate yields empty results and a
        // clean exit instead of aborting the process (which has no other panic boundary).
        assert_eq!(
            panic_safe(|| vec![1, 2, 3]),
            vec![1, 2, 3],
            "the happy path is untouched"
        );
        let caught: Vec<i32> = panic_safe(|| panic!("simulated fst traversal panic"));
        assert!(
            caught.is_empty(),
            "a panic is caught and yields the default, not an abort"
        );
    }

    #[test]
    fn query_near_parser_accepts_south_and_west_coordinates() {
        for (raw, expected) in [("-33.9,-70.7", (-33.9, -70.7)), ("48.8,-1.5", (48.8, -1.5))] {
            let cli =
                Cli::try_parse_from(["gridpin", "query", "sheet.bin", "markt", "--near", raw])
                    .expect("valid signed LAT,LON must parse");
            let Cmd::Query { near, .. } = cli.cmd else {
                panic!("query command expected");
            };
            assert_eq!(near, Some(expected));
        }
    }

    #[test]
    fn query_near_parser_rejects_malformed_or_non_wgs84_values() {
        for raw in [
            "48.8",
            "48.8,1.5,2",
            ",1.5",
            "48.8,",
            "nan,1",
            "48,inf",
            "91,1",
            "48,181",
        ] {
            assert!(
                Cli::try_parse_from(["gridpin", "query", "sheet.bin", "markt", "--near", raw])
                    .is_err(),
                "{raw} must be rejected"
            );
        }
    }
}
