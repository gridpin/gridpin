#!/usr/bin/env python3
"""gridpin public smoke test: engine mechanics on a tiny country (Monaco).

This checks MECHANICS (parsing, typo repair, type padding, reverse), NOT data quality:
expected coordinates were pinned from the same build when the set was created — the
self-reference is deliberate (it is a regression harness for the engine). Accuracy is
measured separately on independent reference sets (see the test bench page).

Usage: python3 eval/smoke/run.py <gridpin-binary> <country.bin> [cases.jsonl]
Exit 0 = all checks passed; otherwise 1 with a failure list.
"""
import json
import math
import pathlib
import subprocess
import sys


def hav_m(a, b, c, d):
    r = 6371000.0
    p1, p2 = math.radians(a), math.radians(c)
    dp, dl = math.radians(c - a), math.radians(d - b)
    x = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * r * math.asin(math.sqrt(x))


def top1(binary, index, case):
    if "reverse" in case:
        lat, lon = case["reverse"]
        cmd = [binary, "reverse", index, str(lat), str(lon), "-k", "1"]
    else:
        cmd = [binary, "query", index, case["q"], "-k", "1"]
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    if out.returncode != 0:
        return None, f"exit {out.returncode}: {out.stderr.strip()[:120]}"
    line = out.stdout.strip().splitlines()
    if not line or not line[0].startswith("{"):
        return None, "empty response"
    return json.loads(line[0]), None


def check(case, hit):
    exp = case["expect"]
    if hit is None:
        return "no answer"
    if "commune" in exp and hit.get("commune") != exp["commune"]:
        return f"commune {hit.get('commune')!r} != {exp['commune']!r}"
    if "street" in exp:
        got = hit.get("street") or ""
        # reverse returns an address line "street number" — compare by prefix
        if got != exp["street"] and not got.startswith(exp["street"] + " "):
            return f"street {got!r} != {exp['street']!r}"
    if "precision" in exp and hit.get("precision") not in exp["precision"]:
        return f"precision {hit.get('precision')!r} not in {exp['precision']}"
    if "lat" in exp:
        d = hav_m(exp["lat"], exp["lon"], hit["lat"], hit["lon"])
        if d > exp.get("max_m", 200):
            return f"too far: {d:.0f} m > {exp.get('max_m', 200)} m"
    return None


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    binary, index = sys.argv[1], sys.argv[2]
    cases_path = pathlib.Path(sys.argv[3] if len(sys.argv) > 3
                              else pathlib.Path(__file__).parent / "mc_cases.jsonl")
    cases = [json.loads(line) for line in open(cases_path, encoding="utf-8") if line.strip()]
    fails = []
    for i, case in enumerate(cases, 1):
        hit, err = top1(binary, index, case)
        problem = err or check(case, hit)
        label = case.get("q") or f"reverse {case['reverse']}"
        if problem:
            fails.append((i, label, problem))
            print(f"  ✗ {i:2d} {label} — {problem}")
        else:
            print(f"  ✓ {i:2d} {label}")
    print(f"\nsmoke: {len(cases) - len(fails)}/{len(cases)} passed")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
