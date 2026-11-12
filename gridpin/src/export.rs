//! Writing a sheet's houses to a file: CSV, JSONL, GeoJSONL, GeoJSON, GPX.
//!
//! Every format opens with the licence and attribution of the sheet, because a copy of the data must
//! carry them (CC BY, ODbL and the other source terms a sheet's `gridpin meta` names). Parquet is not here:
//! the engine stays free of the `parquet` and `arrow` crates, and DuckDB turns the CSV into Parquet or
//! GeoParquet in one statement.

use std::io::Write;
use std::path::Path;

use anyhow::{bail, Result};
use serde_json::json;

use crate::query::{HouseRow, Index};

/// Columns of a CSV row.
pub const COLUMNS: [&str; 7] = [
    "lat",
    "lon",
    "street",
    "housenumber",
    "postcode",
    "commune",
    "region",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Csv,
    Jsonl,
    GeoJsonl,
    GeoJson,
    Gpx,
}

impl Format {
    pub fn parse(name: &str) -> Result<Format> {
        match name.to_ascii_lowercase().as_str() {
            "csv" => Ok(Format::Csv),
            "jsonl" => Ok(Format::Jsonl),
            "geojsonl" | "geojsonseq" => Ok(Format::GeoJsonl),
            "geojson" => Ok(Format::GeoJson),
            "gpx" => Ok(Format::Gpx),
            other => bail!("unknown format {other:?}: use csv, jsonl, geojsonl, geojson or gpx"),
        }
    }

    /// `--format`, else the extension of the output file, else CSV.
    pub fn choose(explicit: Option<&str>, output: Option<&Path>) -> Result<Format> {
        if let Some(name) = explicit {
            return Format::parse(name);
        }
        let extension = output
            .and_then(Path::extension)
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            None | Some("csv") => Ok(Format::Csv),
            Some("jsonl") => Ok(Format::Jsonl),
            Some("geojsonl") | Some("geojsonseq") => Ok(Format::GeoJsonl),
            Some("geojson") => Ok(Format::GeoJson),
            Some("gpx") => Ok(Format::Gpx),
            // a file named .parquet that holds CSV would fail far from here
            Some("parquet") => bail!(
                "the engine does not write Parquet: write CSV and let DuckDB convert it (see the documentation)"
            ),
            Some(other) => bail!("unknown output extension .{other}: give --format csv, jsonl, geojsonl, geojson or gpx"),
        }
    }
}

/// What a copy of the data has to say about itself, from the sheet's own record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub license: String,
    pub attribution: String,
    pub source_release: String,
    pub country: String,
}

impl Provenance {
    pub fn of(index: &Index) -> Provenance {
        let meta = |key: &str| {
            index
                .meta()
                .iter()
                .find(|(k, _)| k.as_str() == key)
                .map(|(_, v)| one_line(v))
                .unwrap_or_default()
        };
        let attribution = match meta("attribution") {
            a if a.is_empty() => meta("sources"),
            a => a,
        };
        Provenance {
            license: meta("license"),
            attribution,
            source_release: meta("source_release"),
            country: meta("country"),
        }
    }
}

/// A value of the sheet's record may hold line breaks; a header line must not.
fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn properties(row: &HouseRow) -> serde_json::Value {
    let mut p = json!({
        "street": row.street, "housenumber": row.housenumber, "postcode": row.postcode, "commune": row.commune,
    });
    if let Some(region) = &row.region {
        p["region"] = json!(region);
    }
    p
}

fn feature(row: &HouseRow) -> serde_json::Value {
    json!({
        "type": "Feature",
        "geometry": { "type": "Point", "coordinates": [row.lon, row.lat] },
        "properties": properties(row),
    })
}

/// Writes rows one by one, so a whole country takes no more memory than one row.
pub struct Writer<W: Write> {
    out: W,
    format: Format,
    rows: u64,
}

impl<W: Write> Writer<W> {
    /// Write the head of the format, with the licence and attribution in it.
    pub fn new(mut out: W, format: Format, provenance: &Provenance) -> Result<Writer<W>> {
        let Provenance {
            license,
            attribution,
            source_release,
            country,
        } = provenance;
        match format {
            Format::Csv => {
                writeln!(out, "# license: {license}")?;
                writeln!(out, "# attribution: {attribution}")?;
                writeln!(out, "{}", COLUMNS.join(","))?;
            }
            Format::Jsonl => writeln!(
                out,
                "{}",
                json!({ "license": license, "attribution": attribution, "source_release": source_release, "country": country })
            )?,
            // a feature without geometry: valid GeoJSON that map tools pass over
            Format::GeoJsonl => writeln!(
                out,
                "{}",
                json!({ "type": "Feature", "geometry": null, "properties": {
                    "license": license, "attribution": attribution, "source_release": source_release, "country": country
                } })
            )?,
            Format::GeoJson => write!(
                out,
                "{{\"type\":\"FeatureCollection\",\"license\":{},\"attribution\":{},\"source_release\":{},\"features\":[",
                json!(license),
                json!(attribution),
                json!(source_release)
            )?,
            Format::Gpx => write!(
                out,
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gpx version=\"1.1\" creator=\"gridpin\" \
                 xmlns=\"http://www.topografix.com/GPX/1/1\">\n<metadata><desc>license: {}; attribution: {}; \
                 source release: {}</desc></metadata>\n",
                escape_xml(license),
                escape_xml(attribution),
                escape_xml(source_release)
            )?,
        }
        Ok(Writer {
            out,
            format,
            rows: 0,
        })
    }

    pub fn write(&mut self, row: &HouseRow) -> Result<()> {
        match self.format {
            Format::Csv => {
                let mut line = csv::WriterBuilder::new()
                    .terminator(csv::Terminator::Any(b'\n'))
                    .from_writer(Vec::with_capacity(96));
                line.write_record([
                    row.lat.to_string().as_str(),
                    row.lon.to_string().as_str(),
                    row.street.as_str(),
                    row.housenumber.as_str(),
                    row.postcode.as_str(),
                    row.commune.as_str(),
                    row.region.as_deref().unwrap_or(""),
                ])?;
                self.out.write_all(&line.into_inner()?)?;
            }
            Format::Jsonl => writeln!(self.out, "{}", serde_json::to_string(row)?)?,
            Format::GeoJsonl => writeln!(self.out, "{}", feature(row))?,
            Format::GeoJson => {
                if self.rows > 0 {
                    self.out.write_all(b",\n")?;
                }
                write!(self.out, "{}", feature(row))?;
            }
            Format::Gpx => {
                let street = [row.housenumber.as_str(), row.street.as_str()]
                    .iter()
                    .copied()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                writeln!(
                    self.out,
                    "<wpt lat=\"{}\" lon=\"{}\"><name>{}, {}</name></wpt>",
                    row.lat,
                    row.lon,
                    escape_xml(&street),
                    escape_xml(&row.commune)
                )?;
            }
        }
        self.rows += 1;
        Ok(())
    }

    /// Close the format and hand back the output.
    pub fn finish(mut self) -> Result<W> {
        match self.format {
            Format::GeoJson => self.out.write_all(b"]}\n")?,
            Format::Gpx => self.out.write_all(b"</gpx>\n")?,
            _ => {}
        }
        self.out.flush()?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance() -> Provenance {
        Provenance {
            license: "CC BY 4.0".into(),
            attribution: "Fuente & <datos>".into(),
            source_release: "2026-01".into(),
            country: "es".into(),
        }
    }

    fn row(region: Option<&str>) -> HouseRow {
        HouseRow {
            lat: 43.73,
            lon: -7.42,
            street: "Calle, \"Mayor\"".into(),
            housenumber: "12bis".into(),
            postcode: "28001".into(),
            commune: "Sant & Pere".into(),
            region: region.map(String::from),
        }
    }

    fn written(format: Format, rows: &[HouseRow]) -> String {
        let mut w = Writer::new(Vec::new(), format, &provenance()).unwrap();
        for r in rows {
            w.write(r).unwrap();
        }
        String::from_utf8(w.finish().unwrap()).unwrap()
    }

    #[test]
    fn csv_opens_with_the_licence_and_quotes_what_needs_quoting() {
        let text = written(Format::Csv, &[row(Some("Madrid"))]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "# license: CC BY 4.0");
        assert_eq!(lines[1], "# attribution: Fuente & <datos>");
        assert_eq!(
            lines[2],
            "lat,lon,street,housenumber,postcode,commune,region"
        );
        assert_eq!(
            lines[3],
            "43.73,-7.42,\"Calle, \"\"Mayor\"\"\",12bis,28001,Sant & Pere,Madrid"
        );
    }

    #[test]
    fn jsonl_opens_with_the_record_and_each_row_is_an_object() {
        let text = written(Format::Jsonl, &[row(None)]);
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["license"], "CC BY 4.0");
        assert_eq!(lines[0]["attribution"], "Fuente & <datos>");
        assert_eq!(lines[1]["housenumber"], "12bis");
        assert!(lines[1].get("region").is_none());
    }

    #[test]
    fn geojsonl_is_one_feature_a_line_with_the_licence_on_a_feature_without_geometry() {
        let text = written(Format::GeoJsonl, &[row(None)]);
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["geometry"], serde_json::Value::Null);
        assert_eq!(lines[0]["properties"]["license"], "CC BY 4.0");
        assert_eq!(lines[1]["type"], "Feature");
        // longitude first, as GeoJSON says
        assert_eq!(lines[1]["geometry"]["coordinates"][0], -7.42);
        assert_eq!(lines[1]["geometry"]["coordinates"][1], 43.73);
    }

    #[test]
    fn geojson_is_one_collection_also_when_empty_and_carries_the_licence() {
        for rows in [vec![], vec![row(None), row(Some("R"))]] {
            let text = written(Format::GeoJson, &rows);
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["type"], "FeatureCollection");
            assert_eq!(v["license"], "CC BY 4.0");
            assert_eq!(v["features"].as_array().unwrap().len(), rows.len());
        }
    }

    #[test]
    fn gpx_escapes_text_and_carries_the_licence() {
        let text = written(Format::Gpx, &[row(None)]);
        assert!(
            text.contains("license: CC BY 4.0; attribution: Fuente &amp; &lt;datos&gt;"),
            "{text}"
        );
        assert!(text.contains("<wpt lat=\"43.73\" lon=\"-7.42\">"), "{text}");
        assert!(
            text.contains("12bis Calle, &quot;Mayor&quot;, Sant &amp; Pere"),
            "{text}"
        );
        assert!(text.trim_end().ends_with("</gpx>"), "{text}");
    }

    #[test]
    fn the_format_follows_the_flag_then_the_extension_then_csv() {
        assert_eq!(
            Format::choose(Some("gpx"), Some(Path::new("a.csv"))).unwrap(),
            Format::Gpx
        );
        assert_eq!(
            Format::choose(None, Some(Path::new("a.GeoJSON"))).unwrap(),
            Format::GeoJson
        );
        assert_eq!(
            Format::choose(None, Some(Path::new("a.geojsonl"))).unwrap(),
            Format::GeoJsonl
        );
        assert_eq!(
            Format::choose(None, Some(Path::new("a.jsonl"))).unwrap(),
            Format::Jsonl
        );
        assert_eq!(
            Format::choose(None, Some(Path::new("a.csv"))).unwrap(),
            Format::Csv
        );
        assert_eq!(
            Format::choose(Some("csv"), Some(Path::new("a.txt"))).unwrap(),
            Format::Csv
        );
        let err = Format::choose(None, Some(Path::new("a.parquet")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("DuckDB"), "{err}");
        assert!(Format::choose(None, Some(Path::new("a.txt"))).is_err());
        assert_eq!(Format::choose(None, None).unwrap(), Format::Csv);
        assert!(Format::choose(Some("parquet"), None).is_err());
    }

    #[test]
    fn a_line_break_in_the_record_never_breaks_the_header() {
        assert_eq!(one_line("a\nb:  c"), "a b: c");
    }
}
