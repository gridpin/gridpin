//! The BAN / Addok dialect of `gridpin serve` under `/ban/`: `/ban/search` and `/ban/reverse`, in the shape the French
//! national address service writes (Géoplateforme `geocodage/search` and `reverse`, read live on 2026-11-14), so a client
//! written for it (geopy `BANFrance`, Mobilizon, Addok clients) only needs a new base URL: `http://host:8080/ban`.
//!
//! Addok is an engine, not only France: the dialect answers on any sheet. What the answer holds: `label`, `score`,
//! `type` (`housenumber`, `street`, `municipality`), `name`, `housenumber` with `street` (never one without the
//! other), `postcode`, `city`, `id` with `--ids`, and `x`, `y` in Lambert-93 on the sheet of France only.
//! What it does not hold, on purpose: `citycode`, `context`, `importance` and `banId` are not kept in a sheet, so they
//! are not written; a `citycode=` filter is refused rather than ignored, since ignoring it answers another town.
//! Places of interest of a POI layer are not addresses and are left out. A weak answer never scores above 0.4: BAN
//! clients choose by `score`.

use serde_json::{json, Map, Value};

use crate::nominatim::place_name;
use crate::query::{self, Hit};
use crate::serve::{
    geocode, geocode_near, metres, reverse_hits, Counters, Engine, Pick, Request, Response,
    MAX_LIMIT,
};

/// The default `limit` of the Géoplateforme service.
const DEFAULT_LIMIT: usize = 10;
/// The highest score a weak answer gets.
const WEAK_SCORE: f32 = 0.4;
const TYPES: [&str; 4] = ["housenumber", "street", "locality", "municipality"];

/// `{"code": 400, "message": …, "detail": […]}`, as the Géoplateforme service refuses.
fn refuse(detail: &str) -> Response {
    Response::json(
        400,
        &json!({ "code": 400, "message": "Failed parsing query", "detail": [detail] }),
    )
}

fn number(req: &Request, name: &str) -> Result<Option<f64>, Response> {
    req.param(name)
        .map(|v| v.trim().parse::<f64>())
        .transpose()
        .map_err(|_| refuse(&format!("{name}: must be a number")))
}

/// `lat` with `lon` (or `lng`), together or not at all.
fn point(req: &Request) -> Result<Option<(f64, f64)>, Response> {
    let lon = match number(req, "lon")? {
        Some(v) => Some(v),
        None => number(req, "lng")?,
    };
    match (number(req, "lat")?, lon) {
        (None, None) => Ok(None),
        (Some(lat), Some(lon)) if lat.abs() <= 90.0 && lon.abs() <= 180.0 => Ok(Some((lat, lon))),
        (Some(_), Some(_)) => Err(refuse("lat: must be within ±90, lon within ±180")),
        _ => Err(refuse("lat and lon: give both or neither")),
    }
}

fn limit(req: &Request) -> Result<usize, Response> {
    match req.param("limit") {
        None => Ok(DEFAULT_LIMIT),
        Some(v) => v
            .trim()
            .parse::<i64>()
            .map(|n| n.clamp(1, MAX_LIMIT as i64) as usize)
            .map_err(|_| refuse("limit: must be a whole number")),
    }
}

/// The BAN type of an answer: an address of any kind is a `housenumber`, a town a `municipality`, else a `street`.
pub(crate) fn kind(h: &Hit) -> &'static str {
    match h.precision {
        "house" | "interp" | "near" | "approximate" => "housenumber",
        "city" => "municipality",
        _ => "street",
    }
}

fn put(o: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.is_empty() {
        o.insert(key.into(), json!(value));
    }
}

/// The score BAN clients choose by: the engine's confidence, never above [`WEAK_SCORE`] for a weak answer.
pub(crate) fn score(h: &Hit) -> f32 {
    if query::hit_is_weak(h) {
        h.confidence.min(WEAK_SCORE)
    } else {
        h.confidence
    }
}

/// One answer as a BAN feature.
pub(crate) fn feature(h: &Hit, country: &str, ids: bool, distance: Option<f64>) -> Value {
    let kind = kind(h);
    let number = h.housenumber.as_deref().unwrap_or("");
    let has_house = kind == "housenumber" && !h.street.is_empty() && !number.is_empty();
    let name = match kind {
        "housenumber" if has_house => format!("{number} {}", h.street),
        "municipality" => h.commune.clone(),
        _ if !h.street.is_empty() => h.street.clone(),
        _ => place_name(h).to_string(),
    };
    let label = [name.as_str(), h.postcode.as_str(), h.commune.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .fold(Vec::<&str>::new(), |mut acc, s| {
            if acc.last() != Some(&s) {
                acc.push(s);
            }
            acc
        })
        .join(" ");
    let mut p = Map::new();
    put(&mut p, "label", &label);
    p.insert("score".into(), json!(round4(score(h))));
    if has_house {
        put(&mut p, "housenumber", number);
    }
    if ids {
        put(&mut p, "id", &crate::locator::encode(h).to_string());
    }
    put(&mut p, "name", &name);
    put(&mut p, "postcode", &h.postcode);
    if country == "fr" {
        if let Some((x, y)) = crate::crs::project(2154, h.lat, h.lon) {
            p.insert("x".into(), json!(round2(x)));
            p.insert("y".into(), json!(round2(y)));
        }
    }
    put(&mut p, "city", &h.commune);
    put(
        &mut p,
        "type",
        if has_house || kind != "housenumber" {
            kind
        } else {
            "street"
        },
    );
    if has_house || kind == "street" {
        put(&mut p, "street", &h.street);
    }
    if let Some(d) = distance {
        p.insert("distance".into(), json!(d.round() as i64));
    }
    json!({
        "type": "Feature",
        "geometry": { "type": "Point", "coordinates": [h.lon, h.lat] },
        "properties": Value::Object(p),
    })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn round4(v: f32) -> f64 {
    (f64::from(v) * 10_000.0).round() / 10_000.0
}

fn collection(
    hits: &[Hit],
    pick: &Pick,
    ids: bool,
    query: Option<&str>,
    at: Option<(f64, f64)>,
) -> Response {
    let features: Vec<Value> = hits
        .iter()
        .map(|h| {
            let distance = at.map(|(lat, lon)| {
                h.distance_m
                    .unwrap_or_else(|| metres(lat, lon, h.lat, h.lon))
            });
            feature(h, pick.country, ids, distance)
        })
        .collect();
    let mut body = json!({ "type": "FeatureCollection", "features": features, "attribution": pick.attribution });
    if let Some(q) = query {
        body["query"] = json!(q);
    }
    Response::json(200, &body)
}

/// The `type=` filter, one of the BAN types.
fn type_filter(req: &Request) -> Result<Option<&str>, Response> {
    match req.param("type").map(str::trim).filter(|t| !t.is_empty()) {
        None => Ok(None),
        Some(t) if TYPES.contains(&t) => Ok(Some(t)),
        Some(t) => Err(refuse(&format!(
            "type: must be one of {} (got {t})",
            TYPES.join(", ")
        ))),
    }
}

/// What every BAN path refuses alike: a commune code the sheet cannot check, and an index other than addresses.
fn unsupported(req: &Request) -> Result<(), Response> {
    if req.param("citycode").is_some_and(|c| !c.trim().is_empty()) {
        return Err(refuse(
            "citycode: not supported, the sheet holds no INSEE codes; filter by postcode instead",
        ));
    }
    match req.param("index").map(str::trim) {
        None | Some("") | Some("address") => Ok(()),
        Some(other) => Err(refuse(&format!(
            "index: only address is served (got {other})"
        ))),
    }
}

fn keep(h: &Hit, kind_wanted: Option<&str>, postcode: Option<&str>) -> bool {
    !h.flags.contains(&"poi_layer")
        && kind_wanted.is_none_or(|t| kind(h) == t)
        && postcode.is_none_or(|p| h.postcode == p)
}

/// `GET /ban/search?q=…`: a search; `type` and `postcode` drop answers from the engine's longest list.
pub(crate) fn search(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let run = || -> Result<Response, Response> {
        let q = req
            .param("q")
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .ok_or_else(|| refuse("q: required param"))?;
        if q.len() > query::MAX_QUERY_BYTES {
            return Err(refuse(&format!("q: over {} bytes", query::MAX_QUERY_BYTES)));
        }
        unsupported(req)?;
        let limit = limit(req)?;
        let wanted = type_filter(req)?;
        let postcode = req
            .param("postcode")
            .map(str::trim)
            .filter(|p| !p.is_empty());
        let near = point(req)?;
        let k = if wanted.is_some() || postcode.is_some() {
            MAX_LIMIT
        } else {
            limit
        };
        let (mut hits, pick) = match near {
            Some((lat, lon)) => {
                geocode_near(engine, counters, q, k, lat, lon, &[]).map_err(|e| refuse(&e))?
            }
            None => geocode(engine, counters, q, k, &[]),
        };
        hits.retain(|h| keep(h, wanted, postcode));
        hits.truncate(limit);
        Ok(collection(&hits, &pick, engine.ids, Some(q), None))
    };
    run().unwrap_or_else(|r| r)
}

/// `GET /ban/reverse?lat=…&lon=…`: the nearest addresses, each with its `distance` in metres.
pub(crate) fn reverse(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let run = || -> Result<Response, Response> {
        let (lat, lon) = point(req)?.ok_or_else(|| refuse("lat and lon: required params"))?;
        unsupported(req)?;
        let limit = limit(req)?;
        let wanted = type_filter(req)?;
        let k = if wanted.is_some() { MAX_LIMIT } else { limit };
        let (mut hits, pick) =
            reverse_hits(engine, counters, lat, lon, k, &[]).map_err(|e| refuse(&e))?;
        hits.retain(|h| keep(h, wanted, None));
        hits.truncate(limit);
        Ok(collection(&hits, &pick, engine.ids, None, Some((lat, lon))))
    };
    run().unwrap_or_else(|r| r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(precision: &'static str, confidence: f32, flags: Vec<&'static str>) -> Hit {
        Hit {
            lat: 49.897442,
            lon: 2.290084,
            precision,
            score: 5.0,
            confidence,
            street: "Boulevard du Port".into(),
            housenumber: (precision != "street" && precision != "city").then(|| "8".to_string()),
            commune: "Amiens".into(),
            postcode: "80000".into(),
            flags,
            region: None,
            distance_m: None,
        }
    }

    #[test]
    fn a_weak_answer_never_scores_above_the_cap_and_a_firm_one_keeps_its_confidence() {
        // a fuzzy street at 0.55 is weak (`hit_is_weak`), a town is always weak
        let fuzzy = hit("house", 0.55, vec!["street_fuzzy"]);
        let town = hit("city", 0.9, vec!["place_exact"]);
        assert!(query::hit_is_weak(&fuzzy) && query::hit_is_weak(&town));
        assert_eq!(score(&fuzzy), WEAK_SCORE);
        assert_eq!(score(&town), WEAK_SCORE);
        let firm = hit("house", 0.94, vec!["street_exact"]);
        assert_eq!(score(&firm), 0.94);
    }

    #[test]
    fn a_number_without_its_street_is_not_a_housenumber() {
        let mut lone = hit("house", 0.94, vec!["street_exact"]);
        lone.street.clear();
        let f = feature(&lone, "fr", false, None);
        assert!(f["properties"].get("housenumber").is_none(), "{f}");
        assert_ne!(f["properties"]["type"], "housenumber", "{f}");
    }

    #[test]
    fn a_place_of_interest_is_not_an_address() {
        let poi = hit("house", 0.9, vec!["poi_layer"]);
        assert!(!keep(&poi, None, None));
        assert!(keep(&hit("house", 0.9, vec!["street_exact"]), None, None));
    }
}
