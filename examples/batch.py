#!/usr/bin/env python3
"""Geocode a small batch of French addresses and print the results as JSON.

Usage:
    python batch.py /path/to/france.bin
"""

import json
import sys

from gridpin import Geocoder

ADDRESSES = [
    "10 rue de Rivoli, 75004 Paris",
    "1 place Bellecour, 69002 Lyon",
    "35 boulevard Michelet, 13008 Marseille",
    "5 allées de Tourny, 33000 Bordeaux",
    "16 rue de la Monnaie, 59800 Lille",
]


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <country-file>")

    geocoder = Geocoder(sys.argv[1])
    out = [{"query": q, "results": geocoder.geocode(q, k=1)} for q in ADDRESSES]
    print(json.dumps(out, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
