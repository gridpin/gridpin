//! The Mapbox Geocoding v5 dialect of `gridpin serve`: `GET /geocoding/v5/mapbox.places/<query>.json`, the path
//! `@mapbox/mapbox-sdk` (and through it `@mapbox/mapbox-gl-geocoder` 5.1.3) asks when its `origin` is this server
//! (checked by running the library against a local server, 2026-11-19). The answer is a Carmen GeoJSON
//! `FeatureCollection`, as the v5 reference describes it (docs.mapbox.com/api/search/geocoding-v5, read 2026-11-19).
//!
//! Understood: `limit` (1 to 10, 5 by default), `proximity=lon,lat`, `autocomplete` (true by default, as in Mapbox:
//! the suggestions of an address field, and the geocoder's answers when the text has none), `country` (the sheets of
//! this server it allows), `types` (`address`, `place`, `postcode`, `region`), a query of two numbers `lon,lat`
//! (reverse). Accepted and ignored: `access_token` (never checked, never logged), `language`, `bbox`, `fuzzyMatch`,
//! `routing`, `worldview`, `proximity=ip`. What Mapbox has and a sheet does not is a documented stand-in: `id` is
//! GridPin's own and lasts as long as the sheet release; `relevance` is GridPin's confidence, a score of the match and
//! not a probability nor Mapbox's relevance; an answer the engine calls weak keeps `properties.weak: true` and
//! `properties.accuracy: "approximate"`.

use serde_json::{json, Value};

use crate::query::{self, match_level, Hit};
use crate::serve::{geocode, geocode_near, reverse_hits, Counters, Engine, Request, Response};

/// The routed form of every v5 path (`route_path`).
pub const ROUTE: &str = "/geocoding/v5/{mode}/{query}.json";
const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 10;
const TYPES: [&str; 4] = ["address", "place", "postcode", "region"];

/// The query and the mode of a v5 path, or `None` when the path is not one.
pub fn parse_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/geocoding/v5/")?;
    let (mode, file) = rest.split_once('/')?;
    if !matches!(mode, "mapbox.places" | "mapbox.places-permanent") {
        return None;
    }
    let raw = file.strip_suffix(".json")?;
    // a path segment: `+` is a plus sign, only `%XX` is decoded
    crate::serve::decode_component(&raw.replace('+', "%2B"))
}

fn refuse(status: u16, message: &str) -> Response {
    Response::json(status, &json!({ "message": message }))
}

/// The Carmen `place_type` of an answer.
pub fn place_type(h: &Hit) -> &'static str {
    match match_level(h).0 {
        "PointAddress" | "StreetAddress" | "StreetAddressExt" | "StreetName" | "StreetInt"
        | "POI" => "address",
        "Postal" => "postcode",
        _ if h.precision == "region" => "region",
        _ => "place",
    }
}

fn accuracy(h: &Hit) -> &'static str {
    if query::hit_is_weak(h) {
        return "approximate";
    }
    match match_level(h).0 {
        "PointAddress" | "POI" => "rooftop",
        "StreetAddress" => "interpolated",
        "StreetName" | "StreetInt" => "street",
        _ => "approximate",
    }
}

fn label(h: &Hit, country_name: &str) -> String {
    let street = [h.housenumber.as_deref().unwrap_or(""), h.street.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let place = [h.postcode.as_str(), h.commune.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    [street.as_str(), place.as_str(), country_name]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(", ")
}

/// One answer as a Carmen feature.
pub fn feature(h: &Hit, country: &str) -> Value {
    let kind = place_type(h);
    let country_name = crate::country_names::name_of(country).unwrap_or("");
    let mut context = Vec::new();
    if !h.postcode.is_empty() && kind != "postcode" {
        context.push(json!({ "id": format!("postcode.{}", h.postcode), "text": h.postcode }));
    }
    if !h.commune.is_empty() && kind == "address" {
        context.push(json!({ "id": format!("place.{}", h.commune), "text": h.commune }));
    }
    if let Some(region) = h.region.as_deref().filter(|r| !r.is_empty()) {
        context.push(json!({ "id": format!("region.{region}"), "text": region }));
    }
    if !country_name.is_empty() {
        context.push(
            json!({ "id": format!("country.{country}"), "text": country_name,
            "short_code": country.to_ascii_lowercase() }),
        );
    }
    let text = match kind {
        "address" if !h.street.is_empty() => h.street.clone(),
        "postcode" => h.postcode.clone(),
        _ => h.commune.clone(),
    };
    let (level, _) = match_level(h);
    let mut f = json!({
        "id": format!("{kind}.{}", crate::locator::encode(h)),
        "type": "Feature",
        "place_type": [kind],
        "relevance": h.confidence,
        "properties": { "accuracy": accuracy(h), "weak": query::hit_is_weak(h), "match_level": level,
            "source": "gridpin" },
        "text": text,
        "place_name": label(h, country_name),
        "center": [h.lon, h.lat],
        "geometry": { "type": "Point", "coordinates": [h.lon, h.lat] },
        "context": context,
    });
    if let Some(number) = h
        .housenumber
        .as_deref()
        .filter(|n| !n.is_empty() && kind == "address")
    {
        f["address"] = json!(number);
    }
    f
}

/// A suggestion of the address field (`/v1/suggest`) as a Carmen feature: a street, or the house of the typed number.
/// It has no match score, so no `relevance`; `properties.suggestion` says what it is.
pub fn suggestion_feature(s: &crate::query::Suggestion, country: &str) -> Value {
    let country_name = crate::country_names::name_of(country).unwrap_or("");
    let mut context = Vec::new();
    if !s.commune.is_empty() {
        context.push(json!({ "id": format!("place.{}", s.commune), "text": s.commune }));
    }
    if !country_name.is_empty() {
        context.push(
            json!({ "id": format!("country.{country}"), "text": country_name,
            "short_code": country.to_ascii_lowercase() }),
        );
    }
    let accuracy = if s.precision == "house" {
        "rooftop"
    } else {
        "street"
    };
    let place_name = [s.label.as_str(), country_name]
        .iter()
        .filter(|x| !x.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    json!({
        "id": format!("address.{:.7},{:.7}", s.lat, s.lon),
        "type": "Feature",
        "place_type": ["address"],
        "properties": { "accuracy": accuracy, "weak": false, "suggestion": true, "source": "gridpin" },
        "text": s.street,
        "place_name": place_name,
        "center": [s.lon, s.lat],
        "geometry": { "type": "Point", "coordinates": [s.lon, s.lat] },
        "context": context,
    })
}

fn collection(query: Value, features: Vec<Value>) -> Response {
    Response::json(
        200,
        &json!({ "type": "FeatureCollection", "query": query, "features": features,
            "attribution": "GridPin; the data of each sheet under its own licence (gridpin sheets)" }),
    )
}

/// `proximity=lon,lat`; `ip` (Mapbox's own guess) and nothing are no point.
fn proximity(req: &Request) -> Result<Option<(f64, f64)>, Response> {
    match req.param("proximity").map(str::trim) {
        None | Some("") | Some("ip") => Ok(None),
        Some(v) => v
            .split_once(',')
            .and_then(|(a, b)| Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?)))
            .filter(|(lon, lat)| lon.abs() <= 180.0 && lat.abs() <= 90.0)
            .map(|(lon, lat)| Some((lat, lon)))
            .ok_or_else(|| refuse(422, "proximity must be lon,lat")),
    }
}

/// Two numbers `lon,lat` in range: a reverse query.
fn as_point(q: &str) -> Option<(f64, f64)> {
    let (a, b) = q.split_once(',')?;
    let (lon, lat) = (a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?);
    (lon.abs() <= 180.0 && lat.abs() <= 90.0).then_some((lat, lon))
}

/// `GET /geocoding/v5/mapbox.places/<query>.json`.
pub fn geocoding(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let run = || -> Result<Response, Response> {
        let q = parse_path(&req.path)
            .ok_or_else(|| refuse(422, "the query of the path is not UTF-8"))?;
        let q = q.trim();
        if q.is_empty() {
            return Err(refuse(422, "Query too short"));
        }
        if q.len() > query::MAX_QUERY_BYTES {
            return Err(refuse(
                422,
                &format!("query over {} bytes", query::MAX_QUERY_BYTES),
            ));
        }
        let limit = match req.param("limit") {
            None => DEFAULT_LIMIT,
            Some(v) => match v.trim().parse::<usize>() {
                Ok(n) if (1..=MAX_LIMIT).contains(&n) => n,
                _ => return Err(refuse(422, "limit must be a whole number from 1 to 10")),
            },
        };
        let types: Vec<&str> = req
            .param("types")
            .map(|t| {
                t.split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(bad) = types.iter().find(|t| !TYPES.contains(t)) {
            return Err(refuse(
                422,
                &format!("Type \"{bad}\" is not one of {}", TYPES.join(", ")),
            ));
        }
        let asked: Vec<String> = req
            .param("country")
            .map(|c| {
                c.split(',')
                    .map(|c| c.trim().to_ascii_lowercase())
                    .filter(|c| !c.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let tokens = json!(q.to_lowercase().split_whitespace().collect::<Vec<_>>());
        if !asked.is_empty() && !asked.iter().any(|c| engine.serves(c)) {
            return Ok(collection(tokens, Vec::new()));
        }
        let keep = |h: &Hit| types.is_empty() || types.contains(&place_type(h));
        if let Some((lat, lon)) = as_point(q) {
            // Mapbox answers a reverse query with one feature unless `limit` is given (with `types`)
            let k = if req.param("limit").is_some() {
                limit
            } else {
                1
            };
            let (hits, pick) =
                reverse_hits(engine, counters, lat, lon, k, &asked).map_err(|e| refuse(422, &e))?;
            let features = hits
                .iter()
                .filter(|h| keep(h))
                .map(|h| feature(h, pick.country))
                .collect();
            return Ok(collection(json!([lon, lat]), features));
        }
        let near = proximity(req)?;
        let autocomplete = req
            .param("autocomplete")
            .is_none_or(|a| a.trim() != "false");
        let (hits, pick) = match near {
            Some((lat, lon)) => geocode_near(engine, counters, q, limit, lat, lon, &asked)
                .map_err(|e| refuse(422, &e))?,
            None => geocode(engine, counters, q, limit, &asked),
        };
        let mut features: Vec<Value> = Vec::new();
        if autocomplete && types.iter().all(|t| *t == "address") {
            // the suggestions of an address field: a street, or the house of the typed number on it
            let p = engine.pick(q, &asked);
            for s in p.index.suggest_near(q, limit, near) {
                features.push(suggestion_feature(&s, p.country));
            }
        }
        if features.is_empty() {
            features = hits
                .iter()
                .filter(|h| keep(h))
                .map(|h| feature(h, pick.country))
                .collect();
        }
        features.truncate(limit);
        Ok(collection(tokens, features))
    };
    run().unwrap_or_else(|r| r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_is_the_last_segment_and_only_v5_places_paths_are_ours() {
        assert_eq!(
            parse_path("/geocoding/v5/mapbox.places/27%20boulevard.json").as_deref(),
            Some("27 boulevard")
        );
        assert_eq!(
            parse_path("/geocoding/v5/mapbox.places-permanent/a.json").as_deref(),
            Some("a")
        );
        // a plus sign in a path is a plus sign
        assert_eq!(
            parse_path("/geocoding/v5/mapbox.places/a+b.json").as_deref(),
            Some("a+b")
        );
        assert_eq!(
            parse_path("/geocoding/v5/mapbox.places/a%2Fb.json").as_deref(),
            Some("a/b")
        );
        // a slash left raw in the query stays in it
        assert_eq!(
            parse_path("/geocoding/v5/mapbox.places/a/b.json").as_deref(),
            Some("a/b")
        );
        assert_eq!(parse_path("/geocoding/v6/mapbox.places/a.json"), None);
        assert_eq!(parse_path("/geocoding/v5/mapbox.other/a.json"), None);
        assert_eq!(parse_path("/geocoding/v5/mapbox.places/a"), None);
    }

    #[test]
    fn two_numbers_are_a_point_lon_first() {
        assert_eq!(as_point("7.42,43.73"), Some((43.73, 7.42)));
        assert_eq!(as_point("200,43"), None);
        assert_eq!(as_point("rue 1, paris"), None);
    }
}
