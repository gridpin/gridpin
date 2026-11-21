//! The Photon dialect of `gridpin serve` under `/photon/`: `/photon/api`, `/photon/reverse`, `/photon/status`, in the
//! shape Photon 1.x writes (`GeoJsonFormatter`, `RequestFactoryBase`, `SimpleSearchRequestFactory`,
//! `ReverseRequestFactory`, read 2026-11-13), so a client written for Photon (geopy `Photon`, the Ruby `geocoder` gem,
//! Dawarich, Dawarich Atlas) only needs a new base URL: `http://host:8080/photon`.
//!
//! What Photon does and we follow: the answer is always a `FeatureCollection` with a `features` array, empty or not;
//! a property is written only when it has a value, and every property is a string; `countrycode` is upper case;
//! `limit` defaults to 15 and is held within its bounds without a refusal; a refusal is `{"message": …}` with 400.
//! What we do differently, on purpose: an unknown parameter is skipped, not refused (Traccar adds `&key=`); `lang`,
//! `zoom`, `location_bias_scale`, `dedupe`, `include`, `exclude`, `debug`, `geometry`, `query_string_filter` and
//! `distance_sort` are accepted and change nothing. A sheet holds no OSM tags, so an `osm_tag` that asks for a tag
//! (`shop`, `shop:supermarket`, `:cafe`) is answered with no features, and one that only excludes a tag (`!shop`,
//! `shop:!bakery`) changes nothing: answering any address for "the supermarkets called Maxi" would be a wrong answer,
//! not a looser one (Dawarich Atlas searches categories this way). No OSM object is invented: no `osm_id`,
//! `osm_type` and `extent`. Photon has no confidence field, so how well the answer matched travels in `extra`, as
//! `extratags` does in the Nominatim dialect.

use serde_json::{json, Map, Value};

use crate::nominatim::place_name;
use crate::query::{self, Hit};
use crate::serve::{
    geocode, geocode_near, metres, reverse_hits, Counters, Engine, Pick, Request, Response,
    MAX_LIMIT,
};

/// Photon's own default for `limit` (`RequestBase.limit`).
const DEFAULT_LIMIT: usize = 15;
/// Photon's own default for `radius` of a reverse request, in kilometres (`ReverseRequest.radius`).
const DEFAULT_RADIUS_KM: f64 = 1.0;
/// Photon's layers (`AVAILABLE_LAYERS`); a sheet answers three of them.
const LAYERS: [&str; 9] = [
    "house", "street", "locality", "district", "city", "county", "state", "country", "other",
];

fn refuse(status: u16, message: &str) -> Response {
    Response::json(status, &json!({ "message": message }))
}

/// Every value of a repeatable parameter (`layer=house&layer=street`, `countrycode=de&countrycode=at`).
fn all<'r>(req: &'r Request, name: &str) -> Vec<&'r str> {
    req.query
        .iter()
        .filter(|(k, _)| k == name)
        .flat_map(|(_, v)| v.split(','))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .collect()
}

fn limit(req: &Request, default: usize) -> Result<usize, Response> {
    match req.param("limit") {
        None => Ok(default),
        Some(v) => v
            .trim()
            .parse::<i64>()
            .map(|n| n.clamp(1, MAX_LIMIT as i64) as usize)
            .map_err(|_| refuse(400, "limit must be a whole number")),
    }
}

fn number(req: &Request, name: &str) -> Result<Option<f64>, Response> {
    req.param(name)
        .map(|v| v.trim().parse::<f64>())
        .transpose()
        .map_err(|_| refuse(400, &format!("{name} must be a number")))
}

/// `lat` and `lon` come together or not at all, inside the range of degrees.
fn point(req: &Request) -> Result<Option<(f64, f64)>, Response> {
    match (number(req, "lat")?, number(req, "lon")?) {
        (None, None) => Ok(None),
        (Some(lat), Some(lon)) if lat.abs() <= 90.0 && lon.abs() <= 180.0 => Ok(Some((lat, lon))),
        (Some(_), Some(_)) => Err(refuse(400, "lat must be within ±90 and lon within ±180")),
        _ => Err(refuse(400, "give lat and lon together")),
    }
}

/// `bbox=minLon,minLat,maxLon,maxLat`, as Photon reads it.
fn bbox(req: &Request) -> Result<Option<[f64; 4]>, Response> {
    let Some(raw) = req.param("bbox") else {
        return Ok(None);
    };
    let n: Vec<f64> = raw
        .split(',')
        .map(|x| x.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|_| refuse(400, "bbox must be minLon,minLat,maxLon,maxLat"))?;
    match n[..] {
        [w, s, e, nn] if w <= e && s <= nn => Ok(Some([w, s, e, nn])),
        _ => Err(refuse(400, "bbox must be minLon,minLat,maxLon,maxLat")),
    }
}

/// `layer=`: one of Photon's layers, repeatable.
fn layers(req: &Request) -> Result<Vec<&str>, Response> {
    let asked = all(req, "layer");
    match asked.iter().find(|l| !LAYERS.contains(l)) {
        Some(bad) => Err(refuse(
            400,
            &format!(
                "Unknown layer type {bad}. Available layers: {}",
                LAYERS.join(", ")
            ),
        )),
        None => Ok(asked),
    }
}

/// Whether a Photon `osm_tag` filter keeps only objects with a tag (`key`, `key:value`, `:value`) rather than
/// excluding one (`!key`, `!key:value`, `key:!value`).
fn asks_for_tag(filter: &str) -> bool {
    let f = filter.trim();
    !f.is_empty() && !f.starts_with('!') && !f.contains(":!")
}

/// The Photon layer of an answer: an address (exact, interpolated, the neighbour's door, or the nearest house of a
/// reverse point far from any, `approximate`) and a place of interest are a `house`, a street and a crossing a
/// `street`, a town or a postcode's place a `city`.
pub(crate) fn layer(h: &Hit) -> &'static str {
    if h.flags.contains(&"poi_layer") {
        return "house";
    }
    match h.precision {
        "house" | "interp" | "near" | "approximate" => "house",
        "city" => "city",
        _ => "street",
    }
}

fn put(o: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.is_empty() {
        o.insert(key.into(), json!(value));
    }
}

/// One answer as a Photon feature; with `ids` (`gridpin serve --ids`) it carries `osm_type` `N` and the answer's own
/// number (`locator.rs`), negative.
pub(crate) fn feature(h: &Hit, country: &str, ids: bool) -> Value {
    let poi = h.flags.contains(&"poi_layer");
    let kind = layer(h);
    let mut p = Map::new();
    if ids {
        p.insert("osm_type".into(), json!("N"));
        p.insert("osm_id".into(), json!(crate::locator::encode(h)));
    }
    // a place of interest of the POI layer: its class is not kept in the sheet, so the value is OSM's "yes"
    put(&mut p, "osm_key", if poi { "amenity" } else { "place" });
    put(&mut p, "osm_value", if poi { "yes" } else { kind });
    put(&mut p, "type", kind);
    let street = if poi { "" } else { h.street.as_str() };
    // Photon never writes a house number without its street, and clients rely on it
    if kind == "house" && !street.is_empty() {
        put(
            &mut p,
            "housenumber",
            h.housenumber.as_deref().unwrap_or(""),
        );
    }
    let name = match kind {
        "city" if !h.commune.is_empty() => h.commune.as_str(),
        _ => place_name(h),
    };
    put(&mut p, "name", name);
    if kind == "house" {
        put(&mut p, "street", street);
    }
    if kind != "city" {
        put(&mut p, "city", &h.commune);
    }
    put(&mut p, "state", h.region.as_deref().unwrap_or(""));
    put(
        &mut p,
        "country",
        crate::country_names::name_of(country).unwrap_or(""),
    );
    put(&mut p, "postcode", &h.postcode);
    put(&mut p, "countrycode", &country.to_ascii_uppercase());
    let (level, status) = query::match_level(h);
    p.insert(
        "extra".into(),
        json!({ "match_level": level, "match_status": status.to_string() }),
    );
    json!({
        "type": "Feature",
        "properties": Value::Object(p),
        "geometry": { "type": "Point", "coordinates": [h.lon, h.lat] },
    })
}

fn collection(hits: &[Hit], pick: &Pick, ids: bool) -> Response {
    Response::json(
        200,
        &json!({
            "type": "FeatureCollection",
            "features": hits.iter().map(|h| feature(h, pick.country, ids)).collect::<Vec<_>>(),
        }),
    )
}

fn inside(h: &Hit, b: &[f64; 4]) -> bool {
    h.lon >= b[0] && h.lat >= b[1] && h.lon <= b[2] && h.lat <= b[3]
}

/// The countries `countrycode=` allows, among the sheets of this server; `None` allows every sheet.
fn countries(engine: &Engine, req: &Request) -> Option<Vec<String>> {
    let codes: Vec<String> = all(req, "countrycode")
        .into_iter()
        .map(str::to_ascii_lowercase)
        .collect();
    (!codes.is_empty()).then(|| codes.into_iter().filter(|c| engine.serves(c)).collect())
}

/// `GET /photon/api?q=…`: a search. With `bbox` or `layer` the engine is asked for its longest list and the answers
/// outside are dropped, so a filter cannot leave fewer answers than the engine had inside it.
pub(crate) fn api(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let run = || -> Result<Response, Response> {
        let q = req
            .param("q")
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .ok_or_else(|| {
                refuse(
                    400,
                    "q parameter is required when no include categories are specified",
                )
            })?;
        if q.len() > query::MAX_QUERY_BYTES {
            return Err(refuse(
                414,
                &format!("query over {} bytes", query::MAX_QUERY_BYTES),
            ));
        }
        let limit = limit(req, DEFAULT_LIMIT)?;
        if all(req, "osm_tag").iter().any(|t| asks_for_tag(t)) {
            return Ok(collection(&[], &engine.picks()[0], engine.ids));
        }
        let near = point(req)?;
        let bbox = bbox(req)?;
        let layers = layers(req)?;
        let filtered = bbox.is_some() || !layers.is_empty();
        let k = if filtered { MAX_LIMIT } else { limit };
        let asked = match countries(engine, req) {
            Some(codes) if codes.is_empty() => {
                return Ok(collection(&[], &engine.picks()[0], engine.ids))
            }
            Some(codes) => codes,
            None => Vec::new(),
        };
        let (mut hits, pick) = match near {
            Some((lat, lon)) => geocode_near(engine, counters, q, k, lat, lon, &asked)
                .map_err(|e| refuse(400, &e))?,
            None => geocode(engine, counters, q, k, &asked),
        };
        hits.retain(|h| {
            bbox.is_none_or(|b| inside(h, &b)) && (layers.is_empty() || layers.contains(&layer(h)))
        });
        hits.truncate(limit);
        Ok(collection(&hits, &pick, engine.ids))
    };
    run().unwrap_or_else(|r| r)
}

/// `GET /photon/reverse?lat=…&lon=…`: the nearest answers within `radius` kilometres (1 by default, as in Photon).
pub(crate) fn reverse(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let run = || -> Result<Response, Response> {
        let (lat, lon) =
            point(req)?.ok_or_else(|| refuse(400, "lat and lon are required for reverse"))?;
        let limit = limit(req, DEFAULT_LIMIT)?;
        let radius_km = number(req, "radius")?.unwrap_or(DEFAULT_RADIUS_KM);
        if !(0.0..=5000.0).contains(&radius_km) {
            return Err(refuse(400, "radius must be from 0 to 5000 km"));
        }
        let layers = layers(req)?;
        let (mut hits, pick) =
            reverse_hits(engine, counters, lat, lon, limit, &[]).map_err(|e| refuse(400, &e))?;
        hits.retain(|h| {
            let d = h
                .distance_m
                .unwrap_or_else(|| metres(lat, lon, h.lat, h.lon));
            d <= radius_km * 1000.0 && (layers.is_empty() || layers.contains(&layer(h)))
        });
        Ok(collection(&hits, &pick, engine.ids))
    };
    run().unwrap_or_else(|r| r)
}

/// `GET /photon/status`: Atlas reads it to know the geocoder is up.
pub(crate) fn status(engine: &Engine) -> Response {
    Response::json(
        200,
        &json!({
            "status": "Ok",
            "import_date": engine.release,
            "version": format!("gridpin {}", env!("CARGO_PKG_VERSION")),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(precision: &'static str, number: Option<&str>, flags: Vec<&'static str>) -> Hit {
        Hit {
            lat: 44.8171,
            lon: 20.4577,
            precision,
            score: 7.0,
            confidence: 0.95,
            street: "Zmaj Jovina".into(),
            housenumber: number.map(String::from),
            commune: "Beograd".into(),
            postcode: "11000".into(),
            flags,
            region: Some("Beograd".into()),
            distance_m: None,
        }
    }

    fn props(h: &Hit) -> Value {
        feature(h, "rs", false)["properties"].clone()
    }

    #[test]
    fn an_address_is_a_house_with_its_street_and_the_country_in_both_forms() {
        let p = props(&hit("house", Some("5v"), vec![]));
        assert_eq!(p["type"], "house");
        assert_eq!(p["osm_key"], "place");
        assert_eq!(p["osm_value"], "house");
        assert_eq!(p["housenumber"], "5v");
        assert_eq!(p["street"], "Zmaj Jovina");
        assert_eq!(p["city"], "Beograd");
        assert_eq!(p["state"], "Beograd");
        assert_eq!(p["postcode"], "11000");
        assert_eq!(p["country"], "Serbia");
        assert_eq!(p["countrycode"], "RS");
        // an address has no name, and no OSM object is invented
        for absent in ["name", "osm_id", "osm_type", "extent"] {
            assert!(p.get(absent).is_none(), "{absent}: {p}");
        }
        let f = feature(&hit("house", Some("5v"), vec![]), "rs", false);
        assert_eq!(f["type"], "Feature");
        assert_eq!(f["geometry"]["coordinates"], json!([20.4577, 44.8171]));
    }

    #[test]
    fn interpolation_and_the_neighbour_are_houses_that_say_how_they_matched() {
        for (precision, level) in [
            ("interp", "StreetAddress"),
            ("near", "StreetAddressExt"),
            // the nearest house of a reverse point far from any: still that house, and `extra` says how far it reaches
            ("approximate", "Locality"),
        ] {
            let p = props(&hit(precision, Some("7"), vec![]));
            assert_eq!(p["type"], "house", "{precision}");
            assert_eq!(p["extra"]["match_level"], level, "{precision}");
        }
        // a weak answer (the engine says: check it) is a house too, and `extra` says it is weak
        let mut weak = hit("house", Some("7"), vec!["street_fuzzy"]);
        weak.confidence = 0.4;
        assert_eq!(props(&weak)["extra"]["match_status"], "T");
        assert_eq!(props(&weak)["type"], "house");
        assert_eq!(
            props(&hit("house", Some("7"), vec![]))["extra"]["match_status"],
            "M"
        );
    }

    #[test]
    fn a_street_and_a_town_are_named_by_themselves_and_carry_no_house() {
        let s = props(&hit("street", None, vec![]));
        assert_eq!(
            (s["type"].as_str(), s["name"].as_str()),
            (Some("street"), Some("Zmaj Jovina"))
        );
        assert!(
            s.get("street").is_none() && s.get("housenumber").is_none(),
            "{s}"
        );
        assert_eq!(s["city"], "Beograd");
        let c = props(&hit("city", None, vec![]));
        assert_eq!(
            (c["type"].as_str(), c["name"].as_str()),
            (Some("city"), Some("Beograd"))
        );
        assert!(c.get("city").is_none(), "a town is not inside itself: {c}");
        assert_eq!(props(&hit("intersection", None, vec![]))["type"], "street");
    }

    #[test]
    fn a_house_number_never_comes_without_its_street() {
        let mut h = hit("house", Some("5"), vec![]);
        h.street.clear();
        let p = props(&h);
        assert!(
            p.get("housenumber").is_none() && p.get("street").is_none(),
            "{p}"
        );
    }

    #[test]
    fn a_place_of_interest_is_a_named_house_of_an_amenity() {
        let p = props(&hit("street", None, vec!["poi_layer"]));
        assert_eq!(p["osm_key"], "amenity");
        assert_eq!(p["osm_value"], "yes");
        assert_eq!(p["type"], "house");
        assert_eq!(p["name"], "Zmaj Jovina");
        assert!(
            p.get("street").is_none() && p.get("housenumber").is_none(),
            "{p}"
        );
    }

    #[test]
    fn empty_values_are_not_written() {
        let mut h = hit("house", Some("5"), vec![]);
        h.postcode.clear();
        h.region = None;
        let p = feature(&h, "", false)["properties"].clone();
        for absent in ["postcode", "state", "country", "countrycode"] {
            assert!(p.get(absent).is_none(), "{absent}: {p}");
        }
    }

    fn request(query: &[(&str, &str)]) -> Request {
        Request {
            method: "GET".into(),
            path: "/photon/api".into(),
            query: query
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: Vec::new(),
            authorization: None,
            headers: Vec::new(),
        }
    }

    #[test]
    fn limit_is_fifteen_by_default_and_held_within_its_bounds() {
        let lim = |q: &[(&str, &str)]| limit(&request(q), DEFAULT_LIMIT).ok();
        assert_eq!(lim(&[]), Some(15));
        assert_eq!(lim(&[("limit", "3")]), Some(3));
        assert_eq!(lim(&[("limit", "0")]), Some(1));
        assert_eq!(lim(&[("limit", "-5")]), Some(1));
        assert_eq!(lim(&[("limit", "1000")]), Some(MAX_LIMIT));
        assert_eq!(lim(&[("limit", "abc")]), None);
    }

    #[test]
    fn repeated_and_comma_separated_values_are_all_read() {
        let r = request(&[
            ("layer", "house"),
            ("layer", "street,city"),
            ("countrycode", "de"),
        ]);
        assert_eq!(all(&r, "layer"), vec!["house", "street", "city"]);
        assert_eq!(layers(&r).ok(), Some(vec!["house", "street", "city"]));
        assert!(layers(&request(&[("layer", "planet")])).is_err());
    }

    #[test]
    fn every_layer_name_of_photon_is_known_and_ours_are_among_them() {
        for h in [
            hit("house", Some("1"), vec![]),
            hit("street", None, vec![]),
            hit("city", None, vec![]),
        ] {
            assert!(LAYERS.contains(&layer(&h)));
        }
    }
}
