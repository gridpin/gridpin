//! The US Census Geocoder dialect of `gridpin serve` under `/census/geocoder/`: `locations/onelineaddress` and
//! `locations/address`, in the shape `geocoding.geo.census.gov` writes (read live on 2026-11-14), so a client written
//! for it (the `censusgeocode` and `geocoder` packages, R `tidygeocoder` with `method = "census"`) only needs a new
//! base URL: `http://host:8080/census/geocoder`.
//!
//! The Census answer lists address matches, and a match there means the house was found on a street range. So
//! `addressMatches` holds only an exact or an interpolated house the engine does not call weak; a street, a town, the
//! neighbour's door and a weak answer give no match. `tigerLine` is not written: the sheet holds no TIGER segment.
//! The street is not split into directions and types: `streetName` carries it whole, the other parts are empty.
//! `benchmark`, `vintage` and `layers` are accepted. `geographies/...` answers 501: the census layer is a side file
//! of gridpin-pro, not of the free server.

use serde_json::{json, Value};

use crate::query::{self, Hit};
use crate::serve::{geocode, Counters, Engine, Request, Response};

/// How many answers are read before the ones that are not address matches are dropped.
const CANDIDATES: usize = 5;
const MAX_STREET: usize = 100;

/// `{"errors": […], "status": "400"}`, as the Census Geocoder refuses.
fn refuse(status: u16, errors: &[&str]) -> Response {
    Response::json(
        status,
        &json!({ "errors": errors, "status": status.to_string() }),
    )
}

/// The benchmark the request names, as the answer echoes it; `None` for a name the Census service would refuse.
fn benchmark(req: &Request) -> Option<Value> {
    benchmark_named(req.param("benchmark").map(str::trim).unwrap_or(""))
}

fn benchmark_named(named: &str) -> Option<Value> {
    let named = Some(named).filter(|b| !b.is_empty());
    match named {
        None | Some("Public_AR_Current") | Some("4") => Some(json!({
            "isDefault": true,
            "benchmarkDescription": "Public Address Ranges - Current Benchmark",
            "id": "4",
            "benchmarkName": "Public_AR_Current",
        })),
        Some(b)
            if b.starts_with("Public_AR_")
                && b[10..].chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            Some(
                json!({ "isDefault": false, "benchmarkDescription": "", "id": "", "benchmarkName": b }),
            )
        }
        Some(b) if !b.is_empty() && b.len() <= 4 && b.chars().all(|c| c.is_ascii_digit()) => Some(
            json!({ "isDefault": false, "benchmarkDescription": "", "id": b, "benchmarkName": "" }),
        ),
        Some(_) => None,
    }
}

/// Whether an answer is what the Census service calls an address match.
pub(crate) fn is_match(h: &Hit) -> bool {
    matches!(h.precision, "house" | "interp")
        && h.housenumber.as_deref().is_some_and(|n| !n.is_empty())
        && !h.street.is_empty()
        && !h.flags.contains(&"poi_layer")
        && !query::hit_is_weak(h)
}

/// One address match.
pub(crate) fn address_match(h: &Hit) -> Value {
    let number = h.housenumber.as_deref().unwrap_or("");
    let state = h.region.as_deref().unwrap_or("");
    let matched = [
        format!("{number} {}", h.street.to_uppercase()),
        h.commune.to_uppercase(),
        state.to_uppercase(),
        h.postcode.clone(),
    ]
    .into_iter()
    .filter(|s| !s.trim().is_empty())
    .collect::<Vec<_>>()
    .join(", ");
    json!({
        "coordinates": { "x": h.lon, "y": h.lat },
        "addressComponents": {
            "zip": h.postcode,
            "streetName": h.street.to_uppercase(),
            "preType": "",
            "city": h.commune.to_uppercase(),
            "preDirection": "",
            "suffixDirection": "",
            "fromAddress": number,
            "state": state.to_uppercase(),
            "suffixType": "",
            "toAddress": number,
            "suffixQualifier": "",
            "preQualifier": "",
        },
        "matchedAddress": matched,
    })
}

fn answer(
    engine: &Engine,
    counters: &Counters,
    q: &str,
    input_address: Value,
    bench: Value,
) -> Response {
    let (hits, _) = geocode(engine, counters, q, CANDIDATES, &[]);
    let matches: Vec<Value> = hits
        .iter()
        .filter(|h| is_match(h))
        .map(address_match)
        .collect();
    Response::json(
        200,
        &json!({
            "result": {
                "input": { "address": input_address, "benchmark": bench },
                "addressMatches": matches,
            }
        }),
    )
}

/// `GET /census/geocoder/locations/onelineaddress?address=…&benchmark=…&format=json`.
pub(crate) fn oneline(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let address = req.param("address").map(str::trim).unwrap_or("");
    if address.is_empty() || address.len() > query::MAX_QUERY_BYTES {
        return refuse(
            400,
            &["Address cannot be empty and cannot exceed 1000 characters"],
        );
    }
    let Some(bench) = benchmark(req) else {
        return refuse(400, &["Invalid benchmark in request"]);
    };
    answer(
        engine,
        counters,
        address,
        json!({ "address": address }),
        bench,
    )
}

/// `GET /census/geocoder/locations/address?street=…&city=…&state=…&zip=…`: the parts joined into one line.
pub(crate) fn address(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let part = |name: &str| req.param(name).map(str::trim).unwrap_or("").to_string();
    let (street, city, state, zip) = (part("street"), part("city"), part("state"), part("zip"));
    if street.is_empty() || street.len() > MAX_STREET {
        return refuse(
            400,
            &[
                "Street address cannot be empty and cannot exceed 100 characters",
                "Specify House number and Street name along with City and State and/or ZIP Code",
            ],
        );
    }
    if city.is_empty() && state.is_empty() && zip.is_empty() {
        return refuse(
            400,
            &["Specify House number and Street name along with City and State and/or ZIP Code"],
        );
    }
    let Some(bench) = benchmark(req) else {
        return refuse(400, &["Invalid benchmark in request"]);
    };
    let place = [state.as_str(), zip.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let line = [street.as_str(), city.as_str(), place.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    let mut input = json!({ "street": street });
    for (key, value) in [("city", &city), ("state", &state), ("zip", &zip)] {
        if !value.is_empty() {
            input[key] = json!(value);
        }
    }
    answer(engine, counters, &line, input, bench)
}

/// The part named `name` of a `multipart/form-data` body; `None` when the body has no such part.
fn form_part<'b>(content_type: &str, body: &'b [u8], name: &str) -> Option<&'b [u8]> {
    let boundary = content_type
        .split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix("boundary="))?
        .trim_matches('"');
    let delimiter = format!("--{boundary}");
    let wanted = format!("name=\"{name}\"");
    let mut rest = body;
    while let Some(at) = find(rest, delimiter.as_bytes()) {
        rest = &rest[at + delimiter.len()..];
        let head_end = find(rest, b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&rest[..head_end]);
        let content = &rest[head_end + 4..];
        let end = find(content, delimiter.as_bytes()).unwrap_or(content.len());
        if head.contains(&wanted) {
            let part = &content[..end];
            return Some(part.strip_suffix(b"\r\n").unwrap_or(part));
        }
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// One CSV field, quoted as the Census batch writes every field.
fn quoted(field: &str) -> String {
    format!("\"{}\"", field.replace('"', "\"\""))
}

/// `POST …/locations/addressbatch` with the form field `addressFile`: a CSV of `id, street, city, state, zip` without
/// a header, answered with one CSV line per row in the column order of the Census batch (`id`, the input address,
/// `Match`, `Exact` or `Non_Exact`, the matched address, `lon,lat`, and the two TIGER fields left empty), or
/// `No_Match`. tidygeocoder posts the batch to the URL it was given, so a POST to any `locations` path is a batch.
pub(crate) fn batch(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let content_type = req
        .headers
        .iter()
        .find(|(k, _)| k == "content-type")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let Some(file) = form_part(content_type, &req.body, "addressFile") else {
        return refuse(
            400,
            &["The batch takes a multipart/form-data body with the file in the field addressFile"],
        );
    };
    let named = form_part(content_type, &req.body, "benchmark")
        .map(|b| String::from_utf8_lossy(b).trim().to_string())
        .unwrap_or_default();
    if benchmark_named(&named).is_none() {
        return refuse(400, &["Invalid benchmark in request"]);
    }
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(file);
    let mut out = String::new();
    for record in reader.records() {
        let Ok(record) = record else {
            return refuse(
                400,
                &["The address file is not a CSV of id, street, city, state, zip"],
            );
        };
        let field = |i: usize| record.get(i).map(str::trim).unwrap_or("");
        let parts: Vec<&str> = (1..5).map(field).collect();
        let input = parts.join(", ");
        let line = parts
            .iter()
            .filter(|f| !f.is_empty() && **f != "NA")
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let (hits, _) = geocode(engine, counters, &line, CANDIDATES, &[]);
        let row = match hits.iter().find(|h| is_match(h)) {
            Some(h) => {
                let m = address_match(h);
                let exact = if h.precision == "house" {
                    "Exact"
                } else {
                    "Non_Exact"
                };
                format!(
                    "{},{},\"Match\",{},{},{},\"\",\"\"\n",
                    quoted(field(0)),
                    quoted(&input),
                    quoted(exact),
                    quoted(m["matchedAddress"].as_str().unwrap_or("")),
                    quoted(&format!("{},{}", h.lon, h.lat)),
                )
            }
            None => format!("{},{},\"No_Match\"\n", quoted(field(0)), quoted(&input)),
        };
        out.push_str(&row);
    }
    Response::text(200, out)
}

/// `GET /census/geocoder/geographies/...`: not served here.
pub(crate) fn geographies() -> Response {
    refuse(
        501,
        &["geographies (census tracts, blocks, districts) are not served by gridpin serve: the census layer is a \
           side file of gridpin-pro; /census/geocoder/locations/... answers addresses"],
    )
}
