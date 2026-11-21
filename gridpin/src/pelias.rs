//! The Pelias dialect of `gridpin serve` under `/pelias/`: `/pelias/v1/autocomplete`, `/search`, `/reverse` and
//! `/place`, in the GeoJSON shape Pelias writes, so a client written for Pelias (Headway, Mobilizon's Pelias provider)
//! only needs a new base URL: `http://host:8080/pelias`.
//!
//! What the answer holds: `label`, `name`, `housenumber` with `street`, `postalcode`, `locality`, `region`, `country`,
//! `country_code`, `layer` (`address`, `street`, `locality`), `confidence`, `match_type`, `accuracy`, `source`
//! `gridpin`; with `--ids`, `id` and `gid` (`gridpin:address:<number>`), which `/place?ids=` reads back. Without
//! `--ids` there is no `gid`: clients that key results by it (Headway skips such a result, Mobilizon gives every
//! event the same place) need the server started with `--ids`.
//! `/autocomplete` reads the street suggestions of `/v1/suggest` and answers each with its geocoded address, so a
//! result can be opened by its `gid`.

use serde_json::{json, Map, Value};

use crate::nominatim::{hit_by_number, place_name};
use crate::query::{self, Hit};
use crate::serve::{
    geocode, geocode_near, guarded, reverse_hits, Counters, Engine, Pick, Request, Response,
    MAX_LIMIT,
};

const DEFAULT_SIZE: usize = 10;
const GID_PREFIX: &str = "gridpin:address:";

/// `{"geocoding": {"errors": [...]}, "type": "FeatureCollection", "features": []}` with 400, as Pelias refuses.
fn refuse(message: &str) -> Response {
    Response::json(
        400,
        &json!({
            "geocoding": { "version": "0.2", "errors": [message] },
            "type": "FeatureCollection",
            "features": [],
        }),
    )
}

fn number(req: &Request, name: &str) -> Result<Option<f64>, Response> {
    req.param(name)
        .map(|v| v.trim().parse::<f64>())
        .transpose()
        .map_err(|_| refuse(&format!("invalid param '{name}': must be a number")))
}

fn size(req: &Request) -> Result<usize, Response> {
    match req.param("size") {
        None => Ok(DEFAULT_SIZE),
        Some(v) => v
            .trim()
            .parse::<i64>()
            .map(|n| n.clamp(1, MAX_LIMIT as i64) as usize)
            .map_err(|_| refuse("invalid param 'size': must be a whole number")),
    }
}

/// The Pelias layer of an answer.
pub(crate) fn layer(h: &Hit) -> &'static str {
    match h.precision {
        "house" | "interp" | "near" | "approximate" => "address",
        "city" => "locality",
        _ => "street",
    }
}

/// `layers=` as Pelias reads it: a list, where `coarse` means the places above streets.
fn layers(req: &Request) -> Vec<&'static str> {
    let mut out = Vec::new();
    for l in req.param("layers").unwrap_or("").split(',').map(str::trim) {
        match l {
            "address" => out.push("address"),
            "street" => out.push("street"),
            "locality" | "localadmin" | "county" | "region" | "coarse" => out.push("locality"),
            _ => {}
        }
    }
    out
}

fn put(o: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.is_empty() {
        o.insert(key.into(), json!(value));
    }
}

/// One answer as a Pelias feature.
pub(crate) fn feature(h: &Hit, country: &str, ids: bool) -> Value {
    let layer = layer(h);
    let number = h.housenumber.as_deref().unwrap_or("");
    let has_house = layer == "address" && !h.street.is_empty() && !number.is_empty();
    let name = match layer {
        "address" if has_house => format!("{number} {}", h.street),
        "locality" => h.commune.clone(),
        _ if !h.street.is_empty() => h.street.clone(),
        _ => place_name(h).to_string(),
    };
    let country_name = crate::country_names::name_of(country).unwrap_or("");
    let mut p = Map::new();
    if ids {
        let n = crate::locator::encode(h);
        p.insert("id".into(), json!(n.to_string()));
        p.insert("gid".into(), json!(format!("{GID_PREFIX}{n}")));
    }
    p.insert(
        "layer".into(),
        json!(if has_house || layer != "address" {
            layer
        } else {
            "street"
        }),
    );
    p.insert("source".into(), json!("gridpin"));
    put(&mut p, "name", &name);
    if has_house {
        put(&mut p, "housenumber", number);
    }
    if has_house || layer == "street" {
        put(&mut p, "street", &h.street);
    }
    put(&mut p, "postalcode", &h.postcode);
    put(&mut p, "locality", &h.commune);
    put(&mut p, "region", h.region.as_deref().unwrap_or(""));
    put(&mut p, "country", country_name);
    put(&mut p, "country_code", &country.to_ascii_uppercase());
    p.insert(
        "confidence".into(),
        json!((f64::from(h.confidence) * 100.0).round() / 100.0),
    );
    let match_type = match h.precision {
        "house" if !query::hit_is_weak(h) => "exact",
        "interp" => "interpolated",
        _ => "fallback",
    };
    p.insert("match_type".into(), json!(match_type));
    p.insert(
        "accuracy".into(),
        json!(if layer == "address" {
            "point"
        } else {
            "centroid"
        }),
    );
    let label = [name.as_str(), h.commune.as_str(), country_name]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    put(&mut p, "label", &label);
    json!({
        "type": "Feature",
        "geometry": { "type": "Point", "coordinates": [h.lon, h.lat] },
        "properties": Value::Object(p),
    })
}

fn collection(hits: &[Hit], pick: &Pick, ids: bool, query: Value) -> Response {
    Response::json(
        200,
        &json!({
            "geocoding": {
                "version": "0.2",
                "attribution": pick.attribution,
                "query": query,
                "engine": { "name": "gridpin", "version": env!("CARGO_PKG_VERSION") },
            },
            "type": "FeatureCollection",
            "features": hits.iter().map(|h| feature(h, pick.country, ids)).collect::<Vec<_>>(),
        }),
    )
}

/// `boundary.country`: the sheet's country by its two- or three-letter code; another country answers nothing.
fn other_country(engine: &Engine, req: &Request) -> bool {
    req.param("boundary.country").is_some_and(|c| {
        let c = c.trim().to_ascii_lowercase();
        let two = alpha2_of(&c).unwrap_or(c.as_str());
        !c.is_empty() && !engine.serves(two)
    })
}

/// ISO 3166 alpha-3 to alpha-2 for the countries of the sheets (Pelias takes either in `boundary.country`).
fn alpha2_of(code: &str) -> Option<&'static str> {
    Some(match code {
        "usa" => "us",
        "can" => "ca",
        "mex" => "mx",
        "bra" => "br",
        "aus" => "au",
        "deu" => "de",
        "esp" => "es",
        "fra" => "fr",
        "ita" => "it",
        "nld" => "nl",
        "srb" => "rs",
        "pol" => "pl",
        "mco" => "mc",
        _ => return None,
    })
}

fn text(req: &Request) -> Result<&str, Response> {
    let t = req
        .param("text")
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| refuse("invalid param 'text': text length, must be >0"))?;
    if t.len() > query::MAX_QUERY_BYTES {
        return Err(refuse(&format!(
            "invalid param 'text': over {} bytes",
            query::MAX_QUERY_BYTES
        )));
    }
    Ok(t)
}

fn focus(req: &Request) -> Result<Option<(f64, f64)>, Response> {
    match (
        number(req, "focus.point.lat")?,
        number(req, "focus.point.lon")?,
    ) {
        (Some(lat), Some(lon)) if lat.abs() <= 90.0 && lon.abs() <= 180.0 => Ok(Some((lat, lon))),
        (None, None) => Ok(None),
        _ => Err(refuse(
            "focus.point.lat and focus.point.lon: give both, within range",
        )),
    }
}

fn keep(hits: &mut Vec<Hit>, wanted: &[&str], size: usize) {
    hits.retain(|h| {
        !h.flags.contains(&"poi_layer") && (wanted.is_empty() || wanted.contains(&layer(h)))
    });
    hits.truncate(size);
}

/// `GET /pelias/v1/search?text=…`.
pub(crate) fn search(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let run = || -> Result<Response, Response> {
        let t = text(req)?;
        let size = size(req)?;
        let wanted = layers(req);
        let near = focus(req)?;
        if other_country(engine, req) {
            return Ok(collection(
                &[],
                &engine.picks()[0],
                engine.ids,
                json!({ "text": t }),
            ));
        }
        let k = if wanted.is_empty() { size } else { MAX_LIMIT };
        let (mut hits, pick) = match near {
            Some((lat, lon)) => {
                geocode_near(engine, counters, t, k, lat, lon, &[]).map_err(|e| refuse(&e))?
            }
            None => geocode(engine, counters, t, k, &[]),
        };
        keep(&mut hits, &wanted, size);
        Ok(collection(
            &hits,
            &pick,
            engine.ids,
            json!({ "text": t, "size": size }),
        ))
    };
    run().unwrap_or_else(|r| r)
}

/// `GET /pelias/v1/autocomplete?text=…`: the street suggestions of the typed start, each answered with its address.
pub(crate) fn autocomplete(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let run = || -> Result<Response, Response> {
        let t = text(req)?;
        let size = size(req)?;
        let wanted = layers(req);
        if other_country(engine, req) {
            return Ok(collection(
                &[],
                &engine.picks()[0],
                engine.ids,
                json!({ "text": t }),
            ));
        }
        let pick = engine.picks()[0];
        let suggestions = guarded(counters, Vec::new(), || engine.addr.suggest(t, size));
        let mut hits: Vec<Hit> = Vec::new();
        for s in suggestions {
            let (found, _) = geocode(engine, counters, &s.label, 1, &[]);
            if let Some(h) = found.into_iter().next() {
                if !hits
                    .iter()
                    .any(|x| x.lat == h.lat && x.lon == h.lon && x.street == h.street)
                {
                    hits.push(h);
                }
            }
        }
        if hits.is_empty() {
            // a whole address typed at once: the suggestions read a start, the engine reads the rest
            hits = geocode(engine, counters, t, size, &[]).0;
        }
        keep(&mut hits, &wanted, size);
        Ok(collection(
            &hits,
            &pick,
            engine.ids,
            json!({ "text": t, "size": size }),
        ))
    };
    run().unwrap_or_else(|r| r)
}

/// `GET /pelias/v1/reverse?point.lat=…&point.lon=…`.
pub(crate) fn reverse(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let run = || -> Result<Response, Response> {
        let (lat, lon) = match (number(req, "point.lat")?, number(req, "point.lon")?) {
            (Some(lat), Some(lon)) if lat.abs() <= 90.0 && lon.abs() <= 180.0 => (lat, lon),
            _ => {
                return Err(refuse(
                    "invalid param 'point.lat' or 'point.lon': both required, within range",
                ))
            }
        };
        let size = size(req)?;
        let wanted = layers(req);
        if other_country(engine, req) {
            return Ok(collection(
                &[],
                &engine.picks()[0],
                engine.ids,
                json!({ "point.lat": lat, "point.lon": lon }),
            ));
        }
        let k = if wanted.is_empty() { size } else { MAX_LIMIT };
        let (mut hits, pick) =
            reverse_hits(engine, counters, lat, lon, k, &[]).map_err(|e| refuse(&e))?;
        keep(&mut hits, &wanted, size);
        Ok(collection(
            &hits,
            &pick,
            engine.ids,
            json!({ "point.lat": lat, "point.lon": lon, "size": size }),
        ))
    };
    run().unwrap_or_else(|r| r)
}

/// `GET /pelias/v1/place?ids=gridpin:address:<number>,…`: the addresses the numbers name; another source's id answers
/// nothing.
pub(crate) fn place(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let ids = req.param("ids").unwrap_or("").trim();
    if ids.is_empty() {
        return refuse("invalid param 'ids': must be one or more gid");
    }
    let mut found = Vec::new();
    let mut pick = engine.picks()[0];
    for gid in ids.split(',').map(str::trim).take(MAX_LIMIT) {
        let Some(n) = gid
            .strip_prefix(GID_PREFIX)
            .and_then(|n| n.parse::<i64>().ok())
        else {
            continue;
        };
        if let Some((h, p)) = hit_by_number(engine, counters, n) {
            pick = p;
            found.push(h);
        }
    }
    collection(&found, &pick, engine.ids, json!({ "ids": ids }))
}
