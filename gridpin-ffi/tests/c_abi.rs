//! The C header and the static library agree: compile `tests/c/abi.c` against
//! `include/gridpin.h`, link it with this crate's static library and run it on a tiny sheet.
//! A Rust test of the same calls would pass even if the header lied about a signature.
use std::path::{Path, PathBuf};
use std::process::Command;

const HDR: &str = "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n";

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sheet(dir: &Path) -> PathBuf {
    let csv = dir.join("in.csv");
    std::fs::write(
        &csv,
        format!("{HDR}rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\nrue a,001,ville,10000,3,,7.4202,43.7302,Rue A,Ville\n"),
    )
    .unwrap();
    let manifest = dir.join("m.json");
    std::fs::write(
        &manifest,
        r#"{"country":"mc","layer":"addresses","license":"test","source_release":"test"}"#,
    )
    .unwrap();
    let bin = dir.join("s.bin");
    gridpin::builder::build(&csv, &bin, None, None, None, None, Some(&manifest)).unwrap();
    bin
}

/// Build the static library the way a C host gets it and return its path and the system
/// libraries rustc says it needs.
fn static_library() -> (PathBuf, Vec<String>) {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let out = Command::new(env!("CARGO"))
        .args([
            "rustc",
            "--lib",
            "--crate-type",
            "staticlib",
            "--manifest-path",
        ])
        .arg(&manifest)
        .args(["--", "--print", "native-static-libs"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "static library build failed: {stderr}"
    );
    let libs = stderr
        .lines()
        .find_map(|l| l.split("native-static-libs:").nth(1))
        .expect("rustc printed native-static-libs")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let lib = target.join(profile).join("libgridpin_ffi.a");
    assert!(lib.exists(), "{} not built", lib.display());
    (lib, libs)
}

#[test]
fn c_program_built_against_the_header_runs_every_call() {
    if cfg!(windows) {
        return; // the release pipeline builds the MSVC library; this check runs on Unix hosts
    }
    let dir = std::env::temp_dir().join(format!("gridpin-ffi-c-abi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _cleanup = Scratch(dir.clone());
    let bin = sheet(&dir);
    let (lib, libs) = static_library();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let exe = dir.join("abi");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let built = Command::new(&cc)
        .args(["-std=c99", "-Wall", "-Werror", "-I"])
        .arg(root.join("include"))
        .arg(root.join("tests/c/abi.c"))
        .arg(&lib)
        .args(&libs)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "C build failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new(&exe).arg(&bin).output().unwrap();
    assert!(
        run.status.success() && String::from_utf8_lossy(&run.stdout).trim() == "OK",
        "C program failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}
