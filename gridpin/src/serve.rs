//! `gridpin serve`: one sheet behind a small local HTTP server.
//!
//! Two dialects on one port. The native API (`/v1/geocode`, `/v1/reverse`) answers `{"results": [...]}` with
//! exactly the hits `gridpin batch` writes. The Nominatim-compatible paths (`/search`, `/reverse`, `/status`)
//! answer in the shape Nominatim documents (`docs/api/Output.md`, checked 2026-11-01), so a client written for
//! Nominatim (geopy, Leaflet Control Geocoder) only needs a new base URL. Fields we do not have are left out,
//! never invented: no `osm_type` / `osm_id`, no `extratags`, no polygons.
//!
//! HTTP is a deliberately small HTTP/1.1 subset on `std::net`, with no dependency: GET, POST and OPTIONS,
//! a body only with `Content-Length`, one request per connection (`Connection: close`), bounded header and
//! body sizes and a read timeout. The server listens on loopback unless told otherwise; exposing it beyond
//! the machine is the operator's choice (a reverse proxy, or `--host 0.0.0.0` inside a container).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::query::{self, Hit, Index};

/// Request header block limit (request line plus headers).
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
/// POST body limit.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Queries in one POST /v1/geocode.
pub const MAX_BATCH: usize = 10_000;
/// Candidates per query; Nominatim documents the same ceiling for `limit`.
pub const MAX_LIMIT: usize = 40;
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The sheet behind the server and what `/health` and `licence` say about it.
pub struct Engine {
    pub addr: Index,
    pub poi: Option<Index>,
    pub file: String,
    pub bytes: u64,
    pub country: String,
    pub release: String,
    pub attribution: String,
}

impl Engine {
    pub fn open(index: &std::path::Path, poi: Option<&std::path::Path>) -> Result<Engine> {
        let addr = Index::open_address(index)?;
        let poi = poi.map(Index::open).transpose()?;
        if let Some(p) = &poi {
            query::check_pair(&addr, p).map_err(|e| anyhow!(e))?;
        }
        let meta = |key: &str| {
            addr.meta()
                .iter()
                .find(|(k, _)| k.as_str() == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        Ok(Engine {
            file: index
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            bytes: std::fs::metadata(index).map(|m| m.len()).unwrap_or(0),
            country: meta("country").to_ascii_lowercase(),
            release: meta("source_release"),
            attribution: meta("attribution"),
            addr,
            poi,
        })
    }
}

/// Requests served and engine panics caught, for `/health`.
#[derive(Default)]
pub struct Counters {
    pub requests: AtomicU64,
    pub panics: AtomicU64,
    /// false until the start-up warming (`warm`) is done; answers are correct either way, only slower
    pub ready: std::sync::atomic::AtomicBool,
}

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    fn param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Response {
    fn json(status: u16, value: &Value) -> Response {
        Response {
            status,
            content_type: "application/json; charset=utf-8",
            body: value.to_string().into_bytes(),
        }
    }
    fn error(status: u16, message: &str) -> Response {
        Response::json(status, &json!({ "error": message }))
    }
    fn text(status: u16, body: String) -> Response {
        Response {
            status,
            content_type: "text/plain; charset=utf-8",
            body: body.into_bytes(),
        }
    }
}

/// `%XX` and `+` decoding of one query-string component; `None` if it is not UTF-8.
pub fn decode_component(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            b'%' => return None,
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn parse_query(raw: &str) -> Option<Vec<(String, String)>> {
    raw.split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            Some((decode_component(k)?, decode_component(v)?))
        })
        .collect()
}

/// Read one request. `Err((status, message))` is answered as is and the connection closed.
pub fn read_request(reader: &mut impl BufRead) -> std::result::Result<Request, (u16, String)> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = (&mut *reader)
            .take((MAX_HEADER_BYTES + 1 - head.len().min(MAX_HEADER_BYTES)) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|e| (400, format!("cannot read the request: {e}")))?;
        if n == 0 {
            return Err((400, "incomplete request".into()));
        }
        head.extend_from_slice(&line);
        if head.len() > MAX_HEADER_BYTES {
            return Err((431, "request headers too large".into()));
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
    }
    let head =
        String::from_utf8(head).map_err(|_| (400, "request head is not UTF-8".to_string()))?;
    let mut lines = head.lines();
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let (method, target) = match (parts.next(), parts.next(), parts.next()) {
        (Some(m), Some(t), Some(v)) if v.starts_with("HTTP/1.") => (m.to_string(), t),
        _ => return Err((400, "malformed request line".into())),
    };
    let mut length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            length = value
                .parse()
                .map_err(|_| (400, "bad Content-Length".to_string()))?;
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && !value.eq_ignore_ascii_case("identity")
        {
            return Err((411, "send the body with Content-Length".into()));
        }
    }
    if length > MAX_BODY_BYTES {
        return Err((413, format!("request body over {MAX_BODY_BYTES} bytes")));
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|e| (400, format!("cannot read the body: {e}")))?;
    let (path, raw_query) = target.split_once('?').unwrap_or((target, ""));
    let path = decode_component(path).ok_or((400, "path is not valid UTF-8".to_string()))?;
    let query =
        parse_query(raw_query).ok_or((400, "query string is not valid UTF-8".to_string()))?;
    Ok(Request {
        method,
        path,
        query,
        body,
    })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    }
}

pub fn write_response(out: &mut impl Write, resp: &Response) -> std::io::Result<()> {
    write!(
        out,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\n\
         Connection: close\r\n\r\n",
        resp.status,
        reason(resp.status),
        resp.content_type,
        resp.body.len()
    )?;
    out.write_all(&resp.body)?;
    out.flush()
}

/// Half-width of the Nominatim `boundingbox` by precision: a door, a street, a town.
fn half_width(precision: &str) -> f64 {
    match precision {
        "house" | "interp" | "near" | "poi" => 0.0005,
        "city" => 0.05,
        _ => 0.005,
    }
}

fn place_type(precision: &str) -> &'static str {
    match precision {
        "house" | "interp" | "near" => "house",
        "city" => "city",
        "poi" => "poi",
        _ => "street",
    }
}

fn place_rank(precision: &str) -> u32 {
    match precision {
        "house" | "interp" | "near" | "poi" => 30,
        "city" => 16,
        _ => 26,
    }
}

/// A stable id from the answer itself (FNV-1a over coordinates and address), kept below 2^53 so every JSON
/// reader holds it exactly. Like Nominatim's own `place_id`, it is not meant to outlive the sheet release.
pub fn place_id(h: &Hit) -> u64 {
    let key = format!(
        "{:.7},{:.7},{},{}",
        h.lat,
        h.lon,
        h.street,
        h.housenumber.as_deref().unwrap_or("")
    );
    let mut x: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        x ^= u64::from(b);
        x = x.wrapping_mul(0x100000001b3);
    }
    x & ((1 << 53) - 1)
}

/// "1600 Pennsylvania Avenue Northwest, Washington, 20500" — the parts the answer has, comma-separated.
pub fn display_name(h: &Hit) -> String {
    let street = match &h.housenumber {
        Some(n) if !h.street.is_empty() => format!("{n} {}", h.street),
        _ => h.street.clone(),
    };
    [
        street,
        h.commune.clone(),
        h.region.clone().unwrap_or_default(),
        h.postcode.clone(),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(", ")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialect {
    Json,
    JsonV2,
    GeoJson,
}

impl Dialect {
    /// `format=`; absent means `jsonv2` (Nominatim's documented default for search). XML and the rest are refused.
    pub fn parse(format: Option<&str>) -> std::result::Result<Dialect, String> {
        match format.unwrap_or("jsonv2") {
            "json" => Ok(Dialect::Json),
            "jsonv2" => Ok(Dialect::JsonV2),
            "geojson" => Ok(Dialect::GeoJson),
            other => Err(format!(
                "format={other} is not supported: use json, jsonv2 or geojson"
            )),
        }
    }
}

/// One answer as a Nominatim place object (`json` / `jsonv2`) or as GeoJSON feature properties.
pub fn nominatim_place(
    h: &Hit,
    dialect: Dialect,
    address_details: bool,
    country: &str,
    licence: &str,
) -> Value {
    let w = half_width(h.precision);
    let mut o = Map::new();
    o.insert("place_id".into(), json!(place_id(h)));
    if dialect != Dialect::GeoJson {
        o.insert("licence".into(), json!(licence));
        o.insert("lat".into(), json!(format!("{:.7}", h.lat)));
        o.insert("lon".into(), json!(format!("{:.7}", h.lon)));
        o.insert(
            "boundingbox".into(),
            json!([
                format!("{:.7}", h.lat - w),
                format!("{:.7}", h.lat + w),
                format!("{:.7}", h.lon - w),
                format!("{:.7}", h.lon + w)
            ]),
        );
    }
    let class_key = if dialect == Dialect::Json {
        "class"
    } else {
        "category"
    };
    o.insert(class_key.into(), json!("place"));
    o.insert("type".into(), json!(place_type(h.precision)));
    if dialect != Dialect::Json {
        o.insert("place_rank".into(), json!(place_rank(h.precision)));
    }
    // the f32 as it prints (0.94), not its widened f64 (0.9399999976158142)
    o.insert(
        "importance".into(),
        json!(h.confidence.to_string().parse::<f64>().unwrap_or(0.0)),
    );
    o.insert("display_name".into(), json!(display_name(h)));
    if address_details {
        let mut a = Map::new();
        if let Some(n) = &h.housenumber {
            a.insert("house_number".into(), json!(n));
        }
        for (key, value) in [
            ("road", &h.street),
            ("city", &h.commune),
            ("postcode", &h.postcode),
        ] {
            if !value.is_empty() {
                a.insert(key.into(), json!(value));
            }
        }
        if let Some(r) = &h.region {
            a.insert("state".into(), json!(r));
        }
        if !country.is_empty() {
            a.insert("country_code".into(), json!(country));
        }
        o.insert("address".into(), Value::Object(a));
    }
    Value::Object(o)
}

fn geojson_feature(h: &Hit, address_details: bool, country: &str, licence: &str) -> Value {
    let w = half_width(h.precision);
    json!({
        "type": "Feature",
        "properties": nominatim_place(h, Dialect::GeoJson, address_details, country, licence),
        "bbox": [h.lon - w, h.lat - w, h.lon + w, h.lat + w],
        "geometry": { "type": "Point", "coordinates": [h.lon, h.lat] }
    })
}

/// A list of answers as Nominatim returns them for `/search`.
pub fn nominatim_list(
    hits: &[Hit],
    dialect: Dialect,
    address_details: bool,
    country: &str,
    licence: &str,
) -> Value {
    match dialect {
        Dialect::GeoJson => json!({
            "type": "FeatureCollection",
            "licence": licence,
            "features": hits.iter().map(|h| geojson_feature(h, address_details, country, licence)).collect::<Vec<_>>()
        }),
        _ => Value::Array(
            hits.iter()
                .map(|h| nominatim_place(h, dialect, address_details, country, licence))
                .collect(),
        ),
    }
}

fn flag(req: &Request, name: &str, default: bool) -> bool {
    match req.param(name) {
        Some(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        None => default,
    }
}

fn count(req: &Request, name: &str, default: usize) -> std::result::Result<usize, Response> {
    match req.param(name) {
        None => Ok(default),
        Some(v) => match v.parse::<usize>() {
            Ok(n) if n >= 1 => Ok(n.min(MAX_LIMIT)),
            _ => Err(Response::error(
                400,
                &format!("{name} must be a whole number from 1"),
            )),
        },
    }
}

fn coordinate(req: &Request, name: &str) -> std::result::Result<f64, Response> {
    req.param(name)
        .ok_or_else(|| Response::error(400, &format!("missing {name}")))?
        .parse::<f64>()
        .map_err(|_| Response::error(400, &format!("{name} must be a number")))
}

/// Free-form `q`, or the structured search fields joined as one free-form query.
fn search_text(req: &Request) -> Option<String> {
    if let Some(q) = req.param("q") {
        return Some(q.to_string());
    }
    let parts: Vec<&str> = ["street", "city", "county", "state", "postalcode", "country"]
        .iter()
        .filter_map(|k| req.param(k))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Run an engine call; a panic inside it is counted and answers as empty.
fn guarded<T>(counters: &Counters, empty: T, f: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        counters.panics.fetch_add(1, Ordering::Relaxed);
        empty
    })
}

fn geocode(engine: &Engine, counters: &Counters, q: &str, k: usize) -> Vec<Hit> {
    guarded(counters, Vec::new(), || {
        query::query_cascade(&engine.addr, engine.poi.as_ref(), q, k)
    })
}

fn too_long(q: &str) -> Option<Response> {
    (q.len() > query::MAX_QUERY_BYTES)
        .then(|| Response::error(414, &format!("query over {} bytes", query::MAX_QUERY_BYTES)))
}

/// Answer one request. Pure apart from the engine call, so the routes are testable without a socket.
pub fn route(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    counters.requests.fetch_add(1, Ordering::Relaxed);
    if req.method == "OPTIONS" {
        return Response {
            status: 204,
            content_type: "text/plain; charset=utf-8",
            body: Vec::new(),
        };
    }
    let post = req.method == "POST";
    if req.method != "GET" && !(post && req.path == "/v1/geocode") {
        return Response::error(405, "use GET (POST only for /v1/geocode)");
    }
    let result = match req.path.as_str() {
        "/" => Ok(Response::text(200, home(engine))),
        "/health" => Ok(Response::json(200, &health(engine, counters))),
        "/v1/geocode" if post => v1_geocode_many(engine, counters, req),
        "/v1/geocode" => v1_geocode(engine, counters, req),
        "/v1/reverse" => v1_reverse(engine, counters, req),
        "/search" | "/search.php" => search(engine, counters, req),
        "/reverse" | "/reverse.php" => reverse(engine, counters, req),
        "/status" | "/status.php" => Ok(status(engine, req)),
        _ => Err(Response::error(404, "no such path; see / for the list")),
    };
    result.unwrap_or_else(|r| r)
}

fn home(engine: &Engine) -> String {
    format!(
        "GridPin {} — offline geocoder, sheet {} ({}).\n\n\
         GET  /v1/geocode?q=...&k=1[&near=lat,lon]   {{\"results\": [...]}}\n\
         POST /v1/geocode  {{\"queries\": [...], \"k\": 1}}   {{\"results\": [[...], ...]}}\n\
         GET  /v1/reverse?lat=...&lon=...&k=1\n\
         GET  /search?q=...&format=jsonv2|json|geojson&limit=10&addressdetails=1   Nominatim-compatible\n\
         GET  /reverse?lat=...&lon=...&format=jsonv2                              Nominatim-compatible\n\
         GET  /status?format=json\n\
         GET  /health\n\n\
         Documentation: https://gridpin.dev/geocoding-server\n",
        env!("CARGO_PKG_VERSION"),
        engine.file,
        engine.country
    )
}

fn health(engine: &Engine, counters: &Counters) -> Value {
    json!({
        "status": if counters.ready.load(Ordering::Acquire) { "ok" } else { "warming" },
        "engine": env!("CARGO_PKG_VERSION"),
        "sheet": {
            "file": engine.file,
            "bytes": engine.bytes,
            "country": engine.country,
            "source_release": engine.release,
        },
        "requests": counters.requests.load(Ordering::Relaxed),
        "panics": counters.panics.load(Ordering::Relaxed),
    })
}

fn v1_geocode(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
) -> std::result::Result<Response, Response> {
    let q = req
        .param("q")
        .ok_or_else(|| Response::error(400, "missing q"))?;
    if let Some(r) = too_long(q) {
        return Err(r);
    }
    let k = count(req, "k", 1)?;
    let hits = match req.param("near") {
        None => geocode(engine, counters, q, k),
        Some(raw) => {
            let (lat, lon) = raw
                .split_once(',')
                .and_then(|(a, b)| {
                    Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))
                })
                .ok_or_else(|| Response::error(400, "near must be LAT,LON"))?;
            guarded(counters, Ok(Vec::new()), || {
                query::query_cascade_near(&engine.addr, engine.poi.as_ref(), q, k, lat, lon)
            })
            .map_err(|e| Response::error(400, &e))?
        }
    };
    Ok(Response::json(200, &json!({ "results": hits })))
}

fn v1_geocode_many(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
) -> std::result::Result<Response, Response> {
    let body: Value = serde_json::from_slice(&req.body)
        .map_err(|_| Response::error(400, "body must be JSON: {\"queries\": [...]}"))?;
    let queries = body
        .get("queries")
        .and_then(Value::as_array)
        .ok_or_else(|| Response::error(400, "body must be JSON: {\"queries\": [...]}"))?;
    if queries.len() > MAX_BATCH {
        return Err(Response::error(
            413,
            &format!("more than {MAX_BATCH} queries; split the batch"),
        ));
    }
    let k = match body.get("k") {
        None => 1,
        Some(v) => v
            .as_u64()
            .filter(|&n| n >= 1)
            .map(|n| (n as usize).min(MAX_LIMIT))
            .ok_or_else(|| Response::error(400, "k must be a whole number from 1"))?,
    };
    let texts: Vec<&str> = queries
        .iter()
        .map(|q| {
            q.as_str()
                .ok_or_else(|| Response::error(400, "every query must be a string"))
        })
        .collect::<std::result::Result<_, _>>()?;
    if let Some(r) = texts.iter().find_map(|q| too_long(q)) {
        return Err(r);
    }
    // spread over the global rayon pool, like `gridpin batch`; order is kept
    let results: Vec<Vec<Hit>> = {
        use rayon::prelude::*;
        texts
            .par_iter()
            .map(|q| geocode(engine, counters, q, k))
            .collect()
    };
    Ok(Response::json(200, &json!({ "results": results })))
}

fn v1_reverse(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
) -> std::result::Result<Response, Response> {
    let (lat, lon) = (coordinate(req, "lat")?, coordinate(req, "lon")?);
    let k = count(req, "k", 1)?;
    let hits = guarded(counters, Ok(Vec::new()), || {
        engine.addr.try_reverse(lat, lon, k)
    })
    .map_err(|e| Response::error(400, &e))?;
    Ok(Response::json(200, &json!({ "results": hits })))
}

fn search(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
) -> std::result::Result<Response, Response> {
    let dialect = Dialect::parse(req.param("format")).map_err(|e| Response::error(400, &e))?;
    let q = search_text(req).ok_or_else(|| Response::error(400, "missing q"))?;
    if let Some(r) = too_long(&q) {
        return Err(r);
    }
    let limit = count(req, "limit", 10)?;
    let details = flag(req, "addressdetails", false);
    let outside = req.param("countrycodes").is_some_and(|codes| {
        let codes: Vec<&str> = codes
            .split(',')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .collect();
        !codes.is_empty()
            && !codes
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&engine.country))
    });
    let hits = if outside {
        Vec::new()
    } else {
        geocode(engine, counters, &q, limit)
    };
    Ok(Response::json(
        200,
        &nominatim_list(
            &hits,
            dialect,
            details,
            &engine.country,
            &engine.attribution,
        ),
    ))
}

fn reverse(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
) -> std::result::Result<Response, Response> {
    let dialect = Dialect::parse(req.param("format")).map_err(|e| Response::error(400, &e))?;
    let (lat, lon) = (coordinate(req, "lat")?, coordinate(req, "lon")?);
    let details = flag(req, "addressdetails", true);
    let hits = guarded(counters, Ok(Vec::new()), || {
        engine.addr.try_reverse(lat, lon, 1)
    })
    .map_err(|e| Response::error(400, &e))?;
    let Some(hit) = hits.first() else {
        // Nominatim's own answer when nothing is there (src/nominatim_api/v1/format_json.py)
        return Ok(Response::json(
            200,
            &json!({ "error": "Unable to geocode" }),
        ));
    };
    Ok(Response::json(
        200,
        &match dialect {
            Dialect::GeoJson => nominatim_list(
                std::slice::from_ref(hit),
                dialect,
                details,
                &engine.country,
                &engine.attribution,
            ),
            _ => nominatim_place(hit, dialect, details, &engine.country, &engine.attribution),
        },
    ))
}

fn status(engine: &Engine, req: &Request) -> Response {
    if req.param("format") == Some("json") {
        Response::json(
            200,
            &json!({
                "status": 0,
                "message": "OK",
                "data_updated": engine.release,
                "software_version": format!("gridpin {}", env!("CARGO_PKG_VERSION")),
                "database_version": engine.release,
            }),
        )
    } else {
        Response::text(200, "OK".into())
    }
}

fn handle(engine: &Engine, counters: &Counters, stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let resp = match read_request(&mut reader) {
        Ok(req) => route(engine, counters, &req),
        Err((status, message)) => Response::error(status, &message),
    };
    let mut out = stream;
    let _ = write_response(&mut out, &resp);
}

/// Queries that send the engine down its fallback paths at start-up. Two of them build a table over every
/// street of the sheet on first use (`field_street_projection`, `field_title_streets` in query.rs): on the
/// United States sheet the first unanswerable query still takes about 3 s (once per distinct name, on all
/// cores), every later one milliseconds — longer than geopy waits (1 s). Warming runs them once in the
/// background before real clients arrive.
const WARM_QUERIES: [&str; 2] = ["zzzz qqqq", "1 zzzz street, qqqq"];

/// Read the sheet into the page cache and run `WARM_QUERIES`; `ready` turns true when done.
fn warm(
    engine: Arc<Engine>,
    path: std::path::PathBuf,
    counters: Arc<Counters>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        if let Ok(mut f) = std::fs::File::open(&path) {
            let mut buf = vec![0u8; 8 << 20];
            while matches!(f.read(&mut buf), Ok(n) if n > 0) {}
        }
        for q in WARM_QUERIES {
            geocode(&engine, &counters, q, 1);
        }
        counters.ready.store(true, Ordering::Release);
        eprintln!(
            "ready: sheet read and fallback tables built in {:.1} s",
            t0.elapsed().as_secs_f64()
        );
    })
}

/// Bind, print `listening on http://HOST:PORT` (the first line of stdout, which tests and containers read)
/// and serve until the process is stopped. `threads` connections are handled at once.
pub fn run(
    engine: Engine,
    sheet: std::path::PathBuf,
    host: &str,
    port: u16,
    threads: usize,
) -> Result<()> {
    let listener = TcpListener::bind((host, port))
        .with_context(|| format!("cannot listen on {host}:{port}"))?;
    let local = listener.local_addr()?;
    let shown = if local.is_ipv6() {
        format!("[{}]", local.ip())
    } else {
        local.ip().to_string()
    };
    println!("listening on http://{shown}:{}", local.port());
    std::io::stdout().flush()?;
    let engine = Arc::new(engine);
    let counters = Arc::new(Counters::default());
    warm(Arc::clone(&engine), sheet, Arc::clone(&counters));
    let mut workers = Vec::new();
    for _ in 0..threads.max(1) {
        let listener = listener.try_clone()?;
        let (engine, counters) = (Arc::clone(&engine), Arc::clone(&counters));
        workers.push(std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                handle(&engine, &counters, stream);
            }
        }));
    }
    for w in workers {
        let _ = w.join();
    }
    bail!("server stopped")
}

/// `--probe URL`: one GET, success only on status 200. For container health checks, where there is no curl.
pub fn probe(url: &str) -> Result<()> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("--probe takes an http:// URL"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr = std::net::ToSocketAddrs::to_socket_addrs(authority)?
        .next()
        .ok_or_else(|| anyhow!("cannot resolve {authority}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    )?;
    let mut first = String::new();
    BufReader::new(stream).read_line(&mut first)?;
    if first.split(' ').nth(1) == Some("200") {
        Ok(())
    } else {
        bail!("probe {url}: {}", first.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(precision: &'static str, number: Option<&str>) -> Hit {
        Hit {
            lat: 38.8976751,
            lon: -77.036547,
            precision,
            score: 7.0,
            confidence: 0.95,
            street: "PENNSYLVANIA Avenue Northwest".into(),
            housenumber: number.map(String::from),
            commune: "Washington".into(),
            postcode: "20500".into(),
            flags: vec![],
            region: None,
            distance_m: None,
        }
    }

    #[test]
    fn a_place_carries_coordinates_as_strings_and_a_box_that_widens_with_precision() {
        let house = nominatim_place(
            &hit("house", Some("1600")),
            Dialect::Json,
            false,
            "us",
            "lic",
        );
        assert_eq!(house["lat"], "38.8976751");
        assert_eq!(house["lon"], "-77.0365470");
        assert_eq!(house["licence"], "lic");
        assert_eq!(house["class"], "place");
        assert_eq!(house["type"], "house");
        assert_eq!(
            house["display_name"],
            "1600 PENNSYLVANIA Avenue Northwest, Washington, 20500"
        );
        let bbox = |v: &Value| -> f64 {
            let b: Vec<f64> = v["boundingbox"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().parse().unwrap())
                .collect();
            assert!(b[0] < b[1] && b[2] < b[3], "{b:?}");
            b[1] - b[0]
        };
        let street = nominatim_place(&hit("street", None), Dialect::Json, false, "us", "lic");
        let city = nominatim_place(&hit("city", None), Dialect::Json, false, "us", "lic");
        assert!(bbox(&house) < bbox(&street) && bbox(&street) < bbox(&city));
        assert_eq!(street["type"], "street");
        assert_eq!(
            street["display_name"],
            "PENNSYLVANIA Avenue Northwest, Washington, 20500"
        );
    }

    #[test]
    fn no_osm_reference_is_invented_and_jsonv2_renames_class() {
        let v1 = nominatim_place(
            &hit("house", Some("1600")),
            Dialect::Json,
            false,
            "us",
            "lic",
        );
        assert!(v1.get("osm_id").is_none() && v1.get("osm_type").is_none());
        let v2 = nominatim_place(
            &hit("house", Some("1600")),
            Dialect::JsonV2,
            false,
            "us",
            "lic",
        );
        assert!(v2.get("class").is_none());
        assert_eq!(v2["category"], "place");
        assert_eq!(v2["place_rank"], 30);
    }

    #[test]
    fn address_details_only_when_asked_and_without_empty_parts() {
        let none = nominatim_place(&hit("street", None), Dialect::Json, false, "us", "lic");
        assert!(none.get("address").is_none());
        let a = &nominatim_place(&hit("street", None), Dialect::Json, true, "us", "lic")["address"];
        assert_eq!(a["road"], "PENNSYLVANIA Avenue Northwest");
        assert_eq!(a["country_code"], "us");
        assert!(a.get("house_number").is_none());
        let a = &nominatim_place(
            &hit("house", Some("1600")),
            Dialect::Json,
            true,
            "us",
            "lic",
        )["address"];
        assert_eq!(a["house_number"], "1600");
    }

    #[test]
    fn geojson_is_a_feature_collection_with_point_and_bbox() {
        let v = nominatim_list(
            &[hit("house", Some("1600"))],
            Dialect::GeoJson,
            false,
            "us",
            "lic",
        );
        assert_eq!(v["type"], "FeatureCollection");
        let f = &v["features"][0];
        assert_eq!(f["geometry"]["coordinates"][0], -77.036547);
        assert_eq!(f["bbox"].as_array().unwrap().len(), 4);
        assert!(f["properties"].get("lat").is_none());
    }

    #[test]
    fn place_ids_are_stable_and_safe_for_json_numbers() {
        let a = place_id(&hit("house", Some("1600")));
        assert_eq!(a, place_id(&hit("house", Some("1600"))));
        assert_ne!(a, place_id(&hit("house", Some("1602"))));
        assert!(a < (1 << 53));
    }

    #[test]
    fn query_components_decode_plus_and_percent_and_refuse_broken_escapes() {
        assert_eq!(
            decode_component("1600+Pennsylvania%20Ave%2C%20DC").unwrap(),
            "1600 Pennsylvania Ave, DC"
        );
        assert_eq!(decode_component("M%C3%BCller").unwrap(), "Müller");
        assert!(decode_component("%E0%A4").is_none());
        assert!(decode_component("50%").is_none());
        assert!(decode_component("%zz").is_none());
    }

    #[test]
    fn requests_are_read_with_bounds() {
        let ok =
            read_request(&mut "GET /search?q=a+b&limit=2 HTTP/1.1\r\nHost: x\r\n\r\n".as_bytes())
                .unwrap();
        assert_eq!((ok.method.as_str(), ok.path.as_str()), ("GET", "/search"));
        assert_eq!(ok.param("q"), Some("a b"));
        let post = read_request(
            &mut "POST /v1/geocode HTTP/1.1\r\nContent-Length: 4\r\n\r\nabcd".as_bytes(),
        )
        .unwrap();
        assert_eq!(post.body, b"abcd");
        let huge = format!(
            "GET / HTTP/1.1\r\nX: {}\r\n\r\n",
            "a".repeat(MAX_HEADER_BYTES)
        );
        assert_eq!(read_request(&mut huge.as_bytes()).err().unwrap().0, 431);
        let big = format!(
            "POST /v1/geocode HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        assert_eq!(read_request(&mut big.as_bytes()).err().unwrap().0, 413);
        let chunked = "POST /v1/geocode HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(read_request(&mut chunked.as_bytes()).err().unwrap().0, 411);
        assert_eq!(
            read_request(&mut "BREW\r\n\r\n".as_bytes())
                .err()
                .unwrap()
                .0,
            400
        );
    }

    #[test]
    fn formats_other_than_json_are_refused_and_jsonv2_is_the_default() {
        assert_eq!(Dialect::parse(None).unwrap(), Dialect::JsonV2);
        assert_eq!(Dialect::parse(Some("json")).unwrap(), Dialect::Json);
        assert!(Dialect::parse(Some("xml")).is_err());
    }
}
