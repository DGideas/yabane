# Yabane Agent Instructions

## Behavior contract

* Treat `GATEWAY_BEHAVIORS.md` as Yabane's authoritative end-to-end behavior checklist.
* Read it before changing gateway, authentication, routing, provider, model-discovery, persistence, or admin-console behavior.
* Update it in the same change whenever observable behavior is added, removed, or altered. Every behavior entry must begin with `*` and describe externally observable outcomes rather than implementation details.
* Add or update automated tests for affected checklist entries. Run `tests/e2e.sh target/debug/yabane` when the change affects executable gateway behavior.
* During reviews and maintenance, compare implementation and tests with the checklist; report or fix stale, missing, and contradictory entries instead of silently leaving drift.

## Project structure

* Keep protocol, authentication, model discovery, provider administration, and static web serving in focused modules; do not accumulate new unrelated responsibilities in `src/main.rs`.
* Preserve Yabane's transparent routing rules: no model capability guessing, hidden parameter rewriting, arbitrary provider fallback, or credential exposure.
