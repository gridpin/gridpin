//! The decompression cap is read from a process-wide environment variable, so setting it inside the library's
//! test binary made every concurrent build over 4 KiB fail at random. This file holds one test on purpose: an
//! integration test binary is its own process, and cargo runs these binaries one after another. Add nothing here.

use std::io::Write as _;

struct Cleanup(std::path::PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_gz_bomb_is_rejected_by_the_cap_from_the_environment() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("gridpin-build-cap-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let _cleanup = Cleanup(dir.clone());

    // A modest stream: compresses far below the ratio guard's concern, and expands past the 4 KiB cap set below.
    let src = dir.join("bomb.csv.gz");
    let mut enc = flate2::write::GzEncoder::new(
        std::fs::File::create(&src).unwrap(),
        flate2::Compression::best(),
    );
    enc.write_all(b"nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n")
        .unwrap();
    for _ in 0..1000 {
        enc.write_all(b"rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\n")
            .unwrap();
    }
    enc.finish().unwrap();

    std::env::set_var("GRIDPIN_MAX_UNCOMPRESSED_BYTES", "4096");
    let res = gridpin::builder::build(&src, &dir.join("bomb.bin"), None, None, None, None, None);
    std::env::remove_var("GRIDPIN_MAX_UNCOMPRESSED_BYTES");

    let err = res.expect_err("an over-cap decompressed stream must fail the build");
    // Must be this cap by name: the ratio guard also rejects such a stream, and a looser check here
    // stayed green with the environment variable ignored altogether.
    assert!(
        err.to_string().contains("GRIDPIN_MAX_UNCOMPRESSED_BYTES"),
        "{err}"
    );
}
