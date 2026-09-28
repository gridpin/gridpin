#!/usr/bin/env python3
"""Generate the Spanish rule pack without duplicating maintained common rules."""
import argparse
import json
from pathlib import Path


CODE = Path(__file__).resolve().parent.parent
FAMILIES = frozenset("""
abbrev2 affix_extra capitals city_alias commune_alias countries_mid countries_tail
fr_ord_cities noise noise_after place_junk place_prefix place_type_strip
region_markers street_types_cyr street_types_extra street_types_latin
""".split())
TYPE_FAMILIES = {"street_types_latin", "street_types_extra"}
MARKER = b"# Generated Spanish additions from es_overlay.json.\n"


def expected_pack(common: Path, overlay: Path) -> dict[str, bytes]:
    """Keep every common byte, including comments and column delimiters."""
    names = {p.stem for p in common.glob("*.tsv")}
    if names != FAMILIES:
        raise ValueError(f"common rule families differ: {sorted(names ^ FAMILIES)}")
    additions = json.loads(overlay.read_text(encoding="utf-8"))
    if not isinstance(additions, dict) or set(additions) - TYPE_FAMILIES:
        raise ValueError("overlay must contain only street type families")
    pack = {}
    for name in sorted(FAMILIES):
        rows = additions.get(name, [])
        if not isinstance(rows, list) or any(
            not isinstance(row, str) or not row or row != row.strip()
            or any(c.isspace() for c in row) or row.startswith("#")
            for row in rows
        ):
            raise ValueError(f"{name}: expected single-column type rows")
        if len(rows) != len(set(rows)):
            raise ValueError(f"{name}: duplicate additions")
        raw = (common / f"{name}.tsv").read_bytes()
        if rows:
            separator = b"" if not raw or raw.endswith(b"\n") else b"\n"
            raw += separator + MARKER + ("\n".join(rows) + "\n").encode("utf-8")
        pack[f"{name}.tsv"] = raw
    return pack


def check_pack(common: Path, overlay: Path, output: Path) -> list[str]:
    expected = expected_pack(common, overlay)
    actual_names = {p.name for p in output.glob("*.tsv")}
    problems = [f"unexpected family: {name}" for name in sorted(actual_names - expected.keys())]
    for name, raw in expected.items():
        path = output / name
        if not path.is_file() or path.read_bytes() != raw:
            problems.append(f"stale or missing: {name}")
    return problems


def generate_pack(common: Path, overlay: Path, output: Path) -> int:
    pack = expected_pack(common, overlay)
    if output.resolve() == common.resolve():
        raise ValueError("output must not replace the common rules")
    extra = {p.name for p in output.glob("*.tsv")} - pack.keys()
    if extra:
        raise ValueError(f"unexpected output families: {sorted(extra)}")
    output.mkdir(parents=True, exist_ok=True)
    for name, raw in pack.items():
        path = output / name
        if not path.is_file() or path.read_bytes() != raw:
            path.write_bytes(raw)
    return sum(map(len, pack.values()))


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--common", type=Path, default=CODE / "rules")
    ap.add_argument("--overlay", type=Path, default=CODE / "rules/es_overlay.json")
    ap.add_argument("--output", type=Path, default=CODE / "rules/es")
    ap.add_argument("--check", action="store_true")
    args = ap.parse_args(argv)
    if args.check:
        problems = check_pack(args.common, args.overlay, args.output)
        if problems:
            print("\n".join(problems))
            return 1
        print(f"Spanish rule pack current: {len(FAMILIES)} families")
    else:
        size = generate_pack(args.common, args.overlay, args.output)
        print(f"Spanish rule pack generated: {len(FAMILIES)} families, {size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
