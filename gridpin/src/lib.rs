//! Geocoder core library (used by both the CLI binary and the Python bindings).

pub mod builder;
mod de;
#[cfg(test)]
mod de_effect_audit;
pub mod diagnosis;
pub mod index;
mod levenshtein;
pub mod ml;
pub mod norm;
pub mod query;
pub mod rules;

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
