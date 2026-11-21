//! `gridpin serve`: one sheet behind a small local HTTP server.
//!
//! Three dialects on one port. The Google path (`/maps/api/geocode/json`, see `google.rs`) answers in the shape
//! of the Google Geocoding API. The native API (`/v1/geocode`, `/v1/reverse`) answers `{"results": [...]}` with
//! exactly the hits `gridpin batch` writes. The Nominatim-compatible paths (`/search`, `/reverse`, `/status`, see
//! `nominatim.rs`) answer in the shape Nominatim documents. Fields we do not have are left out, never invented.
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
use crate::ban;
use crate::census;
use crate::extras::Extras;
use crate::google;
use crate::nominatim;
pub(crate) use crate::nominatim::half_width;
pub use crate::nominatim::{
    display_name, nominatim_list, nominatim_place, place_id, Dialect, Show,
};
use crate::pelias;
use crate::photon;
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
    /// `--ids`: answers of the Nominatim and Photon paths carry `osm_type`/`osm_id`, a number made from the answer
    /// (`locator.rs`), for clients that will not take an answer without one
    pub ids: bool,
    /// `--postcode-layer`: full postcodes to streets (Brazil's eight-digit CEP), asked when a query is only a code
    pub postcodes: Option<crate::postcode_layer::PostcodeLayer>,
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

pub(crate) fn meta_of(index: &Index, key: &str) -> String {
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
    pub(crate) fn picks(&self) -> Vec<Pick<'_>> {
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
    pub(crate) fn serves(&self, cc: &str) -> bool {
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
pub(crate) fn metres(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
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
            ids: false,
            postcodes: None,
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

/// The shortest shared key the open server takes: 24 characters (the setup script makes 43 from 32 random bytes).
pub const MIN_KEY_CHARS: usize = 24;
/// The environment variable a host's secret sets the key in; the command line never carries it (`ps` shows arguments).
pub const KEY_ENV: &str = "GRIDPIN_SERVE_KEY";
/// `1` here is `--require-key`: a host template sets it, so a secret left unset cannot start an open server.
pub const REQUIRE_KEY_ENV: &str = "GRIDPIN_REQUIRE_KEY";
/// The Google-compatible path, whose clients pass the key only as `key=` in the address, as Google's own API takes it.
const GOOGLE_PATH: &str = "/maps/api/geocode/json";

/// One shared key for a server on the open internet (ANSWERS §359): every request but `/health` carries
/// `Authorization: Bearer <key>` (on the Google-compatible path also `key=`), otherwise 401 and the engine does not see
/// it. It counts nothing, limits nothing and logs nothing: several keys, daily limits and a log are `gridpin-pro serve`.
pub struct KeyGate {
    key: Vec<u8>,
}

impl KeyGate {
    /// The gate of `key`; refuses a key shorter than `MIN_KEY_CHARS` or one with spaces or control characters.
    pub fn new(key: &str) -> Result<KeyGate> {
        if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
            bail!("the key has a space or a control character; make a random one: openssl rand -base64 32");
        }
        if key.chars().count() < MIN_KEY_CHARS {
            bail!(
                "the key is shorter than {MIN_KEY_CHARS} characters; make a random one: openssl rand -base64 32"
            );
        }
        Ok(KeyGate {
            key: key.as_bytes().to_vec(),
        })
    }

    fn accepts(&self, given: &str) -> bool {
        same_key(&self.key, given.as_bytes())
    }
}

/// Equal keys, compared in a time that depends only on the length of the expected key, not on where they differ.
fn same_key(expected: &[u8], given: &[u8]) -> bool {
    let mut diff = u8::from(expected.len() != given.len());
    for (i, e) in expected.iter().enumerate() {
        diff |= e ^ given.get(i).copied().unwrap_or(!e);
    }
    diff == 0
}

/// The token of `Bearer <token>`: the scheme in any case, one space, the rest as it stands.
fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, token) = header.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(token)
}

impl Gate for KeyGate {
    fn before(&self, req: &Request) -> Option<Response> {
        if req.path == "/health" {
            return None;
        }
        if req
            .authorization
            .as_deref()
            .and_then(bearer_token)
            .is_some_and(|t| self.accepts(t))
        {
            return None;
        }
        if req.path == GOOGLE_PATH {
            if req.param("key").is_some_and(|k| self.accepts(k)) {
                return None;
            }
            return Some(Response::json(
                200,
                &json!({"results": [], "status": "REQUEST_DENIED", "error_message": "The provided API key is invalid."}),
            ));
        }
        // one answer for any path, so a refused request learns nothing of the routes or the sheet
        Some(Response::error(401, "send Authorization: Bearer <key>"))
    }
}

/// The key of `gridpin serve`: from `GRIDPIN_SERVE_KEY` (`env_key`) or the first line of `--key-file`, never both;
/// none is an open server unless `require` (`--require-key`, `GRIDPIN_REQUIRE_KEY=1`), which refuses to start.
pub fn key_gate(
    env_key: Option<String>,
    key_file: Option<&std::path::Path>,
    require: bool,
) -> Result<Option<KeyGate>> {
    let env_key = env_key.filter(|k| !k.is_empty());
    let key = match (env_key, key_file) {
        (Some(_), Some(_)) => bail!("both {KEY_ENV} and --key-file are set; keep one"),
        (Some(k), None) => Some(k),
        (None, Some(path)) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("cannot read the key file {}", path.display()))?;
            Some(text.lines().next().unwrap_or("").to_string())
        }
        (None, None) => None,
    };
    match key {
        Some(k) => KeyGate::new(&k).map(Some),
        None if require => bail!("no key: set {KEY_ENV} or --key-file (--require-key is on)"),
        None => Ok(None),
    }
}

impl Request {
    pub(crate) fn param(&self, name: &str) -> Option<&str> {
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
    pub(crate) fn json(status: u16, value: &Value) -> Response {
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
    pub(crate) fn text(status: u16, body: String) -> Response {
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
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        422 => "Unprocessable Content",
        414 => "URI Too Long",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    }
}

pub fn write_response(out: &mut impl Write, resp: &Response) -> std::io::Result<()> {
    write_response_allowing(out, resp, None)
}

/// The headers a browser asks to send (`Access-Control-Request-Headers` of a preflight), allowed back as asked, the
/// way public Nominatim answers (13.11.2026: `content-type,user-agent` asked, the same allowed). Only a list of
/// header names is repeated; anything else falls back to `Content-Type`, so the request cannot write a header of its
/// own into the answer.
pub fn allowed_headers(asked: Option<&str>) -> &str {
    match asked.map(str::trim) {
        Some(list)
            if !list.is_empty()
                && list.len() <= 512
                && list
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_, ".contains(&b))
                && list.bytes().any(|b| b.is_ascii_alphanumeric()) =>
        {
            list
        }
        _ => "Content-Type",
    }
}

/// `write_response` with the header names a preflight asked for (see `allowed_headers`).
pub fn write_response_allowing(
    out: &mut impl Write,
    resp: &Response,
    asked_headers: Option<&str>,
) -> std::io::Result<()> {
    write!(
        out,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: {}\r\n{}\
         Connection: close\r\n\r\n",
        resp.status,
        reason(resp.status),
        resp.content_type,
        resp.body.len(),
        allowed_headers(asked_headers),
        // every 401 of the server is a missing or wrong key (RFC 9110 §11.6.1)
        if resp.status == 401 {
            "WWW-Authenticate: Bearer\r\n"
        } else {
            ""
        }
    )?;
    out.write_all(&resp.body)?;
    out.flush()
}

pub(crate) fn flag(req: &Request, name: &str, default: bool) -> bool {
    match req.param(name) {
        Some(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        None => default,
    }
}

pub(crate) fn count(
    req: &Request,
    name: &str,
    default: usize,
) -> std::result::Result<usize, Response> {
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

pub(crate) fn coordinate(req: &Request, name: &str) -> std::result::Result<f64, Response> {
    req.param(name)
        .ok_or_else(|| Response::error(400, &format!("missing {name}")))?
        .parse::<f64>()
        .map_err(|_| Response::error(400, &format!("{name} must be a number")))
}

/// Run an engine call; a panic inside it is counted and answers as empty.
pub(crate) fn guarded<T>(counters: &Counters, empty: T, f: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        counters.panics.fetch_add(1, Ordering::Relaxed);
        empty
    })
}

/// The answers to one line and the sheet that gave them. The sheet is chosen by the line the engine reads (an alias
/// is read as its address) among the countries `asked` allows; on a server of several sheets the hits carry
/// `country_<cc>`. The choice travels with the call, never in a thread-local: `POST /v1/geocode` answers on the rayon
/// pool, whose threads saw no `country=` (revision 0.9.0: 4 answers of 200 instead of 181).
pub(crate) fn geocode<'a>(
    engine: &'a Engine,
    counters: &Counters,
    q: &str,
    k: usize,
    asked: &[String],
) -> (Vec<Hit>, Pick<'a>) {
    if let Some(layer) = &engine.postcodes {
        let hits = layer.hits(q, k);
        if !hits.is_empty() {
            return (hits, engine.pick(q, asked));
        }
    }
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

/// The answers to one line nearest a point (`near=` of the native API, `lat`/`lon` of Photon): the engine's own bias
/// toward the point; an alias is read as its address and marked, the sheet chosen as `geocode` chooses it.
pub(crate) fn geocode_near<'a>(
    engine: &'a Engine,
    counters: &Counters,
    q: &str,
    k: usize,
    lat: f64,
    lon: f64,
    asked: &[String],
) -> std::result::Result<(Vec<Hit>, Pick<'a>), String> {
    let alias = engine.aliases.as_ref().and_then(|table| table.find(q));
    let line = alias.map_or(q, |alias| alias.address.as_str());
    let p = engine.pick(line, asked);
    let mut hits = guarded(counters, Ok(Vec::new()), || {
        query::query_cascade_near(p.index, p.poi, line, k, lat, lon)
    })?;
    if alias.is_some() {
        hits.iter_mut()
            .for_each(|h| h.flags.push(aliases::ALIAS_FLAG));
    }
    p.mark(&mut hits);
    Ok((hits, p))
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
pub(crate) fn extras_of(
    engine: &Engine,
    raw: Option<&str>,
) -> std::result::Result<Option<Extras>, Response> {
    Extras::new(raw, &engine.addr).map_err(|e| Response::error(400, &e.to_string()))
}

/// `with=` read against the sheet that answered (its provenance card); the spec was checked by `extras_of` before.
pub(crate) fn extras_on(raw: Option<&str>, p: &Pick) -> Option<Extras> {
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
pub(crate) fn attach_extras(
    answer: &mut Value,
    hits: &[Hit],
    extras: &Extras,
    idx: &Index,
    q: Option<&str>,
) {
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

pub(crate) fn too_long(q: &str) -> Option<Response> {
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
        "streets that start as the typed text: q, k, near (lat,lon: nearby streets first)",
    ),
    (
        "GET",
        "/v1/exists",
        "does an address exist: yes, maybe or no and why",
    ),
    (
        "GET",
        "/v1/addresses",
        "the addresses of a postcode, optionally of one house number: postcode, number, limit",
    ),
    ("GET", "/v1/explain", "why the engine answered as it did"),
    (
        "GET",
        "/geocoding/v5/{mode}/{query}.json",
        "Mapbox Geocoding v5 search and reverse: mode mapbox.places, query text or lon,lat; limit, proximity, \
         autocomplete, country, types",
    ),
    ("GET", "/search", "Nominatim-compatible search"),
    ("GET", "/search.php", "Nominatim-compatible search"),
    ("GET", "/reverse", "Nominatim-compatible reverse"),
    ("GET", "/reverse.php", "Nominatim-compatible reverse"),
    ("GET", "/status", "Nominatim-compatible status"),
    ("GET", "/status.php", "Nominatim-compatible status"),
    (
        "GET",
        "/nominatim/search",
        "Nominatim-compatible search under its own prefix",
    ),
    (
        "GET",
        "/nominatim/search.php",
        "Nominatim-compatible search under its own prefix",
    ),
    (
        "GET",
        "/nominatim/reverse",
        "Nominatim-compatible reverse under its own prefix",
    ),
    (
        "GET",
        "/nominatim/reverse.php",
        "Nominatim-compatible reverse under its own prefix",
    ),
    (
        "GET",
        "/nominatim/status",
        "Nominatim-compatible status under its own prefix",
    ),
    (
        "GET",
        "/nominatim/status.php",
        "Nominatim-compatible status under its own prefix",
    ),
    (
        "GET",
        "/maps/api/geocode/json",
        "a subset of the Google Geocoding API shape",
    ),
    (
        "GET",
        "/lookup",
        "Nominatim-compatible lookup of the numbers of --ids",
    ),
    (
        "GET",
        "/lookup.php",
        "Nominatim-compatible lookup of the numbers of --ids",
    ),
    (
        "GET",
        "/nominatim/lookup",
        "Nominatim-compatible lookup under its own prefix",
    ),
    (
        "GET",
        "/nominatim/lookup.php",
        "Nominatim-compatible lookup under its own prefix",
    ),
    ("GET", "/photon/api", "Photon-compatible search"),
    ("GET", "/photon/api/", "Photon-compatible search"),
    ("GET", "/photon/reverse", "Photon-compatible reverse"),
    ("GET", "/photon/reverse/", "Photon-compatible reverse"),
    ("GET", "/photon/status", "Photon-compatible status"),
    ("GET", "/pelias/v1/search", "Pelias-compatible search"),
    (
        "GET",
        "/pelias/v1/autocomplete",
        "Pelias-compatible autocomplete",
    ),
    ("GET", "/pelias/v1/reverse", "Pelias-compatible reverse"),
    ("GET", "/pelias/v1/place", "Pelias-compatible place by gid"),
    ("GET", "/ban/search", "BAN (Addok)-compatible search"),
    ("GET", "/ban/search/", "BAN (Addok)-compatible search"),
    ("GET", "/ban/reverse", "BAN (Addok)-compatible reverse"),
    ("GET", "/ban/reverse/", "BAN (Addok)-compatible reverse"),
    (
        "GET",
        "/census/geocoder/locations/onelineaddress",
        "US Census Geocoder-compatible one-line search",
    ),
    (
        "GET",
        "/census/geocoder/locations/address",
        "US Census Geocoder-compatible search by parts",
    ),
    (
        "POST",
        "/census/geocoder/locations/addressbatch",
        "US Census Geocoder-compatible batch (multipart addressFile, CSV out)",
    ),
    (
        "POST",
        "/census/geocoder/locations/onelineaddress",
        "the Census batch posted to the one-line path (tidygeocoder posts to the URL it was given)",
    ),
    (
        "POST",
        "/census/geocoder/locations/address",
        "the Census batch posted to the by-parts path (tidygeocoder posts to the URL it was given)",
    ),
    (
        "GET",
        "/ws/{cep}/json/",
        "a CEP in the shape of ViaCEP, with lat and lon (needs --postcode-layer)",
    ),
];

/// The path a request is routed by: a ViaCEP path `/ws/<cep>/json/` is one route whatever the code.
fn route_path(path: &str) -> &str {
    match path
        .strip_prefix("/ws/")
        .map(|rest| rest.split('/').collect::<Vec<_>>())
    {
        Some(parts) if matches!(parts.as_slice(), [_, "json"] | [_, "json", ""]) => {
            "/ws/{cep}/json/"
        }
        _ if crate::mapbox::parse_path(path).is_some() => crate::mapbox::ROUTE,
        _ => path,
    }
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
    let post_path = matches!(req.path.as_str(), "/v1/geocode" | "/v1/extract")
        || req.path.starts_with("/census/geocoder/locations/");
    if req.method != "GET" && !(post && post_path) {
        return Response::error(
            405,
            "use GET (POST only for /v1/geocode, /v1/extract and the Census batch)",
        );
    }
    // `country=` of the native paths matters only to a server of several sheets; one sheet answers as before, whatever
    // is asked. The Nominatim and Google paths read their own parameters (`countrycodes`, a structured `country` that
    // is a country name, `components=country:`), see `search` and `google_geocode`.
    let foreign = matches!(
        req.path.as_str(),
        "/search"
            | "/search.php"
            | "/reverse"
            | "/reverse.php"
            | "/nominatim/search"
            | "/nominatim/search.php"
            | "/nominatim/reverse"
            | "/nominatim/reverse.php"
            | "/lookup"
            | "/lookup.php"
            | "/nominatim/lookup"
            | "/nominatim/lookup.php"
            | "/photon/api"
            | "/photon/api/"
            | "/photon/reverse"
            | "/photon/reverse/"
            | "/maps/api/geocode/json"
    ) || route_path(&req.path) == crate::mapbox::ROUTE;
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
    let result = match route_path(&req.path) {
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
        "/v1/addresses" => v1_addresses(engine, counters, req),
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
        "/search" | "/search.php" | "/nominatim/search" | "/nominatim/search.php" => {
            nominatim::search(engine, counters, req).and_then(|r| nominatim::jsonp(req, r))
        }
        "/reverse" | "/reverse.php" | "/nominatim/reverse" | "/nominatim/reverse.php" => {
            nominatim::reverse(engine, counters, req).and_then(|r| nominatim::jsonp(req, r))
        }
        "/status" | "/status.php" | "/nominatim/status" | "/nominatim/status.php" => {
            Ok(nominatim::status(engine, req))
        }
        "/maps/api/geocode/json" => Ok(google_geocode(engine, counters, req)),
        "/lookup" | "/lookup.php" | "/nominatim/lookup" | "/nominatim/lookup.php" => {
            nominatim::lookup(engine, counters, req).and_then(|r| nominatim::jsonp(req, r))
        }
        "/photon/api" | "/photon/api/" => Ok(photon::api(engine, counters, req)),
        "/photon/reverse" | "/photon/reverse/" => Ok(photon::reverse(engine, counters, req)),
        "/photon/status" => Ok(photon::status(engine)),
        crate::mapbox::ROUTE => Ok(crate::mapbox::geocoding(engine, counters, req)),
        "/pelias/v1/search" => Ok(pelias::search(engine, counters, req)),
        "/pelias/v1/autocomplete" => Ok(pelias::autocomplete(engine, counters, req)),
        "/pelias/v1/reverse" => Ok(pelias::reverse(engine, counters, req)),
        "/pelias/v1/place" => Ok(pelias::place(engine, counters, req)),
        "/ban/search" | "/ban/search/" => Ok(ban::search(engine, counters, req)),
        "/ban/reverse" | "/ban/reverse/" => Ok(ban::reverse(engine, counters, req)),
        // tidygeocoder posts the batch to the URL it was given, so each `locations` path takes the batch by POST
        "/census/geocoder/locations/addressbatch"
        | "/census/geocoder/locations/onelineaddress"
        | "/census/geocoder/locations/address"
            if post =>
        {
            Ok(census::batch(engine, counters, req))
        }
        "/census/geocoder/locations/onelineaddress" => Ok(census::oneline(engine, counters, req)),
        "/census/geocoder/locations/address" => Ok(census::address(engine, counters, req)),
        p if p.starts_with("/census/geocoder/geographies/") => Ok(census::geographies()),
        "/ws/{cep}/json/" => viacep(engine, &req.path),
        _ => Err(Response::error(404, "no such path; see / for the list")),
    };
    result.unwrap_or_else(|r| r)
}

/// `/ws/<cep>/json/` in the shape of ViaCEP (viacep.com.br, read 17.11.2026): eight digits or the 400 ViaCEP gives a
/// malformed code; a code the layer lacks is `{"erro": "true"}` with 200, as ViaCEP answers. Fields the layer does not
/// hold (bairro, ibge, ddd…) are empty strings, the point is added as `lat` and `lon`. A code of several streets
/// answers the street with most houses; `/v1/geocode?q=<cep>&k=5` lists them all.
fn viacep(engine: &Engine, path: &str) -> std::result::Result<Response, Response> {
    let layer = engine.postcodes.as_ref().ok_or_else(|| {
        Response::error(
            404,
            "no postcode layer: start the server with --postcode-layer",
        )
    })?;
    let raw = path
        .trim_start_matches("/ws/")
        .split('/')
        .next()
        .unwrap_or("");
    let code: String = raw.chars().filter(|c| *c != '-').collect();
    if code.len() != 8 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Response::error(400, "a CEP is eight digits"));
    }
    let Some(r) = layer.lookup(&code).into_iter().next() else {
        return Ok(Response::json(200, &json!({ "erro": "true" })));
    };
    Ok(Response::json(
        200,
        &json!({
            "cep": crate::postcode_layer::display(r.code),
            "logradouro": r.street,
            "complemento": "",
            "unidade": "",
            "bairro": r.district,
            "localidade": r.city,
            "uf": r.region,
            "estado": "",
            "regiao": "",
            "ibge": "",
            "gia": "",
            "ddd": "",
            "siafi": "",
            "lat": r.lat,
            "lon": r.lon,
        }),
    ))
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
         GET  /v1/suggest?q=...&k=8   streets that start as the typed text, for an address field (near=lat,lon: nearby first)\n\
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
    let mut out = health_core(engine, counters);
    if let Some(layer) = &engine.postcodes {
        let meta = |k: &str| {
            layer
                .meta
                .iter()
                .find(|(key, _)| key == k)
                .map_or("", |(_, v)| v.as_str())
                .to_string()
        };
        out["postcode_layer"] = json!({
            "rows": layer.len(),
            "country": meta("country"),
            "release": meta("release"),
        });
    }
    out
}

fn health_core(engine: &Engine, counters: &Counters) -> Value {
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
            geocode_near(engine, counters, q, k, lat, lon, asked)
                .map_err(|e| Response::error(400, &e))?
        }
    };
    // `format=valhalla` is the only format of this path; any other value is left unread, as before it existed
    if req.param("format") == Some("valhalla") {
        return Ok(match crate::valhalla::location(&hits, pick.index) {
            Ok(line) => Response::json(200, &line),
            Err(why) => Response::error(422, &why),
        });
    }
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
/// Rows of a postcode read before the number filter: a postcode with more houses than this asks for a number.
const ADDRESSES_SCAN: u64 = 20_000;

fn same_number(asked: &str, house: &str) -> bool {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_lowercase()
    };
    let (a, h) = (norm(asked), norm(house));
    // "27" finds 27, 27a and 27 bis; "27a" finds 27a only
    !a.is_empty()
        && (h == a
            || (h.starts_with(&a) && !h[a.len()..].starts_with(|c: char| c.is_ascii_digit())))
}

/// `GET /v1/addresses?postcode=&number=&limit=`: the houses of a postcode, for an address finder (postcode, then a
/// list to choose from). `more` says the list was cut: ask for the house number.
fn v1_addresses(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
) -> std::result::Result<Response, Response> {
    let postcode = req
        .param("postcode")
        .map(|p| p.trim().to_uppercase())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| Response::error(400, "missing postcode"))?;
    if let Some(r) = too_long(&postcode) {
        return Err(r);
    }
    let typed = req.param("number").map(str::trim).filter(|n| !n.is_empty());
    if let Some(r) = typed.and_then(too_long) {
        return Err(r);
    }
    // "4", "16a", or the street with it ("Goethestraße 4", "4 Main St"): where a postcode covers many streets, the
    // street words narrow the list; the number is the word that starts with a digit
    let (number, street_words) = match typed {
        None => (None, Vec::new()),
        Some(t) => {
            let words: Vec<&str> = t.split_whitespace().collect();
            let number = words
                .iter()
                .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
                .copied();
            let rest: Vec<String> = words
                .iter()
                .filter(|w| Some(**w) != number)
                .map(|w| crate::norm::normalize(w))
                .filter(|w| !w.is_empty())
                .collect();
            (number, rest)
        }
    };
    let limit = count(req, "limit", 20)?;
    let area = crate::query::Area {
        postcode: Some(postcode),
        ..Default::default()
    };
    let rows = guarded(counters, (Vec::new(), false), || {
        let Ok((filter, _)) = engine.addr.house_filter(&area) else {
            return (Vec::new(), false);
        };
        let (rows, _) = engine.addr.houses(&filter, Some(ADDRESSES_SCAN));
        let mut kept: Vec<_> = rows
            .into_iter()
            .filter(|r| number.is_none_or(|n| same_number(n, &r.housenumber)))
            .filter(|r| {
                street_words.is_empty() || {
                    let street = crate::norm::normalize(&r.street);
                    street_words.iter().all(|w| street.contains(w.as_str()))
                }
            })
            .collect();
        let more = kept.len() > limit;
        kept.truncate(limit);
        (kept, more)
    });
    let (rows, more) = rows;
    // the sheet's own order: "12 Main Street" where it writes the number first, "Hauptstraße 12" elsewhere
    let number_first = engine.addr.number_first();
    let addresses: Vec<Value> = rows
        .iter()
        .map(|r| {
            let parts = if number_first {
                [r.housenumber.as_str(), r.street.as_str()]
            } else {
                [r.street.as_str(), r.housenumber.as_str()]
            };
            let street = parts
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let label = [street.as_str(), r.postcode.as_str(), r.commune.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            json!({"label": label, "lat": r.lat, "lon": r.lon, "street": r.street, "housenumber": r.housenumber,
                   "postcode": r.postcode, "commune": r.commune, "region": r.region})
        })
        .collect();
    Ok(Response::json(
        200,
        &json!({ "addresses": addresses, "more": more }),
    ))
}

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
    // near=lat,lon: streets near the user first; a point that is not two numbers in range is an error, not ignored
    let near = match req.param("near") {
        None => None,
        Some(v) => Some(
            v.split_once(',')
                .and_then(|(a, b)| {
                    Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))
                })
                .filter(|(lat, lon)| (-90.0..=90.0).contains(lat) && (-180.0..=180.0).contains(lon))
                .ok_or_else(|| Response::error(400, "near: lat,lon in degrees"))?,
        ),
    };
    let found = guarded(counters, Vec::new(), || {
        engine.addr.suggest_near(q, k, near)
    });
    Ok(Response::json(200, &json!({ "suggestions": found })))
}

/// The nearest addresses of a point on every path (native, Nominatim, Google): see `Engine::reverse_pick`.
pub(crate) fn reverse_hits<'a>(
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

fn handle(engine: &Engine, counters: &Counters, gate: &dyn Gate, stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let t0 = std::time::Instant::now();
    let (resp, asked) = match read_request(&mut reader) {
        Ok(req) => {
            let resp = gate
                .before(&req)
                .unwrap_or_else(|| route(engine, counters, &req));
            gate.after(&req, &resp, t0.elapsed().as_secs_f64() * 1000.0);
            let asked = req
                .headers
                .iter()
                .find(|(name, _)| name == "access-control-request-headers")
                .map(|(_, value)| value.clone());
            (resp, asked)
        }
        Err((status, message)) => (Response::error(status, &message), None),
    };
    let mut out = stream;
    let _ = write_response_allowing(&mut out, &resp, asked.as_deref());
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
    fn a_preflight_gets_back_the_header_names_it_asked_for_and_nothing_else() {
        assert_eq!(allowed_headers(None), "Content-Type");
        assert_eq!(allowed_headers(Some("")), "Content-Type");
        assert_eq!(
            allowed_headers(Some("content-type,user-agent")),
            "content-type,user-agent"
        );
        assert_eq!(allowed_headers(Some(" X-Foo, x_bar ")), "X-Foo, x_bar");
        // anything but a list of names cannot become a header of the answer
        for bad in ["a\r\nSet-Cookie: x=1", "a: b", "a;b", "a\u{7f}"] {
            assert_eq!(allowed_headers(Some(bad)), "Content-Type", "{bad:?}");
        }
        // separators alone name no header
        assert_eq!(allowed_headers(Some(" , ")), "Content-Type");
        assert_eq!(allowed_headers(Some(&"a".repeat(513))), "Content-Type");
        assert_eq!(allowed_headers(Some(&"a".repeat(512))).len(), 512);
    }
}
