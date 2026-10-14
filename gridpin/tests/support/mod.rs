//! Shared by the integration tests: a fixture directory removed when it goes out of scope.
#![allow(dead_code)]
use std::ffi::OsStr;
use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A directory under the system temp folder, removed when the value goes out of scope.
/// 12.10.2026: the system temp directory held 34 561 `gridpin-*` directories (2 GB) left by test
/// runs that never cleaned up (`src/query.rs` 16 sites, `tests/de_rules.rs` 11, `tests/cli.rs` 2).
/// `path` is what the fixture hands to the test: the directory itself or a sheet inside it.
pub struct Scratch {
    dir: PathBuf,
    path: PathBuf,
}

impl Scratch {
    /// A fresh empty directory `gridpin-<name>-<pid>`; the test receives the directory itself.
    pub fn dir(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("gridpin-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch {
            path: dir.clone(),
            dir,
        }
    }

    /// An already populated `dir`; the test receives `path` inside it (usually a `.bin` sheet).
    pub fn file(dir: PathBuf, path: PathBuf) -> Scratch {
        Scratch { dir, path }
    }

    /// The same directory, but the test receives `path` inside it; ownership moves to the new value.
    pub fn into_file(self, path: PathBuf) -> Scratch {
        let dir = self.dir.clone();
        std::mem::forget(self);
        Scratch { dir, path }
    }
}

impl Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<OsStr> for Scratch {
    fn as_ref(&self) -> &OsStr {
        self.path.as_os_str()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::Scratch;

    #[test]
    fn scratch_removes_its_directory_on_drop_and_into_file_keeps_it_alive() {
        let dir = Scratch::dir("support-self-test");
        let kept: std::path::PathBuf = dir.to_path_buf();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        let sheet = dir.into_file(kept.join("a.txt"));
        assert!(
            kept.is_dir(),
            "into_file hands the directory to the new value instead of removing it"
        );
        assert_eq!(std::fs::read(&*sheet).unwrap(), b"x");
        drop(sheet);
        assert!(
            !kept.exists(),
            "the directory is removed when the value goes out of scope"
        );
    }
}
