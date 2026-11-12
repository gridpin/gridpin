//! `gridpin slice <sheet> --bbox … --out part.bin`: a smaller sheet cut from a sheet, for a browser, a device or one
//! city. The source sheet is not changed.
//!
//! The houses inside the area are written as the builder's CSV and built into a new sheet; then the sections that
//! steer reading — the parse model, the ranking weights and the rules — are copied from the source byte for byte, so
//! a query inside the area is read as the source reads it. The passport is the source's with `derived_from` (its
//! SHA-256) and `slice` (the rule of the cut) added. Region polygons beside the source (`<sheet>_admin.bin`) are not
//! cut. A slice is not the country: its completeness is the area's, and it says so in `slice`.

use std::io::Write as _;
use std::ops::ControlFlow;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::index;
use crate::norm::normalize;
use crate::query::{HouseFilter, Index};

/// Sections copied from the source as they are.
const KEPT: [usize; 3] = [index::SEC_PARSER, index::SEC_RANK, index::SEC_RULES];

/// Cut `source` to the houses `filter` keeps, into `out`; returns the number of houses.
pub fn slice(source: &Path, filter: &HouseFilter, rule: &str, out: &Path) -> Result<usize> {
    let idx = Index::open(source)?;
    let keys = idx.street_keys();
    let mut rows: Vec<(String, String, u32, String, String)> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    idx.walk_houses(filter, &mut |h| {
        let row = idx.house_row(h);
        let key = &keys[h.street_id as usize];
        let (voie, insee) = key.split_once('\u{1f}').unwrap_or((key.as_str(), ""));
        rows.push((
            voie.to_string(),
            insee.to_string(),
            h.number,
            idx.house_rep(h).to_string(),
            String::new(),
        ));
        let csv = |s: &str| {
            if s.contains([',', '"', '\n']) {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.to_string()
            }
        };
        let digits: String = row.postcode.chars().filter(char::is_ascii_digit).collect();
        lines.push(format!(
            "{},{},{},{},{},{},{:.7},{:.7},{},{},{}",
            csv(voie),
            csv(insee),
            csv(&normalize(&row.commune)),
            digits,
            h.number,
            csv(idx.house_rep(h)),
            h.lon,
            h.lat,
            csv(&row.street),
            csv(&row.commune),
            csv(&row.postcode),
        ));
        ControlFlow::Continue(())
    });
    if lines.is_empty() {
        bail!("slice: no house of the sheet is in the area");
    }
    // the builder takes rows sorted by street key, then number
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by(|a, b| {
        (&rows[*a].0, &rows[*a].1, rows[*a].2, &rows[*a].3).cmp(&(
            &rows[*b].0,
            &rows[*b].1,
            rows[*b].2,
            &rows[*b].3,
        ))
    });
    let dir = std::env::temp_dir().join(format!(
        "gridpin-slice-{}-{}",
        std::process::id(),
        out.file_name().and_then(|n| n.to_str()).unwrap_or("out")
    ));
    std::fs::create_dir_all(&dir)?;
    let result = (|| -> Result<usize> {
        let csv_path = dir.join("in.csv");
        let mut f = std::io::BufWriter::new(std::fs::File::create(&csv_path)?);
        writeln!(f, "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune,code_postal_display")?;
        for i in &order {
            writeln!(f, "{}", lines[*i])?;
        }
        f.flush()?;
        drop(f);
        let sha = crate::sheets::sha256_file(source)?;
        let mut meta: serde_json::Map<String, serde_json::Value> = idx
            .meta()
            .iter()
            .filter(|(k, _)| k != "input_blake2b256")
            .map(|(k, v)| (k.clone(), serde_json::Value::from(v.clone())))
            .collect();
        meta.insert("derived_from".into(), sha.into());
        meta.insert(
            "slice".into(),
            format!("{rule}; {} houses; not the whole country", lines.len()).into(),
        );
        let man = dir.join("m.json");
        std::fs::write(&man, serde_json::Value::Object(meta).to_string())?;
        let built = dir.join("built.bin");
        crate::builder::build(&csv_path, &built, None, None, None, None, Some(&man))?;
        splice(source, &built, out)?;
        Ok(lines.len())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// `built` with the KEPT sections of `source` in place of its own, written to `out` after it opens.
fn splice(source: &Path, built: &Path, out: &Path) -> Result<()> {
    let src = std::fs::read(source)?;
    let new = std::fs::read(built)?;
    let src_secs = index::parse_sections_for_repack(&src)?;
    let new_secs = index::parse_sections_for_repack(&new)?;
    let mut sections: Vec<(u8, Vec<u8>)> = new_secs
        .iter()
        .filter(|(id, _, _)| !KEPT.contains(&(*id as usize)))
        .map(|(id, off, len)| (*id, new[*off as usize..(*off + *len) as usize].to_vec()))
        .collect();
    for (id, off, len) in src_secs
        .iter()
        .filter(|(id, _, _)| KEPT.contains(&(*id as usize)))
    {
        sections.push((*id, src[*off as usize..(*off + *len) as usize].to_vec()));
    }
    sections.sort_by_key(|(id, _)| *id);
    let mut toc = Vec::new();
    let mut off = (6 + sections.len() * 17) as u64;
    for (id, s) in &sections {
        toc.push((*id, off, s.len() as u64));
        off += s.len() as u64;
    }
    let mut bytes = Vec::new();
    index::write_header(&mut bytes, &toc);
    for (_, s) in &sections {
        bytes.extend_from_slice(s);
    }
    Index::from_bytes(bytes.clone(), "slice").context("slice: the cut sheet does not open")?;
    crate::builder::write_atomic(out, &[&bytes])?;
    Ok(())
}
