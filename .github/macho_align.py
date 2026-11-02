#!/usr/bin/env python3
"""Make a locally built macOS extension loadable when the linker left its string table misaligned.

Seen with the Apple linker of Command Line Tools 27.0 and 27.2 (ld-27037, ld-27106) on macOS 27: the
indirect symbol table (4 bytes per entry) sits right before the string table, and with an odd number
of entries the string table starts at an offset that is not a multiple of 8. dyld then refuses the
file ("mis-aligned LINKEDIT string pool"), so `import gridpin` fails although the build succeeded.
Whether a build is hit depends only on its size, so it comes and goes with unrelated changes.

This inserts zero bytes before the string table, moves every LINKEDIT offset at or after it, and signs
the file again ad hoc (`codesign`), as the linker would have. An aligned file is left untouched. It is
used by `make bindings` on the installed development module only; release wheels are built by CI.

    python3 .github/macho_align.py path/to/module.so [...]
"""
import struct
import subprocess
import sys

MH_MAGIC_64 = 0xFEEDFACF
LC_SEGMENT_64, LC_SYMTAB, LC_DYSYMTAB, LC_CODE_SIGNATURE = 0x19, 0x2, 0xB, 0x1D
# linkedit_data_command: dataoff at +8 (signature, function starts, data in code, exports trie, fixups, ...)
LINKEDIT_DATA = {0x1D, 0x1E, 0x26, 0x29, 0x2B, 0x2E, 0x80000033, 0x80000034}
# dysymtab_command offsets: tocoff, modtaboff, extrefsymoff, indirectsymoff, extreloff, locreloff
DYSYMTAB_OFFSETS = (32, 40, 48, 56, 64, 72)


def _commands(data):
    magic, _, _, _, ncmds = struct.unpack_from("<IiiII", data, 0)
    if magic != MH_MAGIC_64:
        raise ValueError("not a 64-bit Mach-O file")
    out, off = [], 32
    for _ in range(ncmds):
        cmd, size = struct.unpack_from("<II", data, off)
        out.append((cmd, off))
        off += size
    return out


def string_table_offset(data):
    symtab = next(o for c, o in _commands(data) if c == LC_SYMTAB)
    return struct.unpack_from("<I", data, symtab + 16)[0]


def realign(data):
    """The file with its string table 8-aligned (unsigned: run on a file without a code signature)."""
    return shift_string_table(data, (-string_table_offset(data)) % 8)


def shift_string_table(data, pad):
    """Insert `pad` zero bytes before the string table and move the offsets after it."""
    data = bytearray(data)
    cmds = _commands(data)
    stroff = string_table_offset(data)
    if not pad:
        return bytes(data)
    if any(c == LC_CODE_SIGNATURE for c, _ in cmds):
        raise ValueError("remove the code signature first (codesign --remove-signature)")

    def bump(at):
        value = struct.unpack_from("<I", data, at)[0]
        if value >= stroff:
            struct.pack_into("<I", data, at, value + pad)

    for cmd, off in cmds:
        if cmd == LC_SYMTAB:
            bump(off + 8)
            bump(off + 16)
        elif cmd == LC_DYSYMTAB:
            for field in DYSYMTAB_OFFSETS:
                bump(off + field)
        elif cmd in LINKEDIT_DATA:
            bump(off + 8)
        elif cmd == LC_SEGMENT_64 and bytes(data[off + 8:off + 24]).rstrip(b"\0") == b"__LINKEDIT":
            vmsize, fileoff, filesize = struct.unpack_from("<QQQ", data, off + 32)
            filesize += pad
            vmsize = max(vmsize, (filesize + 0x3FFF) & ~0x3FFF)
            struct.pack_into("<QQQ", data, off + 32, vmsize, fileoff, filesize)
    data[stroff:stroff] = b"\0" * pad
    return bytes(data)


def fix(path):
    """Realign `path` in place and sign it again; True when the file had to be changed."""
    with open(path, "rb") as f:
        if string_table_offset(f.read()) % 8 == 0:
            return False
    subprocess.run(["codesign", "--remove-signature", path], check=True)
    with open(path, "rb") as f:
        fixed = realign(f.read())
    with open(path, "wb") as f:
        f.write(fixed)
    subprocess.run(["codesign", "--force", "--sign", "-", path], check=True, capture_output=True)
    return True


def main(paths):
    for path in paths:
        print(f"{path}: {'string table realigned, signed again' if fix(path) else 'aligned'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
