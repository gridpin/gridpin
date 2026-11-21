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
                    "junk_tail", "homonym_prominence", "postal_glue", "type_before_numeral", "number_slash_flat", "plot_segments", "flat_marks", "ordinal_suffixes", "title_retry", "type_lead", "postbox_words", "region_words", "region_markers", "postcode_town", "number_fraction",
    "homonym_doubt", "postal_pattern", "quadrant_suffixes", "direction_trailing", "city_first", "unit_hyphen", "number_first_types", "glue_road_numbers", "type_first_siblings", "fuzzy_short_forms", "saint_communes", "interp_max_gap", "interp_max_km", "bare_street_types", "postal_pad_zero", "number_mark", "postal_prefix", "commaless_restore", "postal_near_digits", "postal_street_suffix", "no_number_marks", "unit_slash", "postal_street_typo", "house_spans", "commune_short_words", "silence_to_place", "postal_ignore", "silence_place_segments", "next_place_km", "near_next_door_km", "town_house_km", "postal_pair_off", "fuzzy_next_door_km", "glued_street_words", "typo_street_house", "commaless_type_comma", "fuzzy_long_forms", "placeless_retry_km", "type_run_last", "place_first_retry", "street_short_first", "number_last_retry"}
        if not isinstance(config, dict) or not required <= set(config) or set(config) - required - optional:
            raise ValueError("city_tail: expected complete tail grammar")
        # postal_pad_zero: "1" restores the leading zero of a postal code written one digit short (query.rs
        # pad_short_postal, Mexico City "6060" -> "06060").
        if "postal_pad_zero" in config and str(config["postal_pad_zero"]) != "1":
            raise ValueError("city_tail: postal_pad_zero must be \"1\" when present")
        # silence_place_segments: "1" lets silence_to_place try each tail segment as the place (query.rs
        # place_segment_points, New Zealand).
        if "silence_place_segments" in config and str(config["silence_place_segments"]) != "1":
            raise ValueError("city_tail: silence_place_segments must be \"1\" when present")
        # near_next_door_km: an answer near a neighbour on the street written gives way to the very house on
        # that street within so many km (0.1-50) of it (query.rs very_house_next_door, New Zealand).
        if "near_next_door_km" in config and not (
                re.fullmatch(r"(0|[1-9][0-9]?)(\.[0-9])?", str(config["near_next_door_km"]))
                and 0.1 <= float(config["near_next_door_km"]) <= 50):
            raise ValueError("city_tail: near_next_door_km must be 0.1-50 km, one decimal at most, when present")
        # fuzzy_next_door_km: an answer on a street read a letter off gives way to the very house on the street
        # written within so many km (0.1-50) (query.rs very_house_next_door, New Zealand).
        if "fuzzy_next_door_km" in config and not (
                re.fullmatch(r"(0|[1-9][0-9]?)(\.[0-9])?", str(config["fuzzy_next_door_km"]))
                and 0.1 <= float(config["fuzzy_next_door_km"]) <= 50):
            raise ValueError("city_tail: fuzzy_next_door_km must be 0.1-50 km, one decimal at most, when present")
        # postal_pair_off: "1" reads "2703 Te Anga Road" as a house and a word, not the Dutch postcode pair
        # "1012 NZ" (query.rs postal_pair_off, New Zealand).
        # glued_street_words: "1" reads "Camphill Road" as "Camp Hill Road" and back (query.rs glued_word_variants).
        # typo_street_house: "1" keeps the house number on a street the fuzzy reading fixed (query.rs typo_street_house).
        # commaless_type_comma: "1" puts the commas back after the street type of a comma-less query (query.rs).
        # fuzzy_long_forms: "1" retries a fuzzy lookup with short street types spelled out (query.rs fuzzy_long_rest).
        # placeless_retry_km: a house found without the place named stands within these km of it (query.rs placeless_retry).
        if "placeless_retry_km" in config and not (
                re.fullmatch(r"[1-9][0-9]?", str(config["placeless_retry_km"]))
                and 1 <= int(config["placeless_retry_km"]) <= 50):
            raise ValueError("city_tail: placeless_retry_km must be a whole 1-50 km when present")
        # type_run_last: "1" reads "270 Bay View Road" to its last type word in a row (query.rs address_and_tail).
        # place_first_retry: "1" asks "Wairoa, 12 Okare Road" again with the town last (query.rs place_first_retry).
        # street_short_first: "1" reads a street's first word at the other form of commune_short_words (query.rs).
        # number_last_retry: "1" asks "Wharepapa Road 14, Te Awamutu" again with the number first (query.rs).
        if "number_last_retry" in config and config["number_last_retry"] != "1":
            raise ValueError("city_tail: number_last_retry must be \"1\" when present")
        if "street_short_first" in config and config["street_short_first"] != "1":
            raise ValueError("city_tail: street_short_first must be \"1\" when present")
        if "place_first_retry" in config and config["place_first_retry"] != "1":
            raise ValueError("city_tail: place_first_retry must be \"1\" when present")
        if "type_run_last" in config and config["type_run_last"] != "1":
            raise ValueError("city_tail: type_run_last must be \"1\" when present")
        if "fuzzy_long_forms" in config and config["fuzzy_long_forms"] != "1":
            raise ValueError("city_tail: fuzzy_long_forms must be \"1\" when present")
        if "commaless_type_comma" in config and config["commaless_type_comma"] != "1":
            raise ValueError("city_tail: commaless_type_comma must be \"1\" when present")
        if "typo_street_house" in config and config["typo_street_house"] != "1":
            raise ValueError("city_tail: typo_street_house must be \"1\" when present")
        if "glued_street_words" in config and config["glued_street_words"] != "1":
            raise ValueError("city_tail: glued_street_words must be \"1\" when present")
        if "postal_pair_off" in config and config["postal_pair_off"] != "1":
            raise ValueError("city_tail: postal_pair_off must be \"1\" when present")
        # town_house_km: a rural address answered by its postal town alone takes the one house of that number on
        # the street written within so many km (1-150) of the town (query.rs house_near_postal_town, New Zealand).
        if "town_house_km" in config and not (str(config["town_house_km"]).isdigit()
                                              and 1 <= int(config["town_house_km"]) <= 150):
            raise ValueError("city_tail: town_house_km must be a whole number 1-150 when present")
        # next_place_km: the radius in km (1-50) around a named place within which silence_to_place takes the
        # very house written (query.rs next_place_radius_km, New Zealand).
        if "next_place_km" in config and not (str(config["next_place_km"]).isdigit()
                                              and 1 <= int(config["next_place_km"]) <= 50):
            raise ValueError("city_tail: next_place_km must be a whole number 1-50 when present")
        # postal_ignore: "1" drops a postal code that closes a segment on a sheet that keeps none (query.rs
        # drop_unkept_postal, New Zealand).
        if "postal_ignore" in config and str(config["postal_ignore"]) != "1":
            raise ValueError("city_tail: postal_ignore must be \"1\" when present")
        # commaless_restore: "1" gives a query written without commas and ending with a postal code its
        # commas back (query.rs restore_commas, Mexico).
        if "commaless_restore" in config and str(config["commaless_restore"]) != "1":
            raise ValueError("city_tail: commaless_restore must be \"1\" when present")
        # postal_near_digits: how many leading digits of the code written a street's postcode must share when a
        # cityless query read nothing inside the code itself (query.rs bare_street_near_code, Mexico).
        if "postal_near_digits" in config and str(config["postal_near_digits"]) not in {"2", "3", "4"}:
            raise ValueError("city_tail: postal_near_digits must be 2, 3 or 4 when present")
        # postal_street_suffix: "1" lets a query that read nothing take the house of the one street inside the
        # postal code whose name ends with the words written (query.rs postal_street_suffix, Mexico).
        if "postal_street_suffix" in config and str(config["postal_street_suffix"]) != "1":
            raise ValueError("city_tail: postal_street_suffix must be \"1\" when present")
        # no_number_marks: words saying the address has no house number ("s/n"); with postal_street_suffix a
        # street-level answer inside the postal code (query.rs postal_street_without_number, Mexico).
        if "no_number_marks" in config and not str(config["no_number_marks"]).strip():
            raise ValueError("city_tail: no_number_marks must name at least one word when present")
        # number_mark: one character dropped before a house number ("Abasolo #508"); postal_prefix: a word of
        # lowercase letters dropped before a postal code ("C.P. 30450" -> "cp"). query.rs strip_number_marks, Mexico.
        if "number_mark" in config and (len(str(config["number_mark"])) != 1 or str(config["number_mark"]).isalnum()):
            raise ValueError("city_tail: number_mark must be one character that is no letter or digit")
        if "postal_prefix" in config and not re.fullmatch(r"[a-z]{1,4}", str(config["postal_prefix"])):
            raise ValueError("city_tail: postal_prefix must be one to four lowercase letters")
        # bare_street_types: type words tried in front of a typeless street when a cityless query ends
        # with a postal code and the answer is not inside it (query.rs bare_street_postal_variants, Mexico).
        if "bare_street_types" in config and not re.fullmatch(r"[a-z]+( [a-z]+)*", str(config["bare_street_types"])):
            raise ValueError("city_tail: bare_street_types must be lowercase words separated by spaces")
        # homonym_prominence: equal-score homonyms of a cityless query go by size before distance to the
        # sheet's anchor commune (query.rs homonym_prominence_first): "1" — the size of the place (communes
        # within 20 km), "commune" — the commune's own count (Poland: suburbs are separate communes).
        if "homonym_prominence" in config and str(config["homonym_prominence"]) not in ("1", "commune"):
            raise ValueError("city_tail: homonym_prominence must be \"1\" or \"commune\" when present")
        # type_before_numeral: street-type words dropped before a street named by a date ("ul. 3 Maja 93"), query.rs
        # drop_type_before_numeral (Poland): lowercase words separated by spaces.
        if "type_before_numeral" in config and not re.fullmatch(r"[a-ząćęłńóśźż]+( [a-ząćęłńóśźż]+)*", str(config["type_before_numeral"])):
            raise ValueError("city_tail: type_before_numeral must be lowercase words separated by spaces")
        # number_slash_flat: "1" drops the flat of a house number written "house/flat" ("21a/20" -> "21a"), query.rs
        # drop_flat_after_slash (Poland).
        if "number_slash_flat" in config and str(config["number_slash_flat"]) != "1":
            raise ValueError("city_tail: number_slash_flat must be \"1\" when present")
        # plot_segments: "1" drops the land-plot segments a register writes after the address ("dz. nr 12/14", "361/12"),
        # query.rs drop_plot_segments (Poland, UKE).
        # title_retry: "1" asks a line again with its street titles in the other form ("Świętego" <-> "św"), query.rs
        # short_title_retry (Poland; on Brazil the same pairs lose answers).
        if "title_retry" in config and str(config["title_retry"]) != "1":
            raise ValueError("city_tail: title_retry must be \"1\" when present")
        # type_lead: a street-type word opening a segment put as the sheet keys it ("ul= al=aleja"), query.rs
        # rewrite_leading_type (Poland): pairs short=full, the full side may be empty.
        W = r"[a-ząćęłńóśźż]+"
        if "type_lead" in config and not re.fullmatch(rf"{W}=({W})?( {W}=({W})?)*", str(config["type_lead"])):
            raise ValueError("city_tail: type_lead must be pairs short=full separated by spaces")
        # postbox_words, region_words, region_markers: lowercase words (region names may carry a hyphen), query.rs
        # drop_postbox_segment / drop_region_words (Poland).
        for key in ("postbox_words", "region_words", "region_markers"):
            if key in config and not re.fullmatch(r"[a-ząćęłńóśźż-]+( [a-ząćęłńóśźż-]+)*", str(config[key])):
                raise ValueError(f"city_tail: {key} must be lowercase words separated by spaces")
        # postcode_town: "1" answers a postcode and a town alone with the town, query.rs postcode_town_answer (Poland).
        if "postcode_town" in config and str(config["postcode_town"]) != "1":
            raise ValueError("city_tail: postcode_town must be \"1\" when present")
        # number_fraction: "1" reads "10/16" as the house's whole number (query.rs find_house, Poland, PRG).
        if "number_fraction" in config and str(config["number_fraction"]) != "1":
            raise ValueError("city_tail: number_fraction must be \"1\" when present")
        if "plot_segments" in config and str(config["plot_segments"]) != "1":
            raise ValueError("city_tail: plot_segments must be \"1\" when present")
        # flat_marks: marks of a flat written after the house ("m. 24", "lok. 3"), dropped with the flat, query.rs
        # drop_flat_marks (Poland): lowercase words separated by spaces.
        if "flat_marks" in config and not re.fullmatch(r"[a-z]+( [a-z]+)*", str(config["flat_marks"])):
            raise ValueError("city_tail: flat_marks must be lowercase words separated by spaces")
        # ordinal_suffixes: endings of a day written as an ordinal ("3-go Maja", "20-go Stycznia"), dropped from the
        # number so the street is read as the sheet keys it ("3 Maja"), query.rs drop_ordinal_suffixes (Poland):
        # lowercase words separated by spaces.
        if "ordinal_suffixes" in config and not re.fullmatch(r"[a-z]+( [a-z]+)*", str(config["ordinal_suffixes"])):
            raise ValueError("city_tail: ordinal_suffixes must be lowercase words separated by spaces")
        # postal_glue: "1" glues a postal code written with its hyphen at the start of a segment ("31-044 Kraków") into
        # the digits the sheet keys (query.rs glue_split_postcodes, Poland); needs postal_split < postal_digits.
        if "postal_glue" in config and str(config["postal_glue"]) != "1":
            raise ValueError("city_tail: postal_glue must be \"1\" when present")
        # postal_extension: digits of a postal-code extension joined by a hyphen ("62701-1234" -> 4);
        # read by the engine's region_postal form only (query.rs split_region_postal).
        if "postal_extension" in config and not re.fullmatch(r"[1-9]", str(config["postal_extension"])):
            raise ValueError("city_tail: postal_extension must be a single digit 1-9")
        # street_type_last: the street type trails the name ("Main Street"), so type and direction rewrites
        # from abbrev2 also apply to the last word and compose with the leading one (US, 21.10.2026).
        # postal_pattern: an alphanumeric postal code, "A" a letter and "9" a digit, one optional space
        # ("A9A 9A9", Canada); recognised and set aside by the tail grammar (query.rs pattern_postal).
        if "postal_pattern" in config and not re.fullmatch(r"[A9]+( [A9]+)?", str(config["postal_pattern"])):
            raise ValueError("city_tail: postal_pattern must be A/9 letters with at most one space")
        # quadrant_suffixes: direction words the sheet appends to every street of some cities
        # ("105 St Nw", Edmonton, Calgary), tried in turn when the query leaves the quadrant out
        # (query.rs quadrant_variants, Canada 29.10.2026).
        if "quadrant_suffixes" in config and not re.fullmatch(r"[A-Z]{1,2}( [A-Z]{1,2})*",
                                                               str(config["quadrant_suffixes"])):
            raise ValueError("city_tail: quadrant_suffixes must be upper-case direction codes separated by spaces")
        # direction_trailing: the sheet's own (short) direction words, kept after the street type
        # ("10th ave e"); a leading direction in the query is moved there (query.rs
        # direction_trailing_key, Canada 29.10.2026).
        if "direction_trailing" in config and not re.fullmatch(r"[a-z]{1,2}( [a-z]{1,2})*",
                                                                str(config["direction_trailing"])):
            raise ValueError("city_tail: direction_trailing must be short lower-case direction words")
        # city_first: "1" reads "<place> (<address>)", "<place> - <address>" and "<place>, <number> …"
        # address-first as well (query.rs city_first_reordered, Canada 29.10.2026).
        if "city_first" in config and str(config["city_first"]) != "1":
            raise ValueError("city_tail: city_first must be \"1\" when present")
        # unit_hyphen: "1" reads a leading "<unit>-<civic>" as unit and house number (Canada Post form)
        # instead of a range (query.rs unit_hyphen_civic, Canada 29.10.2026).
        if "unit_hyphen" in config and str(config["unit_hyphen"]) != "1":
            raise ValueError("city_tail: unit_hyphen must be \"1\" when present")
        # number_first_types: short street-type words the sheet keys number first ("7 hwy"); "Highway 7",
        # "Highway No. 3" are read that way too (query.rs number_first_key, Canada 29.10.2026).
        if "number_first_types" in config and not re.fullmatch(r"[a-z]+( [a-z]+)*",
                                                                str(config["number_first_types"])):
            raise ValueError("city_tail: number_first_types must be lower-case words")
        # glue_road_numbers: "1" keeps a road's own number in its name ("No. 3 Road" -> "No3", "47 A Avenue"
        # -> "47A") instead of the parser reading it as the house number (query.rs glue_road_numbers).
        if "glue_road_numbers" in config and str(config["glue_road_numbers"]) != "1":
            raise ValueError("city_tail: glue_road_numbers must be \"1\" when present")
        # type_first_siblings: "1" lets a type written in front reach the sheet's other short form of
        # the same type ("Blvd Manseau" -> "boul manseau"; query.rs type_first_sibling_keys).
        if "type_first_siblings" in config and str(config["type_first_siblings"]) != "1":
            raise ValueError("city_tail: type_first_siblings must be \"1\" when present")
        # fuzzy_short_forms: "1" repeats an empty fuzzy street search with type and direction words at the
        # sheet short form ("Ratchfrod Street" -> "ratchfrod st"; query.rs fuzzy_short_rest).
        if "fuzzy_short_forms" in config and str(config["fuzzy_short_forms"]) != "1":
            raise ValueError("city_tail: fuzzy_short_forms must be \"1\" when present")
        # saint_communes: "1" spells out a town opening with "st"/"ste" when the name as written finds no town
        # ("STE-JULIE" -> "sainte julie"; query.rs saint_commune_name).
        if "saint_communes" in config and str(config["saint_communes"]) != "1":
            raise ValueError("city_tail: saint_communes must be \"1\" when present")
        # interp_max_gap / interp_max_km: how wide a bracket of stored house numbers is still interpolated
        # (numbers, km; engine defaults 12 and 0.3) — sparse rural numbering widens both (query.rs
        # interp_limits, Canada 29.10.2026).
        if "interp_max_gap" in config and not re.fullmatch(r"[1-9][0-9]{0,3}", str(config["interp_max_gap"])):
            raise ValueError("city_tail: interp_max_gap must be a whole number 1-9999")
        if "interp_max_km" in config and not re.fullmatch(r"(0|[1-9][0-9]?)\.[0-9]", str(config["interp_max_km"])):
            raise ValueError("city_tail: interp_max_km must be a number of kilometres with one decimal")
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
