//! Build-time provenance: bake the git commit identity into the binary so a built sheet's
//! SEC_META can record EXACTLY which source produced it (builder_git), alongside input_blake2b256
//! and builder_target. Best-effort — outside a git checkout (e.g. a fresh public export before its
//! first commit) it degrades to "unknown", never failing the build. Shipped in the public export so
//! public/CI builds also stamp provenance (the public repo's commit), not "unknown".
use std::path::Path;
use std::process::Command;

fn main() {
    // FULL 40-hex commit: the strict release gate requires exactly 40 lowercase
    // hex and compares against `git rev-parse HEAD` — a --short=12 SHA made the producer and its
    // own gate mutually exclusive (every fresh sheet was rejected as "not a full 40-hex commit").
    let sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    let ident = match sha {
        Some(s) if dirty => format!("{s}-dirty"),
        Some(s) => s,
        None => "unknown".to_string(),
    };
    println!("cargo:rustc-env=GRIDPIN_GIT_SHA={ident}");

    // Re-run when HEAD moves (branch switch) AND when the CURRENT branch's ref moves (a commit does
    // NOT change HEAD's content — it stays `ref: refs/heads/<branch>` — so watching HEAD alone
    // leaves the SHA stale after a commit, -cargo). Ask git where these files are instead of
    // guessing `../.git`: in a linked worktree `.git` is a FILE, HEAD lives in
    // `.git/worktrees/<name>/` and the branch ref in the common directory, so the guessed paths
    // matched nothing and a commit left the previous stamp, `-dirty` included, in the next build.
    // The index is watched too: staging or committing anywhere in the tree re-evaluates `-dirty`.
    // Outside a checkout git answers nothing and nothing is watched, as before.
    let mut watched = vec![
        "HEAD".to_string(),
        "packed-refs".to_string(),
        "index".to_string(),
    ];
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watched.push(reference);
    }
    for name in watched {
        if let Some(path) = git(&["rev-parse", "--git-path", &name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    // Re-run when ANY tracked source changes. Emitting the git-ref watches above
    // OPTS OUT of cargo's default "rerun on any package file change", so editing src/query.rs (which
    // makes the tree DIRTY but moves no ref) would NOT re-run this script — the binary would keep a
    // STALE clean SHA and pass the strict provenance gate despite a dirty build. Watch every source
    // file so `-dirty` is recomputed on any edit. `Cargo.toml`/`build.rs` are watched implicitly.
    watch_tree(Path::new("src"));
}

/// One line of git's answer, or None outside a checkout or when git fails.
fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Emit `rerun-if-changed` for every file under `dir`, recursively, so any source edit re-runs the
/// build script (and re-evaluates the dirty flag). Best-effort: unreadable entries are skipped.
fn watch_tree(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            watch_tree(&path);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
