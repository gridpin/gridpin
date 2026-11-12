#!/bin/sh
# Build gridpin_sqlite (.dylib on macOS, .so on Linux, .dll on Windows under Git Bash) from the gridpin-ffi static
# library. SQLITE_INCLUDE names a folder with sqlite3ext.h when the system has none (the release job uses the
# SQLite amalgamation, pinned by its checksum; Windows and a bare Linux have no header).
set -eu
cd "$(dirname "$0")"
FFI=../../gridpin-ffi
INC="-I $FFI/include"
if [ -n "${SQLITE_INCLUDE:-}" ]; then INC="$INC -I $SQLITE_INCLUDE"; fi
case "$(uname)" in
  MINGW*|MSYS*|CYGWIN*)
    # The libraries the Rust static library needs on Windows are the ones rustc names; asked without colour, so no
    # escape code reaches the linker as a library name (PRINCIPLES 5.4-bis, fifth source).
    LIBS=$(cargo rustc --release --manifest-path "$FFI/Cargo.toml" --lib --crate-type staticlib --color never \
             -- --print native-static-libs 2>&1 | sed -n 's/.*native-static-libs: //p' | tail -1)
    test -n "$LIBS" || { echo "rustc named no native libraries" >&2; exit 1; }
    LINK=""
    for l in $LIBS; do case "$l" in /*) LINK="$LINK -${l#/}";; *) LINK="$LINK $l";; esac; done
    # clang-cl finds Visual Studio itself; -MD matches the CRT the Rust library is built against
    clang-cl -nologo -LD -MD -O2 $INC gridpin_sqlite.c "$FFI/target/release/gridpin_ffi.lib" -Fe:gridpin_sqlite.dll \
      -link $LINK
    ls -l gridpin_sqlite.dll
    exit 0;;
esac
cargo build --release --manifest-path "$FFI/Cargo.toml"
LIB="$FFI/target/release/libgridpin_ffi.a"
case "$(uname)" in
  Darwin) EXT=dylib; FLAGS="-dynamiclib -undefined dynamic_lookup -framework Security -framework CoreFoundation";;
  *) EXT=so; FLAGS="-shared -fPIC -lpthread -ldl -lm";;
esac
cc -O2 -fPIC $INC gridpin_sqlite.c "$LIB" $FLAGS -o "gridpin_sqlite.$EXT"
ls -l "gridpin_sqlite.$EXT"
