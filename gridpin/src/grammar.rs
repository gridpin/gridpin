//! `gridpin grammar <sheet>`: the address grammar a sheet's own rules declare, in the open schema
//! `docs-public/address-grammar.schema.json` (`method: "declared"`). No statistics: the street-type words, which side of
//! the street name they stand on, the digits of the postcode key. The measured grammar (shares over the addresses of
//! a sheet) is `gridpin-pro learn-grammar`, in the same schema with `method: "measured"`.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::query::Index;

/// The schema version this module writes; the schema file carries the same `$id`.
pub const SCHEMA_ID: &str = "https://gridpin.dev/address-grammar.schema.json";

/// The grammar the rules of `idx` declare. `sheet` is the name written in `source.sheet`.
pub fn declared(idx: &Index, sheet: &str) -> Result<Value> {
    // the sheet's identity copied into the output, as the provenance card copies it: not a branch (ANSWERS §60)
    let meta = |k: &str| {
        idx.meta()
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    let country = meta("country")
        .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_lowercase()))
        .ok_or_else(|| {
            anyhow!("the sheet carries no two-letter country code: a grammar is per country")
        })?;
    let r = idx.rule_set();
    let last = r
        .city_tail
        .get("street_type_last")
        .is_some_and(|v| v == "1");
    let digits = r
        .city_tail
        .get("postal_digits")
        .and_then(|d| d.parse::<u64>().ok());
    let mut types: Vec<&str> = r
        .types_latin
        .iter()
        .chain(&r.types_cyr)
        .map(String::as_str)
        .chain(r.types_extra.iter().map(String::as_str))
        .collect();
    types.sort_unstable();
    types.dedup();
    let patterns: Vec<Value> = digits
        .map(|d| vec![json!({"regex": format!("^[0-9]{{{d}}}$"), "share": null, "example": null})])
        .unwrap_or_default();
    Ok(json!({
        "country": country,
        "method": "declared",
        "postcode": {"digits": digits, "patterns": patterns, "position": "unknown"},
        "house_number": {
            "position": if last { "before_street" } else { "after_street" },
            "forms": [],
        },
        "street": {
            "type_position": if last { "last" } else { "first" },
            "types": types.iter().map(|w| json!({"word": w, "share": null})).collect::<Vec<_>>(),
            "particles": [],
            "no_type_share": null,
        },
        "locality": {},
        "region": {"present": null, "kind": "unknown"},
        "source": {
            "sheet": sheet,
            "release": meta("source_release"),
            "rows": null,
            "method": "rules section of the sheet (SEC_RULES)",
        },
    }))
}
