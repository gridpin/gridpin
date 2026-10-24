#!/usr/bin/env python3
"""Generate the Spanish rule pack without duplicating maintained common rules."""
import argparse
import json
import re
from pathlib import Path


CODE = Path(__file__).resolve().parent.parent
FAMILIES = frozenset("""
abbrev2 affix_extra capitals city_alias commune_alias countries_mid countries_tail
fr_ord_cities noise noise_after place_junk place_prefix place_type_strip
region_markers street_types_cyr street_types_extra street_types_latin
""".split())
TYPE_FAMILIES = {"street_types_latin", "street_types_extra"}
MARKER = b"# Generated Spanish additions from es_overlay.json.\n"


def expected_pack(common: Path, overlay: Path, marker: bytes = MARKER) -> dict[str, bytes]:
    """Keep every common byte, including comments and column delimiters."""
    names = {p.stem for p in common.glob("*.tsv")}
    if names != FAMILIES:
        raise ValueError(f"common rule families differ: {sorted(names ^ FAMILIES)}")
    additions = json.loads(overlay.read_text(encoding="utf-8"))
    if not isinstance(additions, dict) or set(additions) - (TYPE_FAMILIES | {"city_tail", "abbrev2", "countries_tail"}):
        raise ValueError("overlay contains an unsupported rule family")
    pack = {}
    for name in sorted(FAMILIES):
        rows = additions.get(name, [])
        if name == "abbrev2":
            if not isinstance(rows, list) or any(
                not isinstance(row, str) or len(row.split("\t")) != 3
                or row.split("\t")[0] not in {"street", "recipient"}
                or (row.split("\t")[0] == "recipient" and row.split("\t")[2] != "1")
                or any(not value or value != value.strip() or "\n" in value or "\r" in value
                       for value in row.split("\t")[1:])
                for row in rows
            ):
                raise ValueError("abbrev2: expected street/key/value rows")
        elif not isinstance(rows, list) or any(
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
            raw += separator + marker + ("\n".join(rows) + "\n").encode("utf-8")
        pack[f"{name}.tsv"] = raw
    if "city_tail" in additions:
        config = additions["city_tail"]
        required = {"enabled", "regions", "forms", "postal_digits", "postal_split"}
        optional = {"street_titles", "street_title_aliases", "postal_extension", "street_type_last", "region_names",
                    "junk_tail", "homonym_prominence"}
        if not isinstance(config, dict) or not required <= set(config) or set(config) - required - optional:
            raise ValueError("city_tail: expected complete tail grammar")
        # homonym_prominence: "1" orders equal-score homonyms of a cityless query by commune size
        # before distance to the sheet's anchor commune (query.rs homonym_prominence_first).
        if "homonym_prominence" in config and str(config["homonym_prominence"]) != "1":
            raise ValueError("city_tail: homonym_prominence must be \"1\" when present")
        # postal_extension: digits of a postal-code extension joined by a hyphen ("62701-1234" -> 4);
        # read by the engine's region_postal form only (query.rs split_region_postal).
        if "postal_extension" in config and not re.fullmatch(r"[1-9]", str(config["postal_extension"])):
            raise ValueError("city_tail: postal_extension must be a single digit 1-9")
        # street_type_last: the street type trails the name ("Main Street"), so type and direction rewrites
        # from abbrev2 also apply to the last word and compose with the leading one (US, 21.10.2026).
        if "street_type_last" in config and str(config["street_type_last"]) not in {"0", "1"}:
            raise ValueError("city_tail: street_type_last must be 0 or 1")
        # junk_tail: words that open a trailing segment the tail grammar ignores ("near the mall", "tel …");
        # read by the engine's city tail only (query.rs junk_tail_segment).
        if "junk_tail" in config and not re.fullmatch(r"[a-z]+( [a-z]+)*", str(config["junk_tail"])):
            raise ValueError("city_tail: junk_tail must be lowercase words separated by single spaces")
        title_keys = {"street_titles", "street_title_aliases"}
        if set(config) & title_keys and not title_keys <= set(config):
            raise ValueError("city_tail: title configuration must be paired")
        if any(not isinstance(v, str) or not v or v != v.strip()
               or "\t" in v or "\n" in v or "\r" in v for v in config.values()):
            raise ValueError("city_tail: expected nonempty single-line values")
        if title_keys <= set(config):
            words = config["street_titles"].split()
            aliases = [pair.split(":") for pair in config["street_title_aliases"].split()]
            if len(set(words)) != len(words) or any(not word.isalpha() for word in words) or any(
                len(pair) != 2 or not pair[0].isalpha() or pair[1] not in words for pair in aliases
            ) or len({pair[0] for pair in aliases}) != len(aliases):
                raise ValueError("city_tail: malformed title vocabulary")
        pack["city_tail.tsv"] = marker + "".join(
            f"{k}\t{config[k]}\n" for k in sorted(config)
        ).encode("utf-8")
    return pack


def check_pack(common: Path, overlay: Path, output: Path, marker: bytes = MARKER) -> list[str]:
    expected = expected_pack(common, overlay, marker)
    actual_names = {p.name for p in output.glob("*.tsv")}
    problems = [f"unexpected family: {name}" for name in sorted(actual_names - expected.keys())]
    for name, raw in expected.items():
        path = output / name
        if not path.is_file() or path.read_bytes() != raw:
            problems.append(f"stale or missing: {name}")
    return problems


def generate_pack(common: Path, overlay: Path, output: Path, marker: bytes = MARKER) -> int:
    pack = expected_pack(common, overlay, marker)
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
