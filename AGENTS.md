# Yabane Agent Instructions

* Read `GATEWAY_BEHAVIORS.md` before changing externally observable behavior. It is the authoritative end-to-end checklist; update it and its automated coverage in the same change, and report contradictory or stale entries instead of leaving drift.
* Preserve transparent proxying: do not infer capabilities from model names, normalize parameters without explicit configuration, choose an arbitrary fallback Provider, hide upstream failures, or expose caller/upstream credentials.
* Keep the three protocol surfaces independent: OpenAI Chat Completions, OpenAI Responses, and Anthropic Messages. Any future cross-protocol conversion must be explicit and independently tested.
* Preserve the configured resource hierarchy in the console and APIs: Provider-wide settings → Endpoint → upstream keys. A key belongs to one Endpoint; Endpoint overrides take precedence over applicable Provider defaults.
* Provider-facing model IDs remove only Yabane's first `provider/` segment. Preserve the remainder exactly, including native upstream namespaces such as `google/model-name`.
* Activity records routing metadata and usage only; never persist prompt or response content.
* Web assets are embedded with `include_str!` and `include_bytes!`; rebuild the binary before browser or E2E verification, otherwise tests may exercise stale UI resources.
