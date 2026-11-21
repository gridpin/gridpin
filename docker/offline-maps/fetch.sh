#!/usr/bin/env bash
# Prepare the offline map stack of one country, once, while the machine is online:
#   - the map libraries of the page, pinned and checked by sha256 (MapLibre GL JS, pmtiles.js, the Protomaps style),
#     the fonts and icons of the style (pinned by commit);
#   - the base map of the country: a PMTiles extract of a daily Protomaps build (OpenStreetMap data, ODbL);
#   - the OpenStreetMap extract of the country from Geofabrik, for Valhalla to build its routing tiles from.
# Nothing here is run by the stack itself: after this, `docker compose up` needs no network.
#
#   ./fetch.sh rs                      # a country of the table below
#   ./fetch.sh xx --bbox W,S,E,N --pbf europe/xx-latest.osm.pbf
#   BUILD=20261111 ./fetch.sh rs       # another Protomaps build (the builds of the last 60 days are kept)
set -euo pipefail
cd "$(dirname "$0")"

cc="${1:?usage: ./fetch.sh <country code> [--bbox W,S,E,N --pbf region/name-latest.osm.pbf] [--maxzoom 14]}"
shift
BUILD="${BUILD:-20261111}"
MAXZOOM=14
BBOX=""
PBF=""
case "$cc" in
  mc) BBOX=7.40,43.72,7.44,43.76; PBF=europe/monaco-latest.osm.pbf ;;
  rs) BBOX=18.81,42.23,23.01,46.19; PBF=europe/serbia-latest.osm.pbf ;;
  nl) BBOX=3.31,50.75,7.23,53.56; PBF=europe/netherlands-latest.osm.pbf ;;
  de) BBOX=5.86,47.27,15.04,55.06; PBF=europe/germany-latest.osm.pbf ;;
  es) BBOX=-9.39,35.94,4.33,43.79; PBF=europe/spain-latest.osm.pbf ;;
  it) BBOX=6.62,35.49,18.52,47.09; PBF=europe/italy-latest.osm.pbf ;;
  fr) BBOX=-5.14,41.33,9.56,51.09; PBF=europe/france-latest.osm.pbf ;;
esac
while [ $# -gt 0 ]; do
  case "$1" in
    --bbox) BBOX="$2"; shift 2 ;;
    --pbf) PBF="$2"; shift 2 ;;
    --maxzoom) MAXZOOM="$2"; shift 2 ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
[ -n "$BBOX" ] && [ -n "$PBF" ] || { echo "no box for '$cc': give --bbox W,S,E,N and --pbf region/name-latest.osm.pbf" >&2; exit 2; }

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

# get URL SHA256 DEST: download, refuse anything whose sum is not the pinned one
get() {
  curl -fsSL --retry 3 -o "$3.part" "$1"
  local got; got="$(sha256 "$3.part")"
  if [ "$got" != "$2" ]; then
    echo "sha256 of $1 is $got, expected $2: not used" >&2
    mv "$3.part" "$3.rejected"
    exit 1
  fi
  mv "$3.part" "$3"
}

mkdir -p data/web/lib data/web/fonts data/web/sprites data/valhalla data/bin

echo "== map libraries"
get https://unpkg.com/maplibre-gl@5.24.0/dist/maplibre-gl.js 45a9b07a9189ce56054c620a947ccf41e291e58c95e9b61533b740aaa65ee5cb data/web/lib/maplibre-gl.js
get https://unpkg.com/maplibre-gl@5.24.0/dist/maplibre-gl.css ab1e70d59ec40465bae7e7030da2f3ccf28133fd502e62bd598eefbadfd7a732 data/web/lib/maplibre-gl.css
get https://unpkg.com/pmtiles@4.5.0/dist/pmtiles.js caf981bc46f6327ee7e65d5dc964d89d38a69f60edca2bd4c5c890c21b554c6c data/web/lib/pmtiles.js
get https://unpkg.com/@protomaps/basemaps@5.7.2/dist/basemaps.js 551864f01da1bfe3005e99987197f22f56d0c35f9a10fa1727a889e2434af16d data/web/lib/basemaps.js

echo "== fonts and icons of the style (protomaps/basemaps-assets at a fixed commit)"
ASSETS=https://raw.githubusercontent.com/protomaps/basemaps-assets/028c18f713baecad011301ff7a69acc39bcc2ae7
for font in "Noto Sans Regular" "Noto Sans Medium" "Noto Sans Italic"; do
  mkdir -p "data/web/fonts/$font"
  # Latin, Latin extended, Greek and Cyrillic, punctuation: the scripts of the countries of the sheets
  for range in 0-255 256-511 512-767 768-1023 1024-1279 8192-8447; do
    curl -fsSL --retry 3 -o "data/web/fonts/$font/$range.pbf" "$ASSETS/fonts/${font// /%20}/$range.pbf"
  done
done
for f in light.json light.png light@2x.json light@2x.png; do
  curl -fsSL --retry 3 -o "data/web/sprites/$f" "$ASSETS/sprites/v4/$f"
done

echo "== pmtiles command (protomaps/go-pmtiles 1.31.2)"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) asset=go-pmtiles-1.31.2_Darwin_arm64.zip; sum=40528f7f616fcbf91207cd48c8fc023d213f6d86c0cbf1f748732803d1880f3d ;;
  Darwin-x86_64) asset=go-pmtiles-1.31.2_Darwin_x86_64.zip; sum=1f0dc02eee6c58312dd6c509faee1b5c32f0596568af1bf51f1b034e7a88a65b ;;
  Linux-aarch64|Linux-arm64) asset=go-pmtiles_1.31.2_Linux_arm64.tar.gz; sum=f8bd47e7ea866863489cad588fbaf2f31f42e5821f7a03f009b3769f05801cb1 ;;
  Linux-x86_64) asset=go-pmtiles_1.31.2_Linux_x86_64.tar.gz; sum=3ed7dbf4ec2e6dfe5e25b6f70d1ffc932729f93c86db353bf514dd71010a312f ;;
  *) echo "no pmtiles build for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
if [ ! -x data/bin/pmtiles ]; then
  get "https://github.com/protomaps/go-pmtiles/releases/download/v1.31.2/$asset" "$sum" "data/bin/$asset"
  case "$asset" in
    *.zip) unzip -oq "data/bin/$asset" pmtiles -d data/bin ;;
    *) tar -xzf "data/bin/$asset" -C data/bin pmtiles ;;
  esac
fi

echo "== base map of $cc: box $BBOX, zoom 0-$MAXZOOM, Protomaps build $BUILD"
data/bin/pmtiles extract "https://build.protomaps.com/$BUILD.pmtiles" data/web/basemap.pmtiles \
  --bbox="$BBOX" --maxzoom="$MAXZOOM"

echo "== OpenStreetMap extract for routing: $PBF"
name="$(basename "$PBF")"
curl -fsSL --retry 3 -o "data/valhalla/$name.part" "https://download.geofabrik.de/$PBF"
curl -fsSL --retry 3 -o "data/valhalla/$name.md5" "https://download.geofabrik.de/$PBF.md5"
want="$(cut -d' ' -f1 "data/valhalla/$name.md5")"
if command -v md5sum >/dev/null; then got="$(md5sum "data/valhalla/$name.part" | cut -d' ' -f1)"; else got="$(md5 -q "data/valhalla/$name.part")"; fi
[ "$got" = "$want" ] || { echo "md5 of $name is $got, Geofabrik says $want" >&2; exit 1; }
mv "data/valhalla/$name.part" "data/valhalla/$name"

IFS=, read -r W S E N <<<"$BBOX"
printf '{"country":"%s","bbox":[%s,%s,%s,%s],"build":"%s","maxzoom":%s}\n' "$cc" "$W" "$S" "$E" "$N" "$BUILD" "$MAXZOOM" \
  > data/web/meta.json

echo "== done"
du -sh data/web data/valhalla
