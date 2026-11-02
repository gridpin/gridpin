# node-gyp build of build/Release/gridpin.node: the release workflow uses it on every platform (on Windows
# node-gyp also supplies node.lib, the import library the module links against). Build gridpin-ffi first:
# cargo build --release --manifest-path ../../gridpin-ffi/Cargo.toml --lib
{
  "targets": [
    {
      "target_name": "gridpin",
      "sources": ["src/gridpin_node.c"],
      "include_dirs": ["../../gridpin-ffi/include"],
      "conditions": [
        ["OS=='win'", {
          "libraries": [
            "<(module_root_dir)/../../gridpin-ffi/target/release/gridpin_ffi.lib",
            "kernel32.lib", "advapi32.lib", "ntdll.lib", "userenv.lib", "ws2_32.lib", "bcrypt.lib", "dbghelp.lib"
          ]
        }],
        ["OS!='win'", {
          "libraries": ["<(module_root_dir)/../../gridpin-ffi/target/release/libgridpin_ffi.a"]
        }]
      ]
    }
  ]
}
