#!/bin/sh
# GridPin on a clean Linux machine, in one command (as root, Debian 12+ or Ubuntu 22.04+):
#   curl -fsSL https://raw.githubusercontent.com/gridpin/gridpin/v0.10.0/docker/public/install.sh | sh -s -- us geo.example.com
# Installs Docker from the distribution's own repository, takes the files of docker/public at the release tag
# below, checks each against its SHA-256, and runs setup.sh (key, memory check, GridPin behind Caddy).
# Nothing else is downloaded; the files go to /opt/gridpin.
set -eu
REF=${GRIDPIN_REF:-v0.10.0}
BASE=${GRIDPIN_BASE:-https://raw.githubusercontent.com/gridpin/gridpin/$REF/docker/public}
DIR=${GRIDPIN_DIR:-/opt/gridpin}
# SHA-256 of the files at $REF (tools/test_docker_public.py holds them equal to the tree)
FILES='
76fa660d91fab602a83e0a7e4e2cbf232d8cddd2a744aa166680227cd4d6264d compose.yaml
58b9ba62db38eccb6d08d62efa017c771c713bf74f47bb29a4427fc638335bcd Caddyfile
cea1a6ffbd6d18710f51e6459abcafe60a7bbeec7189ea436cb47e56c88b222f setup.sh
f5525b3b4548ba9628731c032897095348e919bb36a56711c8db87e5d1449bd8 sheet-memory.txt
'

[ "$(id -u)" = 0 ] || { echo "install: run as root (sudo sh install.sh ...)" >&2; exit 2; }
if ! docker compose version >/dev/null 2>&1; then
	if command -v apt-get >/dev/null 2>&1; then
		export DEBIAN_FRONTEND=noninteractive
		apt-get update -q
		apt-get install -y -q docker.io curl ca-certificates
		# the Compose plugin: docker-compose-v2 on Ubuntu, docker-compose (v2) on Debian 13
		apt-get install -y -q docker-compose-v2 2>/dev/null || apt-get install -y -q docker-compose
	else
		echo "install: no apt-get here; install Docker with Compose, then run setup.sh from $BASE" >&2
		exit 3
	fi
fi
docker compose version >/dev/null 2>&1 || { echo "install: docker compose is missing after installing Docker" >&2; exit 3; }

mkdir -p "$DIR"
cd "$DIR"
echo "$FILES" | while read -r sum name; do
	[ -n "$name" ] || continue
	curl -fsSL "$BASE/$name" -o "$name.part"
	got=$(sha256sum "$name.part" | cut -d' ' -f1)
	[ "$got" = "$sum" ] || { echo "install: $name does not match its SHA-256 ($got)" >&2; exit 4; }
	mv "$name.part" "$name"
done
chmod +x setup.sh
./setup.sh "$@"
