#!/usr/bin/env bash
# Single source of truth for DCO Signed-off-by verification. Run by BOTH the release publish gate
# (tools/export_public.py -> check_public_history) AND the `dco` GitHub workflow
# (.github/workflows/dco.yml). Keeping one script means the local gate and CI cannot drift into
# accepting what the other rejects.
#
# Usage:  dco_verify.sh <repo-dir> [<rev-range>]        (default range: --all)
# Exit 0  every commit in range carries a valid Signed-off-by trailer whose value equals the
#         commit's own author OR committer identity.
# Exit 1  at least one problem, OR git itself failed. Fail-closed: an unreadable repo or a
#         rev-list error is a NON-ZERO exit, never a silently-empty "nothing to check" success.
set -euo pipefail

# Ignore replacement objects: DCO must read the REAL objects that get pushed, not ones locally
# swapped via `git replace` (same OID, different bytes/signatures).
export GIT_NO_REPLACE_OBJECTS=1

repo="${1:?usage: dco_verify.sh <repo-dir> [rev-range]}"
range="${2:---all}"

# An inherited GIT_DIR / GIT_WORK_TREE (etc.) would make `git -C repo` read history from a
# DIFFERENT repository while the intended target goes unchecked. Scrub every repo-redirecting
# variable so `git -C <repo>` resolves strictly inside <repo>.
for v in GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
         GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE GIT_CEILING_DIRECTORIES \
         GIT_DISCOVERY_ACROSS_FILESYSTEM; do
  unset "$v" 2>/dev/null || true
done

# Resolve the revision list FIRST into a variable with an explicit status check, THEN iterate.
# Iterating directly over an unchecked `$(git rev-list …)` would turn a rev-list error into an
# empty word list and still reach exit 0 even under `set -e`. Command substitution in an `if`
# condition lets the failure be observed and turned into a fail-closed exit.
if ! revs="$(git -C "$repo" rev-list $range 2>/dev/null)"; then
  echo "::error::git rev-list '$range' failed in '$repo' — DCO history unverifiable (fail-closed)" >&2
  exit 1
fi

# A valid Signed-off-by value is `Name <email>` with a non-empty, whitespace/@-free email — not
# name-only, not a whitespace/empty email. And at least one such trailer must equal the commit's
# OWN author or committer identity: the DCO is a self-certification, so a sign-off in someone
# else's name is not a certification of this commit.
so_re='^.+<[^<>@[:space:]]+@[^<>@[:space:]]+>$'

bad=0
while IFS= read -r c; do
  [ -n "$c" ] || continue
  author="$(git -C "$repo" show -s --format='%an <%ae>' "$c")"
  committer="$(git -C "$repo" show -s --format='%cn <%ce>' "$c")"
  matched=0
  while IFS= read -r so; do
    so="${so#"${so%%[![:space:]]*}"}"   # ltrim
    so="${so%"${so##*[![:space:]]}"}"   # rtrim
    [ -n "$so" ] || continue
    printf '%s\n' "$so" | grep -Eq "$so_re" || continue      # not `Name <email>` — skip
    if [ "$so" = "$author" ] || [ "$so" = "$committer" ]; then
      matched=1
    fi
  done <<TRAILERS
$(git -C "$repo" show -s --format='%(trailers:key=Signed-off-by,valueonly,separator=%x0A)' "$c")
TRAILERS
  if [ "$matched" != 1 ]; then
    short="$(git -C "$repo" rev-parse --short "$c" 2>/dev/null || printf '%.8s' "$c")"
    echo "::error::commit $short has no valid Signed-off-by 'Name <email>' trailer matching its own author/committer identity (DCO — see CONTRIBUTING.md): git commit -s" >&2
    bad=1
  fi
done <<REVS
$revs
REVS
exit "$bad"
