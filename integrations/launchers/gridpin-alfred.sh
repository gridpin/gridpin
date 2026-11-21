#!/bin/bash
# Alfred Script Filter: the top answers of a local GridPin file as Alfred items. In Alfred: Workflows > + > Blank
# Workflow, add Inputs > Script Filter (bash, "with input as argv"), paste this script's path, and connect it to
# Outputs > Copy to Clipboard. Each item copies "lat, lon"; Cmd opens the map. Needs GRIDPIN_SHEET (workflow variable).
set -u
q=${1:-}
bin=${GRIDPIN_BIN:-gridpin}
sheet=${GRIDPIN_SHEET:-}
item() { python3 -c 'import json,sys; print(json.dumps({"items":[{"title":sys.argv[1],"subtitle":sys.argv[2],"valid":False}]}))' "$1" "$2"; }
if [ -z "$sheet" ] || [ ! -f "$sheet" ]; then item "GridPin: no country file" "Set the workflow variable GRIDPIN_SHEET to a .bin file"; exit 0; fi
if [ -z "${q//[[:space:]]/}" ]; then item "Type an address" "Answers come from $(basename "$sheet"), offline"; exit 0; fi
"$bin" query "$sheet" "$q" -k 5 2>/dev/null | python3 -c '
import json, sys
items = []
for line in sys.stdin:
    h = json.loads(line)
    where = " ".join(p for p in (h.get("housenumber") or "", h.get("street") or "") if p)
    title = ", ".join(p for p in (where, h.get("commune") or "") if p) or h.get("commune") or "?"
    lat, lon, prec = h["lat"], h["lon"], h["precision"]
    coords = f"{lat}, {lon}"
    sub = f"{coords} · {prec}" + (" · weak: check it" if h.get("weak") else "")
    url = f"https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map=18/{lat}/{lon}"
    items.append({"title": title, "subtitle": sub, "arg": coords, "mods": {"cmd": {"arg": url, "subtitle": "Open the map"}}})
if not items:
    items = [{"title": "No answer", "subtitle": "Nothing in the file matches", "valid": False}]
print(json.dumps({"items": items}))
'
