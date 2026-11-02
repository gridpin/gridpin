#!/bin/sh
# Build build/gridpin.node: the gridpin-ffi static library and the Node-API glue in src/, for this machine.
# Needs cargo, a C compiler and Node.js (its include/node headers). The published package carries prebuilt
# binaries per platform; this script is for a source checkout and for CI.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
ffi="$here/../../gridpin-ffi"
cargo build --release --manifest-path "$ffi/Cargo.toml" --lib
node_inc=$(node -p "require('path').join(process.execPath, '..', '..', 'include', 'node')")
[ -f "$node_inc/node_api.h" ] || node_inc=$(node -p "process.config.variables.node_prefix + '/include/node'")
case "$(uname -s)" in
  Darwin) link="-undefined dynamic_lookup" ;;
  *) link="-lpthread -ldl -lm" ;;
esac
mkdir -p "$here/build"
cc -O2 -fPIC -shared -std=c11 -DNODE_GYP_MODULE_NAME=gridpin -I"$node_inc" -I"$ffi/include" \
  "$here/src/gridpin_node.c" "$ffi/target/release/libgridpin_ffi.a" $link -o "$here/build/gridpin.node"
# Some macOS linkers leave the string table misaligned and dyld refuses the module (see the script).
if [ "$(uname -s)" = Darwin ]; then
  python3 "$here/../../.github/macho_align.py" "$here/build/gridpin.node"
fi
echo "built $here/build/gridpin.node"
