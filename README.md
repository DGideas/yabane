# Yabane

A clear, reliable gateway to every LLM.

Yabane is a small, performance-oriented LLM gateway written in Rust. Its first release provides faithful streaming proxying for OpenAI-compatible and Anthropic APIs, plus a minimal provider admin console.

## Features

- OpenAI-compatible `POST /v1/chat/completions`
- OpenAI Responses `POST /v1/responses`, including ChatGPT Plus/Pro OAuth subscription Endpoints
- Anthropic Messages `POST /v1/messages`
- Aggregated OpenAI-compatible `GET /v1/models`
- Streaming upstream responses without buffering
- Providers composed from one or more API endpoints
- Multiple weighted, independently enabled credentials per Endpoint, with automatic, explicitly configured cooldown of an identity that hits a Provider rate limit
- Endpoint types declared by Extensions: an Extension publishes the wire protocol, identity kinds, and sign-in flows of the Endpoints it owns, while Yabane Core keeps only the generic identity shapes, the resource hierarchy, selection, cooldown, and accounting
- OpenAI device-code sign-in by default, with browser sign-in fallback for organizations that disable device sign-in, automatic token refresh, several connectable accounts on one account-based Endpoint, and no password handling
- Optional per-Endpoint `socks5://` or `socks5h://` proxying
- Provider routing through `provider/model` IDs
- Optional exact-name and trailing-wildcard model routes to a specific Endpoint, optionally pinning one credential instead of using the Endpoint's automatic selection
- Two model-route selection modes: `weighted` shares traffic across interchangeable destinations, while `failover` keeps a preferred priority group until every identity behind it is cooling down and then uses the next group, with a request that meets the rate limit still receiving the Provider's own answer
- Gateway API-key authentication with expiration and provider scopes
- Separate one-time-secret Management API keys for control-plane automation
- Activity analytics with tokens, latency, routing metadata, upstream-reported cost, and explicitly configured cost estimates
- Versioned native Rust request-extension Hooks, with trusted extensions selected at build time
- Embedded interactive OpenAPI reference at `/docs` (`/openapi.json`)

## Run

Building Yabane needs Rust 1.88 or newer and Node.js 20 or newer (Node is only used by the test tooling). No database or external service is required: configuration, credentials, and Activity live in the local `data/` directory.

```bash
cp .env.example .env
cargo run --release
```

Yabane automatically loads local environment variables from the repository-root `.env` file, which is ignored by Git. Open <http://127.0.0.1:8080>, add a provider, then send requests to Yabane. Use `yabane --addr 127.0.0.1:9090` to change the listen address, or `yabane --help` to see all command-line options. Logs default to `info`; set `YABANE_LOG` or pass `--log` to use another tracing filter. Request activity metadata is buffered and persisted into one JSON Lines file per UTC day under `data/activity/`. Retention is configurable from Activity → Manage data and removes whole day files once they fall outside the window; `YABANE_ACTIVITY_RETENTION_DAYS` changes the initial 30-day default before a setting has been saved.

Shutdown is bounded. `Ctrl-C` or `SIGTERM` stops accepting new connections and lets requests already in flight finish; after `YABANE_SHUTDOWN_GRACE_SECONDS` (an integer from 1 to 86400, default 60) Yabane interrupts whatever is left, records each interrupted exchange in Activity with failure stage `gateway` and category `shutdown`, and flushes pending Activity before exiting. Errors from interrupted protocol conversion are recorded the same way, so a stop does not leave Activity claiming a completed answer.

Upstream inference connections have three process-wide safety deadlines. Connecting defaults to 15 seconds, each read must make progress within 300 seconds, and the complete request must finish within 28800 seconds (8 hours). A successful read resets only the per-read deadline, so normal long-running streams can continue while a connection that sends only keepalives remains bounded by the total deadline. Override these defaults with `YABANE_UPSTREAM_CONNECT_TIMEOUT_SECONDS`, `YABANE_UPSTREAM_READ_TIMEOUT_SECONDS`, and `YABANE_UPSTREAM_TOTAL_TIMEOUT_SECONDS`; each value must be an integer from 1 through 86400.

The default `release` profile uses Thin LTO and a single codegen unit to prioritize production runtime efficiency without the extra build cost of full LTO, which did not improve Yabane's measured local HTTP throughput. Incremental compilation caches intermediate artifacts, so a clean production build still performs all optimizations while subsequent `cargo build --release` runs after source edits are much faster.

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H 'Authorization: Bearer sk-your-yabane-key' \
  -H 'content-type: application/json' \
  -d '{"model":"chutes/qwen3.8-27b","messages":[{"role":"user","content":"Hello"}]}'
```

To use a ChatGPT Plus, Pro, or Business subscription, choose a subscription Endpoint type such as **OpenAI subscription** while adding a Provider or Endpoint and optionally enter a `socks5://` or `socks5h://` proxy. The Endpoint type supplies the connection, its model catalog, and its sign-in flows; connectivity, selection, cooldown, and accounting stay in Yabane Core. Device-code sign-in is the default. If an organization disables device sign-in, choose browser sign-in, complete authorization, and paste the complete `http://localhost:1455/auth/callback?...` URL from the browser address bar back into Yabane once. The proxy is used for authorization, token exchange and refresh, and inference requests. Subscription models can be called through OpenAI Responses, OpenAI Chat Completions, or Anthropic Messages; Yabane explicitly adapts each caller surface to the streaming Responses connection required by the ChatGPT Codex backend. Configure API-equivalent rates explicitly under **Model pricing** when Activity values are needed; those values are comparison values, not ChatGPT subscription charges.

Provider credentials—including OpenAI subscription OAuth tokens—are stored locally in the private `data/providers.json` file and are never returned by the management API; gateway access configuration is stored in `data/auth.json`; the administrator and hashed Management API keys are stored in `data/admin.json`. Configuration files are atomically replaced, and operations spanning Provider, route, and Gateway-key files use a private rollback journal that is recovered before configuration is loaded after an interrupted process. Gateway API-key authentication is enabled by default, so generate a key in **API access** before calling `/v1/*`. Use a separately generated Management API key for control endpoints, or use the browser session. Management-key creation and revocation remain browser-session-only.

Back up Yabane by copying the whole `data/` directory while the process is stopped; it contains every Provider, credential, route, pricing entry, administrator hash, Management API key, and Activity day file, and nothing outside it is required to restore an instance. Run exactly one Yabane process per data directory: the files are replaced atomically and an interrupted multi-file configuration update is rolled back from its journal on the next start, but concurrent processes would overwrite each other's writes without an interlock. Activity day files are append-only JSON Lines, so a backup can also be taken while Yabane runs if the copy tolerates a partial final line; an interrupted final record is quarantined on the next start and never silently dropped.

On first use, the admin console asks you to create its administrator account without a CAPTCHA. Login CAPTCHA is optional: configure a matching Cloudflare Turnstile widget pair with `TURNSTILE_SITE_KEY` and `TURNSTILE_SECRET`; if either value is unavailable, CAPTCHA is disabled. Optionally set `TURNSTILE_HOSTNAMES` to a comma-separated list of allowed frontend hostnames (defaults to `localhost,127.0.0.1`). The widget must allow the hostname used in the browser. When Cloudflare's documented always-pass test secret is used for local/E2E testing, Yabane automatically serves its matching test site key unless `TURNSTILE_SITE_KEY` is explicitly set. Never commit the Turnstile secret.

End-to-end behavior requirements are maintained in [`GATEWAY_BEHAVIORS.md`](GATEWAY_BEHAVIORS.md). Run `bash tests/check.sh` for the local quality gate — formatting, linting, unit tests, every Extension feature combination, OpenAPI/router drift, HTTP redirect isolation, Activity recovery, client disconnect, and bounded shutdown — or `bash tests/check.sh --full` to add browser and authentication E2E coverage. `bash tests/check.sh --load` adds a capacity baseline (latency percentiles and resident memory) after the gate passes, and `bash tests/behavior-map.sh` reports which rules are named by a test or by the source. Prerequisites and network dependencies are documented in [`tests/README.md`](tests/README.md); engineering findings and follow-up priorities are in [`ENGINEERING_REVIEW.md`](ENGINEERING_REVIEW.md).

Run the automated authentication E2E suite alone with:

```bash
tests/e2e.sh
```

With no argument, the suite rebuilds the debug binary first so embedded console assets match the working tree. Pass an explicit binary path, such as `tests/e2e.sh target/release/yabane`, only when testing an already-built artifact. Set `YABANE_E2E_KEEP=1` to keep the temporary data directory and logs of a failing run for inspection.

## Usage value estimates

Model pricing is managed from its own console page. Within each scope, an exact upstream-model pattern wins; otherwise one trailing `*` performs prefix matching and the longest prefix wins, matching Model routing semantics. After a rule is selected for each scope, Provider and Endpoint entries override only the fields they supply, with `Endpoint > Provider > Global` precedence. The model input suggests patterns from saved prices, model routes, Provider discovery, and Activity, but accepts any valid exact or trailing-wildcard pattern. No model or price list is built in. Global prices are stored in `data/pricing.json`; existing Provider and Endpoint prices remain compatible overrides in `data/providers.json`. Because those overrides are owned by their resource, deleting an Endpoint or Provider deletes its pricing rules in the same persisted configuration update while leaving unrelated global rules intact.

On sufficiently wide desktop layouts, entering at least two model characters searches the public models.dev catalog directly from the browser. The result identifies the models.dev Provider, remains a reference only, and fills the visible rates only after **Use rates** is selected. It is never an automatic or implicit Gateway price source. Cache-write pricing remains accepted in persisted/API data for compatibility but is omitted from the editor because current normalized usage has no cache-write token count and cannot apply that rate.

An upstream-reported cost is recorded as `reported`. When an upstream returns usage without cost, complete inherited rates produce an `estimated` usage value; unknown rates leave the cost unavailable. Saving or changing rates does not automatically modify Activity. An explicit Activity cost refresh can recalculate existing `estimated` values and fill missing values using the current explicit pricing, while never replacing `reported` values. Each stored pricing table's `updated_at` field is a server-assigned Unix timestamp rather than an incrementing version.

Yabane does not ship or refresh a model catalog or rate table. Administrators choose the comparison rates appropriate for their upstream contracts or subscription plans; changing them affects future Activity automatically and affects retained non-reported Activity only when an administrator explicitly runs the Activity cost refresh.

## Extensions

Yabane request extensions are trusted Rust crates statically linked through Cargo features. The default build includes `request-defaults`, `traffic-capture`, and `openai-subscription`. An Extension may declare Provider Endpoint types in addition to Hook stages; it then owns that type's wire protocol, model catalog, identity kinds, sign-in flows, and its own words for them, while Yabane Core stores those identities generically and never names a vendor. OpenAI Subscription owns ChatGPT device authorization, credential refresh, the pi-ai-aligned model catalog, and Codex wire adaptation while its Provider and Endpoint setup remains in the normal console resource hierarchy. Request Defaults owns explicitly configured Extra Header and Extra JSON Body behavior. Its configuration remains in each Provider's resource context, while the Extensions console page identifies the implementation, API version, Hook stages, and enabled state.

An administrator can enable or disable each compiled Extension on the Extensions page. The state is persisted in `data/extensions.json`; disabling Request Defaults keeps its Provider and Endpoint configuration but stops applying it. To disable every compiled Extension for one process without changing persisted settings, run:

```bash
yabane --no-extensions
```

Build the transparent Core without bundled Extensions with:

```bash
cargo build --release --no-default-features
```

See the [Yabane Extensions development guide](.agents/skills/yabane-extensions/SKILL.md) for the Hook lifecycle, crate setup, registration, security rules, and test checklist. The versioned API lives in `crates/yabane-extension-api`; concrete extensions and their policy tests live under `extensions/`. Yabane Core owns ordered Hook dispatch, structured rejection, Header credential isolation, and zero-Hook behavior. Native extensions are trusted process code, not sandboxed runtime packages.

## Design

Yabane starts as a transparent gateway. It reads the request model to select a configured provider, removes that provider prefix, and leaves every other request field unchanged on a native protocol path unless an explicitly configured extension modifies it. Upstream responses remain streamed. Cross-protocol routing uses an explicit adapter. An Endpoint type that declares an Extension-owned connection always connects to that connection, applies the headers and body constraints it requires, and adapts responses back to the caller's selected API surface.
