//! The shape of a Google Geocoding API answer, so a client written for it only needs a new base URL.
//!
//! Follows the documented JSON response (developers.google.com/maps/documentation/geocoding/requests-geocoding,
//! read 2026-11-07). Answers come from the sheet, so what Google has and a sheet does not is a documented
//! stand-in, never a guess: `place_id` is GridPin's own and lasts as long as the sheet release, `viewport` is a
//! box by precision, a `country` component holds the code alone.

use serde_json::{json, Value};

use crate::query::{hit_is_weak, match_level, Hit};
use crate::serve::{display_name, half_width, place_id};

/// `geometry.location_type`, from how far the answer reaches.
pub fn location_type(h: &Hit) -> &'static str {
    match match_level(h).0 {
        "PointAddress" | "POI" => "ROOFTOP",
        "StreetAddress" => "RANGE_INTERPOLATED",
        "StreetName" | "StreetInt" => "GEOMETRIC_CENTER",
        // the neighbour's point, a postcode's place, a town: approximate by Google's own definition
        _ => "APPROXIMATE",
    }
}

/// The top-level `types` of a result.
fn result_types(h: &Hit) -> Vec<&'static str> {
    match match_level(h).0 {
        "PointAddress" | "StreetAddress" | "StreetAddressExt" => vec!["street_address"],
        "POI" => vec!["establishment", "point_of_interest"],
        "StreetName" => vec!["route"],
        "StreetInt" => vec!["intersection"],
        "Postal" => vec!["postal_code"],
        _ => vec!["locality", "political"],
    }
}

fn component(name: &str, types: &[&str]) -> Value {
    json!({ "long_name": name, "short_name": name, "types": types })
}

/// One answer as an element of `results`.
pub fn result(h: &Hit, country: &str) -> Value {
    let mut parts = Vec::new();
    if let Some(number) = &h.housenumber {
        parts.push(component(number, &["street_number"]));
    }
    if !h.street.is_empty() {
        parts.push(component(&h.street, &["route"]));
    }
    if !h.commune.is_empty() {
        parts.push(component(&h.commune, &["locality", "political"]));
    }
    if let Some(region) = &h.region {
        parts.push(component(
            region,
            &["administrative_area_level_1", "political"],
        ));
    }
    if !country.is_empty() {
        parts.push(component(
            &country.to_uppercase(),
            &["country", "political"],
        ));
    }
    if !h.postcode.is_empty() {
        parts.push(component(&h.postcode, &["postal_code"]));
    }
    let w = half_width(h.precision);
    let mut out = json!({
        "address_components": parts,
        "formatted_address": display_name(h),
        "geometry": {
            "location": { "lat": h.lat, "lng": h.lon },
            "location_type": location_type(h),
            "viewport": {
                "northeast": { "lat": h.lat + w, "lng": h.lon + w },
                "southwest": { "lat": h.lat - w, "lng": h.lon - w },
            },
        },
        "place_id": format!("gridpin:{}", place_id(h)),
        "types": result_types(h),
    });
    // Google sets this when it could not match the whole request; here the engine's own "check it"
    if hit_is_weak(h) || h.precision == "near" {
        out["partial_match"] = json!(true);
    }
    out
}

/// The whole answer: `status` is `OK` with at least one result, `ZERO_RESULTS` otherwise, as documented.
pub fn response(hits: &[Hit], country: &str) -> Value {
    json!({
        "results": hits.iter().map(|h| result(h, country)).collect::<Vec<_>>(),
        "status": if hits.is_empty() { "ZERO_RESULTS" } else { "OK" },
    })
}

/// A request the API documents as `INVALID_REQUEST`.
pub fn invalid(message: &str) -> Value {
    json!({ "results": [], "status": "INVALID_REQUEST", "error_message": message })
}

/// `components=country:US|postal_code:94043`: the value of one filter.
pub fn component_filter<'a>(components: Option<&'a str>, name: &str) -> Option<&'a str> {
    components?
        .split('|')
        .filter_map(|part| part.split_once(':'))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(precision: &'static str, flags: Vec<&'static str>) -> Hit {
        Hit {
            lat: 38.8976751,
            lon: -77.036547,
            precision,
            score: 7.0,
            confidence: 0.95,
            street: "Pennsylvania Avenue Northwest".into(),
            housenumber: (precision != "street" && precision != "city").then(|| "1600".to_string()),
            commune: "Washington".into(),
            postcode: "20500".into(),
            flags,
            region: Some("DC".into()),
            distance_m: None,
        }
    }

    fn exact(precision: &'static str) -> Hit {
        hit(precision, vec!["street_exact"])
    }

    #[test]
    fn every_documented_field_of_a_result_is_there_with_its_shape() {
        let r = result(&exact("house"), "us");
        assert_eq!(
            r["formatted_address"],
            "1600 Pennsylvania Avenue Northwest, Washington, DC, 20500"
        );
        assert_eq!(r["geometry"]["location"]["lat"], 38.8976751);
        assert_eq!(r["geometry"]["location"]["lng"], -77.036547);
        assert_eq!(r["geometry"]["location_type"], "ROOFTOP");
        let view = &r["geometry"]["viewport"];
        assert!(view["northeast"]["lat"].as_f64() > view["southwest"]["lat"].as_f64());
        assert!(view["northeast"]["lng"].as_f64() > view["southwest"]["lng"].as_f64());
        assert!(r["place_id"].as_str().unwrap().starts_with("gridpin:"));
        assert_eq!(r["types"], json!(["street_address"]));
        assert!(r.get("partial_match").is_none());
        let of = |t: &str| {
            r["address_components"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["types"].as_array().unwrap().iter().any(|x| x == t))
                .map(|c| c["long_name"].as_str().unwrap().to_string())
        };
        assert_eq!(of("street_number").as_deref(), Some("1600"));
        assert_eq!(
            of("route").as_deref(),
            Some("Pennsylvania Avenue Northwest")
        );
        assert_eq!(of("locality").as_deref(), Some("Washington"));
        assert_eq!(of("administrative_area_level_1").as_deref(), Some("DC"));
        assert_eq!(of("country").as_deref(), Some("US"));
        assert_eq!(of("postal_code").as_deref(), Some("20500"));
        for c in r["address_components"].as_array().unwrap() {
            assert!(c.get("short_name").is_some() && c.get("types").is_some());
        }
    }

    #[test]
    fn location_type_and_types_follow_the_match_level() {
        for (h, location, kind) in [
            (exact("house"), "ROOFTOP", "street_address"),
            (exact("interp"), "RANGE_INTERPOLATED", "street_address"),
            (exact("near"), "APPROXIMATE", "street_address"),
            (exact("street"), "GEOMETRIC_CENTER", "route"),
            (
                hit("intersection", vec!["intersection", "street_exact"]),
                "GEOMETRIC_CENTER",
                "intersection",
            ),
            (hit("city", vec!["place_exact"]), "APPROXIMATE", "locality"),
            (
                hit("city", vec!["place_prefix_group", "postal_place"]),
                "APPROXIMATE",
                "postal_code",
            ),
            (
                hit("house", vec!["poi_layer"]),
                "ROOFTOP",
                "point_of_interest",
            ),
        ] {
            let r = result(&h, "us");
            assert_eq!(r["geometry"]["location_type"], location, "{}", h.precision);
            assert!(
                r["types"].as_array().unwrap().iter().any(|t| t == kind),
                "{kind}: {r}"
            );
        }
    }

    #[test]
    fn a_partial_match_is_what_the_engine_itself_doubts() {
        assert_eq!(result(&exact("near"), "us")["partial_match"], true);
        assert_eq!(
            result(&hit("city", vec!["place_exact"]), "us")["partial_match"],
            true
        );
        assert!(result(&exact("street"), "us")
            .get("partial_match")
            .is_none());
    }

    #[test]
    fn parts_the_answer_does_not_have_are_left_out() {
        let mut h = hit("street", vec!["street_exact"]);
        h.region = None;
        h.postcode.clear();
        let r = result(&h, "");
        let kinds: Vec<String> = r["address_components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["types"][0].as_str().unwrap().to_string())
            .collect();
        assert_eq!(kinds, ["route", "locality"]);
    }

    #[test]
    fn the_status_says_whether_anything_was_found() {
        assert_eq!(response(&[exact("house")], "us")["status"], "OK");
        let none = response(&[], "us");
        assert_eq!(none["status"], "ZERO_RESULTS");
        assert_eq!(none["results"], json!([]));
        assert_eq!(invalid("x")["status"], "INVALID_REQUEST");
        assert_eq!(invalid("x")["error_message"], "x");
    }

    #[test]
    fn a_component_filter_is_found_by_its_name() {
        let c = Some("Country:US|postal_code: 20500 ");
        assert_eq!(component_filter(c, "country"), Some("US"));
        assert_eq!(component_filter(c, "postal_code"), Some("20500"));
        assert_eq!(component_filter(c, "route"), None);
        assert_eq!(component_filter(None, "country"), None);
    }
}
