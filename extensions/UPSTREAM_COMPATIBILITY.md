# Bundled client compatibility review

Reviewed 2026-11-23. These are explicit Endpoint adaptations, not defaults for
ordinary OpenAI-compatible Providers. See `GATEWAY_BEHAVIORS.md` for the contract.

## CodeBuddy / WorkBuddy CN

- Source: npm `@tencent-ai/codebuddy-code@2.185.0` (`dist/codebuddy.js`), also the
  locally installed Homebrew CLI version. This Endpoint imitates the **CLI**,
  not the VS Code extension or WorkBuddy desktop application.
- Identity advanced from 2.179.3 to 2.185.0 in both User-Agent segments and
  `X-IDE-Version`. The inspected client still uses API-key bearer authentication,
  `anonymous_<last-eight-key-characters>`, conversation/correlation headers,
  agent intent/type/runtime, and hyphenless per-message UUIDs. No additional
  mandatory inference-header change was established.
- Catalog source: live `https://copilot.tencent.com/v3/config`, requested with
  the 2.185.0 CLI API-key identity. A deliberately non-secret placeholder key
  received `code: 0` and the `cli` agent advertisement; no working credential,
  inference request, or paid model invocation was needed. Membership is not
  proof that an account can invoke a model. The full `models` array also lists
  legacy/image/video entries; those are not copied into this chat catalog.
- Added `hy4flash`, `hy4flash-free`; removed `hy3`, `hy3-x`, `hy4free`. Kept the
  previously verified `auto` selector separately from the CLI advertisement.
- Streaming-only operation and the previously verified CN `developer` →
  `system` compatibility adaptation remain. They were not live-retested here.
- The inspected CLI also contains error-triggered feature stripping/retry logic.
  Yabane deliberately does not import it: one request is sent once and Provider
  failures remain visible, without silently removing reasoning or image input.

## OpenAI Codex subscription (legacy)

- Source: npm `@earendil-works/pi-ai@1.6.0`, matching the managed local pi install:
  `dist/providers/data/openai-codex.json`, `dist/api/openai-codex-responses.js`,
  `dist/utils/identity.js`, and `dist/auth/oauth/openai-codex.js`.
- Added `gpt-6.1-terra`, `gpt-6.2-astra`, `gpt-6.2-sol`; removed
  `gpt-5.3-codex-spark`, `gpt-5.5`. The separate `openai` provider catalog is
  independent and is not substituted for `openai-codex`.
- pi no longer supplies top-level default `instructions`: it sends system
  guidance as a leading developer input message, noting that GPT-6.2 appends
  `instructions` after input and breaks prefix caching. Yabane now changes only
  ordinary system-message roles to developer in place, retaining all content,
  order and extra fields. Explicit caller `instructions` remain unchanged;
  they are not silently relocated or discarded. A caller using that field
  therefore still opts into its Provider-defined ordering/caching behavior.
- pi now sends `max_output_tokens` (where its metadata permits), `temperature`
  and `service_tier`. Yabane removes its stale blanket exclusions of these
  fields and forwards exact caller values, including limits below pi's client
  minimum of 16; it does not infer capabilities from model names or clamp limits.
  Previously verified exclusions of other unsupported fields remain.
- pi's newer default User-Agent includes its version and actual Node/Bun
  runtime. Yabane chooses pi's supported **short identity**, `User-Agent: pi`,
  rather than claiming a JavaScript runtime. `originator: pi`, account ID,
  bearer authentication, SSE beta headers and cache affinity remain.
- Inline developer updates, additional tools, tool search and
  `configuration_update` items are preserved rather than generated. pi's
  per-model tool/reasoning/effort choices remain the caller's responsibility.
- pi defaults to automatic WebSocket/cached-continuation transport and can fall
  back to HTTP SSE; its SSE path can zstd-compress requests. Yabane remains a
  single HTTP/SSE exchange with plain JSON, no retries, cached transcript,
  WebSocket fallback, or inferred continuation. Native SSE bytes and Provider
  errors remain transparent.
- Browser client ID, PKCE, scopes, callback URL, token exchange/refresh and device
  authorization URLs still match. pi's device polling additionally recognizes
  body-level `deviceauth_authorization_pending` and `slow_down` errors;
  Yabane currently recognizes pending HTTP 403/404 only and surfaces other
  statuses as failures. This remains a known gap, not silently claimed parity.
- Verification here uses source comparison and local automated fixtures, not
  live ChatGPT authorization or inference. The independent OpenAI sign-in
  Endpoint and its existing identity/catalog were outside this update.
