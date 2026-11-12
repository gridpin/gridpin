#!/bin/sh
# .deb and .rpm of the CLI for one musl target (static binary: the packages depend on no library).
# Run from the repository root: sh .github/linux_packages.sh x86_64-unknown-linux-musl
set -eu
T="$1"
cargo build --release --locked --manifest-path gridpin/Cargo.toml --target "$T"
(cd gridpin && cargo deb --no-build --target "$T" && cargo generate-rpm --target "$T")
ls -l gridpin/target/"$T"/debian/*.deb gridpin/target/"$T"/generate-rpm/*.rpm
