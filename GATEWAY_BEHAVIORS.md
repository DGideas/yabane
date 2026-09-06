# Yabane Gateway Behavior Checklist

This file is the source checklist for end-to-end gateway behavior. Every item begins with `*` so it can be converted into automated E2E cases.

## Service and administration

* Yabane loads local environment configuration from `.env` when present while preserving variables already supplied by the process environment.
* `yabane --help` and `yabane --version` print help or Git commit/time build information and exit without loading configuration or starting the server; Yabane does not present an application version, and unknown or malformed command-line arguments fail visibly.
* Yabane listens on the address passed through `--addr`, defaulting to `127.0.0.1:8080`; the listen address is intentionally not configured through an environment variable.
* `GET /healthz` returns `200` and does not require a gateway API key.
* The web admin console uses a separate administrator session and never accepts gateway API keys as admin authentication.
* Admin HTML, CSS, JavaScript, and the Yabane SVG application icon are embedded in the binary and disable browser caching so a restarted local binary does not leave stale console resources.
* The console exposes the Yabane Git commit, commit time, and bundled MIT license information from a desktop sidebar footer and the account menu on narrow layouts; it does not present an application version. The About dialog uses an animated monochrome, faceted Yabane mark while respecting reduced-motion preferences.
* On first use, the console requires creation of the single administrator with username, email, and a password of at least eight characters.
* After setup, unauthenticated visitors see the login screen and can sign in with username or email plus password.
* Administrator passwords are persisted only as Argon2 password hashes.
* Successful setup or login creates an HttpOnly, SameSite=Strict session cookie with a 24-hour server-enforced lifetime; expired sessions are removed from memory, and logout revokes the current session.
* A Help button beside the top-right administrator avatar opens a getting-started guide that uses current Gateway URLs, discovered `provider/model` IDs, public route aliases, and Gateway API keys to generate copyable OpenCode, generic OpenAI-compatible Agent, and curl setup instructions without exposing upstream credentials.
* The top-right administrator avatar opens an account menu showing the signed-in username and email, with explicit Manage profile and Sign out actions.
* An administrator session can update its username and email from the profile dialog; changing the password additionally requires the correct current password and a new password of at least eight characters, and Gateway API keys cannot modify the administrator profile.
* Protected `/admin/*` APIs return `401` without a valid administrator session, while session, setup, login, static resources, and health remain public.
* Initial administrator setup does not require a CAPTCHA.
* Login requires a Cloudflare Turnstile token only when both a secret and a matching site key are available; the backend validates success, the `login` action, and an allowed hostname with Siteverify, while an unconfigured or incomplete Turnstile configuration disables CAPTCHA.
* The login console renders Turnstile with `TURNSTILE_SITE_KEY`; when the always-pass Turnstile test secret is configured and no site key is specified, it uses Cloudflare's matching test site key so local verification works without a production widget.
* Provider, gateway authentication, administrator, and model-route configuration is persisted under `data/`, restored after restart, and replaced through a synced temporary file plus rename so interrupted writes do not expose partial JSON.
* The Providers top-level page presents a concise provider list; selecting one provider opens a visually distinct Provider settings page with breadcrumb context, a Provider identity summary, Provider-wide settings, and a separate child-endpoint hierarchy.
* Provider details visually nest upstream API keys inside their owning Endpoint, and key-creation actions name and preselect that Endpoint so Provider, Endpoint, and key actions are not presented as peers.
* The sidebar selection background and blue indicator animate when switching top-level sections.
* Settings search finds top-level settings, provider-model discovery, gateway-key generation, and configured providers, visibly selects the active result, supports Arrow Up/Down, Home/End, Enter, and Escape from the combobox, then navigates to the selected result.
* Home, Providers, provider details, model routing, API access, Activity, and login have distinct browser URLs that can be opened directly and support browser Back/Forward navigation.
* Authenticated console pages use the full width available beside the sidebar with responsive horizontal gutters, rather than remaining capped to a narrow fixed content column on wide displays.
* Console dialogs remain contained within desktop, tablet, and iOS-sized visual viewports, use one-column controls where needed, provide touch-sized actions, account for safe-area insets, and keep headers and actions reachable while long content scrolls; responsive E2E runs desktop/tablet in Chrome and iPhone-sized layouts in WebKit.
* The authenticated Home page summarizes 24-hour requests and token usage and links to configured providers.
* Gateway API keys authenticate inference requests and model listing with provider scopes, while Management API keys authenticate the control API; administrator profile and Management-key endpoints require a browser session.
* Each administrator can create named Management API keys beginning with `yab_mgmt_`, with optional expiry; the secret is shown once, only its hash is persisted, last-use times are tracked, and revocation is immediate.
* The embedded live API reference at `/docs` renders the bundled OpenAPI specification served at `/openapi.json` and can execute requests against the running instance using the browser session or a pasted Bearer key.
* Boolean settings use accessible animated switches or check controls instead of the browser's default checkbox presentation, and conditionally revealed form fields animate their height and opacity instead of abruptly changing the dialog layout.
* Forms visibly identify required and optional fields.
* Provider-scope selection uses individually labeled checkboxes and an empty selection clearly means unrestricted access.
* Secret-entry fields use the same Show/Hide interaction throughout the admin console.
* Upstream-key traffic distribution is presented as percentages that must total 100% across enabled keys; newly added keys use the standard default weight, while the stored positive weights preserve deterministic weighted round robin.
* The Model routing page explains that rules bind matching models to a specific endpoint and upstream key, shows exact and prefix examples, and makes clear that unmatched models retain weighted load balancing.
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
* A generated gateway API-key secret is returned by the create operation and remains available to authenticated administrators in subsequent list operations.
* The generated-key dialog and API-key list provide Copy API key actions with a standard copy icon and visible success feedback.
* Gateway API-key secrets are persisted so the administrator can reveal or copy them later; their SHA-256 hashes are also retained for request authentication.
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
* Request fields other than the provider prefix in `model` are forwarded without semantic rewriting unless explicit Provider or Endpoint extra request-body fields are configured.
* Provider-level extra headers and JSON request-body fields can apply to every current and future endpoint or to an explicit non-empty selection of that Provider’s endpoints; endpoint-level values override matching provider-level values where the Provider defaults apply.
* The provider Request defaults editor uses structured header and body-field rows, offers all-endpoint and selected-endpoint scope controls, validates endpoint selection, duplicate names, HTTP header syntax, reserved authentication and hop-by-hop headers, and each body field’s JSON value before saving, and shows a live JSON body preview.
* Upstream status codes and response bodies are visible to the caller.
* Upstream response bodies, including SSE, are streamed without full response buffering.
* Proxying strips standard hop-by-hop request and response headers while preserving end-to-end upstream headers, status codes, and body bytes.
* Multiple inference requests can execute concurrently and reuse pooled upstream connections.
* Upstream connection failures return `502` without exposing credentials.
* Request bodies larger than 32 MiB are rejected.

## Upstream endpoints and keys

* A provider can contain multiple API endpoints, and the admin console can add, edit, or delete endpoints after provider creation; editing supports API type, base URL, SOCKS5 proxy, and whether credentials are required without exposing or replacing existing secrets or Endpoint request defaults.
* Endpoint IDs are immutable after creation because discovery metadata, model preferences, routes, credentials, and Activity all reference them.
* Changing or adding an Endpoint clears model availability and preferences tied to that Endpoint before persisting when applicable, then starts one background discovery refresh; the console does not issue a duplicate refresh.
* Deleting a Provider or Endpoint also removes every model-route target referring to that exact resource; routes left without targets are removed, and Endpoint deletion additionally removes its discovered-model availability and preferences.
* Model discovery records which endpoint exposes each model.
* A request for `provider/model` is sent to an endpoint that reported that model, hiding endpoint topology from the caller.
* Models unique to different endpoints of one provider remain externally accessible through the same provider prefix.
* If multiple compatible endpoints report the same model, the first configured matching endpoint is the visible automatic default.
* An administrator can configure one preferred Endpoint for a discovered model and protocol within a Provider; inference uses that preference before the first-compatible-endpoint default, while explicit public model routes remain higher priority.
* Stale model Endpoint preferences are removed when discovery no longer reports the model on that compatible Endpoint, and deleting an Endpoint removes its preferences.
* An explicit model route can select a compatible endpoint different from the first endpoint.
* Endpoint base URLs identify a shared API root; configuration rejects operation-specific URLs ending in `/chat/completions`, `/responses`, `/messages`, or `/models`, while nested roots such as OpenCode Go’s `/zen/go/v1` remain valid for model discovery and inference.
* Each endpoint can optionally route both model-discovery and inference traffic through a `socks5://` or `socks5h://` proxy.
* SOCKS5 proxy settings using another URL scheme are rejected with `400`.
* An endpoint can require an upstream API key or operate without one.
* An endpoint requiring a key cannot proxy when it has no enabled positive-weight key.
* An endpoint can contain multiple independently enabled upstream API keys.
* Default upstream key selection performs deterministic weighted round robin across enabled positive-weight keys.
* Disabled upstream keys receive no default traffic.
* OpenAI-compatible upstream credentials are sent as `Authorization: Bearer <key>`.
* Anthropic upstream credentials are sent as `x-api-key: <key>`.
* The caller's authorization headers are never forwarded to the upstream provider.

## Model-specific routing

* A model route creates a public model alias that clients call without a `provider/` prefix.
* A route target explicitly maps that public pattern to an upstream provider, endpoint, API key, and the exact model ID understood by that upstream; the upstream model does not include Yabane’s Provider prefix, though native namespaced IDs such as `google/model-name` remain valid.
* The route editor defaults to the simple alias task with only destination and upstream model fields, explains upstream model IDs, offers models discovered from the selected Endpoint as optional input suggestions, clearly permits custom IDs that were not discovered, and rejects an accidentally repeated Yabane Provider prefix while preserving legitimate native namespaced model IDs.
* Multi-destination traffic splitting is progressively disclosed behind an explicit action; only then does the editor show percentage shares, require a 100% total, and allow destinations to be added or removed while retaining at least one.
* Existing model routes can be opened in the route editor, including all weighted destinations, and saved with either the original or a changed public model pattern.
* A model route can contain multiple positive-weight targets and selects them using weighted round robin.
* A model route can be an exact model ID or one trailing prefix wildcard.
* Exact model routes take precedence over wildcard routes.
* The longest matching wildcard prefix takes precedence over shorter prefixes.
* If no model route matches, weighted key selection is used.
* A model route explicitly selects both an endpoint and one enabled upstream API key.
* Upstream-key update and deletion URLs include both Provider and Endpoint identity because key IDs are unique only within an Endpoint.
* The Provider detail console can delete each upstream key from within its owning Endpoint.
* Deleting an upstream key removes global-route targets referring to that exact Provider, Endpoint, and key; a route with no remaining targets is removed.

## Model discovery

* Creating a provider automatically starts model discovery without blocking provider creation.
* An administrator can explicitly refresh models for one provider or all providers.
* The console distinguishes discovery not yet run, successful empty results, and discovery failures.
* Provider details summarize the model catalog with unique-model count, shared-model count, configured Endpoint-default count, per-Endpoint coverage, discovery freshness, and a bounded searchable catalog instead of presenting an empty count-only card.
* The model catalog displays each model’s Endpoint availability and effective default routing, provides working search, clear, and complete Previous/Next pagination over all matches, and lets shared models be managed in a searchable bounded preference editor without rendering the full collection by default.
* Large provider model collections are collapsed by default and can be searched in a bounded model browser instead of rendering an unbounded wall of model labels.
* Successful model discovery and the latest discovery status are persisted for the admin console.
* `GET /v1/models` returns an OpenAI-compatible list envelope.
* Model discovery concurrently queries configured provider endpoints and enabled upstream keys.
* OpenAI-compatible discovery accepts both `{ "data": [...] }` and a top-level model array; nested OpenAI-compatible API roots such as `https://opencode.ai/zen/go/v1` resolve discovery at that root’s `/models` resource.
* Anthropic discovery uses the Anthropic authentication and version headers.
* Models are merged across endpoints, deduplicated by ID, and sorted while retaining endpoint availability metadata for internal routing.
* Returned model IDs use the `provider/model` form without duplicating an existing provider prefix.
* Failure to discover models from one provider does not hide successfully discovered models from other providers.
* Discovered models are reference data and do not create locally managed model definitions.

## Activity and statistics

* Each completed proxied response records request timestamp, request ID, API path, public model, provider, endpoint, status, latency, streaming flag, available usage totals, and upstream-reported cost when present; prompts and response content are not retained.
* Upstream cost is read from `usage.cost`, `usage.total_cost`, or `response.usage.cost` in OpenAI and Anthropic responses, including SSE usage events; requests without upstream cost record `null`, which the console displays as `—` or `$0.00` rather than fabricating a value.
* Activity statistics aggregate reported cost alongside request and token totals, and the console shows cost in the Overview metric, chart tooltips, and Request explorer rows.
* OpenAI Chat Completions and Responses usage and Anthropic Messages usage are normalized to input, output, and cached token counters when upstream reports them.
* SSE usage parsing honors event framing across arbitrary network chunks, supports CRLF and multiline data fields, ignores comments and `[DONE]`, reads OpenAI Responses usage nested under `response`, and combines Anthropic usage split across `message_start` and `message_delta` events.
* Activity statistics aggregate request and token totals over a caller-selected time range and group them by provider.
* The Activity Overview presents request, total-token, cache-hit, success-rate, streaming, and average-latency summaries with sparklines, a time-bucketed request/token chart, and ranked Provider and model breakdowns.
* Activity can be filtered by time range and Provider, refreshed on demand, and switched between the visual Overview and a searchable Request explorer with status filtering and detailed request metadata.
* Activity remains immediately queryable in memory and is batch-written after 10 records or 60 seconds rather than writing every request synchronously.
* Normal Activity flushes append JSON Lines instead of rewriting the full history; each periodic flush atomically compacts records older than the persisted retention policy, defaulting initially to 30 days, even when no new requests arrive.
* Pending Activity records are flushed when Yabane completes graceful shutdown.
* Activity data management uses a dedicated dialog rather than immediate Import/Export actions: export requires choosing a time range and previews record count, estimated size, and oldest/newest timestamps before download.
* An administrator can export retained Activity as a timestamp-named, versioned Yabane JSON file and import it into another instance without requiring matching Provider or Endpoint configuration; imported routing metadata remains visible as originally recorded.
* Before an Activity import writes anything, the console validates the selected file and previews its size plus total, new, already-present, and outside-retention record counts; invalid files remain visibly rejected, and the completed import reports its final outcome in the same dialog.
* Every installation persists a random Activity instance ID; Activity import is idempotent by source instance ID plus request ID, so repeated and transitive imports are skipped without treating coincident request IDs from different instances as the same record. Records outside the destination retention window are reported and skipped, accepted records are atomically persisted, and preview and import results report imported, duplicate, expired, and total counts.
* Activity retention defaults to 30 days or a valid initial `YABANE_ACTIVITY_RETENTION_DAYS` value from 1 to 3650, can be changed and persisted from the console, and continuously compacts both memory and `data/activity.jsonl`; invalid initial values fail startup visibly, and reducing retention immediately excludes and removes expired records.

## Admin data safety

* Admin provider responses never expose upstream API-key secrets.
* Authenticated admin gateway API-key list responses expose generated secrets for reveal/copy but never expose secret hashes.
* Technical identifiers, URLs, credentials, and model patterns use monospace presentation in the console.
