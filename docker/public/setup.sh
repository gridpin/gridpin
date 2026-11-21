#!/bin/sh
# Make the key, write .env, check the memory against the sheet and start GridPin behind Caddy.
#   ./setup.sh [country] [domain]      e.g. ./setup.sh rs geo.example.com
# The key is printed once: it opens this geocoding server and nothing else. Keep it with your secrets.
set -eu
cd "$(dirname "$0")"
country=${1:-}
domain=${2:-}
[ -n "$country" ] || { printf 'Country code of the sheet (e.g. us, de, rs): '; read -r country; }
[ -n "$domain" ] || { printf 'Domain that points at this machine (e.g. geo.example.com): '; read -r domain; }
case "$country" in *[!a-z]*|"") echo "setup: the country is a two-letter code such as us" >&2; exit 2;; esac
case "$domain" in *[!A-Za-z0-9.-]*|"") echo "setup: the domain has letters, digits, dots and dashes only" >&2; exit 2;; esac

# memory: the sheet stays in memory, the server needs about its size and some room (United States: 1.45 GB sheet,
# 1.7 GB process). Megabytes of each sheet: https://gridpin.dev/docs#sheets
need_mb=$(awk -v c="$country" '$1 == c {print $2}' sheet-memory.txt)
have_mb=$(awk '/MemTotal/ {print int($2 / 1024)}' /proc/meminfo 2>/dev/null || echo 0)
if [ -n "$need_mb" ] && [ "$have_mb" -gt 0 ] && [ "$have_mb" -lt "$need_mb" ]; then
	echo "setup: the $country sheet wants about $need_mb MB of memory, this machine has $have_mb MB" >&2
	exit 3
fi

umask 077
if [ ! -s gridpin.key ]; then
	# 32 random bytes as 43 characters of base64url: no shell or URL needs quoting around it
	head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n' > gridpin.key
	if [ -t 1 ]; then
		echo "Your key (shown once; it is also in $(pwd)/gridpin.key, readable by you only):"
		cat gridpin.key; echo
	else
		# not a terminal (cloud-init writes this output to a log): the key stays in its file only
		echo "Your key is in $(pwd)/gridpin.key, readable by its owner only."
	fi
fi
printf 'GRIDPIN_COUNTRY=%s\nGRIDPIN_DOMAIN=%s\n' "$country" "$domain" > .env
docker compose up -d
echo "Ask it:  curl -H \"Authorization: Bearer \$(cat gridpin.key)\" https://$domain/v1/geocode?q=..."
