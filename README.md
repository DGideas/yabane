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
- Multiple weighted, independently enabled API keys per endpoint
- OpenAI device-code sign-in with automatic OAuth token refresh and no password handling
- Provider routing through `provider/model` IDs
- Optional exact-name and trailing-wildcard model routes to specific upstream credentials
- Gateway API-key authentication with expiration and provider scopes
- Separate one-time-secret Management API keys for control-plane automation
- Activity analytics with tokens, latency, routing metadata, and upstream-reported cost
- Embedded interactive OpenAPI reference at `/docs` (`/openapi.json`)

## Run

```bash
cp .env.example .env
cargo run --release
```

Yabane automatically loads local environment variables from the repository-root `.env` file, which is ignored by Git. Open <http://127.0.0.1:8080>, add a provider, then send requests to Yabane. Use `yabane --addr 127.0.0.1:9090` to change the listen address, or `yabane --help` to see all command-line options. Logs default to `info`; set `YABANE_LOG` or pass `--log` to use another tracing filter. Request activity metadata is buffered and persisted to `data/activity.jsonl`. Retention is configurable from Activity → Manage data and continuously compacts older records; `YABANE_ACTIVITY_RETENTION_DAYS` changes the initial 30-day default before a setting has been saved.

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H 'Authorization: Bearer sk-your-yabane-key' \
  -H 'content-type: application/json' \
  -d '{"model":"chutes/qwen3.8-27b","messages":[{"role":"user","content":"Hello"}]}'
```

To use a ChatGPT Plus or Pro subscription, choose **OpenAI subscription** while adding a Provider or Endpoint, open the displayed OpenAI device sign-in page, and enter the one-time code. Subscription models are exposed through Yabane's OpenAI Responses API and require `"stream": true`; Chat Completions and Anthropic Messages are not converted.

Provider credentials—including OpenAI subscription OAuth tokens—are stored locally in the private `data/providers.json` file and are never returned by the management API; gateway access configuration is stored in `data/auth.json`; the administrator and hashed Management API keys are stored in `data/admin.json`. Gateway API-key authentication is enabled by default, so generate a key in **API access** before calling `/v1/*`. Use a separately generated Management API key for control endpoints, or use the browser session. Management-key creation and revocation remain browser-session-only.

On first use, the admin console asks you to create its administrator account without a CAPTCHA. Login CAPTCHA is optional: configure a matching Cloudflare Turnstile widget pair with `TURNSTILE_SITE_KEY` and `TURNSTILE_SECRET`; if either value is unavailable, CAPTCHA is disabled. Optionally set `TURNSTILE_HOSTNAMES` to a comma-separated list of allowed frontend hostnames (defaults to `localhost,127.0.0.1`). The widget must allow the hostname used in the browser. When Cloudflare's documented always-pass test secret is used for local/E2E testing, Yabane automatically serves its matching test site key unless `TURNSTILE_SITE_KEY` is explicitly set. Never commit the Turnstile secret.

End-to-end behavior requirements are maintained in [`GATEWAY_BEHAVIORS.md`](GATEWAY_BEHAVIORS.md). Run the automated authentication E2E suite with:

```bash
cargo build && tests/e2e.sh target/debug/yabane
```

## Design

Yabane starts as a transparent gateway. It reads the request model to select a configured provider, removes that provider prefix, and leaves every other request field unchanged. Upstream responses remain streamed. The OpenAI subscription Endpoint is an explicit exception: it maps streaming OpenAI Responses requests to ChatGPT's Codex backend and applies that backend's documented OAuth headers and body constraints; it never converts Chat Completions or Anthropic Messages.
