//! `gridpin serve`: one sheet behind a small local HTTP server.
//!
//! Three dialects on one port. The Google path (`/maps/api/geocode/json`, see `google.rs`) answers in the shape
//! of the Google Geocoding API. The native API (`/v1/geocode`, `/v1/reverse`) answers `{"results": [...]}` with
//! exactly the hits `gridpin batch` writes. The Nominatim-compatible paths (`/search`, `/reverse`, `/status`)
//! answer in the shape Nominatim documents (`docs/api/Output.md`, checked 2026-11-01), so a client written for
//! Nominatim (geopy, Leaflet Control Geocoder) only needs a new base URL. Fields we do not have are left out,
//! never invented: no `osm_type` / `osm_id`, no polygons. `extratags=1` carries `match_level` and
//! `match_status`, which Nominatim has no field for.
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

use crate::aliases;
use crate::extras::Extras;
use crate::google;
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
    /// `--aliases`: names read before the sheet
    pub aliases: Option<crate::aliases::Aliases>,
    /// `--sheets`: more country sheets beside `addr`, each with its country code and its answer flag
    pub others: Vec<Sheet>,
}

/// One more country sheet of a server that answers several countries.
pub struct Sheet {
    pub country: String,
    pub flag: &'static str,
    pub index: Index,
    pub file: String,
    pub bytes: u64,
    pub release: String,
    pub attribution: String,
}

/// The sheet one request is asked of: its index, its country and licence for the answer, and the flag its hits carry
/// on a server of several sheets (`None` with one sheet, so a one-sheet answer is byte for byte what it was).
#[derive(Clone, Copy)]
pub struct Pick<'a> {
    pub index: &'a Index,
    pub poi: Option<&'a Index>,
    pub country: &'a str,
    pub attribution: &'a str,
    pub flag: Option<&'static str>,
}

impl Pick<'_> {
    fn mark(&self, hits: &mut [Hit]) {
        if let Some(f) = self.flag {
            hits.iter_mut().for_each(|h| h.flags.push(f));
        }
    }
}

fn country_flag(cc: &str) -> &'static str {
    // one leaked string per sheet, at start-up: flags are static strings
    Box::leak(format!("country_{cc}").into_boxed_str())
}

fn meta_of(index: &Index, key: &str) -> String {
    index
        .meta()
        .iter()
        .find(|(k, _)| k.as_str() == key)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

impl Engine {
    /// Add a country sheet beside the main one (`serve --sheets`). With one sheet the server answers as before.
    pub fn add_sheet(&mut self, path: &std::path::Path) -> Result<()> {
        let index = Index::open_address(path)?;
        let country = meta_of(&index, "country").to_ascii_lowercase();
        if country.is_empty() {
            bail!(
                "{}: a sheet of --sheets needs a country in its passport",
                path.display()
            );
        }
        if country == self.country || self.others.iter().any(|o| o.country == country) {
            bail!("--sheets: two sheets of the country {country:?}");
        }
        self.others.push(Sheet {
            flag: country_flag(&country),
            file: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            release: meta_of(&index, "source_release"),
            attribution: meta_of(&index, "attribution"),
            country,
            index,
        });
        Ok(())
    }

    /// Every sheet of the server, the main one first. With one sheet its pick carries no flag.
    fn picks(&self) -> Vec<Pick<'_>> {
        let several = !self.others.is_empty();
        std::iter::once(Pick {
            index: &self.addr,
            poi: self.poi.as_ref(),
            country: &self.country,
            attribution: &self.attribution,
            flag: several.then(|| country_flag_cached(&self.country)),
        })
        .chain(self.others.iter().map(|o| Pick {
            index: &o.index,
            poi: None,
            country: &o.country,
            attribution: &o.attribution,
            flag: Some(o.flag),
        }))
        .collect()
    }

    /// Does the server have a sheet of this country (lower case)?
    fn serves(&self, cc: &str) -> bool {
        self.country == cc || self.others.iter().any(|o| o.country == cc)
    }

    /// The sheet a query is asked of. `asked` is the countries the request allows (`country=`, `countrycodes=`,
    /// `components=country:`), every one served by this server; empty allows all. Among the allowed sheets: the one
    /// whose region the last segment names ("…, Los Angeles, CA" is California, not Canada), else the one whose own
    /// country words close the query ("…, Monaco"), else the one the shared table of country names finds at the end,
    /// else the main sheet (the first allowed one when `asked` leaves the main sheet out). Another country's sheet
    /// never answers a query that names neither its country nor one of its regions: four Kosovo rows of the Serbian
    /// set, silent on the Serbian sheet, got a far answer from the Dutch one when every sheet was asked (ANSWERS §279).
    pub fn pick(&self, q: &str, asked: &[String]) -> Pick<'_> {
        let all = self.picks();
        let allowed: Vec<Pick> = all
            .iter()
            .copied()
            .filter(|p| asked.is_empty() || asked.iter().any(|a| a == p.country))
            .collect();
        let Some(&first) = allowed.first() else {
            return all[0];
        };
        if allowed.len() == 1 {
            return first;
        }
        if let Some((_, tail)) = q.rsplit_once(',') {
            let holders: Vec<Pick> = allowed
                .iter()
                .copied()
                .filter(|p| query::names_region_of(p.index, tail))
                .collect();
            if !holders.is_empty() {
                // a region code is never read as a country code; two sheets with that region leave the first sheet
                return if holders.len() == 1 {
                    holders[0]
                } else {
                    first
                };
            }
        }
        let tail = q
            .rsplit(',')
            .next()
            .map(crate::norm::normalize)
            .unwrap_or_default();
        let by_words: Vec<Pick> = allowed
            .iter()
            .copied()
            .filter(|p| !tail.is_empty() && p.index.rule_set().countries_tail.contains(&tail))
            .collect();
        if by_words.len() == 1 {
            return by_words[0];
        }
        // the country named at the end in any language of the shared table (country_names.tsv), for sheets whose rule
        // pack carries no country words (09.11.2026: "…, Serbia", "…, España" stayed on the first sheet)
        crate::country_names::named_at_end(q)
            .and_then(|cc| {
                allowed
                    .iter()
                    .find(|p| p.country.eq_ignore_ascii_case(&cc))
                    .copied()
            })
            .unwrap_or(first)
    }

    /// The nearest addresses of a point: of the sheet `asked` names, else of the sheet whose nearest address is the
    /// closest (a point in Belgrade is answered by the Serbian sheet, not by the nearest house of the main US sheet).
    fn reverse_pick(
        &self,
        lat: f64,
        lon: f64,
        k: usize,
        asked: &[String],
    ) -> std::result::Result<(Vec<Hit>, Pick<'_>), String> {
        let mut best: Option<(f64, Vec<Hit>, Pick)> = None;
        for p in self
            .picks()
            .into_iter()
            .filter(|p| asked.is_empty() || asked.iter().any(|a| a == p.country))
        {
            let hits = p.index.try_reverse(lat, lon, k)?;
            let d = hits
                .first()
                .map_or(f64::INFINITY, |h| metres(lat, lon, h.lat, h.lon));
            if best.as_ref().is_none_or(|(b, _, _)| d < *b) {
                best = Some((d, hits, p));
            }
        }
        let (_, mut hits, p) = best.unwrap_or_else(|| (0.0, Vec::new(), self.picks()[0]));
        p.mark(&mut hits);
        Ok((hits, p))
    }
}

/// Great-circle distance in metres, to compare the nearest answers of two sheets.
fn metres(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let a = ((p2 - p1) / 2.0).sin().powi(2)
        + p1.cos() * p2.cos() * ((lon2 - lon1).to_radians() / 2.0).sin().powi(2);
    2.0 * 6_371_000.0 * a.sqrt().asin()
}

fn country_flag_cached(cc: &str) -> &'static str {
    use std::sync::{Mutex, OnceLock};
    static FLAGS: OnceLock<Mutex<std::collections::HashMap<String, &'static str>>> =
        OnceLock::new();
    let mut m = FLAGS.get_or_init(Default::default).lock().expect("flags");
    m.entry(cc.to_string()).or_insert_with(|| country_flag(cc))
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
            aliases: None,
            others: Vec::new(),
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
    /// the `Authorization` header, for a `Gate` (keys of a paid server); the open server reads none
    pub authorization: Option<String>,
    /// the request headers beyond those the server itself reads, lower-cased names (for a `Gate`)
    pub headers: Vec<(String, String)>,
}

/// A hook around every request: `before` may answer instead of the server (a key refused, a limit reached), `after`
/// sees the answer and its time (counters, a log). The open server runs with `NoGate`; a paid server brings its own.
pub trait Gate: Send + Sync {
    fn before(&self, _req: &Request) -> Option<Response> {
        None
    }
    fn after(&self, _req: &Request, _resp: &Response, _ms: f64) {}
}

/// The gate of the open server: lets every request through, records nothing.
pub struct NoGate;
impl Gate for NoGate {}

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
    fn raw_json(status: u16, body: String) -> Response {
        Response {
            status,
            content_type: "application/json; charset=utf-8",
            body: body.into_bytes(),
        }
    }
    pub fn error(status: u16, message: &str) -> Response {
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
    let mut authorization = None;
    let mut headers = Vec::new();
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
        } else if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.to_string());
        } else if headers.len() < 32 {
            headers.push((name.trim().to_ascii_lowercase(), value.to_string()));
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
        authorization,
        headers,
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
pub(crate) fn half_width(precision: &str) -> f64 {
    match precision {
        "house" | "interp" | "near" | "poi" | "intersection" => 0.0005,
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

/// The optional blocks of a Nominatim answer: `addressdetails=1`, `extratags=1`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Show {
    pub address_details: bool,
    pub extra_tags: bool,
}

/// One answer as a Nominatim place object (`json` / `jsonv2`) or as GeoJSON feature properties.
pub fn nominatim_place(
    h: &Hit,
    dialect: Dialect,
    show: Show,
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
    if show.extra_tags {
        let (level, status) = query::match_level(h);
        o.insert(
            "extratags".into(),
            json!({ "match_level": level, "match_status": status.to_string() }),
        );
    }
    if show.address_details {
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

fn geojson_feature(h: &Hit, show: Show, country: &str, licence: &str) -> Value {
    let w = half_width(h.precision);
    json!({
        "type": "Feature",
        "properties": nominatim_place(h, Dialect::GeoJson, show, country, licence),
        "bbox": [h.lon - w, h.lat - w, h.lon + w, h.lat + w],
        "geometry": { "type": "Point", "coordinates": [h.lon, h.lat] }
    })
}

/// A list of answers as Nominatim returns them for `/search`.
pub fn nominatim_list(
    hits: &[Hit],
    dialect: Dialect,
    show: Show,
    country: &str,
    licence: &str,
) -> Value {
    match dialect {
        Dialect::GeoJson => json!({
            "type": "FeatureCollection",
            "licence": licence,
            "features": hits.iter().map(|h| geojson_feature(h, show, country, licence)).collect::<Vec<_>>()
        }),
        _ => Value::Array(
            hits.iter()
                .map(|h| nominatim_place(h, dialect, show, country, licence))
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

/// The answers to one line and the sheet that gave them. The sheet is chosen by the line the engine reads (an alias
/// is read as its address) among the countries `asked` allows; on a server of several sheets the hits carry
/// `country_<cc>`. The choice travels with the call, never in a thread-local: `POST /v1/geocode` answers on the rayon
/// pool, whose threads saw no `country=` (revision 0.9.0: 4 answers of 200 instead of 181).
fn geocode<'a>(
    engine: &'a Engine,
    counters: &Counters,
    q: &str,
    k: usize,
    asked: &[String],
) -> (Vec<Hit>, Pick<'a>) {
    let mut chosen = None;
    let hits = aliases::answer(engine.aliases.as_ref(), q, |text| {
        let p = engine.pick(text, asked);
        chosen = Some(p);
        let mut hits = guarded(counters, Vec::new(), || {
            query::query_cascade(p.index, p.poi, text, k)
        });
        p.mark(&mut hits);
        hits
    })
    .0;
    (hits, chosen.unwrap_or_else(|| engine.pick(q, asked)))
}

/// The hits of the native API: as they are, and with `alias_name` when the query was a name.
fn native_hits(engine: &Engine, q: &str, hits: &[Hit]) -> Value {
    let name = engine
        .aliases
        .as_ref()
        .and_then(|table| table.find(q))
        .map(|alias| alias.name.as_str());
    Value::Array(
        hits.iter()
            .filter_map(|h| aliases::hit_json(h, name).ok())
            .collect(),
    )
}

/// `with=` of a request read against the sheet; a bad spec is a 400.
fn extras_of(engine: &Engine, raw: Option<&str>) -> std::result::Result<Option<Extras>, Response> {
    Extras::new(raw, &engine.addr).map_err(|e| Response::error(400, &e.to_string()))
}

/// `with=` read against the sheet that answered (its provenance card); the spec was checked by `extras_of` before.
fn extras_on(raw: Option<&str>, p: &Pick) -> Option<Extras> {
    Extras::new(raw, p.index).ok().flatten()
}

/// `{"results": [...]}` with `extras` on every hit, the alias name of `q` when the sheet has aliases.
fn results_with(
    engine: &Engine,
    extras: &Extras,
    q: Option<&str>,
    hits: &[Hit],
    idx: &Index,
) -> Response {
    let name = q
        .and_then(|q| engine.aliases.as_ref()?.find(q))
        .map(|a| a.name.as_str());
    let write = || crate::extras::hits_text(hits, Some(extras), idx, name);
    let rows = match q {
        Some(q) => crate::extras::for_query(q, write),
        None => write(),
    }
    .unwrap_or_else(|_| "[]".to_string());
    Response::raw_json(200, format!("{{\"results\":{rows}}}"))
}

/// `extras` on each place of a Nominatim or Google answer, in hit order: the array itself, the
/// `features[i].properties` of GeoJSON, or the `results` of Google (which also gets its own `plus_code`
/// field when codes are asked for). A single place object (Nominatim reverse) takes it directly.
fn attach_extras(answer: &mut Value, hits: &[Hit], extras: &Extras, idx: &Index, q: Option<&str>) {
    let places: Vec<&mut Value> = match answer {
        Value::Array(items) => items.iter_mut().collect(),
        Value::Object(o) if o.contains_key("features") => o["features"]
            .as_array_mut()
            .map(|f| {
                f.iter_mut()
                    .filter_map(|x| x.get_mut("properties"))
                    .collect()
            })
            .unwrap_or_default(),
        Value::Object(o) if o.contains_key("results") => o["results"]
            .as_array_mut()
            .map(|r| r.iter_mut().collect())
            .unwrap_or_default(),
        Value::Object(_) => vec![answer],
        _ => Vec::new(),
    };
    let google = extras.with.codes;
    for (place, hit) in places.into_iter().zip(hits) {
        if let Value::Object(o) = place {
            let fields = match q {
                Some(q) => crate::extras::for_query(q, || extras.of(hit, idx)),
                None => extras.of(hit, idx),
            };
            if google && o.contains_key("geometry") && o.contains_key("address_components") {
                o.insert(
                    "plus_code".into(),
                    json!({ "global_code": crate::codes::plus_code_encode(hit.lat, hit.lon, 10) }),
                );
            }
            o.insert("extras".into(), Value::Object(fields));
        }
    }
}

fn too_long(q: &str) -> Option<Response> {
    (q.len() > query::MAX_QUERY_BYTES)
        .then(|| Response::error(414, &format!("query over {} bytes", query::MAX_QUERY_BYTES)))
}

/// Every route of the server: (method, path, what it answers). The OpenAPI document (`tools/openapi.py`) is built
/// from this table, and a guard holds it equal to the paths `route` answers.
pub const ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/", "a plain-text list of the routes"),
    (
        "GET",
        "/health",
        "the sheet, the counters and whether the server is warm",
    ),
    ("GET", "/ui", "a page: drop a CSV, get it back geocoded"),
    (
        "GET",
        "/v1/geocode",
        "an address or place: q, k, near, with",
    ),
    ("POST", "/v1/geocode", "many queries: {queries, k, with}"),
    (
        "POST",
        "/v1/extract",
        "the addresses found in a text: {text, verify}",
    ),
    (
        "GET",
        "/v1/reverse",
        "the nearest addresses of a point: lat, lon, k, with",
    ),
    (
        "GET",
        "/v1/suggest",
        "streets that start as the typed text: q, k",
    ),
    (
        "GET",
        "/v1/exists",
        "does an address exist: yes, maybe or no and why",
    ),
    ("GET", "/v1/explain", "why the engine answered as it did"),
    ("GET", "/search", "Nominatim-compatible search"),
    ("GET", "/search.php", "Nominatim-compatible search"),
    ("GET", "/reverse", "Nominatim-compatible reverse"),
    ("GET", "/reverse.php", "Nominatim-compatible reverse"),
    ("GET", "/status", "Nominatim-compatible status"),
    ("GET", "/status.php", "Nominatim-compatible status"),
    (
        "GET",
        "/maps/api/geocode/json",
        "a subset of the Google Geocoding API shape",
    ),
];

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
    if req.method != "GET" && !(post && matches!(req.path.as_str(), "/v1/geocode" | "/v1/extract"))
    {
        return Response::error(405, "use GET (POST only for /v1/geocode and /v1/extract)");
    }
    // `country=` of the native paths matters only to a server of several sheets; one sheet answers as before, whatever
    // is asked. The Nominatim and Google paths read their own parameters (`countrycodes`, a structured `country` that
    // is a country name, `components=country:`), see `search` and `google_geocode`.
    let foreign = matches!(
        req.path.as_str(),
        "/search" | "/search.php" | "/reverse" | "/reverse.php" | "/maps/api/geocode/json"
    );
    let mut asked = Vec::new();
    if let Some(cc) = req
        .param("country")
        .map(|c| c.trim().to_ascii_lowercase())
        .filter(|_| !engine.others.is_empty() && !foreign)
    {
        if !engine.serves(&cc) {
            return Response::error(
                400,
                &format!("country={cc}: this server has no sheet of that country"),
            );
        }
        asked.push(cc);
    }
    let asked = asked.as_slice();
    let result = match req.path.as_str() {
        "/" => Ok(Response::text(200, home(engine))),
        "/health" => Ok(Response::json(200, &health(engine, counters))),
        "/ui" => Ok(Response {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: ui_page().as_bytes().to_vec(),
        }),
        "/v1/geocode" if post => v1_geocode_many(engine, counters, req, asked),
        "/v1/geocode" => v1_geocode(engine, counters, req, asked),
        "/v1/extract" if post => v1_extract(engine, counters, req, asked),
        "/v1/reverse" => v1_reverse(engine, counters, req, asked),
        "/v1/suggest" => v1_suggest(engine, counters, req),
        "/v1/exists" => v1_exists(engine, counters, req, asked),
        "/v1/explain" => req
            .param("q")
            .ok_or_else(|| Response::error(400, "missing q"))
            .map(|q| {
                let p = engine.pick(q, asked);
                Response::json(
                    200,
                    &guarded(counters, Value::Null, || {
                        crate::explain::explain(p.index, q)
                    }),
                )
            }),
        "/search" | "/search.php" => search(engine, counters, req).and_then(|r| jsonp(req, r)),
        "/reverse" | "/reverse.php" => reverse(engine, counters, req).and_then(|r| jsonp(req, r)),
        "/status" | "/status.php" => Ok(status(engine, req)),
        "/maps/api/geocode/json" => Ok(google_geocode(engine, counters, req)),
        _ => Err(Response::error(404, "no such path; see / for the list")),
    };
    result.unwrap_or_else(|r| r)
}

/// Nominatim's `json_callback`: the answer wrapped as `name(…)` for a client that loads it as a script — the search box
/// of ipyleaflet and leafmap (leaflet-search sends `json_callback` by default, ANSWERS §300). The name is a dotted
/// JavaScript identifier and nothing else, so the parameter cannot inject a script; an error is answered as JSON.
fn jsonp(req: &Request, r: Response) -> std::result::Result<Response, Response> {
    let Some(name) = req.param("json_callback") else {
        return Ok(r);
    };
    let identifier = |part: &str| {
        let mut chars = part.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    };
    if name.len() > 128 || !name.split('.').all(identifier) {
        return Err(Response::error(
            400,
            "json_callback must be a JavaScript name",
        ));
    }
    let mut body = Vec::with_capacity(r.body.len() + name.len() + 2);
    body.extend_from_slice(name.as_bytes());
    body.push(b'(');
    body.extend_from_slice(&r.body);
    body.push(b')');
    Ok(Response {
        status: r.status,
        content_type: "application/javascript; charset=utf-8",
        body,
    })
}

/// The page of `/ui`: drop a CSV, get it back geocoded; one file, no outside resource (the CSV code is inlined).
fn ui_page() -> &'static str {
    static PAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PAGE.get_or_init(|| {
        let csv = include_str!("ui/csv.js").replace("export ", "");
        include_str!("ui/ui.html").replace("__CSV_JS__", &csv)
    })
}

fn home(engine: &Engine) -> String {
    let sheets = std::iter::once(format!("{} ({})", engine.file, engine.country))
        .chain(
            engine
                .others
                .iter()
                .map(|o| format!("{} ({})", o.file, o.country)),
        )
        .collect::<Vec<_>>()
        .join(", ");
    let several = if engine.others.is_empty() {
        ""
    } else {
        "\nSeveral sheets: country=CC (or the country or a region named at the end of the query) picks the sheet; \
         countrycodes= on /search, components=country: on the Google path. Reverse answers from the sheet with the \
         nearest address.\n"
    };
    format!(
        "GridPin {} — offline geocoder, sheet {}.{several}\n\n\
         GET  /v1/geocode?q=...&k=1[&near=lat,lon]   {{\"results\": [...]}}\n\
         POST /v1/geocode  {{\"queries\": [...], \"k\": 1}}   {{\"results\": [[...], ...]}}  (a query: text or {{\"street\", \"housenumber\", \"city\", \"region\", \"postcode\"}})\n\
         GET  /v1/reverse?lat=...&lon=...&k=1\n\
         GET  /v1/suggest?q=...&k=8   streets that start as the typed text, for an address field\n\
         GET  /ui   a page: drop a CSV, get it back geocoded\n\
         POST /v1/extract  {{\"text\": \"...\", \"verify\": false}}   addresses found in a free text\n\
         (every path takes with=h3,geohash,placekey,codes,street_point,links,provenance: an `extras` object per answer)\n\
         GET  /search?q=...&format=jsonv2|json|geojson&limit=10&addressdetails=1   Nominatim-compatible\n\
         GET  /reverse?lat=...&lon=...&format=jsonv2                              Nominatim-compatible\n\
         GET  /status?format=json\n\
         GET  /maps/api/geocode/json?address=...|latlng=lat,lng                   Google-compatible\n\
         GET  /health\n\n\
         Documentation: https://gridpin.dev/geocoding-server\n",
        env!("CARGO_PKG_VERSION"),
        sheets,
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
        // every sheet of the server, the main one first (`sheet` above); one entry on a one-sheet server
        "sheets": std::iter::once(json!({
            "file": engine.file,
            "bytes": engine.bytes,
            "country": engine.country,
            "source_release": engine.release,
        }))
        .chain(engine.others.iter().map(|o| json!({
            "file": o.file,
            "bytes": o.bytes,
            "country": o.country,
            "source_release": o.release,
        })))
        .collect::<Vec<_>>(),
        "requests": counters.requests.load(Ordering::Relaxed),
        "panics": counters.panics.load(Ordering::Relaxed),
    })
}

fn v1_geocode(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
    asked: &[String],
) -> std::result::Result<Response, Response> {
    let q = req
        .param("q")
        .ok_or_else(|| Response::error(400, "missing q"))?;
    if let Some(r) = too_long(q) {
        return Err(r);
    }
    let k = count(req, "k", 1)?;
    extras_of(engine, req.param("with"))?;
    let (hits, pick) = match req.param("near") {
        None => geocode(engine, counters, q, k, asked),
        Some(raw) => {
            let (lat, lon) = raw
                .split_once(',')
                .and_then(|(a, b)| {
                    Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))
                })
                .ok_or_else(|| Response::error(400, "near must be LAT,LON"))?;
            let alias = engine.aliases.as_ref().and_then(|table| table.find(q));
            let line = alias.map_or(q, |alias| alias.address.as_str());
            let p = engine.pick(line, asked);
            let mut hits = guarded(counters, Ok(Vec::new()), || {
                query::query_cascade_near(p.index, p.poi, line, k, lat, lon)
            })
            .map_err(|e| Response::error(400, &e))?;
            if alias.is_some() {
                hits.iter_mut()
                    .for_each(|h| h.flags.push(aliases::ALIAS_FLAG));
            }
            p.mark(&mut hits);
            (hits, p)
        }
    };
    if let Some(e) = extras_on(req.param("with"), &pick) {
        return Ok(results_with(engine, &e, Some(q), &hits, pick.index));
    }
    if engine.aliases.is_some() {
        return Ok(Response::json(
            200,
            &json!({ "results": native_hits(engine, q, &hits) }),
        ));
    }
    if req.param("near").is_none() {
        if let Some(beside) = beside_results(engine, counters, q, &hits, &pick) {
            let mut body = json!({ "results": hits });
            body.as_object_mut().expect("object").extend(beside);
            return Ok(Response::json(200, &body));
        }
    }
    Ok(Response::json(200, &json!({ "results": hits })))
}

/// `clarify` (the places to choose from) or `reason` (why nothing was found) to put beside `results`; `None`
/// when the answer needs neither, so the answer is byte for byte what it was.
fn beside_results(
    engine: &Engine,
    counters: &Counters,
    q: &str,
    hits: &[Hit],
    pick: &Pick,
) -> Option<Map<String, Value>> {
    use crate::outcome::Outcome;
    let needs = hits.is_empty() || hits[0].flags.contains(&"ambiguous_far");
    if !needs {
        return None;
    }
    let outcome = guarded(
        counters,
        Outcome::Answer {
            results: Vec::new(),
        },
        || {
            // an alias is read as its address, as `geocode` did
            let line = engine
                .aliases
                .as_ref()
                .and_then(|t| t.find(q))
                .map_or(q, |a| a.address.as_str());
            crate::outcome::query_outcome(pick.index, pick.poi, line, hits.len().max(1))
        },
    );
    let mut out = Map::new();
    match outcome {
        Outcome::Clarify {
            missing, options, ..
        } => {
            out.insert(
                "clarify".into(),
                json!({ "missing": missing, "options": options }),
            );
        }
        Outcome::None { reason } => {
            out.insert("reason".into(), reason.into());
        }
        Outcome::Answer { .. } => return None,
    }
    Some(out)
}

/// One entry of `POST /v1/geocode`: a line, or the fields of `gridpin batch` JSONL (a house number
/// under `housenumber` or `number`, as text or as a number) plus `region`.
enum Asked {
    Line(String),
    Fields {
        street: String,
        number: Option<String>,
        city: String,
        region: Option<String>,
        postcode: Option<String>,
    },
}

impl Asked {
    fn parse(q: &Value) -> std::result::Result<Asked, Response> {
        if let Some(text) = q.as_str() {
            return Ok(Asked::Line(text.to_string()));
        }
        let bad = || Response::error(400, "every query must be a string or an object with street");
        let fields = q.as_object().ok_or_else(bad)?;
        let field = |name: &str| -> Option<String> {
            fields
                .get(name)
                .and_then(|v| {
                    v.as_str()
                        .map(String::from)
                        .or_else(|| v.as_u64().map(|n| n.to_string()))
                })
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        Ok(Asked::Fields {
            street: field("street").ok_or_else(bad)?,
            number: field("housenumber").or_else(|| field("number")),
            city: field("city").unwrap_or_default(),
            region: field("region"),
            postcode: field("postcode"),
        })
    }

    /// The entry as one line: the text, or the fields joined by commas (what `with=` and the sheet choice read).
    fn line(&self) -> String {
        match self {
            Asked::Line(text) => text.clone(),
            Asked::Fields {
                street,
                number,
                city,
                region,
                postcode,
            } => [
                Some(street.as_str()),
                number.as_deref(),
                Some(city.as_str()),
                region.as_deref(),
                postcode.as_deref(),
            ]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        }
    }

    fn too_long(&self) -> Option<Response> {
        match self {
            Asked::Line(q) => too_long(q),
            Asked::Fields {
                street,
                number,
                city,
                region,
                postcode,
            } => [
                Some(street),
                number.as_ref(),
                Some(city),
                region.as_ref(),
                postcode.as_ref(),
            ]
            .into_iter()
            .flatten()
            .find_map(|field| too_long(field)),
        }
    }
}

/// Does an address exist: `?q=...` -> `{"exists": "yes|maybe|no", "reason": ...}` (`gridpin exists`).
fn v1_exists(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
    asked: &[String],
) -> std::result::Result<Response, Response> {
    let q = req
        .param("q")
        .ok_or_else(|| Response::error(400, "missing q"))?;
    if let Some(r) = too_long(q) {
        return Err(r);
    }
    let p = engine.pick(q, asked);
    let e = guarded(counters, None, || Some(crate::exists::judge(p.index, q)))
        .ok_or_else(|| Response::error(500, "the engine failed on this query"))?;
    Ok(Response::json(200, &serde_json::json!(e)))
}

/// Addresses in a text: `{"text": "...", "verify": false}` -> `{"results": [...]}` (`gridpin extract`).
fn v1_extract(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
    asked: &[String],
) -> std::result::Result<Response, Response> {
    let body: Value = serde_json::from_slice(&req.body)
        .map_err(|_| Response::error(400, "body must be JSON: {\"text\": \"...\"}"))?;
    let text = body
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| Response::error(400, "body must be JSON: {\"text\": \"...\"}"))?;
    if text.len() > MAX_EXTRACT_BYTES {
        return Err(Response::error(413, "text longer than 64 KiB; split it"));
    }
    let verify = body.get("verify").and_then(Value::as_bool).unwrap_or(false);
    // the sheet of `country=`, else the one whose country or region the text names at its end, else the main one
    let p = engine.pick(text.trim_end(), asked);
    let mut found = guarded(counters, Vec::new(), || {
        crate::extract::extract(p.index, text, verify).0
    });
    if let Some(flag) = p.flag {
        for h in found.iter_mut().filter_map(|f| f.hit.as_mut()) {
            h.flags.push(flag);
        }
    }
    Ok(Response::json(
        200,
        &serde_json::json!({ "results": found }),
    ))
}

/// One `/v1/extract` text at most: the window cap bounds the work, this bounds the body.
const MAX_EXTRACT_BYTES: usize = 64 * 1024;

fn v1_geocode_many(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
    asked: &[String],
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
    let raw_with = body.get("with").and_then(Value::as_str);
    extras_of(engine, raw_with)?;
    let entries: Vec<Asked> = queries
        .iter()
        .map(Asked::parse)
        .collect::<std::result::Result<_, _>>()?;
    if let Some(r) = entries.iter().find_map(Asked::too_long) {
        return Err(r);
    }
    // spread over the global rayon pool, like `gridpin batch`; order is kept. The sheet is chosen per entry and
    // passed down: the pool's threads share nothing with the thread that read the request.
    let results: Vec<(Vec<Hit>, Pick)> = {
        use rayon::prelude::*;
        entries
            .par_iter()
            .map(|q| match q {
                Asked::Line(text) => geocode(engine, counters, text, k, asked),
                Asked::Fields {
                    street,
                    number,
                    city,
                    region,
                    postcode,
                } => {
                    // the region last, where the sheet choice reads it ("…, Los Angeles, CA")
                    let place = [
                        Some(street.as_str()),
                        Some(city.as_str()),
                        postcode.as_deref(),
                        region.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ");
                    let p = engine.pick(&place, asked);
                    let mut hits = guarded(counters, Vec::new(), || {
                        query::query_fields_cascade(
                            p.index,
                            p.poi,
                            query::AddressFields {
                                street,
                                number: number.as_deref(),
                                city,
                                region: region.as_deref(),
                                postcode: postcode.as_deref(),
                            },
                            k,
                        )
                    });
                    p.mark(&mut hits);
                    (hits, p)
                }
            })
            .collect()
    };
    if raw_with.is_some() {
        let rows: Vec<String> = entries
            .iter()
            .zip(&results)
            .map(|(q, (hits, p))| {
                let name = match q {
                    Asked::Line(text) => engine.aliases.as_ref().and_then(|t| t.find(text)),
                    _ => None,
                }
                .map(|a| a.name.as_str());
                let Some(e) = extras_on(raw_with, p) else {
                    return serde_json::to_string(hits).unwrap_or_else(|_| "[]".to_string());
                };
                crate::extras::for_query(&q.line(), || {
                    crate::extras::hits_text(hits, Some(&e), p.index, name)
                })
                .unwrap_or_else(|_| "[]".to_string())
            })
            .collect();
        return Ok(Response::raw_json(
            200,
            format!("{{\"results\":[{}]}}", rows.join(",")),
        ));
    }
    if engine.aliases.is_some() {
        let rows: Vec<Value> = entries
            .iter()
            .zip(&results)
            .map(|(q, (hits, _))| match q {
                Asked::Line(text) => native_hits(engine, text, hits),
                _ => json!(hits),
            })
            .collect();
        return Ok(Response::json(200, &json!({ "results": rows })));
    }
    let hits: Vec<&Vec<Hit>> = results.iter().map(|(h, _)| h).collect();
    Ok(Response::json(200, &json!({ "results": hits })))
}

/// Streets that start as the typed text does, for an address field (`<gridpin-address>`). Nothing is kept: no
/// session, no count per caller; the answer depends on `q` and `k` only.
fn v1_suggest(
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
    let k = count(req, "k", 8)?;
    let found = guarded(counters, Vec::new(), || engine.addr.suggest(q, k));
    Ok(Response::json(200, &json!({ "suggestions": found })))
}

/// The nearest addresses of a point on every path (native, Nominatim, Google): see `Engine::reverse_pick`.
fn reverse_hits<'a>(
    engine: &'a Engine,
    counters: &Counters,
    lat: f64,
    lon: f64,
    k: usize,
    asked: &[String],
) -> std::result::Result<(Vec<Hit>, Pick<'a>), String> {
    guarded(counters, Ok((Vec::new(), engine.picks()[0])), || {
        engine.reverse_pick(lat, lon, k, asked)
    })
}

fn v1_reverse(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
    asked: &[String],
) -> std::result::Result<Response, Response> {
    let (lat, lon) = (coordinate(req, "lat")?, coordinate(req, "lon")?);
    let k = count(req, "k", 1)?;
    extras_of(engine, req.param("with"))?;
    let (hits, pick) =
        reverse_hits(engine, counters, lat, lon, k, asked).map_err(|e| Response::error(400, &e))?;
    if let Some(e) = extras_on(req.param("with"), &pick) {
        return Ok(results_with(engine, &e, None, &hits, pick.index));
    }
    Ok(Response::json(200, &json!({ "results": hits })))
}

/// The countries a Nominatim search allows: `countrycodes=` (ISO codes, comma-separated) and, on a server of several
/// sheets, the structured `country=`, which Nominatim documents as a country name ("Serbia"; a code is read too).
/// `None` allows every sheet; `Some(empty)` means no sheet of this server is allowed, and the answer is empty.
fn search_countries(engine: &Engine, req: &Request) -> Option<Vec<String>> {
    let mut allowed: Option<Vec<String>> = req.param("countrycodes").and_then(|codes| {
        let codes: Vec<String> = codes
            .split(',')
            .map(|c| c.trim().to_ascii_lowercase())
            .filter(|c| !c.is_empty())
            .collect();
        (!codes.is_empty()).then_some(codes)
    });
    let structured = req.param("q").is_none() && !engine.others.is_empty();
    if let Some(name) = req
        .param("country")
        .map(str::trim)
        .filter(|n| structured && !n.is_empty())
    {
        let code = crate::country_names::code_of(name).or_else(|| {
            let lower = name.to_ascii_lowercase();
            (lower.len() == 2 && engine.serves(&lower)).then_some(lower)
        });
        // a name the shared table does not know restricts nothing: it stays in the text of the query
        if let Some(cc) = code {
            allowed = Some(match allowed {
                Some(codes) => codes.into_iter().filter(|c| *c == cc).collect(),
                None => vec![cc],
            });
        }
    }
    allowed.map(|codes| codes.into_iter().filter(|c| engine.serves(c)).collect())
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
    let show = Show {
        address_details: flag(req, "addressdetails", false),
        extra_tags: flag(req, "extratags", false),
    };
    extras_of(engine, req.param("with"))?;
    let (hits, pick) = match search_countries(engine, req) {
        Some(codes) if codes.is_empty() => (Vec::new(), engine.picks()[0]),
        Some(codes) => geocode(engine, counters, &q, limit, &codes),
        None => geocode(engine, counters, &q, limit, &[]),
    };
    // the country and the licence of the sheet that answered, not of the main sheet
    let mut answer = nominatim_list(&hits, dialect, show, pick.country, pick.attribution);
    if let Some(e) = extras_on(req.param("with"), &pick) {
        attach_extras(&mut answer, &hits, &e, pick.index, Some(&q));
    }
    Ok(Response::json(200, &answer))
}

/// `GET /maps/api/geocode/json`: `address` or `latlng`, with `components` narrowing a forward query. A
/// `key`, `language` and `region` are accepted and have no effect: there is no account, and a sheet has one
/// language. A status other than OK and ZERO_RESULTS keeps the shape a client of that API expects. On a server of
/// several sheets `components=country:XX` picks the sheet of that country.
fn google_geocode(engine: &Engine, counters: &Counters, req: &Request) -> Response {
    let invalid = |message: &str| Response::json(400, &google::invalid(message));
    if let Err(e) = Extras::new(req.param("with"), &engine.addr) {
        return invalid(&e.to_string());
    }
    let answer = |hits: &[Hit], q: Option<&str>, pick: &Pick| {
        let mut v = google::response(hits, pick.country);
        if let Some(e) = extras_on(req.param("with"), pick) {
            attach_extras(&mut v, hits, &e, pick.index, q);
        }
        Response::json(200, &v)
    };
    let components = req.param("components");
    let limit = match count(req, "limit", 1) {
        Ok(limit) => limit,
        Err(_) => return invalid("limit must be a whole number from 1"),
    };
    if let Some(raw) = req.param("latlng") {
        let point = raw.split_once(',').and_then(|(a, b)| {
            Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))
        });
        let Some((lat, lon)) = point else {
            return invalid("latlng must be LAT,LNG");
        };
        return match reverse_hits(engine, counters, lat, lon, limit, &[]) {
            Ok((hits, pick)) => answer(&hits, None, &pick),
            Err(e) => invalid(&e),
        };
    }
    let mut parts: Vec<&str> = req
        .param("address")
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .into_iter()
        .collect();
    for name in ["route", "locality", "administrative_area", "postal_code"] {
        parts.extend(google::component_filter(components, name));
    }
    if parts.is_empty() {
        return invalid("give address, components or latlng");
    }
    let line = parts.join(", ");
    if let Some(r) = too_long(&line) {
        return r;
    }
    let asked: Vec<String> = match google::component_filter(components, "country") {
        None => Vec::new(),
        Some(code) => {
            let code = code.to_ascii_lowercase();
            if !engine.serves(&code) {
                return answer(&[], Some(&line), &engine.picks()[0]);
            }
            vec![code]
        }
    };
    let (hits, pick) = geocode(engine, counters, &line, limit, &asked);
    answer(&hits, Some(&line), &pick)
}

fn reverse(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
) -> std::result::Result<Response, Response> {
    let dialect = Dialect::parse(req.param("format")).map_err(|e| Response::error(400, &e))?;
    let (lat, lon) = (coordinate(req, "lat")?, coordinate(req, "lon")?);
    let show = Show {
        address_details: flag(req, "addressdetails", true),
        extra_tags: flag(req, "extratags", false),
    };
    extras_of(engine, req.param("with"))?;
    let (hits, pick) =
        reverse_hits(engine, counters, lat, lon, 1, &[]).map_err(|e| Response::error(400, &e))?;
    let Some(hit) = hits.first() else {
        // Nominatim's own answer when nothing is there (src/nominatim_api/v1/format_json.py)
        return Ok(Response::json(
            200,
            &json!({ "error": "Unable to geocode" }),
        ));
    };
    let mut answer = match dialect {
        Dialect::GeoJson => nominatim_list(
            std::slice::from_ref(hit),
            dialect,
            show,
            pick.country,
            pick.attribution,
        ),
        _ => nominatim_place(hit, dialect, show, pick.country, pick.attribution),
    };
    if let Some(e) = extras_on(req.param("with"), &pick) {
        attach_extras(&mut answer, std::slice::from_ref(hit), &e, pick.index, None);
    }
    Ok(Response::json(200, &answer))
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

fn handle(engine: &Engine, counters: &Counters, gate: &dyn Gate, stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let t0 = std::time::Instant::now();
    let resp = match read_request(&mut reader) {
        Ok(req) => {
            let resp = gate
                .before(&req)
                .unwrap_or_else(|| route(engine, counters, &req));
            gate.after(&req, &resp, t0.elapsed().as_secs_f64() * 1000.0);
            resp
        }
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
            geocode(&engine, &counters, q, 1, &[]);
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
    run_gated(engine, sheet, host, port, threads, Arc::new(NoGate))
}

/// `run` with a `Gate` around every request (the paid server's keys, limits, metrics and log).
pub fn run_gated(
    engine: Engine,
    sheet: std::path::PathBuf,
    host: &str,
    port: u16,
    threads: usize,
    gate: Arc<dyn Gate>,
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
        let (engine, counters, gate) = (
            Arc::clone(&engine),
            Arc::clone(&counters),
            Arc::clone(&gate),
        );
        workers.push(std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                handle(&engine, &counters, gate.as_ref(), stream);
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

    const HIDE: Show = Show {
        address_details: false,
        extra_tags: false,
    };
    const DETAILS: Show = Show {
        address_details: true,
        extra_tags: false,
    };

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
            HIDE,
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
        let street = nominatim_place(&hit("street", None), Dialect::Json, HIDE, "us", "lic");
        let city = nominatim_place(&hit("city", None), Dialect::Json, HIDE, "us", "lic");
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
            HIDE,
            "us",
            "lic",
        );
        assert!(v1.get("osm_id").is_none() && v1.get("osm_type").is_none());
        let v2 = nominatim_place(
            &hit("house", Some("1600")),
            Dialect::JsonV2,
            HIDE,
            "us",
            "lic",
        );
        assert!(v2.get("class").is_none());
        assert_eq!(v2["category"], "place");
        assert_eq!(v2["place_rank"], 30);
    }

    #[test]
    fn address_details_only_when_asked_and_without_empty_parts() {
        let none = nominatim_place(&hit("street", None), Dialect::Json, HIDE, "us", "lic");
        assert!(none.get("address").is_none());
        let a =
            &nominatim_place(&hit("street", None), Dialect::Json, DETAILS, "us", "lic")["address"];
        assert_eq!(a["road"], "PENNSYLVANIA Avenue Northwest");
        assert_eq!(a["country_code"], "us");
        assert!(a.get("house_number").is_none());
        let a = &nominatim_place(
            &hit("house", Some("1600")),
            Dialect::Json,
            DETAILS,
            "us",
            "lic",
        )["address"];
        assert_eq!(a["house_number"], "1600");
    }

    #[test]
    fn extratags_carry_the_match_level_only_when_asked() {
        let house = hit("house", Some("1600"));
        assert!(nominatim_place(&house, Dialect::Json, HIDE, "us", "lic")
            .get("extratags")
            .is_none());
        let show = Show {
            address_details: false,
            extra_tags: true,
        };
        let tags = &nominatim_place(&house, Dialect::JsonV2, show, "us", "lic")["extratags"];
        assert_eq!(tags["match_level"], "PointAddress");
        // Nominatim's extratags are strings, so the status is one too
        assert_eq!(tags["match_status"], "M");
    }

    #[test]
    fn a_batch_entry_is_a_line_or_fields_with_the_names_of_batch_jsonl() {
        assert!(matches!(
            Asked::parse(&json!("1 Rue A")).ok().unwrap(),
            Asked::Line(q) if q == "1 Rue A"
        ));
        let Asked::Fields {
            street,
            number,
            city,
            region,
            postcode,
        } = Asked::parse(
            &json!({"street": " Rue A ", "housenumber": 5, "city": "Ville", "region": "X", "postcode": "10000"}),
        )
        .ok()
        .unwrap()
        else {
            panic!("fields expected");
        };
        assert_eq!(
            (
                street.as_str(),
                number.as_deref(),
                city.as_str(),
                region.as_deref(),
                postcode.as_deref()
            ),
            ("Rue A", Some("5"), "Ville", Some("X"), Some("10000"))
        );
        // `number` is the same field; a blank street is no query at all, as in JSONL
        let Asked::Fields { number, .. } =
            Asked::parse(&json!({"street": "Rue A", "number": "7b"}))
                .ok()
                .unwrap()
        else {
            panic!("fields expected");
        };
        assert_eq!(number.as_deref(), Some("7b"));
        assert!(Asked::parse(&json!({"street": "  ", "city": "Ville"})).is_err());
        assert!(Asked::parse(&json!(5)).is_err());
    }

    #[test]
    fn geojson_is_a_feature_collection_with_point_and_bbox() {
        let v = nominatim_list(
            &[hit("house", Some("1600"))],
            Dialect::GeoJson,
            HIDE,
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
