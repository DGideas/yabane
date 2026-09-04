# Yabane

A clear, reliable gateway to every LLM.

Yabane is a small, performance-oriented LLM gateway written in Rust. Its first release provides faithful streaming proxying for OpenAI-compatible and Anthropic APIs, plus a minimal provider admin console.

## Features

- OpenAI-compatible `POST /v1/chat/completions`
- OpenAI Responses `POST /v1/responses`
- Anthropic Messages `POST /v1/messages`
- Aggregated OpenAI-compatible `GET /v1/models`
- Streaming upstream responses without buffering
- Providers composed from one or more API endpoints
- Multiple weighted, independently enabled API keys per endpoint
- Provider routing through `provider/model` IDs
- Optional exact-name and trailing-wildcard model routes to specific API keys
- Gateway API-key authentication with expiration and provider scopes

## Run

```bash
cargo run --release
```

Open <http://127.0.0.1:8080>, add a provider, then send requests to Yabane. Set `YABANE_ADDR` to change the listen address. Logs default to `info`; set `YABANE_LOG` to a valid tracing filter to override it.

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H 'Authorization: Bearer sk-your-yabane-key' \
  -H 'content-type: application/json' \
  -d '{"model":"chutes/qwen3.8-27b","messages":[{"role":"user","content":"Hello"}]}'
```

Provider credentials are stored locally in `data/providers.json`; gateway access configuration is stored in `data/auth.json`. Gateway API-key authentication is enabled by default, so generate a key in **API access** before calling `/v1/*`. The admin console itself has no authentication in this development release; do not expose it publicly.

End-to-end behavior requirements are maintained in [`GATEWAY_BEHAVIORS.md`](GATEWAY_BEHAVIORS.md). Run the automated authentication E2E suite with:

```bash
cargo build && tests/e2e.sh target/debug/yabane
```

## Design

Yabane starts as a transparent gateway. It reads the request model to select a configured provider, removes that provider prefix, and leaves every other request field unchanged. Upstream responses remain streamed. API translation, advanced routing policies, authentication, observability, and persistent databases will be introduced incrementally.
