//! A postcode layer beside a sheet: a full postcode to its street, town, region and a point, for countries whose sheet
//! keeps only part of the code (Brazil: the sheet keys the first five CEP digits, the layer the eight).
//!
//! The file (`prep/br_cep_layer.py` builds Brazil's) is text, plain or gzip: a header line `#gridpin postcode layer 1`
//! with key=value fields, then tab-separated rows sorted by code: code, street, district, city, region, lat, lon,
//! houses. A code may have several rows (several streets); the one with most houses comes first. A row without a
//! street is a code of a whole town.
//!
//! Nothing here depends on a country: the layer is data. A query is asked of the layer only when it is a postcode and
//! nothing else, so every other query answers as without the layer.

use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

use crate::query::Hit;

const MAGIC: &str = "#gridpin postcode layer 1";
/// Flag of every answer read from the layer.
pub const LAYER_FLAG: &str = "postcode_layer";

/// One row of the layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry<'a> {
    pub code: &'a str,
    pub street: &'a str,
    pub district: &'a str,
    pub city: &'a str,
    pub region: &'a str,
    pub lat: f64,
    pub lon: f64,
    pub houses: u64,
}

/// The layer in memory: its text and where each row starts. About the size of the unpacked file (Brazil: 70 MB).
pub struct PostcodeLayer {
    text: String,
    rows: Vec<(u32, u32)>,
    /// the header's key=value fields (country, release, source)
    pub meta: Vec<(String, String)>,
}

/// The key of a code: its letters and digits, upper case ("01310-100" and "01310100" are one code).
pub fn key(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Is the whole query a postcode, and which: letters, digits, spaces, '-' and '.' only, with a digit, 3 to 10
/// characters of key. A street, a town or a house number around it makes it an address, not a code.
pub fn query_code(q: &str) -> Option<String> {
    let q = q.trim();
    if q.is_empty()
        || !q
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '.'))
        || !q.chars().any(|c| c.is_ascii_digit())
    {
        return None;
    }
    let k = key(q);
    (3..=10).contains(&k.len()).then_some(k)
}

fn parse_row(line: &str) -> Option<Entry<'_>> {
    let mut f = line.split('\t');
    let e = Entry {
        code: f.next()?,
        street: f.next()?,
        district: f.next()?,
        city: f.next()?,
        region: f.next()?,
        lat: f.next()?.parse().ok()?,
        lon: f.next()?.parse().ok()?,
        houses: f.next()?.parse().ok()?,
    };
    (e.lat.is_finite() && e.lon.is_finite()).then_some(e)
}

impl PostcodeLayer {
    /// Read a layer file (gzip when it starts with the gzip magic). A file that is not a layer, a row that does not
    /// parse or rows out of order are refused with the line number.
    pub fn open(path: &Path) -> Result<PostcodeLayer> {
        let raw = std::fs::read(path).with_context(|| format!("{}", path.display()))?;
        let text = if raw.starts_with(&[0x1f, 0x8b]) {
            let mut s = String::new();
            flate2::read::GzDecoder::new(raw.as_slice())
                .read_to_string(&mut s)
                .with_context(|| format!("{}: not a gzip text", path.display()))?;
            s
        } else {
            String::from_utf8(raw).map_err(|_| anyhow!("{}: not UTF-8 text", path.display()))?
        };
        Self::parse(text).map_err(|e| anyhow!("{}: {e}", path.display()))
    }

    /// The layer from its text (see `open`).
    pub fn parse(text: String) -> Result<PostcodeLayer> {
        if text.len() > u32::MAX as usize {
            bail!("layer over 4 GB");
        }
        let mut lines = text.split_inclusive('\n');
        let header = lines.next().unwrap_or("").trim_end();
        let mut fields = header.split('\t');
        if fields.next() != Some(MAGIC) {
            bail!("not a postcode layer: the first line must start with `{MAGIC}`");
        }
        let meta = fields
            .filter_map(|f| f.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut rows = Vec::new();
        let mut at = header.len() + 1;
        let mut last = String::new();
        for (n, line) in lines.enumerate() {
            let body = line.trim_end_matches(['\n', '\r']);
            if !body.is_empty() {
                let e =
                    parse_row(body).ok_or_else(|| anyhow!("line {}: not a layer row", n + 2))?;
                if e.code < last.as_str() {
                    bail!("line {}: rows are not sorted by code", n + 2);
                }
                last = e.code.to_string();
                rows.push((at as u32, (at + body.len()) as u32));
            }
            at += line.len();
        }
        Ok(PostcodeLayer { text, rows, meta })
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn row(&self, i: usize) -> Entry<'_> {
        let (a, b) = self.rows[i];
        parse_row(&self.text[a as usize..b as usize]).expect("checked when opened")
    }

    fn code_at(&self, i: usize) -> &str {
        let (a, b) = self.rows[i];
        let line = &self.text[a as usize..b as usize];
        line.split('\t').next().unwrap_or("")
    }

    /// Every row of a code (any form: "01310-100", "01310100"), most houses first.
    pub fn lookup(&self, code: &str) -> Vec<Entry<'_>> {
        let k = key(code);
        let lo = self.rows.partition_point(|r| {
            let line = &self.text[r.0 as usize..r.1 as usize];
            line.split('\t').next().unwrap_or("") < k.as_str()
        });
        (lo..self.rows.len())
            .take_while(|&i| self.code_at(i) == k)
            .map(|i| self.row(i))
            .collect()
    }

    /// The answers of a query that is a postcode, at most `k`, or none when the query is not a code or the layer
    /// lacks it (the sheet then answers as usual). A street row answers at street precision, a town row at town
    /// precision; the confidence is the row's share of the code's houses, so a stray street of a code is weak.
    pub fn hits(&self, q: &str, k: usize) -> Vec<Hit> {
        let Some(code) = query_code(q) else {
            return Vec::new();
        };
        let rows = self.lookup(&code);
        let total: u64 = rows.iter().map(|r| r.houses).sum::<u64>().max(1);
        rows.iter()
            .take(k.max(1))
            .map(|r| {
                let share = r.houses as f32 / total as f32;
                let town = r.street.is_empty();
                Hit {
                    lat: r.lat,
                    lon: r.lon,
                    precision: if town { "city" } else { "street" },
                    score: share,
                    confidence: if town { 0.5 } else { 0.9 * share },
                    street: r.street.to_string(),
                    housenumber: None,
                    commune: r.city.to_string(),
                    postcode: display(r.code),
                    flags: if town {
                        vec![LAYER_FLAG, "postal_place"]
                    } else {
                        vec![LAYER_FLAG]
                    },
                    region: (!r.region.is_empty()).then(|| r.region.to_string()),
                    distance_m: None,
                }
            })
            .collect()
    }
}

/// A code as people write it: eight digits as "01310-100" (Brazil's CEP), anything else as stored.
pub fn display(code: &str) -> String {
    if code.len() == 8 && code.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}-{}", &code[..5], &code[5..])
    } else {
        code.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYER: &str = "#gridpin postcode layer 1\tcountry=BR\trelease=r\n\
        01001000\t\t\tSão Paulo\tSP\t-23.5496830\t-46.6336900\t4715\n\
        01001001\tPraca Da Se\t\tSão Paulo\tSP\t-23.5504950\t-46.6340870\t41\n\
        01001001\tEstrada Da Ponte Seca\t\tSão Paulo\tSP\t-23.9231910\t-46.7359160\t19\n\
        01310100\tAvenida Paulista\t\tSão Paulo\tSP\t-23.5645260\t-46.6525040\t89\n";

    fn layer() -> PostcodeLayer {
        PostcodeLayer::parse(LAYER.to_string()).unwrap()
    }

    #[test]
    fn a_code_in_any_form_finds_its_rows_most_houses_first() {
        let l = layer();
        assert_eq!(l.len(), 4);
        for form in ["01310-100", "01310100", " 01310.100 "] {
            let rows = l.lookup(form);
            assert_eq!(rows.len(), 1, "{form}");
            assert_eq!(rows[0].street, "Avenida Paulista");
        }
        let two = l.lookup("01001-001");
        assert_eq!(
            two.iter().map(|r| r.street).collect::<Vec<_>>(),
            ["Praca Da Se", "Estrada Da Ponte Seca"]
        );
        assert!(l.lookup("01310-101").is_empty());
        assert!(l.lookup("00000000").is_empty());
        assert!(l.lookup("99999999").is_empty());
    }

    #[test]
    fn only_a_query_that_is_a_code_is_asked() {
        assert_eq!(query_code("01310-100").as_deref(), Some("01310100"));
        assert_eq!(query_code("v6b 1a1").as_deref(), Some("V6B1A1"));
        assert_eq!(query_code("Avenida Paulista 1000, 01310-100"), None);
        assert_eq!(query_code("01310-100 São Paulo"), None);
        assert_eq!(query_code("Paulista"), None);
        assert_eq!(query_code("12"), None);
        // short enough to be a key, but a street and a number: an address, not a code
        assert_eq!(query_code("Rua 5, 12"), None);
        assert_eq!(query_code("Rua 5/12"), None);
        assert_eq!(query_code(""), None);
    }

    #[test]
    fn hits_say_street_or_town_and_a_stray_street_is_weak() {
        let l = layer();
        let h = l.hits("01310-100", 5);
        assert_eq!(h.len(), 1);
        assert_eq!(
            (h[0].precision, h[0].street.as_str(), h[0].postcode.as_str()),
            ("street", "Avenida Paulista", "01310-100")
        );
        assert_eq!(h[0].region.as_deref(), Some("SP"));
        assert!(h[0].flags.contains(&LAYER_FLAG));
        assert!(!crate::query::hit_is_weak(&h[0]));

        let two = l.hits("01001001", 5);
        assert_eq!(two.len(), 2);
        assert!(two[0].confidence > two[1].confidence);
        assert!(
            crate::query::hit_is_weak(&two[1]),
            "19 of 60 houses: a check before use"
        );
        assert_eq!(l.hits("01001001", 1).len(), 1);

        let town = l.hits("01001-000", 1);
        assert_eq!((town[0].precision, town[0].street.as_str()), ("city", ""));
        assert_eq!(crate::query::match_level(&town[0]).0, "Postal");

        assert!(l.hits("Avenida Paulista", 5).is_empty());
        assert!(l.hits("01310-999", 5).is_empty());
    }

    #[test]
    fn a_file_that_is_not_a_layer_or_out_of_order_is_refused() {
        assert!(PostcodeLayer::parse("code\tstreet\n".to_string()).is_err());
        let unsorted = "#gridpin postcode layer 1\n2\ta\t\tb\tc\t1\t2\t3\n1\ta\t\tb\tc\t1\t2\t3\n";
        let err = PostcodeLayer::parse(unsorted.to_string())
            .err()
            .expect("refused");
        assert!(err.to_string().contains("line 3"), "{err}");
        let bad = "#gridpin postcode layer 1\n1\ta\t\tb\tc\tx\t2\t3\n";
        assert!(PostcodeLayer::parse(bad.to_string()).is_err());
    }
}
