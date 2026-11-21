//! Markdown output for a reader rather than a program: `query --format md`, `reverse --format md` and
//! `batch --output-format md`. An agent or a person reads the answer as a table; the values are those of the JSON
//! answer, in its order, and a weak answer says so in words as well as in its `weak` field.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{bail, Result};
use serde_json::Value;

/// The most rows `batch --output-format md` writes; a larger file belongs in CSV.
pub const MAX_TABLE_ROWS: usize = 200;

/// The words a weak answer carries next to its `weak` field.
pub const WEAK_NOTE: &str = "weak answer: check it before use";

/// One answer per `##` heading and a table of its fields, from the JSON lines `query` or `reverse` print.
pub fn answers(json_lines: &[String], nothing: &str) -> Result<String> {
    if json_lines.is_empty() {
        return Ok(format!("{nothing}\n"));
    }
    let mut out = String::new();
    for (i, line) in json_lines.iter().enumerate() {
        let fields = ordered_fields(line)?;
        let weak = fields
            .iter()
            .any(|(k, v, _)| k == "weak" && v == &Value::Bool(true));
        let mut heading = format!("## {}. {}", i + 1, label(&fields));
        if weak {
            heading.push_str(" (weak)");
        }
        if i > 0 {
            out.push('\n');
        }
        writeln!(out, "{heading}\n\n| field | value |\n|---|---|")?;
        if weak {
            writeln!(out, "| note | {WEAK_NOTE} |")?;
        }
        for (key, _, shown) in &fields {
            writeln!(out, "| {} | {} |", cell(key), cell(shown))?;
        }
    }
    Ok(out)
}

/// The rows of a `batch` CSV output as one Markdown table, with a `note` column naming weak answers.
pub fn table_from_csv(csv_bytes: &[u8]) -> Result<String> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(csv_bytes);
    let headers = reader.headers()?.clone();
    let weak_col = headers.iter().position(|h| h == "gp_weak");
    let mut out = String::new();
    let head: Vec<String> = headers.iter().map(cell).collect();
    writeln!(out, "| {} | note |", head.join(" | "))?;
    writeln!(out, "|{}---|", "---|".repeat(head.len()))?;
    for (n, record) in reader.records().enumerate() {
        if n >= MAX_TABLE_ROWS {
            bail!(too_many("this run writes more, one row per answer (-k)"));
        }
        let record = record?;
        let note = match weak_col.and_then(|c| record.get(c)) {
            Some("true") => WEAK_NOTE,
            _ => "",
        };
        let row: Vec<String> = record.iter().map(cell).collect();
        writeln!(out, "| {} | {note} |", row.join(" | "))?;
    }
    Ok(out)
}

/// Refuses a `batch` input of more data rows than a Markdown table holds, before any of it is geocoded.
pub fn check_input_rows(input: &Path, delimiter: u8) -> Result<()> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(true)
        .from_reader(crate::tabular::open_table(input)?);
    let mut rows = 0usize;
    for record in reader.records() {
        record?;
        rows += 1;
        if rows > MAX_TABLE_ROWS {
            bail!(too_many(&format!(
                "this file has more than {MAX_TABLE_ROWS}"
            )));
        }
    }
    Ok(())
}

fn too_many(what: &str) -> String {
    format!(
        "batch: Markdown output holds at most {MAX_TABLE_ROWS} rows and {what}; \
         write CSV instead (--output-format csv) and read it with a spreadsheet or a script"
    )
}

/// `4 Avenue de la Madone, 98000 Monaco` from the fields an answer has.
fn label(fields: &[(String, Value, String)]) -> String {
    let get = |key: &str| {
        fields
            .iter()
            .find(|(k, _, _)| k == key)
            .and_then(|(_, v, _)| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let street = match (get("housenumber"), get("street").or(get("name"))) {
        (Some(n), Some(s)) => Some(format!("{n} {s}")),
        (None, Some(s)) => Some(s.to_string()),
        _ => None,
    };
    let place = match (get("postcode"), get("commune")) {
        (Some(p), Some(c)) => Some(format!("{p} {c}")),
        (p, c) => p.or(c).map(str::to_string),
    };
    let parts: Vec<String> = [street, place].into_iter().flatten().collect();
    if parts.is_empty() {
        "answer".to_string()
    } else {
        parts.join(", ")
    }
}

/// A table cell: a `|` would end the cell and a line break the row.
fn cell(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

/// The top-level fields of one JSON object in the order they were written (a `serde_json::Map` sorts them), each
/// with the text a reader sees. Numbers keep the exact text of the JSON answer: parsing `41.881708700000004` into an
/// `f64` and writing it again can change its last digit.
fn ordered_fields(line: &str) -> Result<Vec<(String, Value, String)>> {
    let line = line.trim();
    let mut out = Vec::new();
    for member in elements(line, b'{')? {
        let (key, raw) = split_member(member)?;
        let value: Value = serde_json::from_str(raw)?;
        out.push((key, value, shown(raw)?));
    }
    Ok(out)
}

/// The text of one value as a reader sees it: text without quotes, a list of plain values joined by commas,
/// anything else as written.
fn shown(raw: &str) -> Result<String> {
    Ok(match raw.as_bytes().first() {
        Some(b'"') => serde_json::from_str::<String>(raw)?,
        Some(b'[') => {
            let items = elements(raw, b'[')?;
            if items
                .iter()
                .any(|i| i.starts_with('{') || i.starts_with('['))
            {
                raw.to_string()
            } else {
                items
                    .iter()
                    .map(|i| shown(i))
                    .collect::<Result<Vec<_>>>()?
                    .join(", ")
            }
        }
        _ => raw.to_string(),
    })
}

/// `"key": value` of an object member, the key unquoted and the value as written.
fn split_member(member: &str) -> Result<(String, &str)> {
    let end = string_end(member.as_bytes(), 0)?;
    let key: String = serde_json::from_str(&member[..end])?;
    let rest = member[end..].trim_start();
    let Some(value) = rest.strip_prefix(':') else {
        bail!("no ':' after the key {key:?}");
    };
    Ok((key, value.trim()))
}

/// The members of an object or the items of an array, each as written, trimmed.
fn elements(raw: &str, open: u8) -> Result<Vec<&str>> {
    let b = raw.as_bytes();
    let close = if open == b'{' { b'}' } else { b']' };
    if b.first() != Some(&open) || b.last() != Some(&close) {
        bail!(
            "not a JSON {}",
            if open == b'{' { "object" } else { "array" }
        );
    }
    let (mut out, mut depth, mut start, mut i) = (Vec::new(), 0usize, 1usize, 1usize);
    while i < b.len() - 1 {
        match b[i] {
            b'"' => {
                i = string_end(b, i)?;
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                out.push(raw[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let last = raw[start..b.len() - 1].trim();
    if !last.is_empty() {
        out.push(last);
    }
    Ok(out)
}

/// The index just past the JSON string that starts at `at`.
fn string_end(b: &[u8], at: usize) -> Result<usize> {
    if b.get(at) != Some(&b'"') {
        bail!("a JSON string was expected");
    }
    let mut i = at + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Ok(i + 1),
            _ => i += 1,
        }
    }
    bail!("an unterminated JSON string")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_keeps_the_text_of_the_json_answer() {
        let fields = ordered_fields(
            r#"{"lat":41.881708700000004,"q":"a, \"b\"","codes":{"h3":"8a"},"n":[1.10,"x"]}"#,
        )
        .unwrap();
        let shown: Vec<&str> = fields.iter().map(|(_, _, s)| s.as_str()).collect();
        assert_eq!(
            shown,
            [
                "41.881708700000004",
                "a, \"b\"",
                r#"{"h3":"8a"}"#,
                "1.10, x"
            ]
        );
    }

    #[test]
    fn a_pipe_and_a_line_break_stay_inside_their_cell() {
        assert_eq!(cell("a|b\nc"), "a\\|b c");
        assert_eq!(cell("a\\|b"), "a\\\\\\|b");
    }

    #[test]
    fn the_fields_keep_the_order_of_the_json_answer() {
        let fields =
            ordered_fields(r#"{"lat":1,"lon":2,"precision":"house","flags":["a","b"]}"#).unwrap();
        let keys: Vec<&str> = fields.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(keys, ["lat", "lon", "precision", "flags"]);
        assert_eq!(fields[3].2, "a, b");
    }
}
