#!/bin/bash
# Runs the plugin tests headless inside the official QGIS images: the LTR and the current release.
# Tags are pinned versions, not ltr/latest (latest is the development branch).
#   bash integrations/qgis/tests/run_in_docker.sh [TAG ...]
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
sheet=$(cd "$here/../.." && pwd)/data/mc.bin
for tag in "${@:-3.44.15 4.2.3}"; do
  for t in $tag; do
    echo "== qgis/qgis:$t"
    docker run --rm --platform linux/amd64 -v "$here:/plugin:ro" -v "$sheet:/sheet/mc.bin:ro" \
      -e PYTHONDONTWRITEBYTECODE=1 qgis/qgis:"$t" bash -c 'set -e
        python3 -m pip install -q --break-system-packages --target /tmp/dep -r /plugin/requirements-test.txt 2>&1 | grep -v "root.*user" || true
        cd /tmp && PYTHONPATH=/tmp/dep:/usr/share/qgis/python/plugins:/usr/share/qgis/python:${PYTHONPATH:-} python3 -m pytest -q -p no:cacheprovider /plugin/tests'
  done
done
