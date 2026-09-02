# Yabane

A clear, reliable gateway to every LLM.

Yabane is a small, performance-oriented LLM gateway written in Rust. Its first release provides faithful streaming proxying for OpenAI-compatible and Anthropic APIs, plus a minimal provider admin console.

## Features

- OpenAI-compatible `POST /v1/chat/completions`
- OpenAI Responses `POST /v1/responses`
- Anthropic Messages `POST /v1/messages`
- Streaming request and response bodies without JSON rewriting
- Add and remove providers from the web console
- Explicit provider selection through `x-yabane-provider`

## Run

```bash
cargo run --release
```

Open <http://127.0.0.1:8080>, add a provider, then send requests to Yabane. Set `YABANE_ADDR` to change the listen address.

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -H 'x-yabane-provider: chutes' \
  -d '{"model":"qwen3.8-27b","messages":[{"role":"user","content":"Hello"}]}'
```

Provider credentials are stored locally in `data/providers.json`. The admin console has no authentication in this first development release; do not expose it publicly.

## Design

Yabane starts as a transparent gateway: request bodies and upstream responses are streamed without semantic conversion. API translation, routing policies, authentication, observability, and persistent databases will be introduced incrementally.
