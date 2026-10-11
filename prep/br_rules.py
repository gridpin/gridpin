#!/usr/bin/env python3
"""Generate the Brazilian rule pack: common rules + Portuguese street types (br_overlay.json).

Same mechanism as the Spanish pack (prep/es_rules.py): rules are data, the engine gets no
per-country branch. Build: `make br`; freshness: `python3 prep/br_rules.py --check`.
"""
import argparse
import gzip
import json
import re
import unicodedata
from collections import defaultdict
from pathlib import Path

from es_rules import FAMILIES, expected_pack as common_pack

CODE = Path(__file__).resolve().parent.parent
MARKER = b"# Generated Brazilian additions from br_overlay.json.\n"


REFERENCE = CODE / "eval/reference/br/ibge-municipios.json.gz"


def normalize(value):
    letters = "".join(c for c in unicodedata.normalize("NFD", value.lower())
                      if unicodedata.category(c) != "Mn")
    return re.sub(r"[^a-z0-9]+", " ", letters).strip()


def expected_pack(common, overlay, marker=MARKER, reference=REFERENCE):
    pack = common_pack(common, overlay, marker)
    cities = defaultdict(set)
    states = {}
    for municipality in json.loads(gzip.decompress(reference.read_bytes())):
        uf = ((municipality.get("microrregiao") or {}).get("mesorregiao", {}).get("UF")
              or municipality["regiao-imediata"]["regiao-intermediaria"]["UF"])
        cities[normalize(municipality["nome"])].add(uf["sigla"])
        states[normalize(uf["nome"])] = uf["sigla"]
    table = {
        "regions": " ".join(sorted(set(states.values()))),
        "street_markers": "rua ru r avenida av alameda al travessa tv estrada rodovia praca pca pc largo beco viela",
        "district_markers": "bairro vila jardim jd distrito setor parque pq residencial loteamento colonia col",
    }
    table.update({"city:" + name: " ".join(sorted(ufs)) for name, ufs in cities.items()})
    table.update({"state:" + name: uf for name, uf in states.items()})
    for family in ["city_alias.tsv", "commune_alias.tsv"]:
        for line in pack[family].decode().splitlines():
            if not line.startswith("#") and "\t" in line:
                alias, name = line.split("\t", 1)
                if normalize(name) in cities:
                    table["alias:" + normalize(alias)] = normalize(name)
    pack["municipality_guard.tsv"] = b"# Generated from IBGE municipalities (names and UF).\n" + "".join(
        f"{key}\t{table[key]}\n" for key in sorted(table)).encode()
    return pack


def check_pack(common, overlay, output, marker=MARKER, reference=REFERENCE):
    expected = expected_pack(common, overlay, marker, reference)
    problems = [f"unexpected family: {name}" for name in sorted(
        {p.name for p in output.glob("*.tsv")} - expected.keys())]
    for name, raw in expected.items():
        path = output / name
        if not path.is_file() or path.read_bytes() != raw:
            problems.append(f"stale or missing: {name}")
    return problems


def generate_pack(common, overlay, output, marker=MARKER, reference=REFERENCE):
    pack = expected_pack(common, overlay, marker, reference)
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
    ap.add_argument("--overlay", type=Path, default=CODE / "rules/br_overlay.json")
    ap.add_argument("--output", type=Path, default=CODE / "rules/br")
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--reference", type=Path, default=REFERENCE)
    args = ap.parse_args(argv)
    if args.check:
        problems = check_pack(args.common, args.overlay, args.output, MARKER, args.reference)
        if problems:
            print("\n".join(problems))
            return 1
        print(f"Brazilian rule pack current: {len(expected_pack(args.common, args.overlay, MARKER, args.reference))} families")
    else:
        size = generate_pack(args.common, args.overlay, args.output, MARKER, args.reference)
        print(f"Brazilian rule pack generated: {len(expected_pack(args.common, args.overlay, MARKER, args.reference))} families, {size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
