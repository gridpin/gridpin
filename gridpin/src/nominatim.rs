//! The Nominatim dialect of `gridpin serve`: `/search`, `/reverse`, `/status` (and their `.php` names) in the shape
//! Nominatim documents (`docs/api/Output.md`, checked 2026-11-01), so a client written for Nominatim (geopy, Leaflet
//! Control Geocoder) only needs a new base URL. Fields we do not have are left out, never invented: no `osm_type` /
//! `osm_id`, no polygons. `extratags=1` carries `match_level` and `match_status`, which Nominatim has no field for.
//! `name`, `addresstype`, `namedetails` and `address.country` follow Nominatim's own writer
//! (`src/nominatim_api/v1/format_json.py` and `classtypes.get_label_tag`, read 2026-11-13): an address has no name,
//! a street is named by its street, a town by itself, a place of interest by its own name. The same paths answer
//! under `/nominatim/` too, so a client can be pointed at the dialect by its base URL.

use serde_json::{json, Map, Value};

use crate::query::{self, Hit};
use crate::serve::{
    attach_extras, coordinate, count, extras_of, extras_on, flag, geocode, reverse_hits, too_long,
    Counters, Engine, Request, Response, MAX_LIMIT,
};

/// Half-width of the Nominatim `boundingbox` by precision: a door, a street, a town.
pub(crate) fn half_width(precision: &str) -> f64 {
    match precision {
        "house" | "interp" | "near" | "approximate" | "poi" | "intersection" => 0.0005,
        "city" => 0.05,
        _ => 0.005,
    }
}

/// The `type` of an answer. A reverse point far from any house is answered by the nearest house, `approximate`: it is
/// still that house (its number is in the answer), and how far it reaches shows in `extratags` and `importance`.
fn place_type(precision: &str) -> &'static str {
    match precision {
        "house" | "interp" | "near" | "approximate" => "house",
        "city" => "city",
        "poi" => "poi",
        _ => "street",
    }
}

fn place_rank(precision: &str) -> u32 {
    match precision {
        "house" | "interp" | "near" | "approximate" | "poi" => 30,
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
    GeocodeJson,
    Xml,
}

impl Dialect {
    /// `format=`; absent means `jsonv2` (Nominatim's documented default for search, and ours for reverse too, where
    /// Nominatim's is xml: a client of this server since 0.7.0 asks reverse without `format`).
    pub fn parse(format: Option<&str>) -> std::result::Result<Dialect, String> {
        match format.unwrap_or("jsonv2") {
            "json" => Ok(Dialect::Json),
            "jsonv2" => Ok(Dialect::JsonV2),
            "geojson" => Ok(Dialect::GeoJson),
            "geocodejson" => Ok(Dialect::GeocodeJson),
            "xml" => Ok(Dialect::Xml),
            other => Err(format!(
                "format={other} is not supported: use json, jsonv2, geojson, geocodejson or xml"
            )),
        }
    }
}

/// Text as an XML attribute or element holds it: the five reserved characters escaped, and the control characters
/// XML 1.0 does not allow (all below U+0020 but tab, line feed and carriage return) left out.
pub(crate) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// `Fri, 14 Nov 2026 01:02:03 +00:00`: Nominatim's `timestamp` attribute, in UTC.
fn xml_timestamp(unix: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (days, secs) = (unix / 86_400, unix % 86_400);
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{}, {day:02} {} {year} {:02}:{:02}:{:02} +00:00",
        DAYS[(days % 7) as usize],
        MONTHS[(month - 1) as usize],
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The attributes of `<place>` (search) or `<result>` (reverse), from the jsonv2 place of the same hit.
fn xml_place_attributes(place: &Value, with_details: bool) -> String {
    let mut a = String::new();
    let mut put = |k: &str, v: String| {
        a.push_str(&format!(" {k}=\"{}\"", xml_escape(&v)));
    };
    put("place_id", text_of(&place["place_id"]));
    for key in ["osm_type", "osm_id"] {
        if let Some(v) = place.get(key) {
            put(key, text_of(v));
        }
    }
    // Nominatim writes the name as `ref` when there is no ref tag ("bug reproduced from PHP")
    if let Some(name) = place["name"].as_str().filter(|n| !n.is_empty()) {
        put("ref", name.to_string());
    }
    put("lat", text_of(&place["lat"]));
    put("lon", text_of(&place["lon"]));
    let bbox: Vec<String> = place["boundingbox"]
        .as_array()
        .map(|b| b.iter().map(text_of).collect())
        .unwrap_or_default();
    put("boundingbox", bbox.join(","));
    put("place_rank", text_of(&place["place_rank"]));
    put("address_rank", text_of(&place["place_rank"]));
    if with_details {
        put("display_name", text_of(&place["display_name"]));
        put("class", text_of(&place["category"]));
        put("type", text_of(&place["type"]));
        put("importance", text_of(&place["importance"]));
    }
    a
}

/// The address parts, extratags and namedetails of a place as XML children, in Nominatim's order of ranks.
fn xml_children(place: &Value, address_tag: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(address) = place.get("address").and_then(Value::as_object) {
        let mut parts = String::new();
        for key in [
            "house_number",
            "road",
            "city",
            "state",
            "postcode",
            "country",
            "country_code",
        ] {
            if let Some(v) = address.get(key) {
                parts.push_str(&format!("<{key}>{}</{key}>", xml_escape(&text_of(v))));
            }
        }
        match address_tag {
            Some(tag) => out.push_str(&format!("<{tag}>{parts}</{tag}>")),
            None => out.push_str(&parts),
        }
    }
    if let Some(tags) = place.get("extratags").and_then(Value::as_object) {
        out.push_str("<extratags>");
        for (k, v) in tags {
            out.push_str(&format!(
                "<tag key=\"{}\" value=\"{}\"/>",
                xml_escape(k),
                xml_escape(&text_of(v))
            ));
        }
        out.push_str("</extratags>");
    }
    if let Some(names) = place.get("namedetails").and_then(Value::as_object) {
        out.push_str("<namedetails>");
        for (k, v) in names {
            out.push_str(&format!(
                "<name desc=\"{}\">{}</name>",
                xml_escape(k),
                xml_escape(&text_of(v))
            ));
        }
        out.push_str("</namedetails>");
    }
    out
}

fn xml_root(root: &str, licence: &str, querystring: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<{root} timestamp=\"{}\" attribution=\"{}\" querystring=\"{}\">",
        xml_timestamp(now_unix()),
        xml_escape(licence),
        xml_escape(querystring)
    )
}

/// `/search` (`searchresults`) and `/lookup` (`lookupresults`) as XML: one `<place>` per answer, its address parts as
/// children.
pub(crate) fn xml_list(
    hits: &[Hit],
    show: Show,
    country: &str,
    licence: &str,
    root: &str,
    querystring: &str,
) -> String {
    let mut out = xml_root(root, licence, querystring);
    for h in hits {
        let place = nominatim_place(h, Dialect::JsonV2, show, country, licence);
        out.push_str(&format!(
            "<place{}>{}</place>",
            xml_place_attributes(&place, true),
            xml_children(&place, None)
        ));
    }
    out.push_str(&format!("</{root}>"));
    out
}

/// `/reverse` as XML: `<result>` with the full address as its text, the parts in `<addressparts>`; nothing found is
/// `<error>Unable to geocode</error>`, as Nominatim writes it.
pub(crate) fn xml_reverse(
    hit: Option<&Hit>,
    show: Show,
    country: &str,
    licence: &str,
    querystring: &str,
) -> String {
    let mut out = xml_root("reversegeocode", licence, querystring);
    match hit {
        None => out.push_str("<error>Unable to geocode</error>"),
        Some(h) => {
            let place = nominatim_place(h, Dialect::JsonV2, show, country, licence);
            out.push_str(&format!(
                "<result{}>{}</result>{}",
                xml_place_attributes(&place, false),
                xml_escape(&text_of(&place["display_name"])),
                xml_children(&place, Some("addressparts"))
            ));
        }
    }
    out.push_str("</reversegeocode>");
    out
}

fn xml_response(body: String) -> Response {
    Response {
        status: 200,
        content_type: "text/xml; charset=utf-8",
        body: body.into_bytes(),
    }
}

/// The optional blocks of a Nominatim answer: `addressdetails=1`, `extratags=1`, `namedetails=1`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Show {
    pub address_details: bool,
    pub extra_tags: bool,
    pub name_details: bool,
    /// `gridpin serve --ids`: write `osm_type`/`osm_id`, a number made from the answer (`locator.rs`)
    pub ids: bool,
}

/// `name` of a place as Nominatim writes it (`result.locale_name or ''`): an address point has none, a street is
/// its street, a town its town, a postcode place its code, a place of interest its own name (the POI layer keeps
/// the name where an address keeps its street).
pub(crate) fn place_name(h: &Hit) -> &str {
    if h.flags.contains(&"poi_layer") {
        return &h.street;
    }
    match h.precision {
        "street" | "intersection" => &h.street,
        "city" if query::match_level(h).0 == "Postal" => &h.postcode,
        "city" => &h.commune,
        _ => "",
    }
}

/// `addresstype` by Nominatim's `get_label_tag` read on our category (`place`, `type`): a postcode place is
/// `postcode`, ranks up to 28 are labelled by their type, an address point by its class (`place`).
fn address_type(h: &Hit) -> &'static str {
    if query::match_level(h).0 == "Postal" {
        "postcode"
    } else if place_rank(h.precision) <= 28 {
        place_type(h.precision)
    } else {
        "place"
    }
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
    if show.ids {
        o.insert("osm_type".into(), json!("node"));
        o.insert("osm_id".into(), json!(crate::locator::encode(h)));
    }
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
    // Nominatim writes `place_rank` in every format; json used to leave it out, and adding it changes
    // no field a client already read
    o.insert("place_rank".into(), json!(place_rank(h.precision)));
    // the f32 as it prints (0.94), not its widened f64 (0.9399999976158142)
    o.insert(
        "importance".into(),
        json!(h.confidence.to_string().parse::<f64>().unwrap_or(0.0)),
    );
    o.insert("addresstype".into(), json!(address_type(h)));
    o.insert("name".into(), json!(place_name(h)));
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
        if let Some(name) = crate::country_names::name_of(country) {
            a.insert("country".into(), json!(name));
        }
        if !country.is_empty() {
            a.insert("country_code".into(), json!(country));
        }
        o.insert("address".into(), Value::Object(a));
    }
    if show.name_details {
        // Nominatim writes the name tags of the object; ours has one name or none. An empty object, not null, so a
        // client reading `namedetails.name` gets nothing instead of an error
        let name = place_name(h);
        o.insert(
            "namedetails".into(),
            if name.is_empty() {
                json!({})
            } else {
                json!({ "name": name })
            },
        );
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

/// The GeocodeJSON `type` of an answer (`GEOCODEJSON_RANKS` of Nominatim, by address rank): a house, a street, a
/// town.
fn geocodejson_type(h: &Hit) -> &'static str {
    match place_rank(h.precision) {
        30 => "house",
        26 => "street",
        _ => "city",
    }
}

/// One answer as a GeocodeJSON feature (`format_base_geocodejson` of Nominatim, read 2026-11-13): every element has
/// both `geometry` and `properties.geocoding` (a client such as Mobilizon drops the whole answer when one lacks either).
/// `accuracy` is the distance in metres from the point asked, on reverse only.
fn geocodejson_feature(h: &Hit, show: Show, country: &str, reverse: bool) -> Value {
    let kind = geocodejson_type(h);
    let mut g = Map::new();
    g.insert("place_id".into(), json!(place_id(h)));
    if show.ids {
        g.insert("osm_type".into(), json!("node"));
        g.insert("osm_id".into(), json!(crate::locator::encode(h)));
    }
    g.insert("osm_key".into(), json!("place"));
    g.insert("osm_value".into(), json!(place_type(h.precision)));
    g.insert("type".into(), json!(kind));
    if reverse {
        if let Some(d) = h.distance_m {
            g.insert("accuracy".into(), json!(d.round() as i64));
        }
    }
    g.insert("label".into(), json!(display_name(h)));
    let name = place_name(h);
    if !name.is_empty() {
        g.insert("name".into(), json!(name));
    }
    if show.address_details {
        let mut put = |k: &str, v: &str| {
            if !v.is_empty() {
                g.insert(k.into(), json!(v));
            }
        };
        put("postcode", &h.postcode);
        if kind == "house" {
            put("housenumber", h.housenumber.as_deref().unwrap_or(""));
            put("street", &h.street);
        }
        if kind != "city" {
            put("city", &h.commune);
        }
        put("state", h.region.as_deref().unwrap_or(""));
        put(
            "country",
            crate::country_names::name_of(country).unwrap_or(""),
        );
        put("country_code", country);
        g.insert("admin".into(), json!({}));
    }
    if show.extra_tags {
        let (level, status) = query::match_level(h);
        g.insert(
            "extra".into(),
            json!({ "match_level": level, "match_status": status.to_string() }),
        );
    }
    json!({
        "type": "Feature",
        "properties": { "geocoding": Value::Object(g) },
        "geometry": { "type": "Point", "coordinates": [h.lon, h.lat] },
    })
}

/// A GeocodeJSON answer: the `geocoding` header (spec version 0.1.0, the sheet's attribution and licence, the query)
/// and one feature per answer.
pub fn geocodejson(
    hits: &[Hit],
    show: Show,
    country: &str,
    attribution: &str,
    licence: &str,
    query: &str,
    reverse: bool,
) -> Value {
    let mut head = Map::new();
    head.insert("version".into(), json!("0.1.0"));
    head.insert("attribution".into(), json!(attribution));
    if !licence.is_empty() {
        head.insert("licence".into(), json!(licence));
    }
    // Nominatim writes the query of a search; a reverse request has none
    if !query.is_empty() {
        head.insert("query".into(), json!(query));
    }
    json!({
        "type": "FeatureCollection",
        "geocoding": Value::Object(head),
        "features": hits.iter().map(|h| geocodejson_feature(h, show, country, reverse)).collect::<Vec<_>>(),
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
        Dialect::GeocodeJson => geocodejson(hits, show, country, licence, "", "", false),
        _ => Value::Array(
            hits.iter()
                .map(|h| nominatim_place(h, dialect, show, country, licence))
                .collect(),
        ),
    }
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

/// Nominatim's `json_callback`: the answer wrapped as `name(…)` for a client that loads it as a script — the search box
/// of ipyleaflet and leafmap (leaflet-search sends `json_callback` by default, ANSWERS §300). The name is a dotted
/// JavaScript identifier and nothing else, so the parameter cannot inject a script; an error is answered as JSON.
pub(crate) fn jsonp(req: &Request, r: Response) -> std::result::Result<Response, Response> {
    let Some(name) = req.param("json_callback") else {
        return Ok(r);
    };
    // Nominatim reads json_callback for the JSON formats only; an XML answer goes out as it is
    if !r.content_type.starts_with("application/json") {
        return Ok(r);
    }
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

pub(crate) fn search(
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
        name_details: flag(req, "namedetails", false),
        ids: engine.ids,
    };
    extras_of(engine, req.param("with"))?;
    let (hits, pick) = match search_countries(engine, req) {
        Some(codes) if codes.is_empty() => (Vec::new(), engine.picks()[0]),
        Some(codes) => geocode(engine, counters, &q, limit, &codes),
        None => geocode(engine, counters, &q, limit, &[]),
    };
    // the country and the licence of the sheet that answered, not of the main sheet
    if dialect == Dialect::Xml {
        return Ok(xml_response(xml_list(
            &hits,
            show,
            pick.country,
            pick.attribution,
            "searchresults",
            &q,
        )));
    }
    let mut answer = match dialect {
        Dialect::GeocodeJson => geocodejson(
            &hits,
            show,
            pick.country,
            pick.attribution,
            &crate::serve::meta_of(pick.index, "license"),
            &q,
            false,
        ),
        _ => nominatim_list(&hits, dialect, show, pick.country, pick.attribution),
    };
    if let Some(e) = extras_on(req.param("with"), &pick) {
        attach_extras(&mut answer, &hits, &e, pick.index, Some(&q));
    }
    Ok(Response::json(200, &answer))
}

/// `zoom` of a reverse request, 0–18 as Nominatim documents it (18 by default): 17 and above answer the house, 14–16
/// the street of the nearest address, below that its town. The point stays the nearest address: the sheet keeps no
/// outline of a street or a town to put a centre on.
pub(crate) fn zoomed(mut h: Hit, zoom: u8) -> Hit {
    if zoom >= 17 {
        return h;
    }
    h.housenumber = None;
    if zoom >= 14 {
        h.precision = "street";
    } else {
        h.precision = "city";
        h.street.clear();
    }
    h
}

fn zoom_of(req: &Request) -> std::result::Result<u8, Response> {
    match req.param("zoom") {
        None => Ok(18),
        // above 18 is held at 18, as Nominatim does (TeslaMate asks 19)
        Some(v) => v
            .trim()
            .parse::<u32>()
            .map(|z| z.min(18) as u8)
            .map_err(|_| Response::error(400, "zoom must be a whole number from 0 to 18")),
    }
}

pub(crate) fn reverse(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
) -> std::result::Result<Response, Response> {
    let dialect = Dialect::parse(req.param("format")).map_err(|e| Response::error(400, &e))?;
    let (lat, lon) = (coordinate(req, "lat")?, coordinate(req, "lon")?);
    let show = Show {
        address_details: flag(req, "addressdetails", true),
        extra_tags: flag(req, "extratags", false),
        name_details: flag(req, "namedetails", false),
        ids: engine.ids,
    };
    extras_of(engine, req.param("with"))?;
    let zoom = zoom_of(req)?;
    let (hits, pick) =
        reverse_hits(engine, counters, lat, lon, 1, &[]).map_err(|e| Response::error(400, &e))?;
    let hits: Vec<Hit> = hits.into_iter().map(|h| zoomed(h, zoom)).collect();
    if dialect == Dialect::Xml {
        let querystring: Vec<String> = req.query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        return Ok(xml_response(xml_reverse(
            hits.first(),
            show,
            pick.country,
            pick.attribution,
            &querystring.join("&"),
        )));
    }
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
        Dialect::GeocodeJson => geocodejson(
            std::slice::from_ref(hit),
            show,
            pick.country,
            pick.attribution,
            &crate::serve::meta_of(pick.index, "license"),
            "",
            true,
        ),
        _ => nominatim_place(hit, dialect, show, pick.country, pick.attribution),
    };
    if let Some(e) = extras_on(req.param("with"), &pick) {
        attach_extras(&mut answer, std::slice::from_ref(hit), &e, pick.index, None);
    }
    Ok(Response::json(200, &answer))
}

/// Numbers asked of `/lookup` at most, as Nominatim documents it.
const MAX_LOOKUP: usize = 50;

/// The address an object number (`locator.rs`, `--ids`) names, read back from its point: `None` when the number is not
/// an address, when the cell holds more addresses than one answer list shows, or when two addresses share the number.
pub(crate) fn hit_by_number<'a>(
    engine: &'a Engine,
    counters: &Counters,
    id: i64,
) -> Option<(Hit, crate::serve::Pick<'a>)> {
    let Some((crate::locator::Kind::Address, lat, lon)) = crate::locator::decode(id) else {
        return None;
    };
    let (hits, pick) = reverse_hits(engine, counters, lat, lon, MAX_LIMIT, &[]).ok()?;
    // every address of the cell has to be in view: a full list whose last answer is still in the cell may hide
    // more of them, and one of those could share the number
    let complete =
        hits.len() < MAX_LIMIT || hits.last().is_none_or(|h| !crate::locator::in_cell(id, h));
    // two addresses that share the number (one point, the same three bits) make it nobody's: no answer rather
    // than one of them
    let mut same: Vec<Hit> = hits
        .into_iter()
        .filter(|h| crate::locator::encode(h) == id)
        .collect();
    (complete && same.len() == 1).then(|| (same.remove(0), pick))
}

/// `GET /lookup?osm_ids=N-…,…`: the answers whose numbers (`gridpin serve --ids`) are asked. A number is turned back
/// into its point, the nearest addresses there are asked, and one is answered only when its own number is the one
/// asked: a number the sheet no longer has, someone else's (a positive OpenStreetMap id), a street's or a town's (no
/// house sits at their point) or a broken one gives no answer, never another house. Address details are on by default,
/// as in Nominatim.
pub(crate) fn lookup(
    engine: &Engine,
    counters: &Counters,
    req: &Request,
) -> std::result::Result<Response, Response> {
    let dialect = Dialect::parse(req.param("format")).map_err(|e| Response::error(400, &e))?;
    let raw = req
        .param("osm_ids")
        .ok_or_else(|| Response::error(400, "missing osm_ids"))?;
    let asked: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if asked.len() > MAX_LOOKUP {
        return Err(Response::error(
            400,
            &format!("at most {MAX_LOOKUP} osm_ids at a time"),
        ));
    }
    let show = Show {
        address_details: flag(req, "addressdetails", true),
        extra_tags: flag(req, "extratags", false),
        name_details: flag(req, "namedetails", false),
        ids: true,
    };
    let mut found: Vec<Hit> = Vec::new();
    let mut pick = engine.picks()[0];
    for id in asked.iter().filter_map(|s| crate::locator::parse_osm_id(s)) {
        if let Some((hit, p)) = hit_by_number(engine, counters, id) {
            pick = p;
            found.push(hit);
        }
    }
    if dialect == Dialect::Xml {
        return Ok(xml_response(xml_list(
            &found,
            show,
            pick.country,
            pick.attribution,
            "lookupresults",
            raw,
        )));
    }
    let answer = match dialect {
        Dialect::GeocodeJson => geocodejson(
            &found,
            show,
            pick.country,
            pick.attribution,
            &crate::serve::meta_of(pick.index, "license"),
            "",
            false,
        ),
        _ => nominatim_list(&found, dialect, show, pick.country, pick.attribution),
    };
    Ok(Response::json(200, &answer))
}

pub(crate) fn status(engine: &Engine, req: &Request) -> Response {
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

#[cfg(test)]
mod tests {
    use super::*;

    const HIDE: Show = Show {
        address_details: false,
        extra_tags: false,
        name_details: false,
        ids: false,
    };
    const DETAILS: Show = Show {
        address_details: true,
        extra_tags: false,
        name_details: false,
        ids: false,
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
            name_details: false,
            ids: false,
        };
        let tags = &nominatim_place(&house, Dialect::JsonV2, show, "us", "lic")["extratags"];
        assert_eq!(tags["match_level"], "PointAddress");
        // Nominatim's extratags are strings, so the status is one too
        assert_eq!(tags["match_status"], "M");
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
    fn formats_nominatim_does_not_have_are_refused_and_jsonv2_is_the_default() {
        assert_eq!(Dialect::parse(None).unwrap(), Dialect::JsonV2);
        assert_eq!(Dialect::parse(Some("json")).unwrap(), Dialect::Json);
        assert_eq!(Dialect::parse(Some("xml")).unwrap(), Dialect::Xml);
        assert!(Dialect::parse(Some("html")).is_err());
        assert!(
            Dialect::parse(Some("XML")).is_err(),
            "Nominatim reads the format in lower case"
        );
    }

    #[test]
    fn name_and_addresstype_follow_nominatim_for_every_kind_of_answer() {
        let with = |precision, flags: Vec<&'static str>| Hit {
            flags,
            ..hit(precision, None)
        };
        let cases = [
            (hit("house", Some("1600")), "", "place"),
            (hit("interp", Some("1600")), "", "place"),
            (hit("near", Some("1602")), "", "place"),
            (
                hit("street", None),
                "PENNSYLVANIA Avenue Northwest",
                "street",
            ),
            (
                hit("intersection", None),
                "PENNSYLVANIA Avenue Northwest",
                "street",
            ),
            (hit("city", None), "Washington", "city"),
            (with("city", vec!["postal_place"]), "20500", "postcode"),
            // a place of interest keeps its own name where an address keeps its street
            (
                with("house", vec!["poi_layer"]),
                "PENNSYLVANIA Avenue Northwest",
                "place",
            ),
        ];
        for (h, name, kind) in cases {
            assert_eq!(place_name(&h), name, "{} {:?}", h.precision, h.flags);
            assert_eq!(address_type(&h), kind, "{} {:?}", h.precision, h.flags);
        }
    }

    #[test]
    fn every_format_carries_place_rank_addresstype_and_name() {
        for dialect in [Dialect::Json, Dialect::JsonV2, Dialect::GeoJson] {
            let v = nominatim_place(&hit("street", None), dialect, HIDE, "us", "lic");
            assert_eq!(v["place_rank"], 26, "{dialect:?}");
            assert_eq!(v["addresstype"], "street", "{dialect:?}");
            assert_eq!(v["name"], "PENNSYLVANIA Avenue Northwest", "{dialect:?}");
            let house = nominatim_place(&hit("house", Some("1600")), dialect, HIDE, "us", "lic");
            // an address point has no name: the field is there and empty, as Nominatim writes it
            assert_eq!(house["name"], "", "{dialect:?}");
        }
    }

    #[test]
    fn namedetails_only_when_asked_and_an_object_even_without_a_name() {
        assert!(
            nominatim_place(&hit("street", None), Dialect::JsonV2, HIDE, "us", "lic")
                .get("namedetails")
                .is_none()
        );
        let asked = Show {
            name_details: true,
            ..HIDE
        };
        let street = nominatim_place(&hit("street", None), Dialect::JsonV2, asked, "us", "lic");
        assert_eq!(
            street["namedetails"],
            json!({ "name": "PENNSYLVANIA Avenue Northwest" })
        );
        let house = nominatim_place(
            &hit("house", Some("1600")),
            Dialect::Json,
            asked,
            "us",
            "lic",
        );
        assert_eq!(house["namedetails"], json!({}));
    }

    #[test]
    fn address_names_the_country_of_the_sheet_beside_its_code() {
        let a = &nominatim_place(
            &hit("house", Some("1600")),
            Dialect::Json,
            DETAILS,
            "us",
            "lic",
        )["address"];
        assert_eq!(a["country"], "United States");
        assert_eq!(a["country_code"], "us");
        let unknown = &nominatim_place(
            &hit("house", Some("1600")),
            Dialect::Json,
            DETAILS,
            "zz",
            "lic",
        )["address"];
        assert!(unknown.get("country").is_none(), "{unknown}");
        let none = &nominatim_place(
            &hit("house", Some("1600")),
            Dialect::Json,
            DETAILS,
            "",
            "lic",
        )["address"];
        assert!(none.get("country").is_none() && none.get("country_code").is_none());
    }

    #[test]
    fn xml_text_escapes_the_five_reserved_characters_and_drops_what_xml_cannot_hold() {
        assert_eq!(
            xml_escape("a & b <c> \"d\" 'e'"),
            "a &amp; b &lt;c&gt; &quot;d&quot; &apos;e&apos;"
        );
        assert_eq!(xml_escape("Straße\tÖ\u{1}x\u{1f}"), "Straße\tÖx");
        assert_eq!(xml_escape("Улица 1"), "Улица 1");
    }

    #[test]
    fn the_xml_timestamp_is_nominatims_utc_date() {
        for (unix, want) in [
            (0, "Thu, 01 Jan 1970 00:00:00 +00:00"),
            (259_200, "Sun, 04 Jan 1970 00:00:00 +00:00"),
            (951_782_400, "Tue, 29 Feb 2000 00:00:00 +00:00"),
            (1_794_614_400, "Sat, 14 Nov 2026 00:00:00 +00:00"),
            (4_102_444_800, "Fri, 01 Jan 2100 00:00:00 +00:00"),
            (1_794_614_400 + 3_661, "Sat, 14 Nov 2026 01:01:01 +00:00"),
        ] {
            assert_eq!(xml_timestamp(unix), want, "{unix}");
        }
    }

    #[test]
    fn an_xml_place_carries_the_attributes_and_children_of_its_json_place() {
        let mut h = hit("house", Some("1600"));
        h.street = "Rue \"A\" & <B>".into();
        let x = xml_list(&[h], DETAILS, "us", "lic & co", "searchresults", "q & r");
        assert!(
            x.starts_with(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<searchresults timestamp=\""
            ),
            "{x}"
        );
        assert!(
            x.contains(" attribution=\"lic &amp; co\" querystring=\"q &amp; r\">"),
            "{x}"
        );
        assert!(x.contains(" lat=\"38.8976751\" lon=\"-77.0365470\""), "{x}");
        assert!(
            x.contains(" boundingbox=\"38.8971751,38.8981751,-77.0370470,-77.0360470\""),
            "{x}"
        );
        assert!(x.contains(" class=\"place\" type=\"house\""), "{x}");
        assert!(
            x.contains(
                "<house_number>1600</house_number><road>Rue &quot;A&quot; &amp; &lt;B&gt;</road>"
            ),
            "{x}"
        );
        assert!(
            x.contains("<country>United States</country><country_code>us</country_code>"),
            "{x}"
        );
        assert!(x.ends_with("</place></searchresults>"), "{x}");
        assert!(
            !x.contains(" ref=\""),
            "an address has no name, so no ref: {x}"
        );
        let street = xml_list(
            &[hit("street", None)],
            HIDE,
            "us",
            "lic",
            "searchresults",
            "q",
        );
        assert!(
            street.contains(" ref=\"PENNSYLVANIA Avenue Northwest\""),
            "{street}"
        );
        assert!(
            !street.contains("<road>"),
            "no address parts unless asked: {street}"
        );
    }

    #[test]
    fn an_xml_reverse_is_a_result_with_its_address_parts_or_an_error() {
        let x = xml_reverse(
            Some(&hit("house", Some("1600"))),
            DETAILS,
            "us",
            "lic",
            "lat=1&lon=2",
        );
        assert!(x.contains("<reversegeocode timestamp=\""), "{x}");
        assert!(x.contains(" querystring=\"lat=1&amp;lon=2\">"), "{x}");
        assert!(
            x.contains(">1600 PENNSYLVANIA Avenue Northwest, Washington, 20500</result><addressparts><house_number>1600</house_number>"),
            "{x}"
        );
        assert!(
            !x.contains(" display_name=") && !x.contains(" class="),
            "{x}"
        );
        let none = xml_reverse(None, DETAILS, "us", "lic", "");
        assert!(
            none.contains("><error>Unable to geocode</error></reversegeocode>"),
            "{none}"
        );
    }

    #[test]
    fn geocodejson_has_its_header_and_every_feature_both_geometry_and_geocoding() {
        let v = geocodejson(
            &[
                hit("house", Some("1600")),
                hit("street", None),
                hit("city", None),
            ],
            DETAILS,
            "us",
            "attr",
            "CC0",
            "the query",
            false,
        );
        assert_eq!(v["type"], "FeatureCollection");
        assert_eq!(
            v["geocoding"],
            json!({"version": "0.1.0", "attribution": "attr", "licence": "CC0", "query": "the query"})
        );
        let f = v["features"].as_array().unwrap();
        assert_eq!(f.len(), 3);
        for x in f {
            assert_eq!(x["type"], "Feature");
            assert_eq!(
                x["geometry"]["coordinates"],
                json!([-77.036547, 38.8976751])
            );
            assert!(x["properties"]["geocoding"].is_object(), "{x}");
        }
        let house = &f[0]["properties"]["geocoding"];
        assert_eq!(house["type"], "house");
        assert_eq!(house["housenumber"], "1600");
        assert_eq!(house["street"], "PENNSYLVANIA Avenue Northwest");
        assert_eq!(house["city"], "Washington");
        assert_eq!(house["country"], "United States");
        assert_eq!(house["country_code"], "us");
        // Nominatim writes `admin` with address details; a sheet has no admin levels, so it is empty, not absent
        assert_eq!(house["admin"], json!({}));
        assert_eq!(
            house["label"],
            "1600 PENNSYLVANIA Avenue Northwest, Washington, 20500"
        );
        assert!(
            house.get("name").is_none() && house.get("accuracy").is_none(),
            "{house}"
        );
        let street = &f[1]["properties"]["geocoding"];
        assert_eq!(
            (street["type"].as_str(), street["name"].as_str()),
            (Some("street"), Some("PENNSYLVANIA Avenue Northwest"))
        );
        assert!(
            street.get("street").is_none() && street.get("housenumber").is_none(),
            "{street}"
        );
        let city = &f[2]["properties"]["geocoding"];
        assert_eq!(
            (city["type"].as_str(), city["name"].as_str()),
            (Some("city"), Some("Washington"))
        );
        assert!(city.get("city").is_none(), "{city}");
    }

    #[test]
    fn geocodejson_writes_accuracy_on_reverse_only_and_parts_only_when_asked() {
        let mut h = hit("house", Some("1600"));
        h.distance_m = Some(12.4);
        let back = geocodejson(std::slice::from_ref(&h), HIDE, "us", "a", "", "", true);
        let g = &back["features"][0]["properties"]["geocoding"];
        assert_eq!(g["accuracy"], 12);
        assert!(
            g.get("housenumber").is_none() && g.get("country_code").is_none(),
            "{g}"
        );
        assert!(
            back["geocoding"].get("query").is_none() && back["geocoding"].get("licence").is_none(),
            "{back}"
        );
        let forward = geocodejson(std::slice::from_ref(&h), HIDE, "us", "a", "", "q", false);
        assert!(forward["features"][0]["properties"]["geocoding"]
            .get("accuracy")
            .is_none());
        let tags = Show {
            extra_tags: true,
            ..HIDE
        };
        let extra = &geocodejson(std::slice::from_ref(&h), tags, "us", "a", "", "q", false)
            ["features"][0]["properties"]["geocoding"]["extra"];
        assert_eq!(extra["match_level"], "PointAddress");
    }

    #[test]
    fn zoom_answers_the_house_from_17_the_street_from_14_and_the_town_below() {
        let h = hit("house", Some("1600"));
        for z in [17, 18] {
            let x = zoomed(h.clone(), z);
            assert_eq!(
                (x.precision, x.housenumber.as_deref()),
                ("house", Some("1600")),
                "{z}"
            );
        }
        for z in [14, 16] {
            let x = zoomed(h.clone(), z);
            assert_eq!(
                (x.precision, x.housenumber.as_deref()),
                ("street", None),
                "{z}"
            );
            assert_eq!(x.street, "PENNSYLVANIA Avenue Northwest");
        }
        for z in [0, 13] {
            let x = zoomed(h.clone(), z);
            assert_eq!(
                (x.precision, x.housenumber.as_deref(), x.street.as_str()),
                ("city", None, ""),
                "{z}"
            );
            assert_eq!(x.commune, "Washington");
        }
        let x = zoomed(h.clone(), 13);
        assert_eq!(
            (x.lat, x.lon),
            (h.lat, h.lon),
            "the point stays the nearest address"
        );
    }

    #[test]
    fn the_nearest_house_of_a_far_point_is_a_house_and_says_how_far_it_reaches() {
        let far = hit("approximate", Some("1600"));
        let v = nominatim_place(&far, Dialect::JsonV2, DETAILS, "us", "lic");
        assert_eq!(
            (v["type"].as_str(), &v["place_rank"]),
            (Some("house"), &json!(30))
        );
        assert_eq!(v["address"]["house_number"], "1600");
        let b: Vec<f64> = v["boundingbox"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().parse().unwrap())
            .collect();
        assert!(
            (b[1] - b[0] - 0.001).abs() < 1e-9,
            "the box of a house: {b:?}"
        );
        let show = Show {
            extra_tags: true,
            ..HIDE
        };
        assert_eq!(
            nominatim_place(&far, Dialect::JsonV2, show, "us", "lic")["extratags"]["match_level"],
            "Locality"
        );
        let g = geocodejson(std::slice::from_ref(&far), HIDE, "us", "a", "", "", true);
        assert_eq!(g["features"][0]["properties"]["geocoding"]["type"], "house");
    }
}
