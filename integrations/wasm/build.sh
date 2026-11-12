#!/bin/sh
# Build gridpin_wasm.wasm (no wasm-bindgen; `rustup target add wasm32-unknown-unknown` once).
set -eu
cd "$(dirname "$0")"
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/gridpin_wasm.wasm .
ls -l gridpin_wasm.wasm
