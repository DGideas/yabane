#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"
case "${1:-}" in
  '') full=false ;;
  --full) full=true ;;
  --load) full=false; load=true ;;
  --quick) full=false; quick=true ;;
  *) echo 'Usage: bash tests/check.sh [--quick|--full|--load]' >&2; exit 2 ;;
esac
if [[ $# -gt 1 ]]; then echo 'Usage: bash tests/check.sh [--quick|--full|--load]' >&2; exit 2; fi
load=${load:-false}
quick=${quick:-false}
started=$SECONDS
stage() {
  local name=$1 start=$SECONDS status=0
  shift
  "$@" || status=$?
  printf 'TIMING %s: %ss (exit %s)\n' "$name" "$((SECONDS - start))" "$status"
  return "$status"
}

rust_version=$(rustc --version | awk '{print $2}')
cargo fmt --all -- --check
node --check web/app.js
for script in tests/*.mjs; do node --check "$script"; done
bash -n tests/check.sh tests/e2e.sh tests/behavior-map.sh
node tests/check-gate.mjs
stage build-identity node tests/build-identity.mjs
stage workspace-lint cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
stage workspace-tests cargo test --workspace --all-features --locked
# Explicit quick gate: all workspace tests once, no process-level HTTP or UI checks.
# It is not a substitute for the default gate or --full before release.
if [[ $quick == true ]]; then
  printf 'Quick gate passed in %ss; HTTP and browser tests were NOT run.\n' "$((SECONDS - started))"
  exit 0
fi

# Rebuild before process-level checks so HTTP tests use the default distribution
# and the current embedded assets, never a stale binary.
stage default-build cargo build --locked
for script in openapi-paths cooldown-copy cooldown-layout activity-outcomes activity-date-axis; do
  stage "$script" node "tests/$script.mjs"
done
for script in cooldown-history http-redirects reasoning-conversion stream-semantics activity-recovery client-disconnect stream-completion response-limits shutdown; do
  stage "$script" node "tests/$script.mjs" "$repo/target/debug/yabane"
done
if [[ $full == true ]]; then
  stage accessibility node tests/accessibility.mjs "$repo/target/debug/yabane"
  stage browser-auth bash tests/e2e.sh "$repo/target/debug/yabane"
fi
if command -v cargo-audit >/dev/null; then
  cargo audit
else
  printf 'Skipping cargo audit: cargo-audit is not installed.\n'
fi
if [[ $load == true ]]; then
  node tests/load.mjs
fi
printf '\nYabane quality checks passed in %ss (rustc %s; full E2E: %s; load: %s).\n' "$((SECONDS - started))" "$rust_version" "$full" "$load"
if [[ $full == false ]]; then
  printf 'Offline gate only: browser and authentication E2E was not run. Use `bash tests/check.sh --full` or `bash tests/e2e.sh` before treating UI- or auth-visible changes as verified.\n'
fi
