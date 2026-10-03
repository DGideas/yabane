#!/usr/bin/env bash
# Reports which GATEWAY_BEHAVIORS.md rules are named by a test or by the code, so
# the gap between the behavior contract and its automated coverage stays visible.
# This is a report, not a gate: it always exits 0.
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"
behaviors=GATEWAY_BEHAVIORS.md
[[ -f $behaviors ]] || { echo "$behaviors not found" >&2; exit 1; }

ids=$(grep -oE '\*\*[A-Z]+-[0-9]+\*\*' "$behaviors" | tr -d '*' | sort -u)
total=$(printf '%s\n' "$ids" | wc -l | tr -d ' ')
named=0
unnamed=()
while IFS= read -r id; do
  [[ -n $id ]] || continue
  if grep -rqF "$id" tests src crates extensions README.md 2>/dev/null; then
    named=$((named + 1))
  else
    unnamed+=("$id")
  fi
done <<<"$ids"

printf 'Behavior coverage: %s/%s rules are named by a test, the source, or the README.\n' "$named" "$total"
if [[ ${#unnamed[@]} -gt 0 ]]; then
  printf '\nUnnamed rules (%s):\n' "${#unnamed[@]}"
  printf '%s\n' "${unnamed[@]}" | sed 's/^/  /'
fi
