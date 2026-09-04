# Yabane Gateway Behavior Checklist

This file is the source checklist for end-to-end gateway behavior. Every item begins with `*` so it can be converted into automated E2E cases.

## Service and administration

* Yabane listens on `YABANE_ADDR`, defaulting to `127.0.0.1:8080`.
* `GET /healthz` returns `200` and does not require a gateway API key.
* The web admin console and `/admin/*` APIs do not require a gateway API key.
* Provider and authentication configuration is persisted under `data/` and is restored after restart.
* The Providers top-level page presents a concise provider list; selecting one provider opens its detailed endpoints, upstream keys, and model-discovery controls.
* The sidebar selection background and blue indicator animate when switching top-level sections.
* Settings search finds top-level settings, provider-model discovery, gateway-key generation, and configured providers, then navigates to the selected result.
* Boolean settings use an accessible animated switch instead of the browser's default checkbox presentation.
* Forms visibly identify required and optional fields.
* Provider-scope selection uses individually labeled checkboxes and an empty selection clearly means unrestricted access.
* Secret-entry fields use the same Show/Hide interaction throughout the admin console.
* Upstream-key traffic weights explain relative distribution and provide an immediately visible slider value and common presets.
* Missing configuration files produce empty/default configuration; malformed or unreadable configuration fails startup visibly.
* `YABANE_LOG` controls logging, defaults to `info`, and an invalid filter fails startup.

## Gateway authentication

* Gateway API-key authentication is enabled by default.
* An administrator can enable or disable gateway API-key authentication globally.
* When authentication is disabled, `/v1/*` requests do not require `Authorization`.
* When authentication is enabled, every `/v1/*` request without an `Authorization: Bearer <key>` header returns `401`.
* When authentication is enabled, a malformed authorization header returns `401`.
* When authentication is enabled, an unknown or deleted gateway API key returns `401`.
* An expired gateway API key returns `401`.
* A valid non-expired gateway API key authorizes requests within its provider scope.
* A gateway API key with an empty provider scope can access every configured provider.
* A provider-scoped gateway API key receives `403` when an inference request selects a provider outside its scope.
* `GET /v1/models` returns only models belonging to providers allowed by the gateway API key.
* Generated gateway API keys begin with `sk-` and use cryptographically secure random bytes.
* A generated gateway API-key secret is returned only by the create operation and is never returned by list operations.
* The one-time generated-key dialog provides an explicit Copy API key action, confirms a successful clipboard copy, and selects the secret for manual copying if clipboard access is blocked.
* Gateway API-key secrets are persisted as SHA-256 hashes, not plaintext.
* Gateway API keys support an optional note, optional expiration time, and optional provider allowlist.
* Creating a gateway API key with an expiration time in the past is rejected with `400`.
* Creating a gateway API key with an unknown provider in its allowlist is rejected with `400`.
* Deleting a gateway API key revokes it immediately.

## Provider routing and proxying

* Inference requests select a provider from the `provider/model` request model ID.
* Only the first model path segment is removed; the remaining namespaced upstream model ID is preserved.
* A model without a provider prefix is rejected with `400`.
* An unknown provider prefix is rejected with `400`.
* A provider without an endpoint matching the requested protocol is rejected with `400`.
* OpenAI Chat Completions requests are accepted only at `POST /v1/chat/completions`.
* OpenAI Responses requests are accepted only at `POST /v1/responses`.
* Anthropic Messages requests are accepted only at `POST /v1/messages`.
* Request fields other than the provider prefix in `model` are forwarded without semantic rewriting.
* Upstream status codes and response bodies are visible to the caller.
* Upstream response bodies, including SSE, are streamed without full response buffering.
* Multiple inference requests can execute concurrently and reuse pooled upstream connections.
* Upstream connection failures return `502` without exposing credentials.
* Request bodies larger than 32 MiB are rejected.

## Upstream endpoints and keys

* A provider can contain multiple API endpoints.
* An endpoint can require an upstream API key or operate without one.
* An endpoint requiring a key cannot proxy when it has no enabled positive-weight key.
* An endpoint can contain multiple independently enabled upstream API keys.
* Default upstream key selection performs deterministic weighted round robin across enabled positive-weight keys.
* Disabled upstream keys receive no default traffic.
* OpenAI-compatible upstream credentials are sent as `Authorization: Bearer <key>`.
* Anthropic upstream credentials are sent as `x-api-key: <key>`.
* The caller's authorization headers are never forwarded to the upstream provider.

## Model-specific routing

* A model route can be an exact model ID or one trailing prefix wildcard.
* Exact model routes take precedence over wildcard routes.
* The longest matching wildcard prefix takes precedence over shorter prefixes.
* If no model route matches, weighted key selection is used.
* A model route explicitly selects both an endpoint and one enabled upstream API key.
* Routes referring to deleted keys are removed when the key is deleted.

## Model discovery

* Creating a provider automatically starts model discovery without blocking provider creation.
* An administrator can explicitly refresh models for one provider or all providers.
* The console distinguishes discovery not yet run, successful empty results, and discovery failures.
* Large provider model collections are collapsed by default and can be searched in a bounded model browser instead of rendering an unbounded wall of model labels.
* Successful model discovery and the latest discovery status are persisted for the admin console.
* `GET /v1/models` returns an OpenAI-compatible list envelope.
* Model discovery concurrently queries configured provider endpoints and enabled upstream keys.
* OpenAI-compatible discovery accepts both `{ "data": [...] }` and a top-level model array.
* Anthropic discovery uses the Anthropic authentication and version headers.
* Models are merged, deduplicated by ID, and sorted.
* Returned model IDs use the `provider/model` form without duplicating an existing provider prefix.
* Failure to discover models from one provider does not hide successfully discovered models from other providers.
* Discovered models are reference data and do not create locally managed model definitions.

## Admin data safety

* Admin provider responses never expose upstream API-key secrets.
* Admin gateway API-key list responses never expose generated secrets or secret hashes.
* Technical identifiers, URLs, credentials, and model patterns use monospace presentation in the console.
