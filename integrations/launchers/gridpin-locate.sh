#!/bin/bash
# Required parameters:
# @raycast.schemaVersion 1
# @raycast.title Locate address (GridPin, offline)
# @raycast.mode compact
# @raycast.packageName GridPin
# Optional parameters:
# @raycast.icon 📍
# @raycast.argument1 { "type": "text", "placeholder": "address (empty: the clipboard)", "optional": true }
# @raycast.description The point of an address from a local GridPin file; copies "lat, lon" and prints a map link.
#
# Also a plain shell command:  GRIDPIN_SHEET=~/gridpin/france.bin gridpin-locate.sh "1 rue de la Paix, Paris"
# Needs the gridpin CLI on PATH (or GRIDPIN_BIN) and GRIDPIN_SHEET (a .bin file). Nothing leaves the machine.
set -u
q=${1:-}
[ -n "$q" ] || q=$(pbpaste 2>/dev/null || true)
bin=${GRIDPIN_BIN:-gridpin}
sheet=${GRIDPIN_SHEET:-}
if [ -z "$sheet" ] || [ ! -f "$sheet" ]; then echo "GridPin: set GRIDPIN_SHEET to a country file (.bin)"; exit 1; fi
if ! command -v "$bin" >/dev/null 2>&1; then echo "GridPin: the gridpin command is not on PATH (or set GRIDPIN_BIN)"; exit 1; fi
if [ -z "${q//[[:space:]]/}" ]; then echo "GridPin: no address (type one, or copy one first)"; exit 1; fi
hit=$("$bin" query "$sheet" "$q" -k 1 2>/dev/null | head -1)
if [ -z "$hit" ]; then echo "GridPin: no answer for \"$q\""; exit 1; fi
line=$(printf '%s' "$hit" | python3 -c '
import json, sys
h = json.load(sys.stdin)
where = " ".join(p for p in (h.get("housenumber") or "", h.get("street") or "") if p)
place = ", ".join(p for p in (where, h.get("commune") or "") if p)
warn = " — check it (weak answer)" if h.get("weak") else ""
lat, lon, prec = h["lat"], h["lon"], h["precision"]
print(f"{lat}, {lon}\t{place} [{prec}]{warn}\thttps://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map=18/{lat}/{lon}")
')
coords=${line%%$'\t'*}
rest=${line#*$'\t'}
[ -n "${GRIDPIN_NO_CLIPBOARD:-}" ] || { printf "%s" "$coords" | pbcopy 2>/dev/null || true; }
echo "$coords — ${rest%%$'\t'*}"
echo "${rest#*$'\t'}"
