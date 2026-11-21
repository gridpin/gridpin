//! An answer as a Valhalla route location (`gridpin query --format valhalla`, `GET /v1/geocode?format=valhalla`):
//! one line `{"locations":[{"lat":…,"lon":…,"display_lat":…,"display_lon":…}]}` that a routing request can take as it
//! is.
//!
//! Valhalla routes to the edge nearest `lat`/`lon`; a house point set back from its street can snap to the street
//! behind the house. So `lat`/`lon` is the point on the street in front of the house (the `street_point` extra: half
//! way to the line of the houses across), and the house itself goes to `display_lat`/`display_lon`, which Valhalla
//! hands back untouched. Without such a point the house is the location. Valhalla's `street` (a street name to snap
//! to) is not written: Valhalla documents it as not yet read (13.11.2026, valhalla/valhalla#6467), and a field that
//! does nothing is left out until it does. No answer, or a weak one, is an error, not a location: a route to the
//! wrong house is worse than no route.

use serde_json::{json, Map, Value};

use crate::query::{hit_is_weak, Hit, Index};

/// The location of the top answer, or why there is none.
pub fn location(hits: &[Hit], idx: &Index) -> Result<Value, String> {
    let h = hits
        .first()
        .ok_or_else(|| "no answer to route to".to_string())?;
    if hit_is_weak(h) {
        return Err(format!(
            "the answer is weak ({}): check the address before routing to it",
            crate::query::match_level(h).0
        ));
    }
    let mut loc = Map::new();
    match crate::extras::street_point(h, idx) {
        Some((lat, lon)) => {
            loc.insert("lat".into(), json!(round7(lat)));
            loc.insert("lon".into(), json!(round7(lon)));
            loc.insert("display_lat".into(), json!(h.lat));
            loc.insert("display_lon".into(), json!(h.lon));
        }
        None => {
            loc.insert("lat".into(), json!(h.lat));
            loc.insert("lon".into(), json!(h.lon));
        }
    }
    Ok(json!({ "locations": [Value::Object(loc)] }))
}

fn round7(v: f64) -> f64 {
    (v * 1e7).round() / 1e7
}
