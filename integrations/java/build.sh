#!/bin/sh
# Build the shared library the Java wrapper loads, and compile the wrapper. Needs Rust (cargo) and JDK 22 or later.
#   sh integrations/java/build.sh [out-dir]      (from code/; default out-dir: integrations/java/build)
# The library is linked from the static library of gridpin-ffi, so the C interface is the one in gridpin-ffi/include.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
code=$(cd "$here/../.." && pwd)
out=${1:-$here/build}
mkdir -p "$out/classes"
(cd "$code/gridpin-ffi" && cargo build --release --lib)
lib=$code/gridpin-ffi/target/release/libgridpin_ffi.a
case "$(uname -s)" in
  Darwin) cc -shared -o "$out/libgridpin.dylib" -Wl,-force_load,"$lib" -liconv -lc -lm ;;
  Linux) cc -shared -o "$out/libgridpin.so" -Wl,--whole-archive "$lib" -Wl,--no-whole-archive -lpthread -ldl -lm -lc ;;
  *) echo "build.sh: Linux and macOS only; on Windows link gridpin_ffi.lib into a DLL yourself" >&2; exit 1 ;;
esac
javac -d "$out/classes" "$here/src/dev/gridpin/GridPin.java" "$here/test/Check.java"
echo "library and classes in $out"
