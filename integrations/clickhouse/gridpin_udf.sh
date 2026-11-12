#!/bin/sh
# The process behind the ClickHouse function geocode(address): one address per line on stdin (TabSeparated), one
# line back per address: the JSON of the top answer, or an empty line. Set the sheet with GRIDPIN_SHEET.
exec gridpin batch "${GRIDPIN_SHEET:-/var/lib/gridpin/us.bin}" - - --lines
