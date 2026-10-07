#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"
case "${1:-}" in
  '') full=false ;;
  --full) full=true ;;
  --load) full=false; load=true ;;
  *) echo 'Usage: bash tests/check.sh [--full|--load]' >&2; exit 2 ;;
esac
if [[ $# -gt 1 ]]; then echo 'Usage: bash tests/check.sh [--full|--load]' >&2; exit 2; fi
load=${load:-false}

rust_version=$(rustc --version | awk '{print $2}')
cargo fmt --all -- --check
node --check web/app.js
for script in tests/*.mjs; do node --check "$script"; done
bash -n tests/e2e.sh tests/behavior-map.sh
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked

# Check all remaining subsets of the three independently optional Extensions.
# Keep Core tests active here: adding a dev dependency or disabling those tests
# would hide accidental coupling to a bundled implementation.
for features in \
  '' \
  'extension-request-defaults' \
  'extension-traffic-capture' \
  'extension-openai-subscription' \
  'extension-request-defaults,extension-traffic-capture' \
  'extension-request-defaults,extension-openai-subscription' \
  'extension-traffic-capture,extension-openai-subscription'; do
  printf '\nChecking Extension subset: %s\n' "${features:-Core only}"
  cargo clippy -p yabane --all-targets --no-default-features --features "$features" --locked -- -D warnings
  cargo test -p yabane --no-default-features --features "$features" --locked
done

# Rebuild after feature-matrix checks so HTTP tests use the default distribution
# and the current embedded assets, never a previous Core-only binary.
cargo build --locked
node tests/openapi-paths.mjs
node tests/cooldown-copy.mjs
node tests/cooldown-history.mjs "$repo/target/debug/yabane"
node tests/http-redirects.mjs "$repo/target/debug/yabane"
node tests/reasoning-conversion.mjs "$repo/target/debug/yabane"
node tests/stream-semantics.mjs "$repo/target/debug/yabane"
node tests/activity-recovery.mjs "$repo/target/debug/yabane"
node tests/client-disconnect.mjs "$repo/target/debug/yabane"
node tests/response-limits.mjs "$repo/target/debug/yabane"
node tests/shutdown.mjs "$repo/target/debug/yabane"
if [[ $full == true ]]; then
  bash tests/e2e.sh "$repo/target/debug/yabane"
fi
if command -v cargo-audit >/dev/null; then
  cargo audit
else
  printf 'Skipping cargo audit: cargo-audit is not installed.\n'
fi
if [[ $load == true ]]; then
  node tests/load.mjs
fi
printf '\nYabane quality checks passed (rustc %s; full E2E: %s; load: %s).\n' "$rust_version" "$full" "$load"
if [[ $full == false ]]; then
  printf 'Offline gate only: browser and authentication E2E was not run. Use `bash tests/check.sh --full` or `bash tests/e2e.sh` before treating UI- or auth-visible changes as verified.\n'
fi
