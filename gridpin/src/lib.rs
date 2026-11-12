//! Geocoder core library (used by both the CLI binary and the Python bindings).

pub mod abbrev;
pub mod aliases;
pub mod bench;
pub mod builder;
pub mod cite;
pub mod cluster;
pub mod codes;
pub mod compare;
pub mod country_names;
pub mod crs;
mod de;
#[cfg(test)]
mod de_effect_audit;
pub mod diagnosis;
pub mod exists;
pub mod explain;
pub mod export;
pub mod extract;
pub mod extras;
pub mod forms;
pub mod geojson_io;
pub mod google;
pub mod grammar;
pub mod index;
pub mod iso20022;
mod levenshtein;
pub mod ml;
pub mod norm;
pub mod outcome;
pub mod places;
pub mod points;
pub mod postcodes;
pub mod publish;
pub mod query;
pub mod receipt;
pub mod rules;
pub mod serve;
pub mod sheets;
pub mod slice;
pub mod tabular;
pub mod verdict;

#[cfg(feature = "python")]
mod py;

/// Test-fixture directory that is removed when the guard goes out of scope.
/// 12.10.2026: the system temp directory held 34 561 `gridpin-*` directories (2 GB) left by test
/// runs that never cleaned up. A memory mapping survives unlinking its file, so a fixture that
/// returns an `Index` removes its directory right when it returns. Directories shared by a whole
/// process (`gridpin-builder-test-<pid>-<start nanos>`) are not guarded: parallel tests share them.
#[cfg(test)]
pub(crate) mod scratch {
    use std::path::{Path, PathBuf};

    pub struct Scratch(PathBuf);

    impl Scratch {
        /// Guard an existing directory: it is removed as a whole when the guard drops.
        pub fn keep(dir: &Path) -> Scratch {
            Scratch(dir.to_path_buf())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
