#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"
case "${1:-}" in
  '') full=false ;;
  --full) full=true ;;
  --load) full=false; load=true ;;
  --quick) full=false; quick=true ;;
  --matrix) full=false; matrix=true ;;
  *) echo 'Usage: bash tests/check.sh [--quick|--full|--load|--matrix]' >&2; exit 2 ;;
esac
if [[ $# -gt 1 ]]; then echo 'Usage: bash tests/check.sh [--quick|--full|--load|--matrix]' >&2; exit 2; fi
load=${load:-false}
quick=${quick:-false}
matrix=${matrix:-false}
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
# Explicit quick gate: all workspace tests once, no feature matrix or HTTP/UI.
# It is not a substitute for the default gate or --full before release.
if [[ $quick == true ]]; then
  printf 'Quick gate passed in %ss; feature matrix, HTTP and browser tests were NOT run.\n' "$((SECONDS - started))"
  exit 0
fi

# Core-only runs every Core test too: no dev dependency on bundled Extensions.
# Other builds still lint/compile all tests, but run feature-sensitive modules
# rather than repeating unrelated protocol/Activity/storage tests seventeen times.
# Default: singles and pairs + Core/all. --matrix and --full: all 32 subsets.
subsets=( \
  '' \
  'extension-request-defaults' \
  'extension-traffic-capture' \
  'extension-openai-subscription' \
  'extension-openai' \
  'extension-codebuddy' \
  'extension-request-defaults,extension-traffic-capture' \
  'extension-request-defaults,extension-openai-subscription' \
  'extension-request-defaults,extension-openai' \
  'extension-request-defaults,extension-codebuddy' \
  'extension-traffic-capture,extension-openai-subscription' \
  'extension-traffic-capture,extension-openai' \
  'extension-traffic-capture,extension-codebuddy' \
  'extension-openai-subscription,extension-openai' \
  'extension-openai-subscription,extension-codebuddy' \
  'extension-openai,extension-codebuddy' )
if [[ $matrix == true || $full == true ]]; then
  subsets=()
  extensions=(extension-request-defaults extension-traffic-capture extension-openai-subscription extension-openai extension-codebuddy)
  # All-features was tested above; enumerate the other 31 bitsets.
  for ((mask=0; mask<31; mask++)); do
    features=''
    for ((bit=0; bit<5; bit++)); do
      if ((mask & (1 << bit))); then
        features+="${features:+,}${extensions[bit]}"
      fi
    done
    subsets+=("$features")
  done
fi
for features in "${subsets[@]}"; do
  printf '\nChecking Extension subset: %s\n' "${features:-Core only}"
  stage "lint:${features:-core}" cargo clippy -p yabane --all-targets --no-default-features --features "$features" --locked -- -D warnings
  if [[ -z $features ]]; then
    stage core-tests cargo test -p yabane --no-default-features --locked
  else
    # Runtime surfaces conditional on Extension availability; ordinary protocol,
    # durability, limits and auth tests already ran in all- and no-feature builds.
    stage "feature-tests:$features" cargo test -p yabane --no-default-features --features "$features" --locked -- \
      extensions:: config:: control:: gateway:: endpoint_signin:: models::
  fi
done

# Rebuild after feature-matrix checks so HTTP tests use the default distribution
# and the current embedded assets, never a previous Core-only binary.
stage default-build cargo build --locked
for script in openapi-paths cooldown-copy cooldown-layout activity-outcomes; do
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
