//! Country sheets of the release: the registry, the cache folder, verification and download.
//!
//! The registry (`sheets.json`, compiled in) pins every sheet of the release by size and SHA-256,
//! copied from the signed release attestation. A sheet is stored at `<home>/<release>/<file>`, the
//! same place gridpin-mcp uses, so a sheet fetched by one tool is found by the other.
//!
//! Downloading runs the system `curl` (macOS, Linux and Windows 10+ ship it): no HTTP or TLS code
//! is compiled into the engine. The file goes to a temporary name next to its place, its size and
//! SHA-256 are checked, and only then is it renamed; a mismatch leaves nothing behind.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const REGISTRY: &str = include_str!("../sheets.json");

#[derive(Deserialize)]
pub struct Registry {
    pub release: String,
    pub base_url: String,
    /// SHA-256 of the signed sheets receipt the registry is written from (0.7.1 and later).
    #[serde(default)]
    pub receipt_sha256: String,
    /// SHA-256 of the release attestation; only registries before 0.7.1 carry it.
    #[serde(default)]
    pub attestation_sha256: String,
    pub sheets: Vec<Sheet>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct Sheet {
    pub code: String,
    pub name: String,
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

pub fn registry() -> &'static Registry {
    static PARSED: OnceLock<Registry> = OnceLock::new();
    PARSED.get_or_init(|| {
        serde_json::from_str(REGISTRY).expect("sheets.json is checked in the tests")
    })
}

impl Sheet {
    pub fn url(&self) -> String {
        let r = registry();
        format!("{}/{}/{}", r.base_url, r.release, self.file)
    }
}

/// A country code or English name ("us", "US", "United States") to its sheet.
pub fn resolve(country: &str) -> Result<&'static Sheet> {
    let key = country.trim().to_lowercase();
    registry()
        .sheets
        .iter()
        .find(|s| s.code == key || s.name.to_lowercase() == key)
        .ok_or_else(|| {
            let known: Vec<String> = registry()
                .sheets
                .iter()
                .map(|s| format!("{} ({})", s.code, s.name))
                .collect();
            anyhow!(
                "unknown country {country:?}; available: {}",
                known.join(", ")
            )
        })
}

/// Folder of downloaded sheets: `GRIDPIN_HOME`, else `$XDG_CACHE_HOME/gridpin`, else
/// `~/.cache/gridpin` — the rule of `gridpin_mcp.sheets.home()`, on every system.
pub fn home() -> Result<PathBuf> {
    home_from(|name| std::env::var_os(name).filter(|v| !v.is_empty()))
}

fn home_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<PathBuf> {
    if let Some(explicit) = var("GRIDPIN_HOME") {
        return Ok(expand_tilde(PathBuf::from(explicit), &var));
    }
    if let Some(cache) = var("XDG_CACHE_HOME") {
        return Ok(expand_tilde(PathBuf::from(cache), &var).join("gridpin"));
    }
    let user = var("HOME")
        .or_else(|| var("USERPROFILE"))
        .ok_or_else(|| anyhow!("no home folder: set GRIDPIN_HOME to the folder for sheets"))?;
    Ok(PathBuf::from(user).join(".cache").join("gridpin"))
}

// Python's Path.expanduser(): a leading "~" is the home folder.
fn expand_tilde(path: PathBuf, var: &impl Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path;
    };
    match var("HOME").or_else(|| var("USERPROFILE")) {
        Some(user) => PathBuf::from(user).join(rest),
        None => path,
    }
}

pub fn local_path(sheet: &Sheet) -> Result<PathBuf> {
    Ok(home()?.join(&registry().release).join(&sheet.file))
}

/// Same rule as gridpin-mcp: the file is there with the pinned size (the digest is `verify`'s job).
pub fn is_downloaded(sheet: &Sheet) -> Result<bool> {
    let path = local_path(sheet)?;
    Ok(std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() == sheet.size))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut block = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        hash.update(&block[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Ok,
    Missing,
    WrongSize(u64),
    WrongDigest(String),
}

/// Size first (cheap), then the full SHA-256.
pub fn verify(path: &Path, sheet: &Sheet) -> Result<Verdict> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(Verdict::Missing);
    };
    if meta.len() != sheet.size {
        return Ok(Verdict::WrongSize(meta.len()));
    }
    let digest = sha256_file(path)?;
    Ok(if digest == sheet.sha256 {
        Verdict::Ok
    } else {
        Verdict::WrongDigest(digest)
    })
}

/// `GRIDPIN_OFFLINE` set to anything but "" or "0" forbids the network, like `--offline`.
pub fn offline_from_env() -> bool {
    std::env::var("GRIDPIN_OFFLINE").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Download one sheet with `curl` (`GRIDPIN_CURL` names another program) unless it is already
/// in place with the pinned size and digest. Returns its path.
pub fn fetch(sheet: &Sheet, offline: bool) -> Result<PathBuf> {
    let target = local_path(sheet)?;
    if verify(&target, sheet)? == Verdict::Ok {
        return Ok(target);
    }
    if offline {
        bail!(
            "offline: the {} sheet is not downloaded. Fetch it where the network is allowed:\n  curl -fL -o {} {}\nthen `gridpin sheets verify {}`",
            sheet.name,
            target.display(),
            sheet.url(),
            sheet.code
        );
    }
    let folder = target.parent().expect("a release folder");
    std::fs::create_dir_all(folder)
        .with_context(|| format!("cannot create {}", folder.display()))?;
    let part = folder.join(format!("{}.{}.part", sheet.file, std::process::id()));
    let curl = std::env::var_os("GRIDPIN_CURL").unwrap_or_else(|| "curl".into());
    let status = Command::new(&curl)
        .args([
            "-fL",
            "--retry",
            "3",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "-o",
        ])
        .arg(&part)
        .arg(sheet.url())
        .status();
    let result = match status {
        Err(e) => Err(anyhow!(
            "cannot run {:?} ({e}); install curl or download {} yourself into {}",
            curl,
            sheet.url(),
            target.display()
        )),
        Ok(s) if !s.success() => Err(anyhow!(
            "download of {} failed ({s}); nothing was kept, retry",
            sheet.url()
        )),
        Ok(_) => match verify(&part, sheet)? {
            Verdict::Ok => std::fs::rename(&part, &target).map_err(Into::into),
            Verdict::Missing => Err(anyhow!("{} produced no file", sheet.url())),
            Verdict::WrongSize(n) => Err(anyhow!(
                "{}: got {n} bytes, expected {}; not used",
                sheet.url(),
                sheet.size
            )),
            Verdict::WrongDigest(_) => Err(anyhow!(
                "{}: SHA-256 does not match the signed release; not used",
                sheet.url()
            )),
        },
    };
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result.map(|()| target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<std::ffi::OsString> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).map(Into::into)
    }

    #[test]
    fn the_registry_parses_and_every_sheet_is_pinned() {
        let r = registry();
        assert!(r.release.starts_with('v'));
        let pins = [&r.receipt_sha256, &r.attestation_sha256];
        assert_eq!(
            pins.iter().filter(|p| p.len() == 64).count(),
            1,
            "one signed document pins the sheets"
        );
        assert!(!r.sheets.is_empty());
        for s in &r.sheets {
            assert_eq!(s.sha256.len(), 64, "{}", s.code);
            assert!(
                s.sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "{}",
                s.code
            );
            assert!(s.size > 0 && s.file.ends_with(".bin"), "{}", s.code);
        }
    }

    #[test]
    fn countries_resolve_by_code_or_name_in_any_case() {
        let first = &registry().sheets[0];
        assert_eq!(resolve(&first.code.to_uppercase()).unwrap(), first);
        assert_eq!(
            resolve(&format!(" {} ", first.name.to_lowercase())).unwrap(),
            first
        );
        let err = resolve("atlantis").unwrap_err().to_string();
        assert!(
            err.contains("available") && err.contains(&first.code),
            "{err}"
        );
    }

    #[test]
    fn the_cache_folder_follows_the_gridpin_mcp_rule() {
        let p = |pairs: &[(&str, &str)]| home_from(env(pairs)).unwrap();
        assert_eq!(
            p(&[
                ("GRIDPIN_HOME", "/g"),
                ("XDG_CACHE_HOME", "/x"),
                ("HOME", "/h")
            ]),
            PathBuf::from("/g")
        );
        assert_eq!(
            p(&[("XDG_CACHE_HOME", "/x"), ("HOME", "/h")]),
            PathBuf::from("/x/gridpin")
        );
        assert_eq!(p(&[("HOME", "/h")]), PathBuf::from("/h/.cache/gridpin"));
        assert_eq!(
            p(&[("USERPROFILE", "C:/Users/a")]),
            PathBuf::from("C:/Users/a/.cache/gridpin")
        );
        assert_eq!(
            p(&[("GRIDPIN_HOME", "~/sheets"), ("HOME", "/h")]),
            PathBuf::from("/h/sheets")
        );
        assert!(home_from(env(&[])).is_err());
    }

    #[test]
    fn verify_reads_size_then_digest() {
        let dir =
            std::env::temp_dir().join(format!("gridpin-sheets-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.bin");
        std::fs::write(&file, b"abc").unwrap();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let sheet = |size, sha256: &str| Sheet {
            code: "xx".into(),
            name: "X".into(),
            file: "x.bin".into(),
            size,
            sha256: sha256.into(),
        };
        assert_eq!(verify(&file, &sheet(3, abc)).unwrap(), Verdict::Ok);
        assert_eq!(
            verify(&file, &sheet(4, abc)).unwrap(),
            Verdict::WrongSize(3)
        );
        let other = abc.replace('b', "c");
        assert_eq!(
            verify(&file, &sheet(3, &other)).unwrap(),
            Verdict::WrongDigest(abc.into())
        );
        assert_eq!(
            verify(&dir.join("none.bin"), &sheet(3, abc)).unwrap(),
            Verdict::Missing
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
