# Contributing to GridPin

Thanks for your interest in GridPin — the offline geocoder. Off the grid. On the grid.

We welcome contributions to:

- **Engine** — the Rust core, CLI, Python bindings (pyo3/maturin), DuckDB extension (`gridpin_ext`)
- **Data pipeline** — the DuckDB SQL under `prep/`
- **Docs** — guides, examples, corrections

Small, focused pull requests are easiest to review and merge. For anything non-trivial, open an issue first so we can agree on the approach before you invest time.

## Developer Certificate of Origin (DCO)

We use the [DCO](https://developercertificate.org/), not a CLA. By signing off a commit you certify that you wrote the change (or have the right to submit it) under the project's license (Apache-2.0). That's it — no paperwork, no copyright assignment.

Sign off every commit:

```sh
git commit -s -m "fix: handle empty street names in NL parser"
```

This adds a `Signed-off-by:` line to the commit message. PRs with unsigned commits cannot be merged — the `dco` workflow checks every commit of a PR; `git commit --amend -s` or `git rebase --signoff` fixes them retroactively. Community standards live in [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md); the name and logo are covered by [TRADEMARKS.md](TRADEMARKS.md), not by Apache-2.0.

## Development setup

You need stable Rust.

```sh
cargo build --release --manifest-path gridpin/Cargo.toml
cargo test --manifest-path gridpin/Cargo.toml
```

Python bindings:

```sh
pip install maturin
cd gridpin && maturin develop --release
```

Smoke test — builds a tiny country file for Monaco from an OSM extract and runs live queries against it (needs Python 3 with `pip install duckdb` for the data pipeline):

```sh
make smoke
```

If `cargo test` and `make smoke` pass locally, you are in good shape: CI runs the same on three operating systems.

## Style

- **Match the surrounding style.** The codebase predates a strict rustfmt profile; keep diffs minimal and consistent with neighboring code.
- **No new dependencies without discussion.** GridPin ships as a single self-contained binary that people embed; every dependency is a cost. Open an issue before adding one.
- **Comments explain constraints, not history.** Write *why the code must be this way* ("BAN house numbers can contain letters, so this is not an integer"), not *what you changed* — git history already records that.

## What lives in the private lab (and why)

Some parts of GridPin are intentionally not in this repository:

- **Evaluation corpora** — curated live queries (with independent coordinates where available) and internal stress test sets
- **Rule tables** — shipped inside the paid data files as `SEC_RULES`
- **ML training** — models and training pipelines

These stay private for one reason: a public test set is a test set you can fit a release to. Keeping the evaluation corpora closed is what lets a regression show up as a regression rather than as a number we tuned. We therefore do not take contributions to them, and we cannot grant access — but everything needed to build, test and extend the engine itself is public, including the smoke test, the benchmark runner and the truth-corpus recipes. Contributions are welcome in the parser and normalisation rules, the CLI and bindings, the sheet build pipeline, documentation, and country coverage: an OSM/registry country we have not built yet is the single most useful thing you can bring.

## Reporting issues

A good geocoding bug report contains:

1. **The exact query** you sent (verbatim string)
2. **The country file** and its version (e.g. `france v5`)
3. **Expected result** — the address/coordinates you believe are correct, ideally with a source
4. **Actual result** — what GridPin returned

"It returns the wrong place for some addresses" is not actionable; the four items above almost always are. For performance issues, include the OS, architecture, and whether you were running single-threaded or batch mode.

## Security

Please do not report security vulnerabilities in public issues. Email **security@gridpin.dev** and we will respond promptly.
