//! `batch --receipt`: a JSON file beside the output that says how it was made — the engine version, the SHA-256 of
//! the sheet, of the input and of the output, the command line and the time. Someone who has the same sheet and the
//! same engine reruns the command line and compares the output's hash: that is the method of a geocoding step, in a
//! form a reviewer can check.
//!
//! Receipts of one output folder form a chain: each carries `prev`, the SHA-256 of the receipt written before it in
//! that folder (kept in `.gridpin-chain` beside them; absent for the first), so a removed or edited receipt breaks
//! the chain. `gridpin-pro verify-chain <folder>` walks it back and checks every output's hash.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use serde_json::json;

fn utc(t: SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The path of the receipt of `output`.
pub fn path_of(output: &Path) -> PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".receipt.json");
    output.with_file_name(name)
}

/// The file of a folder that holds the SHA-256 of its last receipt.
pub const CHAIN: &str = ".gridpin-chain";

pub fn write(
    sheet: &Path,
    input: &Path,
    output: &Path,
    rows: u64,
    started: SystemTime,
) -> Result<PathBuf> {
    let hash = crate::sheets::sha256_file;
    let file = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let chain = output.with_file_name(CHAIN);
    let prev = std::fs::read_to_string(&chain)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() == 64);
    let body = json!({
        "receipt": "gridpin batch",
        "engine": env!("CARGO_PKG_VERSION"),
        "command": std::env::args().collect::<Vec<_>>(),
        "sheet": {"file": file(sheet), "sha256": hash(sheet)?},
        "input": {"file": file(input), "sha256": hash(input)?},
        "output": {"file": file(output), "sha256": hash(output)?},
        "rows": rows,
        "started": utc(started),
        "finished": utc(SystemTime::now()),
        "prev": prev,
    });
    let path = path_of(output);
    std::fs::write(&path, serde_json::to_string_pretty(&body)? + "\n")?;
    std::fs::write(&chain, hash(&path)? + "\n")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_is_utc_iso_and_the_receipt_sits_beside_the_output() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_794_141_075);
        assert_eq!(utc(t), "2026-11-08T12:31:15Z");
        assert_eq!(utc(std::time::UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            utc(std::time::UNIX_EPOCH + std::time::Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
        );
        assert_eq!(
            path_of(Path::new("out/a.csv")),
            Path::new("out/a.csv.receipt.json")
        );
    }
}
