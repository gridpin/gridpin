//! `gridpin batch in.geojson out.geojson --column address`: GeoJSON (a FeatureCollection) and GeoJSONL (one Feature
//! per line) as the input of `batch`, written back in the same form.
//!
//! The `properties` of each Feature stay as they are, and the answer is added to them as `gp_lat`, `gp_lon`,
//! `gp_precision`, `gp_match_level`, `gp_street`, `gp_housenumber`, `gp_commune`, `gp_postcode`, `gp_confidence`
//! (empty values without an answer). The geometry is kept, or replaced by the answer's point with
//! `--replace-geometry`. With `--reverse` the point asked is the geometry's own: a Point itself, any other geometry
//! the mean of its vertices (`gp_centroid: vertex-mean`, said so in the output, not a true centroid).

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};

use crate::query::Hit;

/// Is this path a GeoJSON input (by its extension)? `Some(true)` for GeoJSONL.
pub fn kind_of(path: &Path) -> Option<bool> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("geojson") => Some(false),
        Some("geojsonl") | Some("geojsons") => Some(true),
        _ => None,
    }
}

/// The mean of all vertices of a geometry, and whether it is a Point.
pub fn vertex_mean(g: &Value) -> Option<(f64, f64, bool)> {
    fn walk(v: &Value, acc: &mut (f64, f64, usize)) {
        match v.as_array() {
            Some(a) if a.len() >= 2 && a[0].is_number() && a[1].is_number() => {
                acc.0 += a[0].as_f64().unwrap_or(0.0);
                acc.1 += a[1].as_f64().unwrap_or(0.0);
                acc.2 += 1;
            }
            Some(a) => a.iter().for_each(|x| walk(x, acc)),
            None => {}
        }
    }
    let mut acc = (0.0, 0.0, 0);
    walk(&g["coordinates"], &mut acc);
    (acc.2 > 0).then(|| {
        (
            acc.1 / acc.2 as f64,
            acc.0 / acc.2 as f64,
            g["type"] == "Point",
        )
    })
}

fn answer_props(props: &mut Map<String, Value>, h: Option<&Hit>) {
    let s = |v: Option<String>| Value::from(v.unwrap_or_default());
    props.insert("gp_lat".into(), h.map_or(json!(""), |h| json!(h.lat)));
    props.insert("gp_lon".into(), h.map_or(json!(""), |h| json!(h.lon)));
    props.insert("gp_precision".into(), s(h.map(|h| h.precision.to_string())));
    props.insert(
        "gp_match_level".into(),
        s(h.map(|h| crate::query::match_level(h).0.to_string())),
    );
    props.insert("gp_street".into(), s(h.map(|h| h.street.clone())));
    props.insert(
        "gp_housenumber".into(),
        s(h.and_then(|h| h.housenumber.clone())),
    );
    props.insert("gp_commune".into(), s(h.map(|h| h.commune.clone())));
    props.insert("gp_postcode".into(), s(h.map(|h| h.postcode.clone())));
    props.insert(
        "gp_confidence".into(),
        h.map_or(json!(""), |h| json!(h.confidence)),
    );
}

/// What one Feature asks: a line from its properties, or (with `reverse`) its point.
pub enum Ask {
    Line(String),
    Point(f64, f64),
    Nothing,
}

pub struct Options<'a> {
    pub column: Option<&'a str>,
    pub reverse: bool,
    pub replace_geometry: bool,
}

fn ask_of(f: &Value, o: &Options) -> Ask {
    if o.reverse {
        return match vertex_mean(&f["geometry"]) {
            Some((lat, lon, _)) => Ask::Point(lat, lon),
            None => Ask::Nothing,
        };
    }
    let props = &f["properties"];
    let text = match o.column {
        Some(cols) => cols
            .split(',')
            .filter_map(|c| {
                props.get(c.trim()).and_then(|v| {
                    v.as_str()
                        .map(String::from)
                        .or_else(|| v.as_i64().map(|n| n.to_string()))
                })
            })
            .collect::<Vec<_>>()
            .join(", "),
        None => ["address", "query", "q"]
            .iter()
            .find_map(|k| props.get(*k).and_then(Value::as_str).map(String::from))
            .unwrap_or_default(),
    };
    if text.trim().is_empty() {
        Ask::Nothing
    } else {
        Ask::Line(text)
    }
}

fn answered(mut f: Value, h: Option<&Hit>, o: &Options) -> Value {
    if !f["properties"].is_object() {
        f["properties"] = json!({});
    }
    let geometry = f["geometry"].clone();
    let props = f["properties"].as_object_mut().expect("object");
    answer_props(props, h);
    if o.reverse && geometry["type"] != "Point" {
        props.insert("gp_centroid".into(), json!("vertex-mean"));
    }
    if o.replace_geometry {
        f["geometry"] = h.map_or(
            Value::Null,
            |h| json!({"type": "Point", "coordinates": [h.lon, h.lat]}),
        );
    }
    f
}

/// Answer every Feature of `input` into `out`; returns the number of Features.
pub fn run(
    input: &Path,
    out: &mut dyn Write,
    o: &Options,
    answer: impl Fn(&Ask) -> Option<Hit> + Sync,
) -> Result<u64> {
    use rayon::prelude::*;
    let lines = kind_of(input).ok_or_else(|| anyhow!("not a .geojson or .geojsonl file"))?;
    let features: Vec<Value> = if lines {
        let file = std::io::BufReader::new(std::fs::File::open(input)?);
        file.lines()
            .filter(|l| l.as_ref().map_or(true, |l| !l.trim().is_empty()))
            .map(|l| Ok(serde_json::from_str(&l?)?))
            .collect::<Result<_>>()?
    } else {
        let v: Value =
            serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(input)?))?;
        match v["type"].as_str() {
            Some("FeatureCollection") => v["features"].as_array().cloned().unwrap_or_default(),
            Some("Feature") => vec![v],
            _ => bail!("{}: a FeatureCollection or a Feature", input.display()),
        }
    };
    let done: Vec<Value> = features
        .into_par_iter()
        .map(|f| {
            let h = answer(&ask_of(&f, o));
            answered(f, h.as_ref(), o)
        })
        .collect();
    let n = done.len() as u64;
    if lines {
        for f in &done {
            writeln!(out, "{f}")?;
        }
    } else {
        writeln!(
            out,
            "{}",
            json!({"type": "FeatureCollection", "features": done})
        )?;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_point_of_a_polygon_is_the_mean_of_its_vertices() {
        let g = json!({"type": "Polygon", "coordinates": [[[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]]]});
        assert_eq!(vertex_mean(&g), Some((1.0, 1.0, false)));
        assert_eq!(
            vertex_mean(&json!({"type": "Point", "coordinates": [7.4, 43.7]})),
            Some((43.7, 7.4, true))
        );
    }
}
