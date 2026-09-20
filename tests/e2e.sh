#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
if [[ $# -gt 0 ]]; then
  binary=$1
  binary=$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")
else
  cargo build --manifest-path "$repo/Cargo.toml"
  binary="$repo/target/debug/yabane"
fi
work=$(mktemp -d)
available_port() {
  python3 -c 'import socket; sock = socket.socket(); sock.bind(("127.0.0.1", 0)); print(sock.getsockname()[1]); sock.close()'
}
port=${YABANE_E2E_PORT:-$(available_port)}
upstream_port=$(available_port)
while [[ $upstream_port == "$port" ]]; do upstream_port=$(available_port); done
pid=
upstream_pid=
cleanup() { [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true; [[ -n "$upstream_pid" ]] && kill "$upstream_pid" 2>/dev/null || true; rm -rf "$work"; }
trap cleanup EXIT
cd "$work"
version_output=$("$binary" --version)
[[ $version_output =~ ^yabane\ ([0-9a-f]{8}|unknown)\ \(.+\)$ ]]
[[ $("$binary" --help) == *"Initial Activity retention before a setting is saved"* ]]
[[ $("$binary" --help) == *"Upstream total request deadline [default: 28800]"* ]]
[[ $("$binary" --help) == *"--no-extensions"* ]]
if YABANE_ACTIVITY_RETENTION_DAYS=0 "$binary" --addr "127.0.0.1:$port" >invalid-retention.log 2>&1; then
  echo "invalid activity retention unexpectedly started" >&2; exit 1
fi
grep -q 'YABANE_ACTIVITY_RETENTION_DAYS must be between 1 and 3650' invalid-retention.log
if YABANE_UPSTREAM_READ_TIMEOUT_SECONDS=0 "$binary" --addr "127.0.0.1:$port" >invalid-upstream-timeout.log 2>&1; then
  echo "invalid upstream timeout unexpectedly started" >&2; exit 1
fi
grep -q 'YABANE_UPSTREAM_READ_TIMEOUT_SECONDS must be an integer between 1 and 86400' invalid-upstream-timeout.log
mkdir -p data
cat >data/providers.json <<'JSON'
[{"id":"subscription-fixture","name":"Subscription fixture","extra_headers":{},"extra_body":{},"defaults_endpoint_ids":[],"endpoints":[{"id":"chatgpt","api_type":"openai_codex","base_url":"https://chatgpt.com/backend-api","socks5_proxy":null,"extra_headers":{},"extra_body":{},"requires_api_key":false,"api_keys":[],"openai_subscription":{"access_token":"fixture","refresh_token":"fixture","expires_at":4102444800,"account_id":"fixture"}}],"discovered_models":[],"model_endpoints":{},"model_endpoint_preferences":[],"models_discovered_at":null,"model_discovery_error":null}]
JSON
# Simulate crashes after multi-file replacements but before their rollback
# journals were removed. Startup must restore each complete old file set before
# parsing either configuration or Activity.
python3 - <<'PY'
import json
from pathlib import Path

path = Path('data/providers.json')
previous = path.read_bytes()
path.write_bytes(b'{interrupted')
Path('data/config-transaction.json').write_text(json.dumps({
    'version': 1,
    'entries': [{'path': str(path), 'previous': list(previous)}],
}))
activity_path = Path('data/activity.jsonl')
settings_path = Path('data/activity-settings.json')
activity_path.write_bytes(b'')
settings_path.write_text(json.dumps({'retention_days': 30}))
activity_previous = activity_path.read_bytes()
settings_previous = settings_path.read_bytes()
activity_path.write_bytes(b'{interrupted')
settings_path.write_text(json.dumps({'retention_days': 1}))
Path('data/activity-transaction.json').write_text(json.dumps({
    'version': 1,
    'entries': [
        {'path': str(activity_path), 'previous': list(activity_previous)},
        {'path': str(settings_path), 'previous': list(settings_previous)},
    ],
}))
PY
TURNSTILE_SECRET="${TURNSTILE_TEST_SECRET:-1x0000000000000000000000000000000AA}" \
YABANE_UPSTREAM_CONNECT_TIMEOUT_SECONDS=1 \
YABANE_UPSTREAM_READ_TIMEOUT_SECONDS=1 \
YABANE_UPSTREAM_TOTAL_TIMEOUT_SECONDS=3 \
"$binary" --addr "127.0.0.1:$port" >server.log 2>&1 & pid=$!
ready=false
for _ in $(seq 1 50); do
  if curl -sf "http://127.0.0.1:$port/healthz" >/dev/null; then ready=true; break; fi
  if ! kill -0 "$pid" 2>/dev/null; then cat server.log >&2; echo "Yabane exited before becoming ready" >&2; exit 1; fi
  sleep .1
done
if [[ $ready != true ]]; then cat server.log >&2; echo "Yabane did not become ready" >&2; exit 1; fi
[[ ! -e data/config-transaction.json ]]
[[ ! -e data/activity-transaction.json ]]
[[ ! -s data/activity.jsonl ]]
[[ $(jq -r '.retention_days' data/activity-settings.json) == 30 ]]
[[ $(jq -r '.[0].id' data/providers.json) == subscription-fixture ]]
base="http://127.0.0.1:$port"
cookie="$work/cookie.txt"
about=$(curl -fsS "$base/about")
[[ $(printf '%s' "$about" | jq -r .name) == yabane ]]
[[ $(printf '%s' "$about" | jq -r 'has("version")') == false ]]
[[ $(printf '%s' "$about" | jq -r .commit) =~ ^([0-9a-f]{8}|unknown)$ ]]
[[ $(printf '%s' "$about" | jq -r .commit_time) =~ ^([0-9]{4}-[0-9]{2}-[0-9]{2}T.*|unknown)$ ]]
[[ $(printf '%s' "$about" | jq -r .license) == MIT ]]
[[ $(printf '%s' "$about" | jq -r .license_text) == "MIT License"* ]]
turnstile_config=$(curl -fsS "$base/admin/turnstile-config")
[[ $(printf '%s' "$turnstile_config" | jq -r .enabled) == true ]]
[[ $(printf '%s' "$turnstile_config" | jq -r .site_key) == 1x00000000000000000000AA ]]
setup_payload='{"username":"admin","email":"admin@example.com","password":"password123"}'
curl -sS -c "$work/cookie-one.txt" -o "$work/setup-one.json" -w '%{http_code}' -X POST "$base/admin/setup" -H 'content-type: application/json' -d "$setup_payload" >"$work/setup-one.status" & setup_one_pid=$!
curl -sS -c "$work/cookie-two.txt" -o "$work/setup-two.json" -w '%{http_code}' -X POST "$base/admin/setup" -H 'content-type: application/json' -d "$setup_payload" >"$work/setup-two.status" & setup_two_pid=$!
wait "$setup_one_pid" "$setup_two_pid"
[[ $(sort "$work/setup-one.status" "$work/setup-two.status" | paste -sd, -) == 204,409 ]]
if [[ $(<"$work/setup-one.status") == 204 ]]; then cp "$work/cookie-one.txt" "$cookie"; else cp "$work/cookie-two.txt" "$cookie"; fi
[[ $(curl -sS -o /dev/null -w '%{http_code}' -X POST "$base/admin/setup" -H 'content-type: application/json' -d '{"username":"other","email":"other@example.com","password":"password456"}') == 409 ]]
cat >upstream.py <<'PY'
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import sys
import time

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        endpoint = self.headers.get('authorization', 'Bearer unknown').removeprefix('Bearer ')
        if self.path not in ['/v1/models', '/nested/v1/models']:
            self.send_response(404); self.end_headers(); return
        models = {'one': ['model-a', 'shared'], 'two': ['model-b', 'shared'], 'nested': ['nested-model']}.get(endpoint, [])
        if self.path == '/nested/v1/models' and endpoint != 'nested':
            models = []
        body = json.dumps({'object': 'list', 'data': [{'id': model} for model in models]}).encode()
        self.send_response(200); self.send_header('content-type', 'application/json'); self.end_headers(); self.wfile.write(body)
    def do_POST(self):
        length = int(self.headers.get('content-length', 0)); request = json.loads(self.rfile.read(length))
        endpoint = self.headers.get('authorization', '').removeprefix('Bearer ') or self.headers.get('x-api-key', 'unknown')
        if request.get('model') == 'timeout-before-headers':
            time.sleep(2)
            try:
                self.send_response(200); self.send_header('content-type', 'application/json'); self.end_headers(); self.wfile.write(b'{}')
            except BrokenPipeError:
                pass
            return
        if request.get('model') == 'timeout-idle-stream':
            self.send_response(200); self.send_header('content-type', 'text/event-stream'); self.end_headers(); self.wfile.flush()
            time.sleep(2)
            try:
                self.wfile.write(b'data: [DONE]\\n\\n')
            except BrokenPipeError:
                pass
            return
        if request.get('model') == 'timeout-keepalive-stream':
            self.send_response(200); self.send_header('content-type', 'text/event-stream'); self.end_headers()
            try:
                for _ in range(16):
                    self.wfile.write(b': keepalive\\n\\n'); self.wfile.flush(); time.sleep(0.25)
            except BrokenPipeError:
                pass
            return
        if self.path == '/v1/messages':
            if self.headers.get('openai-organization') or self.headers.get('openai-project') or self.headers.get('anthropic-version') != '2023-06-01' or self.headers.get('accept-encoding') != 'identity':
                self.send_response(400); self.end_headers(); return
            if not isinstance(request.get('messages'), list) or 'max_tokens' not in request:
                self.send_response(400); self.end_headers(); return
            if request.get('stream'):
                frames = [
                    ('message_start', {'type': 'message_start', 'message': {'id': 'msg-converted', 'type': 'message', 'role': 'assistant', 'model': request['model'], 'content': [], 'usage': {'input_tokens': 7, 'output_tokens': 0}}}),
                    ('content_block_start', {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}}),
                    ('content_block_delta', {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': 'from-anthropic-stream'}}),
                    ('content_block_stop', {'type': 'content_block_stop', 'index': 0}),
                    ('message_delta', {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn', 'stop_sequence': None}, 'usage': {'output_tokens': 2}}),
                    ('message_stop', {'type': 'message_stop'}),
                ]
                body = ''.join(f'event: {event}\ndata: {json.dumps(data)}\n\n' for event, data in frames).encode()
                self.send_response(200); self.send_header('content-type', 'text/event-stream'); self.end_headers(); self.wfile.write(body); return
            body = json.dumps({'id': 'msg-converted', 'type': 'message', 'role': 'assistant', 'model': request['model'], 'content': [{'type': 'text', 'text': 'from-anthropic'}], 'stop_reason': 'end_turn', 'stop_sequence': None, 'usage': {'input_tokens': 7, 'output_tokens': 2}}).encode()
            self.send_response(200); self.send_header('content-type', 'application/json'); self.send_header('etag', '"anthropic-body"'); self.send_header('digest', 'sha-256=upstream'); self.end_headers(); self.wfile.write(body); return
        if endpoint == 'responses-stream':
            bad_format = request.get('text', {}).get('format', {}).get('type') == 'json_schema' and 'json_schema' in request.get('text', {}).get('format', {})
            bad_choice = isinstance(request.get('tool_choice'), dict) and request['tool_choice'].get('type') == 'function' and 'function' in request['tool_choice']
            if self.path != '/v1/responses' or not isinstance(request.get('input'), list) or 'max_tokens' in request or 'frequency_penalty' in request or bad_format or bad_choice:
                self.send_response(400); self.end_headers(); return
            if request.get('model') == 'gpt-document' and request.get('input', [{}])[0].get('content', [{}])[0].get('file_url') != 'https://example.com/report.pdf':
                self.send_response(400); self.end_headers(); return
            if request.get('model') == 'gpt-failed':
                body = json.dumps({'id': 'resp-failed', 'object': 'response', 'status': 'failed', 'model': request['model'], 'error': {'code': 'server_error', 'message': 'mock overloaded'}, 'output': []}).encode()
                self.send_response(200); self.send_header('content-type', 'application/json'); self.end_headers(); self.wfile.write(body); return
            if request.get('model') == 'gpt-stream-failed':
                frames = [
                    ('response.created', {'type': 'response.created', 'response': {'id': 'resp-stream-failed', 'object': 'response', 'status': 'in_progress', 'model': request['model'], 'output': []}}),
                    ('response.failed', {'type': 'response.failed', 'response': {'id': 'resp-stream-failed', 'object': 'response', 'status': 'failed', 'model': request['model'], 'error': {'code': 'server_error', 'message': 'mock stream overloaded'}, 'output': []}}),
                ]
                body = ''.join(f'event: {event}\ndata: {json.dumps(data)}\n\n' for event, data in frames).encode()
                self.send_response(200); self.send_header('content-type', 'text/event-stream'); self.end_headers(); self.wfile.write(body); return
            response = {'id': 'resp-stream', 'object': 'response', 'created_at': 11, 'status': 'completed', 'model': request['model'], 'output': [{'id': 'msg-stream', 'type': 'message', 'role': 'assistant', 'status': 'completed', 'content': [{'type': 'output_text', 'text': 'from-responses-stream'}]}], 'usage': {'input_tokens': 4, 'output_tokens': 2, 'total_tokens': 6}}
            frames = [
                ('response.created', {'type': 'response.created', 'response': {**response, 'status': 'in_progress', 'output': [], 'usage': None}}),
                ('response.output_text.delta', {'type': 'response.output_text.delta', 'output_index': 0, 'content_index': 0, 'delta': 'from-responses-stream'}),
                ('response.completed', {'type': 'response.completed', 'response': response}),
            ]
            body = ''.join(f'event: {event}\ndata: {json.dumps(data)}\n\n' for event, data in frames).encode()
            self.send_response(200); self.send_header('content-type', 'text/event-stream'); self.end_headers(); self.wfile.write(body); return
        if endpoint == 'convert':
            if self.headers.get('anthropic-beta') or self.headers.get('anthropic-version'):
                self.send_response(400); self.end_headers(); return
            malformed_tools = any(tool.get('type') != 'function' or not tool.get('function', {}).get('name') for tool in request.get('tools', []))
            bad_format = request.get('response_format', {}).get('type') == 'json_schema' and 'json_schema' not in request.get('response_format', {})
            bad_choice = isinstance(request.get('tool_choice'), dict) and not request['tool_choice'].get('function', {}).get('name')
            if self.path != '/v1/chat/completions' or not isinstance(request.get('messages'), list) or 'max_tool_calls' in request or 'prompt_cache_key' in request or malformed_tools or bad_format or bad_choice:
                self.send_response(400); self.end_headers(); return
            body = json.dumps({'id': 'chat-converted', 'object': 'chat.completion', 'created': 10, 'model': request['model'], 'choices': [{'index': 0, 'message': {'role': 'assistant', 'content': 'from-openai'}, 'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 6, 'completion_tokens': 2, 'total_tokens': 8, 'cost': 0.0042}}).encode()
            self.send_response(200); self.send_header('content-type', 'application/json'); self.end_headers(); self.wfile.write(body); return
        if request.get('model') == 'provider-error':
            body = json.dumps({'error': {'message': 'provider exploded', 'type': 'server_error'}}).encode()
            self.send_response(500); self.send_header('content-type', 'application/json'); self.send_header('x-yabane-error-origin', 'yabane'); self.send_header('x-yabane-request-id', 'req-forged'); self.end_headers(); self.wfile.write(body); return
        body = json.dumps({'endpoint': endpoint, 'model': request['model'], 'headers': {'x-provider': self.headers.get('x-provider'), 'x-endpoint': self.headers.get('x-endpoint'), 'cookie': self.headers.get('cookie')}, 'extra': request.get('extra'), 'endpoint_extra': request.get('endpoint_extra'), 'usage': {'prompt_tokens': 1200, 'completion_tokens': 300, 'prompt_tokens_details': {'cached_tokens': 200}, 'cost': 0.0042}}).encode()
        self.send_response(200); self.send_header('content-type', 'application/json'); self.send_header('set-cookie', 'yabane_session=upstream'); self.end_headers(); self.wfile.write(body)
    def log_message(self, *_): pass

ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), Handler).serve_forever()
PY
python3 upstream.py "$upstream_port" & upstream_pid=$!
status() { curl -sS -o response.json -w '%{http_code}' "$@"; }
admin() { curl -sS -b "$cookie" "$@"; }
admin_status() { curl -sS -b "$cookie" -o response.json -w '%{http_code}' "$@"; }
[[ $(status "$base/v1/models") == 401 ]]
[[ $(admin_status -X POST "$base/admin/providers/missing/models/refresh") == 404 ]]
extensions=$(admin -f "$base/admin/extensions")
[[ $(printf '%s' "$extensions" | jq -r '.[] | select(.id == "request-defaults") | [.implementation, (.api_version | tostring), (.hooks | join(","))] | join(":")') == native_rust:1:upstream_request,upstream_headers ]]
[[ $(printf '%s' "$extensions" | jq -r '.[] | select(.id == "traffic-capture") | [.implementation, (.api_version | tostring), (.hooks | join(",")), (.enabled | tostring)] | join(":")') == native_rust:1:upstream_exchange:true ]]
[[ $(printf '%s' "$extensions" | jq -r '.[] | select(.id == "openai-subscription") | [.implementation, (.api_version | tostring), (.hooks | join(",")), (.enabled | tostring)] | join(":")') == native_rust:1:provider_endpoint:true ]]
# Provider Endpoint Extensions remain independently switchable. Their configured
# resources are retained but unavailable until the implementation is enabled again.
[[ $(admin -f -X PATCH "$base/admin/extensions/openai-subscription" -H 'content-type: application/json' -d '{"enabled":false}' | jq -r .enabled) == false ]]
[[ $(admin -f "$base/admin/providers" | jq -r '.[] | select(.id == "subscription-fixture") | .endpoints[0].id') == chatgpt ]]
[[ $(admin_status -X POST "$base/admin/openai-subscriptions/device-code" -H 'content-type: application/json' -d '{"provider_id":"unavailable","provider_name":"Unavailable"}') == 409 ]]
[[ $(jq -r '.error.message' response.json) == "OpenAI Subscription Extension is not enabled" ]]
admin -f -X PATCH "$base/admin/auth" -H 'content-type: application/json' -d '{"enabled":false}' >/dev/null
[[ $(status -X POST "$base/v1/responses" -H 'content-type: application/json' -d '{"model":"subscription-fixture/gpt-5.2","input":"test"}') == 400 ]]
[[ $(jq -r '.error.message' response.json) == "OpenAI Subscription Extension is not enabled" ]]
# Automatic routing can bypass the unavailable Extension-owned Endpoint when
# another configured Endpoint remains eligible for a model.
admin -f -X POST "$base/admin/providers/subscription-fixture/endpoints" -H 'content-type: application/json' -d "{\"id\":\"fallback\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true,\"api_key\":\"one\"}" >/dev/null
fallback_response=$(curl -sf -X POST "$base/v1/responses" -H 'content-type: application/json' -d '{"model":"subscription-fixture/custom-model","input":"test"}')
[[ $(printf '%s' "$fallback_response" | jq -r .endpoint) == one ]]
[[ $(admin_status -X POST "$base/admin/routes" -H 'content-type: application/json' -d '{"pattern":"disabled-subscription","targets":[{"provider_id":"subscription-fixture","endpoint_id":"chatgpt","api_key_id":"","upstream_model":"gpt-5.2","weight":100,"enabled":true}]}') == 409 ]]
[[ $(jq -r '.error.message' response.json) == "Enable the OpenAI Subscription Extension before routing to this Endpoint" ]]
admin -f -X PATCH "$base/admin/auth" -H 'content-type: application/json' -d '{"enabled":true}' >/dev/null
admin -f -X PATCH "$base/admin/extensions/openai-subscription" -H 'content-type: application/json' -d '{"enabled":true}' >/dev/null
admin -f -X DELETE "$base/admin/providers/subscription-fixture" >/dev/null
[[ $(admin -f "$base/admin/extensions/traffic-capture/status" | jq -r '[.config.active, .config.remaining, .retained] | join(":")') == false:0:0 ]]
created=$(admin -f -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"E2E unrestricted","expires_at":null,"provider_ids":[]}')
secret=$(printf '%s' "$created" | jq -r .secret)
id=$(printf '%s' "$created" | jq -r .api_key.id)
[[ $secret == sk-* ]]
admin -f "$base/admin/auth" | grep -Fq "$secret"
grep -Fq "$secret" data/auth.json
[[ $(status "$base/v1/models" -H 'Authorization: Basic nope') == 401 ]]
[[ $(status "$base/v1/models" -H 'Authorization: Bearer sk-invalid') == 401 ]]
[[ $(status "$base/v1/models" -H "Authorization: Bearer $secret") == 200 ]]
future_expiry=$(($(date +%s) + 3600))
admin -f -X PATCH "$base/admin/auth/keys/$id" -H 'content-type: application/json' -d "{\"note\":\"Edited note\",\"expires_at\":$future_expiry,\"provider_ids\":[]}" >/dev/null
edited_key=$(admin -f "$base/admin/auth" | jq -c ".api_keys[] | select(.id == \"$id\")")
[[ $(printf '%s' "$edited_key" | jq -r .note) == "Edited note" ]]
[[ $(printf '%s' "$edited_key" | jq -r .expires_at) == "$future_expiry" ]]
[[ $(printf '%s' "$edited_key" | jq -r .secret) == "$secret" ]]
# PATCH updates only supplied fields; editing a note must not silently clear expiry or scope.
admin -f -X PATCH "$base/admin/auth/keys/$id" -H 'content-type: application/json' -d '{"note":"Note only"}' >/dev/null
[[ $(admin -f "$base/admin/auth" | jq -r ".api_keys[] | select(.id == \"$id\") | .expires_at") == "$future_expiry" ]]
[[ $(status "$base/v1/models" -H "Authorization: Bearer $secret") == 200 ]]
[[ $(admin_status -X PATCH "$base/admin/auth/keys/$id" -H 'content-type: application/json' -d '{"note":"expired","expires_at":1,"provider_ids":[]}') == 400 ]]
[[ $(admin_status -X PATCH "$base/admin/auth/keys/$id" -H 'content-type: application/json' -d '{"note":"bad scope","expires_at":null,"provider_ids":["missing"]}') == 400 ]]
admin -f -X DELETE "$base/admin/auth/keys/$id" >/dev/null
[[ $(status "$base/v1/models" -H "Authorization: Bearer $secret") == 401 ]]
[[ $(admin_status -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"expired","expires_at":1,"provider_ids":[]}') == 400 ]]
[[ $(admin_status -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"bad scope","expires_at":null,"provider_ids":["missing"]}') == 400 ]]
short_expiry=$(($(date +%s) + 1))
expiring=$(admin -f -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d "{\"note\":\"Short lived\",\"expires_at\":$short_expiry,\"provider_ids\":[]}")
expiring_secret=$(printf '%s' "$expiring" | jq -r .secret)
expiring_id=$(printf '%s' "$expiring" | jq -r .api_key.id)
sleep 2
[[ $(status "$base/v1/models" -H "Authorization: Bearer $expiring_secret") == 401 ]]
[[ $(jq -r '.error.message' response.json) == "API key has expired" ]]
# Editing an expired key to remove expiry reactivates the same secret.
admin -f -X PATCH "$base/admin/auth/keys/$expiring_id" -H 'content-type: application/json' -d '{"note":"Reactivated","expires_at":null,"provider_ids":[]}' >/dev/null
[[ $(status "$base/v1/models" -H "Authorization: Bearer $expiring_secret") == 200 ]]
[[ $(admin_status -X POST "$base/admin/providers" -H 'content-type: application/json' -d '{"id":"bad-proxy","name":"Bad proxy","endpoint":{"api_type":"openai_compatible","base_url":"http://127.0.0.1:1/v1","socks5_proxy":"http://127.0.0.1:1080","requires_api_key":false,"api_key":null}}') == 400 ]]
[[ $(admin_status -X POST "$base/admin/openai-subscriptions/device-code" -H 'content-type: application/json' -d '{"provider_id":"bad-subscription-proxy","provider_name":"Bad subscription proxy","endpoint_id":"chatgpt","socks5_proxy":"http://127.0.0.1:1080"}') == 400 ]]
[[ $(admin_status -X POST "$base/admin/openai-subscriptions/oauth" -H 'content-type: application/json' -d '{"provider_id":"bad-oauth-proxy","provider_name":"Bad OAuth proxy","endpoint_id":"chatgpt","socks5_proxy":"https://127.0.0.1:1080"}') == 400 ]]
browser_oauth=$(admin -f -X POST "$base/admin/openai-subscriptions/oauth" -H 'content-type: application/json' -d '{"provider_id":"browser-oauth","provider_name":"Browser OAuth","endpoint_id":"chatgpt"}')
browser_oauth_id=$(printf '%s' "$browser_oauth" | jq -r .id)
browser_oauth_url=$(printf '%s' "$browser_oauth" | jq -r .authorization_url)
[[ -n $browser_oauth_id && $browser_oauth_url == https://auth.openai.com/oauth/authorize\?* ]]
[[ $browser_oauth_url == *client_id=app_EMoamEEZ73f0CkXaXp7hrann* ]]
[[ $browser_oauth_url == *redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback* ]]
[[ $browser_oauth_url == *code_challenge_method=S256* ]]
[[ $(admin_status -X POST "$base/admin/openai-subscriptions/oauth/$browser_oauth_id/complete" -H 'content-type: application/json' -d '{"redirect_url":"http://127.0.0.1:1455/auth/callback?code=code&state=state"}') == 400 ]]
[[ $(jq -r '.error.message' response.json) == "Callback URL must start with http://localhost:1455/auth/callback" ]]
[[ $(admin_status -X POST "$base/admin/openai-subscriptions/oauth/$browser_oauth_id/complete" -H 'content-type: application/json' -d '{"redirect_url":"http://localhost:1455/auth/callback?code=code&state=wrong"}') == 400 ]]
[[ $(jq -r '.error.message' response.json) == "OpenAI callback state does not match this sign-in" ]]
[[ $(admin_status -X POST "$base/admin/providers" -H 'content-type: application/json' -d '{"id":"bad-operation-url","name":"Bad operation URL","endpoint":{"api_type":"openai_compatible","base_url":"https://example.com/v1/responses","requires_api_key":false,"api_key":null}}') == 400 ]]
# Nested OpenAI-compatible roots, including OpenCode Go's /zen/go/v1 shape, append /models at that shared root.
[[ $(admin_status -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"nested-root\",\"name\":\"Nested root\",\"endpoint\":{\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/nested/v1\",\"requires_api_key\":true,\"api_key\":\"nested\"}}") == 204 ]]
admin -f -X POST "$base/admin/providers/nested-root/models/refresh" >/dev/null
[[ $(admin -f "$base/admin/providers" | jq -r '.[] | select(.id == "nested-root") | .discovered_models[0]') == nested-model ]]
admin -f -X DELETE "$base/admin/providers/nested-root" >/dev/null
for provider in allowed denied; do
  admin -f -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"$provider\",\"name\":\"$provider\",\"endpoint\":{\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:1/v1\",\"requires_api_key\":false,\"api_key\":null}}" >/dev/null
done
admin -f -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"multi\",\"name\":\"Multi endpoint\",\"endpoint\":{\"id\":\"one\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true,\"api_key\":\"one\"}}" >/dev/null
admin -f -X POST "$base/admin/providers/multi/endpoints" -H 'content-type: application/json' -d "{\"id\":\"two\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true,\"api_key\":\"two\"}" >/dev/null
admin -f -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"id":"multi","name":"Renamed provider"}' >/dev/null
[[ $(admin -f "$base/admin/providers" | jq -r '.[] | select(.id == "multi") | [.id, .name] | join(":")') == multi:Renamed\ provider ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"id":"renamed"}') == 400 ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"name":"  "}') == 400 ]]
admin -f -X PATCH "$base/admin/providers/multi/endpoints/two" -H 'content-type: application/json' -d "{\"id\":\"two\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1/\",\"socks5_proxy\":null,\"requires_api_key\":true}" >/dev/null
[[ $(admin -f "$base/admin/providers" | jq -r '.[] | select(.id == "multi") | .endpoints[] | select(.id == "two") | .base_url') == "http://127.0.0.1:$upstream_port/v1" ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi/endpoints/two" -H 'content-type: application/json' -d "{\"id\":\"one\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true}") == 409 ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi/endpoints/two" -H 'content-type: application/json' -d "{\"id\":\"Bad ID\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true}") == 400 ]]
# The mock identifies endpoints from their bearer credentials.
# Discovery and inference must map unique models to the endpoint that reported them.
admin -f -X POST "$base/admin/providers/multi/models/refresh" >/dev/null
providers_json=$(admin -f "$base/admin/providers")
[[ $(printf '%s' "$providers_json" | jq -r '.[] | select(.id == "multi") | .model_endpoints["model-a"][0]') == one ]]
[[ $(printf '%s' "$providers_json" | jq -r '.[] | select(.id == "multi") | .model_endpoints["model-b"][0]') == two ]]
[[ $(printf '%s' "$providers_json" | jq -r '.[] | select(.id == "multi") | .model_endpoints["shared"] | join(",")') == one,two ]]
# Shared models use the first configured compatible endpoint by default and accept an explicit preference.
shared_default=$(curl -sf -D shared-response.headers -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $(admin -f -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"Shared model setup","expires_at":null,"provider_ids":[]}' | jq -r .secret)" -H 'Cookie: yabane_session=caller-secret' -H 'content-type: application/json' -d '{"model":"multi/shared","messages":[]}')
[[ $(printf '%s' "$shared_default" | jq -r .endpoint) == one ]]
[[ $(printf '%s' "$shared_default" | jq -r '.headers.cookie') == null ]]
! grep -qi '^set-cookie:' shared-response.headers
admin -f -X PATCH "$base/admin/providers/multi/model-endpoint-preferences" -H 'content-type: application/json' -d '{"preferences":[{"model":"shared","api_type":"openai_compatible","endpoint_id":"two"}]}' >/dev/null
providers_json=$(admin -f "$base/admin/providers")
[[ $(printf '%s' "$providers_json" | jq -r '.[] | select(.id == "multi") | .model_endpoint_preferences[0].endpoint_id') == two ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi/model-endpoint-preferences" -H 'content-type: application/json' -d '{"preferences":[{"model":"model-a","api_type":"openai_compatible","endpoint_id":"two"}]}') == 400 ]]
# Renaming an Endpoint keeps credentials attached and atomically rewrites every current
# reference without invalidating model discovery when connection settings are unchanged.
admin -f -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"extra_headers":{"x-provider":"yes"},"extra_body":{"extra":"provider"},"defaults_endpoint_ids":["two"]}' >/dev/null
admin -f -X POST "$base/admin/routes" -H 'content-type: application/json' -d '{"pattern":"rename-fixture","targets":[{"provider_id":"multi","endpoint_id":"two","api_key_id":"default","upstream_model":"model-b","weight":100}]}' >/dev/null
admin -f -X PATCH "$base/admin/extensions/traffic-capture/status" -H 'content-type: application/json' -d '{"active":false,"remaining":0,"expires_at":null,"provider_id":"multi","endpoint_id":"two","model":"","body_limit":1024,"retention_days":1,"redacted_headers":[]}' >/dev/null
admin -f -X PATCH "$base/admin/providers/multi/endpoints/two" -H 'content-type: application/json' -d "{\"id\":\"regional\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true}" >/dev/null
providers_json=$(admin -f "$base/admin/providers")
[[ $(printf '%s' "$providers_json" | jq -r '.[] | select(.id == "multi") | .endpoints[] | select(.id == "regional") | .api_keys[0].id') == default ]]
[[ $(printf '%s' "$providers_json" | jq -r '.[] | select(.id == "multi") | [.model_endpoints["model-b"][0], .model_endpoint_preferences[0].endpoint_id, .defaults_endpoint_ids[0]] | join(":")') == regional:regional:regional ]]
[[ $(admin -f "$base/admin/routes" | jq -r '.[] | select(.pattern == "rename-fixture") | .targets[0].endpoint_id') == regional ]]
[[ $(admin -f "$base/admin/extensions/traffic-capture/status" | jq -r .config.endpoint_id) == regional ]]
active_rename_expiry=$(($(date +%s) + 600))
admin -f -X PATCH "$base/admin/extensions/traffic-capture/status" -H 'content-type: application/json' -d "{\"active\":true,\"remaining\":1,\"expires_at\":$active_rename_expiry,\"provider_id\":\"multi\",\"endpoint_id\":\"regional\",\"model\":\"\",\"body_limit\":1024,\"retention_days\":1,\"redacted_headers\":[]}" >/dev/null
[[ $(admin_status -X PATCH "$base/admin/providers/multi/endpoints/regional" -H 'content-type: application/json' -d "{\"id\":\"blocked-while-capturing\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true}") == 409 ]]
admin -f -X POST "$base/admin/extensions/traffic-capture/stop" >/dev/null
admin -f -X PATCH "$base/admin/providers/multi/endpoints/regional" -H 'content-type: application/json' -d "{\"id\":\"two\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true}" >/dev/null
admin -f -X DELETE "$base/admin/routes/rename-fixture" >/dev/null
admin -f -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"extra_headers":{},"extra_body":{},"defaults_endpoint_ids":[]}' >/dev/null
scoped=$(admin -f -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"Scoped","expires_at":null,"provider_ids":["allowed"]}')
scoped_secret=$(printf '%s' "$scoped" | jq -r .secret)
# Provider scope is carried from the authorization middleware to model listing and inference.
model_scoped=$(admin -f -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"Scoped model listing","expires_at":null,"provider_ids":["multi"]}')
model_scoped_id=$(printf '%s' "$model_scoped" | jq -r .api_key.id)
model_scoped_secret=$(printf '%s' "$model_scoped" | jq -r .secret)
multi_provider_scoped=$(admin -f -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"Multi-provider scope","expires_at":null,"provider_ids":["multi","allowed"]}')
multi_provider_scoped_id=$(printf '%s' "$multi_provider_scoped" | jq -r .api_key.id)
scoped_models=$(curl -fsS "$base/v1/models" -H "Authorization: Bearer $model_scoped_secret")
[[ $(printf '%s' "$scoped_models" | jq '.data | length') -gt 0 ]]
[[ $(printf '%s' "$scoped_models" | jq '[.data[] | select(.id | startswith("multi/") | not)] | length') == 0 ]]
[[ $(status -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $scoped_secret" -H 'content-type: application/json' -d '{"model":"denied/model","messages":[]}') == 403 ]]
[[ $(status -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $scoped_secret" -H 'content-type: application/json' -d '{"model":"allowed/model","messages":[]}') == 502 ]]
connection_failure_logs=$(admin -f "$base/admin/activity/logs?since=0&limit=1000")
[[ $(printf '%s' "$connection_failure_logs" | jq '[.[] | select(.model == "allowed/model" and .status == 502 and .gateway_ms != null and .upstream_response_ms != null and .first_byte_ms == null and .failure.stage == "upstream_connect" and .failure.category == "connect_failed")] | length') == 1 ]]
unrestricted=$(admin -f -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"Multi endpoint","expires_at":null,"provider_ids":[]}')
unrestricted_id=$(printf '%s' "$unrestricted" | jq -r .api_key.id)
unrestricted_prefix=$(printf '%s' "$unrestricted" | jq -r .api_key.prefix)
unrestricted_secret=$(printf '%s' "$unrestricted" | jq -r .secret)
# Upstream safety deadlines release requests that stop making progress, including
# streams that keep the TCP connection active with comment-only keepalives.
for timeout_model in timeout-before-headers timeout-idle-stream timeout-keepalive-stream; do
  admin -f -X POST "$base/admin/routes" -H 'content-type: application/json' -d "{\"pattern\":\"$timeout_model\",\"targets\":[{\"provider_id\":\"multi\",\"endpoint_id\":\"one\",\"api_key_id\":\"default\",\"upstream_model\":\"$timeout_model\",\"weight\":100}]}" >/dev/null
done
curl -sS -D timeout-before-headers.headers -o timeout-before-headers.json -w '%{http_code}' -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"timeout-before-headers","messages":[]}' >timeout-before-headers.status & timeout_headers_pid=$!
curl -sS -o timeout-idle-stream.body -w '%{http_code}' -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"timeout-idle-stream","messages":[],"stream":true}' >timeout-idle-stream.status & timeout_idle_pid=$!
curl -sS -o timeout-keepalive-stream.body -w '%{http_code}' -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"timeout-keepalive-stream","messages":[],"stream":true}' >timeout-keepalive-stream.status & timeout_keepalive_pid=$!
wait "$timeout_headers_pid" || true
wait "$timeout_idle_pid" || true
wait "$timeout_keepalive_pid" || true
[[ $(<timeout-before-headers.status) == 502 ]]
[[ $(jq -r '.error.message' timeout-before-headers.json) == "Upstream request timed out" ]]
grep -qi '^x-yabane-error-origin: yabane' timeout-before-headers.headers
[[ $(<timeout-idle-stream.status) == 200 ]]
[[ $(<timeout-keepalive-stream.status) == 200 ]]
timeout_logs=$(admin -f "$base/admin/activity/logs?since=0&limit=1000")
[[ $(printf '%s' "$timeout_logs" | jq '[.[] | select(.model == "timeout-before-headers" and .status == 502 and .failure.stage == "upstream_response" and .failure.category == "timeout")] | length') == 1 ]]
[[ $(printf '%s' "$timeout_logs" | jq '[.[] | select(.model == "timeout-idle-stream" and .status == 502 and .failure.stage == "upstream_stream" and .failure.category == "timeout")] | length') == 1 ]]
[[ $(printf '%s' "$timeout_logs" | jq '[.[] | select(.model == "timeout-keepalive-stream" and .status == 502 and .failure.stage == "upstream_stream" and .failure.category == "timeout")] | length') == 1 ]]
# Cross-protocol adapters let every caller surface use providers with a different native API.
admin -f -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"anthropic-only\",\"name\":\"Anthropic only\",\"endpoint\":{\"id\":\"messages\",\"api_type\":\"anthropic\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"requires_api_key\":true,\"api_key\":\"anthropic\",\"pricing\":{\"models\":{\"endpoint-create-check\":{\"input_per_million\":3,\"output_per_million\":4}}}}}" >/dev/null
[[ $(admin -f "$base/admin/providers" | jq -r 'map(select(.id == "anthropic-only") | (.endpoints[0].pricing.updated_at > 0 and .endpoints[0].pricing.models["endpoint-create-check"].output_per_million == 4)) == [true]') == true ]]
[[ $(admin_status -X PATCH "$base/admin/pricing" -H 'content-type: application/json' -d '{"models":{"claude":{"input_per_million":-1,"output_per_million":2}}}') == 400 ]]
[[ $(admin_status -X PATCH "$base/admin/pricing" -H 'content-type: application/json' -d '{"models":{"claude*-invalid":{"input_per_million":1,"output_per_million":2}}}') == 400 ]]
admin -f -X PATCH "$base/admin/pricing" -H 'content-type: application/json' -d '{"models":{"claude*":{"input_per_million":1,"output_per_million":1.5,"cache_read_per_million":0.5}}}' >/dev/null
[[ $(admin -f "$base/admin/pricing" | jq -r '.updated_at > 0 and .models["claude*"].input_per_million == 1') == true ]]
[[ $(jq -r '.models["claude*"].output_per_million' data/pricing.json) == 1.5 ]]
[[ $(admin_status -X PATCH "$base/admin/pricing/providers/missing" -H 'content-type: application/json' -d '{"models":{}}') == 404 ]]
admin -f -X PATCH "$base/admin/pricing/providers/anthropic-only" -H 'content-type: application/json' -d '{"models":{"claude":{"output_per_million":2}}}' >/dev/null
[[ $(admin -f "$base/admin/providers" | jq -r 'map(select(.id == "anthropic-only") | (.pricing.models.claude.input_per_million == null and .pricing.models.claude.output_per_million == 2)) == [true]') == true ]]
admin -f -X PATCH "$base/admin/pricing/providers/anthropic-only/endpoints/messages" -H 'content-type: application/json' -d '{"models":{"claude":{"cache_read_per_million":0.25}}}' >/dev/null
[[ $(admin -f "$base/admin/providers" | jq -r 'map(select(.id == "anthropic-only") | .endpoints[0].pricing.models.claude.cache_read_per_million == 0.25) == [true]') == true ]]
admin -f -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"openai-chat-only\",\"name\":\"OpenAI Chat only\",\"endpoint\":{\"id\":\"chat\",\"api_type\":\"openai_chat_completions\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"requires_api_key\":true,\"api_key\":\"convert\"}}" >/dev/null
admin -f -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"openai-responses-only\",\"name\":\"OpenAI Responses only\",\"endpoint\":{\"id\":\"responses\",\"api_type\":\"openai_responses\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"requires_api_key\":true,\"api_key\":\"responses-stream\"}}" >/dev/null
chat_converted=$(curl -sf -D conversion.headers -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'OpenAI-Organization: org-caller' -H 'OpenAI-Project: project-caller' -H 'content-type: application/json' -d '{"model":"anthropic-only/claude","messages":[{"role":"system","content":"Be concise"},{"role":"user","content":"hello"}],"max_completion_tokens":64}')
[[ $(printf '%s' "$chat_converted" | jq -r '.choices[0].message.content') == from-anthropic ]]
grep -qi '^x-yabane-protocol-conversion: anthropic_messages->openai_chat_completions' conversion.headers
! grep -Eqi '^(etag|digest|content-encoding):' conversion.headers
responses_converted=$(curl -sf -X POST "$base/v1/responses" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"anthropic-only/claude","input":"hello","max_output_tokens":64}')
[[ $(printf '%s' "$responses_converted" | jq -r '.output[0].content[0].text') == from-anthropic ]]
messages_converted=$(curl -sf -X POST "$base/v1/messages" -H "Authorization: Bearer $unrestricted_secret" -H 'anthropic-version: 2099-01-01' -H 'anthropic-beta: caller-secret-beta' -H 'content-type: application/json' -d '{"model":"openai-chat-only/gpt","messages":[{"role":"user","content":"hello"}],"max_tokens":64}')
[[ $(printf '%s' "$messages_converted" | jq -r '.content[0].text') == from-openai ]]
responses_via_chat=$(curl -sf -X POST "$base/v1/responses" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"openai-chat-only/gpt","input":"hello","max_output_tokens":64,"max_tool_calls":3,"prompt_cache_key":"caller-cache","text":{"format":{"type":"json_schema","name":"answer","schema":{"type":"object"}}},"tools":[{"type":"web_search_preview"},{"type":"function","name":"lookup","parameters":{"type":"object"},"strict":true}],"tool_choice":{"type":"function","name":"lookup"}}')
[[ $(printf '%s' "$responses_via_chat" | jq -r '.output[0].content[0].text') == from-openai ]]
chat_via_streaming_responses=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"openai-responses-only/gpt","messages":[{"role":"user","content":"hello"}],"max_tokens":64,"frequency_penalty":1,"response_format":{"type":"json_schema","json_schema":{"name":"answer","schema":{"type":"object"}}},"tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object"},"strict":true}}],"tool_choice":{"type":"function","function":{"name":"lookup"}}}')
[[ $(printf '%s' "$chat_via_streaming_responses" | jq -r '.choices[0].message.content') == from-responses-stream ]]
[[ $(status -D conversion-failure.headers -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"openai-responses-only/gpt-failed","messages":[{"role":"user","content":"hello"}]}') == 502 ]]
grep -q 'mock overloaded' response.json
# A re-rendered upstream protocol failure is a message Yabane wrote, so it names Yabane.
grep -qi '^x-yabane-error-origin: yabane' conversion-failure.headers
# The same failure received as a native 200 body stays verbatim, with no origin claimed
# before the bytes are read, and is still recorded by its final semantic status.
[[ $(status -D native-failed.headers -X POST "$base/v1/responses" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"openai-responses-only/gpt-failed","input":[{"role":"user","content":"hi"}]}') == 200 ]]
[[ $(jq -r .status response.json) == "failed" ]]
! grep -qi '^x-yabane-error-origin:' native-failed.headers
grep -qi '^x-yabane-request-id: req-' native-failed.headers
[[ $(admin -f "$base/admin/activity/logs?since=0&limit=1000" | jq '[.[] | select(.model == "openai-responses-only/gpt-failed" and .status == 502 and .failure.stage == "upstream_response" and .failure.category == "protocol_failure")] | length') == 1 ]]
# Yabane-owned response headers: the proxy marks who authored an error body and always
# carries the Activity request ID; an upstream cannot forge either of them.
[[ $(status -D provider-error.headers -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/provider-error","messages":[]}') == 500 ]]
grep -qi '^x-yabane-error-origin: upstream' provider-error.headers
grep -qi '^x-yabane-request-id: req-' provider-error.headers
! grep -qi 'req-forged' provider-error.headers
[[ $(jq -r '.error.message' response.json) == "provider exploded" ]]
[[ $(jq -r '.error.type' response.json) == "server_error" ]]
[[ $(status -D yabane-error.headers -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $scoped_secret" -H 'content-type: application/json' -d '{"model":"denied/model","messages":[]}') == 403 ]]
grep -qi '^x-yabane-error-origin: yabane' yabane-error.headers
grep -qi '^x-yabane-request-id: req-' yabane-error.headers
curl -sS -D correlated.headers -o /dev/null -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/model-a","messages":[]}'
request_id=$(grep -i '^x-yabane-request-id:' correlated.headers | tr -d '\r' | awk '{print $2}')
[[ $request_id == req-* ]]
[[ $(admin -f "$base/admin/activity/logs?since=0&limit=1000" | jq --arg id "$request_id" '[.[] | select(.request_id == $id and .status == 200)] | length') == 1 ]]
stream_failure=$(curl -sN -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"openai-responses-only/gpt-stream-failed","messages":[{"role":"user","content":"hello"}],"stream":true}')
[[ $stream_failure == *'mock stream overloaded'* ]]
document_via_responses=$(curl -sf -X POST "$base/v1/messages" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"openai-responses-only/gpt-document","messages":[{"role":"user","content":[{"type":"document","title":"report.pdf","source":{"type":"url","url":"https://example.com/report.pdf"}},{"type":"text","text":"summarize"}]}],"max_tokens":64}')
[[ $(printf '%s' "$document_via_responses" | jq -r '.content[0].text') == from-responses-stream ]]
stream_converted=$(curl -sfN -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"anthropic-only/claude","messages":[{"role":"user","content":"hello"}],"max_completion_tokens":64,"stream":true}')
[[ $stream_converted == *'from-anthropic-stream'* ]]
[[ $stream_converted == *'data: [DONE]'* ]]
conversion_logs=$(admin -f "$base/admin/activity/logs?since=0&limit=1000")
[[ $(printf '%s' "$conversion_logs" | jq '[.[] | select(.caller_protocol == "openai_chat_completions" and .upstream_protocol == "anthropic_messages")] | length') -ge 2 ]]
[[ $(printf '%s' "$conversion_logs" | jq '[.[] | select(.upstream_protocol == "anthropic_messages" and .finish_reason == "end_turn")] | length') -ge 2 ]]
[[ $(printf '%s' "$conversion_logs" | jq '[.[] | select(.model == "anthropic-only/claude" and .cost_source == "estimated" and .cost == 0.000011)] | length') -ge 2 ]]
[[ $(printf '%s' "$conversion_logs" | jq '[.[] | select(.model == "multi/model-a" and .cost_source == "reported" and .cost == 0.0042)] | length') -ge 1 ]]
[[ $(printf '%s' "$conversion_logs" | jq '[.[] | select(.model == "openai-chat-only/gpt" and .finish_reason == "stop")] | length') -ge 1 ]]
[[ $(printf '%s' "$conversion_logs" | jq '[.[] | select(.model == "openai-responses-only/gpt" and .finish_reason == "completed")] | length') -ge 1 ]]
[[ $(printf '%s' "$conversion_logs" | jq '[.[] | select(.model == "openai-responses-only/gpt-failed" and .status == 502 and .failure.stage == "protocol_conversion" and .failure.category == "invalid_response")] | length') == 1 ]]
[[ $(printf '%s' "$conversion_logs" | jq '[.[] | select(.model == "openai-responses-only/gpt-stream-failed" and .status == 502 and .streaming == true and .failure.stage == "upstream_stream" and .failure.category == "interrupted")] | length') == 1 ]]
[[ $(printf '%s' "$conversion_logs" | jq '[.[] | select(.model == "anthropic-only/claude" and .gateway_ms != null and .upstream_response_ms != null and .first_byte_ms != null and .generation_ms != null and .latency_ms >= .first_byte_ms)] | length') == 1 ]]
preferred_shared=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/shared","messages":[]}')
[[ $(printf '%s' "$preferred_shared" | jq -r .endpoint) == two ]]
model_a=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/model-a","messages":[]}')
model_b=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/model-b","messages":[]}')
[[ $(printf '%s' "$model_a" | jq -r .endpoint) == one ]]
[[ $(printf '%s' "$model_b" | jq -r .endpoint) == two ]]
[[ $(printf '%s' "$model_a" | jq -r .model) == model-a ]]
admin -f -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"extra_headers":{"x-provider":"yes"},"extra_body":{"extra":"provider"},"defaults_endpoint_ids":["one"]}' >/dev/null
admin -f -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"name":"Renamed without replacing defaults"}' >/dev/null
[[ $(admin -f "$base/admin/providers" | jq -r '.[] | select(.id == "multi") | [.name, .extra_headers["x-provider"], .extra_body.extra, .defaults_endpoint_ids[0]] | join(":")') == Renamed\ without\ replacing\ defaults:yes:provider:one ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"extra_headers":{"authorization":"unsafe"},"extra_body":{}}') == 400 ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"extra_headers":{"cookie":"unsafe"},"extra_body":{}}') == 400 ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"extra_headers":{"chatgpt-account-id":"unsafe"},"extra_body":{}}') == 400 ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"extra_headers":{"bad header":"unsafe"},"extra_body":{}}') == 400 ]]
# Routes can explicitly target an Endpoint that is configured not to require authentication.
admin -f -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"keyless\",\"name\":\"Keyless local\",\"endpoint\":{\"id\":\"local\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"requires_api_key\":false,\"api_key\":null}}" >/dev/null
keyless_route_payload='{"pattern":"local-alias","targets":[{"provider_id":"keyless","endpoint_id":"local","api_key_id":"","upstream_model":"local-model","weight":100}]}'
admin -f -X POST "$base/admin/routes" -H 'content-type: application/json' -d "$keyless_route_payload" >/dev/null
[[ $(admin -f "$base/admin/routes" | jq -r '.[] | select(.pattern == "local-alias") | .targets[0].api_key_id') == "" ]]
keyless_response=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"local-alias","messages":[]}')
[[ $(printf '%s' "$keyless_response" | jq -r .endpoint) == unknown ]]
[[ $(printf '%s' "$keyless_response" | jq -r .model) == local-model ]]
[[ $(admin_status -X POST "$base/admin/routes" -H 'content-type: application/json' -d '{"pattern":"bad-keyless-route","targets":[{"provider_id":"keyless","endpoint_id":"local","api_key_id":"invented","upstream_model":"local-model","weight":100}]}') == 400 ]]
[[ $(admin_status -X PATCH "$base/admin/providers/keyless/endpoints/local" -H 'content-type: application/json' -d "{\"id\":\"local\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"requires_api_key\":true}") == 409 ]]
# Removing an Endpoint's authentication requirement atomically clears route credential references.
admin -f -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"auth-switch\",\"name\":\"Authentication switch\",\"endpoint\":{\"id\":\"local\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"requires_api_key\":true,\"api_key\":\"switch-secret\"}}" >/dev/null
admin -f -X POST "$base/admin/routes" -H 'content-type: application/json' -d '{"pattern":"auth-switch-alias","targets":[{"provider_id":"auth-switch","endpoint_id":"local","api_key_id":"default","upstream_model":"switch-model","weight":100}]}' >/dev/null
admin -f -X PATCH "$base/admin/providers/auth-switch/endpoints/local" -H 'content-type: application/json' -d "{\"id\":\"local\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"requires_api_key\":false}" >/dev/null
[[ $(admin -f "$base/admin/routes" | jq -r '.[] | select(.pattern == "auth-switch-alias") | .targets[0].api_key_id') == "" ]]
auth_switch_response=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"auth-switch-alias","messages":[]}')
[[ $(printf '%s' "$auth_switch_response" | jq -r .model) == switch-model ]]
route_payload='{"pattern":"friendly-model","targets":[{"provider_id":"multi","endpoint_id":"one","api_key_id":"default","upstream_model":"model-a","weight":50},{"provider_id":"multi","endpoint_id":"two","api_key_id":"default","upstream_model":"model-b","weight":50}]}'
[[ $(admin_status -X POST "$base/admin/routes" -H 'content-type: application/json' -d '{"pattern":"bad-prefixed-model","targets":[{"provider_id":"multi","endpoint_id":"one","api_key_id":"default","upstream_model":"multi/model-a","weight":1}]}') == 400 ]]
admin -f -X POST "$base/admin/routes" -H 'content-type: application/json' -d "$route_payload" >/dev/null
# Existing routes can be edited, including renaming the public pattern and replacing destinations.
admin -f -X PATCH "$base/admin/routes/friendly-model" -H 'content-type: application/json' -d '{"pattern":"friendly-model-edited","targets":[{"provider_id":"multi","endpoint_id":"one","api_key_id":"default","upstream_model":"custom-model-not-discovered","weight":100}]}' >/dev/null
[[ $(admin -f "$base/admin/routes" | jq -r '.[] | select(.pattern == "friendly-model-edited") | .targets[0].upstream_model') == custom-model-not-discovered ]]
[[ $(admin_status -X PATCH "$base/admin/routes/missing-route" -H 'content-type: application/json' -d "$route_payload") == 404 ]]
admin -f -X PATCH "$base/admin/routes/friendly-model-edited" -H 'content-type: application/json' -d "$route_payload" >/dev/null
friendly_one=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"friendly-model","messages":[]}')
friendly_two=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"friendly-model","messages":[]}')
[[ $(printf '%s' "$friendly_one" | jq -r .endpoint) == one ]]
[[ $(printf '%s' "$friendly_two" | jq -r .endpoint) == two ]]
[[ $(printf '%s' "$friendly_one" | jq -r .extra) == provider ]]
[[ $(printf '%s' "$friendly_one" | jq -r '.headers["x-provider"]') == yes ]]
[[ $(printf '%s' "$friendly_two" | jq -r .extra) == null ]]
[[ $(printf '%s' "$friendly_two" | jq -r '.headers["x-provider"]') == null ]]
# Activity preserves both the caller's alias and the exact destination model selected by weighted routing.
friendly_activity=$(admin -f "$base/admin/activity/logs?since=0&limit=1000")
[[ $(printf '%s' "$friendly_activity" | jq '[.[] | select(.model == "friendly-model" and .provider == "multi" and .endpoint == "one" and .upstream_model == "model-a")] | length') -ge 1 ]]
[[ $(printf '%s' "$friendly_activity" | jq '[.[] | select(.model == "friendly-model" and .provider == "multi" and .endpoint == "two" and .upstream_model == "model-b")] | length') -ge 1 ]]
[[ $(admin -f "$base/admin/activity/logs/page?since=0&query=model-b&limit=100" | jq '[.data[] | select(.model == "friendly-model" and .upstream_model == "model-b")] | length') -ge 1 ]]
# Runtime disabling preserves Request Defaults configuration but bypasses all of its Hooks.
[[ $(admin_status -X PATCH "$base/admin/extensions/missing" -H 'content-type: application/json' -d '{"enabled":false}') == 404 ]]
disabled_extension=$(admin -f -X PATCH "$base/admin/extensions/request-defaults" -H 'content-type: application/json' -d '{"enabled":false}')
[[ $(printf '%s' "$disabled_extension" | jq -r '.enabled') == false ]]
[[ $(jq -r '.enabled["request-defaults"]' data/extensions.json) == false ]]
without_defaults=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/model-a","messages":[]}')
[[ $(printf '%s' "$without_defaults" | jq -r .extra) == null ]]
[[ $(printf '%s' "$without_defaults" | jq -r '.headers["x-provider"]') == null ]]
admin -f -X PATCH "$base/admin/extensions/request-defaults" -H 'content-type: application/json' -d '{"enabled":true}' >/dev/null
[[ $(admin -f "$base/admin/providers" | jq -r '.[] | select(.id == "multi") | .extra_body.extra') == provider ]]
# Traffic Capture is explicitly scoped and stopped by default. It observes final upstream
# bytes, redacts both Core-managed and configured headers, and stays outside Activity.
capture_expiry=$(($(date +%s) + 600))
[[ $(admin_status -X PATCH "$base/admin/extensions/traffic-capture/status" -H 'content-type: application/json' -d "{\"active\":true,\"remaining\":101,\"expires_at\":$capture_expiry,\"provider_id\":\"multi\",\"endpoint_id\":\"one\",\"model\":\"multi/model-a\",\"body_limit\":1024,\"retention_days\":1,\"redacted_headers\":[]}") == 400 ]]
[[ $(jq -r '.error.message' response.json) == "Capture count must be between 1 and 100" ]]
admin -f -X PATCH "$base/admin/extensions/traffic-capture/status" -H 'content-type: application/json' -d "{\"active\":true,\"remaining\":1,\"expires_at\":$capture_expiry,\"provider_id\":\"multi\",\"endpoint_id\":\"one\",\"model\":\"multi/model-a\",\"body_limit\":1024,\"retention_days\":1,\"redacted_headers\":[\"x-provider\"]}" >/dev/null
capture_response=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/model-a","messages":[{"role":"user","content":"traffic-capture-secret-marker"}]}')
[[ $(printf '%s' "$capture_response" | jq -r .endpoint) == one ]]
capture_id=
for _ in $(seq 1 50); do
  capture_id=$(admin -f "$base/admin/extensions/traffic-capture/captures" | jq -r '.[0].request_id // empty')
  [[ -n "$capture_id" ]] && break
  sleep .02
done
[[ -n "$capture_id" ]]
capture=$(admin -f "$base/admin/extensions/traffic-capture/captures/$capture_id")
[[ $(printf '%s' "$capture" | jq -r '[.status, .outcome, .request_truncated, .response_truncated] | join(":")') == 200:complete:false:false ]]
[[ $(printf '%s' "$capture" | jq -r '.duration_ms | type') == number ]]
[[ $(admin -f "$base/admin/extensions/traffic-capture/captures" | jq -r '.[0].duration_ms | type') == number ]]
[[ $(printf '%s' "$capture" | jq -r '.request_body | implode | fromjson | [.model, .extra, .messages[0].content] | join(":")') == model-a:provider:traffic-capture-secret-marker ]]
[[ $(printf '%s' "$capture" | jq -r '.response_body | implode | fromjson | [.endpoint, .model] | join(":")') == one:model-a ]]
[[ $(printf '%s' "$capture" | jq -r '.request_headers[] | select(.name == "authorization") | .value') == '[REDACTED]' ]]
[[ $(printf '%s' "$capture" | jq -r '.request_headers[] | select(.name == "x-provider") | .value') == '[REDACTED]' ]]
[[ $(printf '%s' "$capture" | jq -r '.response_headers[] | select(.name == "set-cookie") | .value') == '[REDACTED]' ]]
[[ $(admin -f "$base/admin/extensions/traffic-capture/status" | jq -r '[.config.active, .config.remaining, .retained] | join(":")') == false:0:1 ]]
! admin -f "$base/admin/activity/export?since=0" | grep -q 'traffic-capture-secret-marker'
admin -f -X DELETE "$base/admin/extensions/traffic-capture/captures/$capture_id" >/dev/null
[[ $(admin -f "$base/admin/extensions/traffic-capture/captures" | jq length) == 0 ]]
admin -f -X PATCH "$base/admin/extensions/traffic-capture" -H 'content-type: application/json' -d '{"enabled":false}' >/dev/null
[[ $(admin_status -X PATCH "$base/admin/extensions/traffic-capture/status" -H 'content-type: application/json' -d "{\"active\":true,\"remaining\":1,\"expires_at\":$capture_expiry,\"provider_id\":\"multi\",\"endpoint_id\":\"one\",\"model\":\"\",\"body_limit\":1024,\"retention_days\":1,\"redacted_headers\":[]}") == 409 ]]
admin -f -X PATCH "$base/admin/extensions/traffic-capture" -H 'content-type: application/json' -d '{"enabled":true}' >/dev/null
# Active capture must not silently become inert or orphaned.
admin -f -X PATCH "$base/admin/extensions/traffic-capture/status" -H 'content-type: application/json' -d "{\"active\":true,\"remaining\":1,\"expires_at\":$capture_expiry,\"provider_id\":\"multi\",\"endpoint_id\":\"one\",\"model\":\"\",\"body_limit\":1024,\"retention_days\":1,\"redacted_headers\":[]}" >/dev/null
[[ $(admin_status -X PATCH "$base/admin/extensions/traffic-capture" -H 'content-type: application/json' -d '{"enabled":false}') == 409 ]]
[[ $(admin_status -X DELETE "$base/admin/providers/multi/endpoints/one") == 409 ]]
[[ $(admin_status -X DELETE "$base/admin/providers/multi") == 409 ]]
admin -f -X POST "$base/admin/extensions/traffic-capture/stop" >/dev/null
# Request Defaults is an extension-owned policy; Endpoint-level values override Provider defaults inside its own crate.
# Yabane's E2E only confirms that the compiled extension is wired into the Hook pipeline and scoped routing context.
# Disabled route targets remain configured and receive no traffic, enabling an instant A/B cutover.
disabled_route_payload='{"pattern":"friendly-model","targets":[{"provider_id":"multi","endpoint_id":"one","api_key_id":"default","upstream_model":"model-a","weight":0,"enabled":false},{"provider_id":"multi","endpoint_id":"two","api_key_id":"default","upstream_model":"model-b","weight":100,"enabled":true}]}'
admin -f -X PATCH "$base/admin/routes/friendly-model" -H 'content-type: application/json' -d "$disabled_route_payload" >/dev/null
[[ $(admin -f "$base/admin/routes" | jq -r '.[] | select(.pattern == "friendly-model") | [.targets[].enabled] | join(",")') == false,true ]]
for _ in $(seq 1 4); do
  switched=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"friendly-model","messages":[]}')
  [[ $(printf '%s' "$switched" | jq -r .endpoint) == two ]]
done
[[ $(admin_status -X PATCH "$base/admin/routes/friendly-model" -H 'content-type: application/json' -d '{"pattern":"friendly-model","targets":[{"provider_id":"multi","endpoint_id":"one","api_key_id":"default","upstream_model":"model-a","weight":0,"enabled":false},{"provider_id":"multi","endpoint_id":"two","api_key_id":"default","upstream_model":"model-b","weight":0,"enabled":false}]}') == 400 ]]
admin -f -X PATCH "$base/admin/routes/friendly-model" -H 'content-type: application/json' -d '{"pattern":"friendly-model","targets":[{"provider_id":"multi","endpoint_id":"one","api_key_id":"default","upstream_model":"model-a","weight":0,"enabled":true},{"provider_id":"multi","endpoint_id":"two","api_key_id":"default","upstream_model":"model-b","weight":100,"enabled":true}]}' >/dev/null
[[ $(admin -f "$base/admin/routes" | jq -r '.[] | select(.pattern == "friendly-model") | [.targets[0].weight, .targets[0].enabled] | join(",")') == 0,false ]]
[[ $(admin_status -X PATCH "$base/admin/routes/friendly-model" -H 'content-type: application/json' -d '{"pattern":"friendly-model","targets":[{"provider_id":"multi","endpoint_id":"one","api_key_id":"default","upstream_model":"model-a","weight":60,"enabled":true},{"provider_id":"multi","endpoint_id":"two","api_key_id":"default","upstream_model":"model-b","weight":30,"enabled":true}]}') == 400 ]]
# A key referenced by any retained route target cannot be disabled, including a disabled target kept for cutover.
[[ $(admin_status -X PATCH "$base/admin/providers/multi/endpoints/one/keys/default" -H 'content-type: application/json' -d '{"enabled":false}') == 409 ]]
# Endpoint identity is part of an upstream-key mutation, because key IDs are only endpoint-local.
admin -f -X POST "$base/admin/providers/multi/keys" -H 'content-type: application/json' -d '{"endpoint_id":"two","name":"Temporary","secret":"temporary","weight":10}' >/dev/null
[[ $(admin_status -X PATCH "$base/admin/providers/multi/endpoints/one/keys/temporary" -H 'content-type: application/json' -d '{"enabled":false}') == 404 ]]
admin -f -X PATCH "$base/admin/providers/multi/endpoints/two/traffic" -H 'content-type: application/json' -d '{"weights":[{"key_id":"default","weight":75},{"key_id":"temporary","weight":25}]}' >/dev/null
[[ $(admin -f "$base/admin/providers" | jq -r '.[] | select(.id == "multi") | .endpoints[] | select(.id == "two") | [.api_keys[] | .weight] | sort | join(",")') == 25,75 ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi/endpoints/two/traffic" -H 'content-type: application/json' -d '{"weights":[{"key_id":"default","weight":60},{"key_id":"temporary","weight":30}]}') == 400 ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi/endpoints/two/traffic" -H 'content-type: application/json' -d '{"weights":[{"key_id":"missing","weight":100}]}') == 400 ]]
admin -f -X DELETE "$base/admin/providers/multi/endpoints/two/keys/temporary" >/dev/null
[[ $(admin_status -X PATCH "$base/admin/providers/multi/keys/default" -H 'content-type: application/json' -d '{"enabled":false}') == 404 ]]
# Deleting a key also removes global-route destinations that refer to that exact endpoint key.
admin -f -X POST "$base/admin/providers/multi/keys" -H 'content-type: application/json' -d '{"endpoint_id":"two","name":"Routed temporary","secret":"routed-temporary","weight":10}' >/dev/null
admin -f -X POST "$base/admin/routes" -H 'content-type: application/json' -d '{"pattern":"temporary-route","targets":[{"provider_id":"multi","endpoint_id":"two","api_key_id":"routed-temporary","upstream_model":"model-b","weight":100}]}' >/dev/null
admin -f -X DELETE "$base/admin/providers/multi/endpoints/two/keys/routed-temporary" >/dev/null
[[ $(admin -f "$base/admin/routes" | jq '[.[] | select(.pattern == "temporary-route")] | length') == 0 ]]
# Explicit cost refresh recalculates estimated values, fills missing values, and preserves
# both upstream-reported costs and legacy costs without cost_source.
admin -f -X PATCH "$base/admin/pricing/providers/multi" -H 'content-type: application/json' -d '{"models":{"model-a":{"input_per_million":1,"output_per_million":2}}}' >/dev/null
cost_refresh_at=$(date +%s)
cost_refresh_payload=$(jq -cn --argjson at "$cost_refresh_at" '{format:"yabane-activity",version:1,instance_id:"cost-refresh-test",records:[
  {timestamp:$at,request_id:"req-cost-estimated",path:"/v1/chat/completions",model:"multi/model-a",upstream_model:"model-a",provider:"multi",endpoint:"one",status:200,latency_ms:1,input_tokens:1000,output_tokens:1000,cached_tokens:0,cost:9,cost_source:"estimated",streaming:false},
  {timestamp:$at,request_id:"req-cost-missing",path:"/v1/chat/completions",model:"multi/model-a",upstream_model:"model-a",provider:"multi",endpoint:"one",status:200,latency_ms:1,input_tokens:1000,output_tokens:1000,cached_tokens:0,cost:null,streaming:false},
  {timestamp:$at,request_id:"req-cost-reported",path:"/v1/chat/completions",model:"multi/model-a",upstream_model:"model-a",provider:"multi",endpoint:"one",status:200,latency_ms:1,input_tokens:1000,output_tokens:1000,cached_tokens:0,cost:7,cost_source:"reported",streaming:false},
  {timestamp:$at,request_id:"req-cost-legacy",path:"/v1/chat/completions",model:"multi/model-a",upstream_model:"model-a",provider:"multi",endpoint:"one",status:200,latency_ms:1,input_tokens:1000,output_tokens:1000,cached_tokens:0,cost:8,streaming:false}
]}')
[[ $(admin -f -X POST "$base/admin/activity/import" -H 'content-type: application/json' -d "$cost_refresh_payload" | jq -r .imported) == 4 ]]
cost_collision_payload=$(jq -cn --argjson at "$cost_refresh_at" '{format:"yabane-activity",version:1,instance_id:"cost-refresh-collision",records:[
  {timestamp:$at,request_id:"req-cost-estimated",path:"/v1/chat/completions",model:"multi/model-a",upstream_model:"model-a",provider:"multi",endpoint:"one",status:200,latency_ms:1,input_tokens:1000,output_tokens:1000,cached_tokens:0,cost:11,cost_source:"estimated",streaming:false}
]}')
[[ $(admin -f -X POST "$base/admin/activity/import" -H 'content-type: application/json' -d "$cost_collision_payload" | jq -r .imported) == 1 ]]
cost_recalculated=$(admin -f -X POST "$base/admin/activity/recalculate-costs" -H 'content-type: application/json' -d '{"request_id":"req-cost-estimated","source_instance_id":"cost-refresh-test"}')
[[ $(printf '%s' "$cost_recalculated" | jq -r '[.updated,.filled,.recalculated,.reported_preserved] | join(":")') == 1:0:1:0 ]]
cost_filled=$(admin -f -X POST "$base/admin/activity/recalculate-costs" -H 'content-type: application/json' -d '{"request_id":"req-cost-missing","source_instance_id":"cost-refresh-test"}')
[[ $(printf '%s' "$cost_filled" | jq -r '[.updated,.filled,.recalculated] | join(":")') == 1:1:0 ]]
[[ $(admin -f -X POST "$base/admin/activity/recalculate-costs" -H 'content-type: application/json' -d '{"request_id":"req-cost-reported","source_instance_id":"cost-refresh-test"}' | jq -r '[.updated,.reported_preserved] | join(":")') == 0:1 ]]
[[ $(admin -f -X POST "$base/admin/activity/recalculate-costs" -H 'content-type: application/json' -d '{"request_id":"req-cost-legacy","source_instance_id":"cost-refresh-test"}' | jq -r '[.updated,.reported_preserved] | join(":")') == 0:1 ]]
refreshed_cost_logs=$(admin -f "$base/admin/activity/logs?since=0&limit=1000")
[[ $(printf '%s' "$refreshed_cost_logs" | jq -r 'any(.[]; .request_id == "req-cost-estimated" and .source_instance_id == "cost-refresh-test" and .cost == 0.003 and .cost_source == "estimated")') == true ]]
[[ $(printf '%s' "$refreshed_cost_logs" | jq -r 'any(.[]; .request_id == "req-cost-estimated" and .source_instance_id == "cost-refresh-collision" and .cost == 11 and .cost_source == "estimated")') == true ]]
[[ $(printf '%s' "$refreshed_cost_logs" | jq -r 'any(.[]; .request_id == "req-cost-missing" and .source_instance_id == "cost-refresh-test" and .cost == 0.003 and .cost_source == "estimated")') == true ]]
[[ $(printf '%s' "$refreshed_cost_logs" | jq -r 'any(.[]; .request_id == "req-cost-reported" and .source_instance_id == "cost-refresh-test" and .cost == 7 and .cost_source == "reported")') == true ]]
[[ $(printf '%s' "$refreshed_cost_logs" | jq -r 'any(.[]; .request_id == "req-cost-legacy" and .source_instance_id == "cost-refresh-test" and .cost == 8 and .cost_source == null)') == true ]]
admin -f -X PATCH "$base/admin/pricing/providers/multi" -H 'content-type: application/json' -d '{"models":{"model-a":{"input_per_million":2,"output_per_million":4}}}' >/dev/null
[[ $(admin -f -X POST "$base/admin/activity/recalculate-costs" -H 'content-type: application/json' -d '{"request_id":"req-cost-estimated","source_instance_id":"cost-refresh-test"}' | jq -r '[.updated,.recalculated] | join(":")') == 1:1 ]]
[[ $(admin -f "$base/admin/activity/logs?since=0&limit=1000" | jq -r 'any(.[]; .request_id == "req-cost-estimated" and .source_instance_id == "cost-refresh-test" and .cost == 0.006 and .cost_source == "estimated")') == true ]]
[[ $(curl -fsS "$base/openapi.json" | jq -r '.paths["/admin/activity/recalculate-costs"].post.responses | has("500")') == true ]]
# Deleting an endpoint removes its keys, discovery availability, and exact route destinations.
admin -f -X POST "$base/admin/providers/multi/keys" -H 'content-type: application/json' -d '{"endpoint_id":"two","name":"Endpoint deletion route","secret":"endpoint-deletion-route","weight":10}' >/dev/null
admin -f -X POST "$base/admin/routes" -H 'content-type: application/json' -d '{"pattern":"endpoint-deletion-route","targets":[{"provider_id":"multi","endpoint_id":"two","api_key_id":"endpoint-deletion-route","upstream_model":"model-b","weight":100}]}' >/dev/null
admin -f -X PATCH "$base/admin/extensions/traffic-capture/status" -H 'content-type: application/json' -d '{"active":false,"remaining":0,"expires_at":null,"provider_id":"multi","endpoint_id":"two","model":"","body_limit":1024,"retention_days":1,"redacted_headers":[]}' >/dev/null
admin -f -X PATCH "$base/admin/pricing/providers/multi/endpoints/two" -H 'content-type: application/json' -d '{"models":{"model-b*":{"input_per_million":0.2,"output_per_million":0.8}}}' >/dev/null
admin -f -X DELETE "$base/admin/providers/multi/endpoints/two" >/dev/null
[[ $(admin -f "$base/admin/providers" | jq '[.[] | select(.id == "multi") | .endpoints[] | select(.id == "two")] | length') == 0 ]]
[[ $(admin -f "$base/admin/providers" | jq -r '.[] | select(.id == "multi") | has("model_endpoints") and (.model_endpoints | has("model-b") | not)') == true ]]
[[ $(admin -f "$base/admin/routes" | jq '[.[] | select(.pattern == "endpoint-deletion-route")] | length') == 0 ]]
[[ $(admin -f "$base/admin/extensions/traffic-capture/status" | jq -r '[.config.provider_id, .config.endpoint_id] | join(":")') == : ]]
[[ $(admin_status -X PATCH "$base/admin/pricing/providers/multi/endpoints/two" -H 'content-type: application/json' -d '{"models":{}}') == 404 ]]
[[ $(jq '[.[] | select(.id == "multi") | .endpoints[] | select(.id == "two") | .pricing] | length' data/providers.json) == 0 ]]
[[ $(admin_status -X DELETE "$base/admin/providers/multi/endpoints/missing") == 404 ]]
# Deleting a Provider removes every route destination that refers to it.
admin -f -X POST "$base/admin/routes" -H 'content-type: application/json' -d '{"pattern":"provider-deletion-route","targets":[{"provider_id":"multi","endpoint_id":"one","api_key_id":"default","upstream_model":"model-a","weight":100}]}' >/dev/null
admin -f -X PATCH "$base/admin/pricing/providers/multi" -H 'content-type: application/json' -d '{"models":{"model-a*":{"input_per_million":0.3,"output_per_million":0.9}}}' >/dev/null
admin -f -X DELETE "$base/admin/providers/denied" >/dev/null
admin -f -X DELETE "$base/admin/providers/multi" >/dev/null
[[ $(admin -f "$base/admin/routes" | jq '[.[] | select(.pattern == "provider-deletion-route")] | length') == 0 ]]
[[ $(admin_status -X PATCH "$base/admin/pricing/providers/multi" -H 'content-type: application/json' -d '{"models":{}}') == 404 ]]
[[ $(jq '[.[] | select(.id == "multi") | .pricing] | length' data/providers.json) == 0 ]]
[[ $(admin -f "$base/admin/pricing" | jq -r '.models["claude*"].input_per_million == 1') == true ]]
# Deletion cannot turn an exact Provider scope into the empty-list "all Providers" meaning.
# Keys scoped only to the deleted Provider are revoked, while multi-Provider scopes retain
# their remaining allowlist entries.
[[ $(admin -f "$base/admin/auth" | jq --arg id "$model_scoped_id" '[.api_keys[] | select(.id == $id)] | length') == 0 ]]
[[ $(status "$base/v1/models" -H "Authorization: Bearer $model_scoped_secret") == 401 ]]
[[ $(admin -f "$base/admin/auth" | jq -r --arg id "$multi_provider_scoped_id" '.api_keys[] | select(.id == $id) | .provider_ids | join(",")') == allowed ]]
# Recreate the Provider needed by the remaining Activity checks.
admin -f -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"multi\",\"name\":\"Multi endpoint\",\"endpoint\":{\"id\":\"one\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true,\"api_key\":\"one\"}}" >/dev/null
# A completed batch is appended in groups of ten without depending on earlier Activity totals.
before_batch_count=$(wc -l < data/activity.jsonl | tr -d ' ')
after_batch_count=$before_batch_count
for _ in $(seq 1 10); do
  curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/model-a","messages":[]}' >/dev/null
  after_batch_count=$(wc -l < data/activity.jsonl | tr -d ' ')
  [[ $after_batch_count -gt $before_batch_count ]] && break
done
[[ $((after_batch_count - before_batch_count)) -eq 10 ]]
stats=$(admin -f "$base/admin/activity/stats?since=0&buckets=24")
[[ $(printf '%s' "$stats" | jq -r .requests) -ge 10 ]]
[[ $(curl -fsS "$base/openapi.json" | jq -r '.components.schemas.Stats.required | index("provider_buckets") != null') == true ]]
[[ $(curl -fsS "$base/openapi.json" | jq -r '.components.schemas.Stats.required | index("filter_options") != null') == true ]]
[[ $(curl -fsS "$base/openapi.json" | jq -r '.components.schemas.Stats.required | index("reported_requests") != null and index("estimated_requests") != null') == true ]]
[[ $(curl -fsS "$base/openapi.json" | jq -r '.components.schemas.RequestLog.properties.cost_source.enum | join(",")') == reported,estimated ]]
[[ $(curl -fsS "$base/openapi.json" | jq -r '.components.schemas.ProviderView.properties.pricing.allOf[0]."$ref"') == '#/components/schemas/PricingTable' ]]
[[ $(curl -fsS "$base/openapi.json" | jq -r '.paths["/admin/pricing"].get.responses["200"].content["application/json"].schema."$ref"') == '#/components/schemas/PricingTable' ]]
[[ $(curl -fsS "$base/openapi.json" | jq -r '.paths["/admin/activity/recalculate-costs"].post.requestBody.content["application/json"].schema.properties | has("source_instance_id")') == true ]]
[[ $(admin_status -X POST "$base/admin/activity/recalculate-costs" -H 'content-type: application/json' -d '{"source_instance_id":"remote-only"}') == 400 ]]
curl -fsS "$base/model-pricing" -o "$work/model-pricing.html"
grep -q 'id="pricing-view"' "$work/model-pricing.html"
[[ $(curl -fsS "$base/openapi.json" | jq -r '.components.schemas.Stats.properties.provider_buckets.items."$ref"') == '#/components/schemas/ActivityProviderBuckets' ]]
[[ $(curl -fsS "$base/openapi.json" | jq -r '.components.schemas.ActivityBucket.properties | has("input") and has("output")') == true ]]
[[ $(printf '%s' "$stats" | jq -r .input_tokens) -ge 4800 ]]
[[ $(printf '%s' "$stats" | jq -r '.buckets | length') == 24 ]]
[[ $(printf '%s' "$stats" | jq -r '[.buckets[] | select(.tokens != (.input + .output))] | length') == 0 ]]
[[ $(printf '%s' "$stats" | jq -r '.filter_options.providers | index("multi") != null') == true ]]
[[ $(printf '%s' "$stats" | jq -r --arg id "$unrestricted_id" --arg prefix "$unrestricted_prefix" '[.filter_options.api_keys[] | select(.id == $id and .prefix == $prefix)] | length') == 1 ]]
! printf '%s' "$stats" | grep -Fq "$unrestricted_secret"
[[ $(printf '%s' "$stats" | jq -r '[.buckets[].requests] | add') == $(printf '%s' "$stats" | jq -r .requests) ]]
[[ $(printf '%s' "$stats" | jq -r '.provider_buckets[] | select(.name == "multi") | .requests | length') == 24 ]]
[[ $(printf '%s' "$stats" | jq -r '.provider_buckets[] | select(.name == "multi") | [.requests[]] | add') == $(printf '%s' "$stats" | jq -r '.by_provider[] | select(.name == "multi") | .requests') ]]
[[ $(printf '%s' "$stats" | jq -r '.by_model | length > 0') == true ]]
[[ $(printf '%s' "$stats" | jq -r '.priced_requests > 0') == true ]]
[[ $(printf '%s' "$stats" | jq -r '.priced_requests == (.reported_requests + .estimated_requests)') == true ]]
[[ $(printf '%s' "$stats" | jq -r '.reported_requests > 0 and .estimated_requests > 0') == true ]]
[[ $(printf '%s' "$stats" | jq -r '[.buckets[] | select(.priced_requests != (.reported_requests + .estimated_requests))] | length') == 0 ]]
[[ $(printf '%s' "$stats" | jq -r '[.by_provider[], .by_model[], .by_api_key[] | select(.priced_requests != (.reported_requests + .estimated_requests))] | length') == 0 ]]
[[ $(printf '%s' "$stats" | jq -r '.priced_requests as $priced | ([.buckets[].priced_requests] | add) == $priced') == true ]]
[[ $(printf '%s' "$stats" | jq -r '.priced_requests as $priced | ([.by_model[].priced_requests] | add) == $priced') == true ]]
[[ $(printf '%s' "$stats" | jq -r --arg id "$unrestricted_id" '[.by_api_key[] | select(.id == $id and .name == "Multi endpoint")] | length > 0') == true ]]
[[ $(printf '%s' "$stats" | jq -r '.requests as $requests | ([.by_api_key[].requests] | add) == $requests') == true ]]
filtered_stats=$(admin -f "$base/admin/activity/stats?since=0&provider=multi&buckets=12")
[[ $(printf '%s' "$filtered_stats" | jq -r '.buckets | length') == 12 ]]
[[ $(printf '%s' "$filtered_stats" | jq '[.by_provider[] | select(.name != "multi")] | length') == 0 ]]
multi_filtered_stats=$(admin -f "$base/admin/activity/stats?since=0&providers=multi&models=multi/model-a&api_keys=$unrestricted_id&buckets=12")
[[ $(printf '%s' "$multi_filtered_stats" | jq -r '.requests > 0') == true ]]
[[ $(printf '%s' "$multi_filtered_stats" | jq '[.by_provider[] | select(.name != "multi")] + [.by_model[] | select(.name != "multi/model-a")] | length') == 0 ]]
[[ $(printf '%s' "$multi_filtered_stats" | jq -r '.filter_options.providers | length > 1') == true ]]
multi_filtered_logs=$(admin -f "$base/admin/activity/logs?since=0&providers=multi&models=multi/model-a&api_keys=$unrestricted_id&limit=1000")
[[ $(printf '%s' "$multi_filtered_logs" | jq --arg id "$unrestricted_id" '[.[] | select(.provider != "multi" or .model != "multi/model-a" or .gateway_api_key_id != $id)] | length') == 0 ]]
multi_filtered_page=$(admin -f "$base/admin/activity/logs/page?since=0&providers=multi&models=multi/model-a&api_keys=$unrestricted_id&limit=100")
[[ $(printf '%s' "$multi_filtered_page" | jq --arg id "$unrestricted_id" '[.data[] | select(.provider != "multi" or .model != "multi/model-a" or .gateway_api_key_id != $id)] | length') == 0 ]]
[[ $(printf '%s' "$multi_filtered_page" | jq -r '.total >= (.data | length) and (.data | length) > 0') == true ]]
logs=$(admin -f "$base/admin/activity/logs?since=0")
[[ $(printf '%s' "$logs" | jq 'length') -ge 4 ]]
filtered_logs=$(admin -f "$base/admin/activity/logs?since=0&provider=multi&limit=1000")
[[ $(printf '%s' "$filtered_logs" | jq '[.[] | select(.provider != "multi")] | length') == 0 ]]
log_page=$(admin -f "$base/admin/activity/logs/page?since=0&provider=multi&status=success&query=multi&offset=0&limit=2")
[[ $(printf '%s' "$log_page" | jq '.data | length') == 2 ]]
[[ $(printf '%s' "$log_page" | jq -r '.limit') == 2 ]]
[[ $(printf '%s' "$log_page" | jq -r '.total') -ge 2 ]]
[[ $(printf '%s' "$log_page" | jq '[.data[] | select(.provider != "multi" or .status >= 400)] | length') == 0 ]]
[[ $(admin -f "$base/admin/activity/logs/page?since=0&limit=0" | jq -r '.limit') == 1 ]]
stats=$(admin -f "$base/admin/activity/stats?since=0")
[[ $(printf '%s' "$stats" | jq -r '.cost > 0') == true ]]
logs=$(admin -f "$base/admin/activity/logs?since=0")
[[ $(printf '%s' "$logs" | jq '[.[] | select(.cost == 0.0042)] | length') -ge 4 ]]
[[ $(printf '%s' "$logs" | jq -r --arg id "$unrestricted_id" --arg prefix "$unrestricted_prefix" '[.[] | select(.gateway_api_key_id == $id and .gateway_api_key_note == "Multi endpoint" and .gateway_api_key_prefix == $prefix)] | length > 0') == true ]]
! printf '%s' "$logs" | grep -Fq "$unrestricted_secret"
# Activity data can be previewed, range-filtered, and retention is persisted through the control API.
summary=$(admin -f "$base/admin/activity/export/preview?since=0")
[[ $(printf '%s' "$summary" | jq -r .records) -gt 0 ]]
[[ $(printf '%s' "$summary" | jq -r .estimated_bytes) -gt 0 ]]
[[ $(admin -f "$base/admin/activity/settings" | jq -r .retention_days) == 30 ]]
[[ $(admin_status -X PATCH "$base/admin/activity/settings" -H 'content-type: application/json' -d '{"retention_days":0}') == 400 ]]
[[ $(admin -f -X PATCH "$base/admin/activity/settings" -H 'content-type: application/json' -d '{"retention_days":45}' | jq -r .retention_days) == 45 ]]
[[ $(jq -r .retention_days "$work/data/activity-settings.json") == 45 ]]
old_retained_at=$(( $(date +%s) - 2 * 86400 ))
# Legacy records without upstream_model remain importable for backward compatibility.
old_retained_payload="{\"format\":\"yabane-activity\",\"version\":1,\"instance_id\":\"retention-test\",\"records\":[{\"timestamp\":$old_retained_at,\"request_id\":\"req-retention-old\",\"path\":\"/v1/responses\",\"model\":\"old/model\",\"provider\":\"old\",\"endpoint\":\"old\",\"status\":200,\"latency_ms\":1,\"input_tokens\":0,\"output_tokens\":0,\"cached_tokens\":0,\"cost\":null,\"streaming\":false}]}"
[[ $(admin -f -X POST "$base/admin/activity/import" -H 'content-type: application/json' -d "$old_retained_payload" | jq -r .imported) == 1 ]]
admin -f -X PATCH "$base/admin/activity/settings" -H 'content-type: application/json' -d '{"retention_days":1}' >/dev/null
[[ $(admin -f "$base/admin/activity/logs?since=0&limit=1000" | jq '[.[] | select(.request_id == "req-retention-old")] | length') == 0 ]]
! grep -q 'req-retention-old' "$work/data/activity.jsonl"
admin -f -X PATCH "$base/admin/activity/settings" -H 'content-type: application/json' -d '{"retention_days":45}' >/dev/null
# Activity exports are portable metadata snapshots. Imports preview and deduplicate by source instance and request ID.
admin -f -D activity-export.headers "$base/admin/activity/export?since=0" > activity-export.json
grep -Eq 'content-disposition: attachment; filename="yabane-activity-[0-9]+.json"' <(tr -d '\r' < activity-export.headers)
[[ $(jq -r .format activity-export.json) == yabane-activity ]]
[[ $(jq -r .version activity-export.json) == 1 ]]
[[ $(jq -r '.instance_id | length' activity-export.json) == 32 ]]
export_count=$(jq '.records | length' activity-export.json)
import_preview=$(admin -f -X POST "$base/admin/activity/import/preview" -H 'content-type: application/json' --data-binary @activity-export.json)
[[ $(printf '%s' "$import_preview" | jq -r .imported) == 0 ]]
[[ $(printf '%s' "$import_preview" | jq -r .duplicates) == "$export_count" ]]
import_result=$(admin -f -X POST "$base/admin/activity/import" -H 'content-type: application/json' --data-binary @activity-export.json)
[[ $(printf '%s' "$import_result" | jq -r .imported) == 0 ]]
[[ $(printf '%s' "$import_result" | jq -r .duplicates) == "$export_count" ]]
imported_at=$(date +%s)
import_payload="{\"format\":\"yabane-activity\",\"version\":1,\"instance_id\":\"remote-instance\",\"exported_at\":$imported_at,\"records\":[{\"timestamp\":$imported_at,\"request_id\":\"req-imported-remote\",\"path\":\"/v1/responses\",\"model\":\"remote/model\",\"provider\":\"remote-only\",\"endpoint\":\"remote-endpoint\",\"status\":200,\"latency_ms\":42,\"input_tokens\":10,\"output_tokens\":5,\"cached_tokens\":0,\"cost\":null,\"streaming\":false}]}"
import_result=$(admin -f -X POST "$base/admin/activity/import" -H 'content-type: application/json' -d "$import_payload")
[[ $(printf '%s' "$import_result" | jq -r .imported) == 1 ]]
[[ $(admin -f "$base/admin/activity/logs?since=0&limit=1000" | jq '[.[] | select(.request_id == "req-imported-remote" and .source_instance_id == "remote-instance" and .provider == "remote-only")] | length') == 1 ]]
import_result=$(admin -f -X POST "$base/admin/activity/import" -H 'content-type: application/json' -d "$import_payload")
[[ $(printf '%s' "$import_result" | jq -r .duplicates) == 1 ]]
# Records without a per-record origin belong to the export-level instance. This avoids
# collapsing coincident request IDs from distinct direct-export sources.
local_request=$(jq -r '.records[] | select(.source_instance_id == null) | .request_id' activity-export.json | head -1)
local_record=$(jq -c --arg id "$local_request" '.records[] | select(.request_id == $id)' activity-export.json)
relay_payload=$(jq -cn --argjson record "$local_record" '{format:"yabane-activity",version:1,instance_id:"relay-instance",records:[$record]}')
import_result=$(admin -f -X POST "$base/admin/activity/import" -H 'content-type: application/json' -d "$relay_payload")
[[ $(printf '%s' "$import_result" | jq -r .imported) == 1 ]]
import_result=$(admin -f -X POST "$base/admin/activity/import" -H 'content-type: application/json' -d "$relay_payload")
[[ $(printf '%s' "$import_result" | jq -r .duplicates) == 1 ]]
[[ $(admin_status -X POST "$base/admin/activity/import/preview" -H 'content-type: application/json' -d '{"format":"unknown","version":1,"records":[]}') == 400 ]]
[[ $(admin_status -X POST "$base/admin/activity/import" -H 'content-type: application/json' -d '{"format":"unknown","version":1,"records":[]}') == 400 ]]
[[ $(curl -fsS "$base/openapi.json" | jq -r '.paths["/admin/activity/import"].post.responses | has("500")') == true ]]
# Management API keys call control endpoints without a browser session.
management=$(admin -f -X POST "$base/admin/management-keys" -H 'content-type: application/json' -d '{"name":"E2E","expires_at":null}')
management_secret=$(printf '%s' "$management" | jq -r .secret)
management_id=$(printf '%s' "$management" | jq -r .api_key.id)
[[ $management_secret == yab_mgmt_* ]]
[[ $(status "$base/admin/activity/stats?since=0" -H "Authorization: Bearer $management_secret") == 200 ]]
[[ $(status "$base/admin/providers" -H "Authorization: Bearer $management_secret") == 200 ]]
[[ $(admin -f "$base/admin/management-keys" | jq -r --arg id "$management_id" '.[] | select(.id == $id) | .last_used_at != null') == true ]]
[[ $(jq -r --arg id "$management_id" '.management_api_keys[] | select(.id == $id) | .last_used_at != null' data/admin.json) == true ]]
# Management keys cannot touch the profile or management-key endpoints.
[[ $(status -X PATCH "$base/admin/profile" -H "Authorization: Bearer $management_secret" -H 'content-type: application/json' -d '{"username":"x","email":"x@example.com"}') == 401 ]]
admin -f -X DELETE "$base/admin/management-keys/$management_id" >/dev/null
[[ $(status "$base/admin/providers" -H "Authorization: Bearer $management_secret") == 401 ]]
# Live API docs and the embedded OpenAPI spec are public.
[[ $(curl -sS -o /dev/null -w '%{http_code}' "$base/docs") == 200 ]]
[[ $(curl -sS "$base/openapi.json" | jq -r .openapi) == 3.0.3 ]]
session_cookie=$(awk '$6 == "yabane_session" { print $7 }' "$cookie" | tail -1)
YABANE_UI_BASE="$base" YABANE_SESSION_COOKIE="$session_cookie" node "$repo/tests/responsive-ui.mjs"
admin -f -X PATCH "$base/admin/auth" -H 'content-type: application/json' -d '{"enabled":false}' >/dev/null
[[ $(status "$base/v1/models") == 200 ]]
admin -f -X PATCH "$base/admin/auth" -H 'content-type: application/json' -d '{"enabled":true}' >/dev/null
[[ $(status "$base/v1/models") == 401 ]]
admin -f -X POST "$base/admin/logout" >/dev/null
[[ $(admin_status "$base/admin/providers") == 401 ]]
# Exercise the real logged-out console separately: authenticated responsive coverage cannot verify login focus.
YABANE_UI_BASE="$base" node "$repo/tests/responsive-ui.mjs"
login_status=$(curl -sS -c "$cookie" -o response.json -w '%{http_code}' -X POST "$base/admin/login" -H 'content-type: application/json' -d '{"username":"admin","email":null,"password":"password123","turnstile_token":"XXXX.DUMMY.TOKEN.XXXX"}')
[[ $login_status == 204 ]]
[[ $(admin_status "$base/admin/providers") == 200 ]]
[[ $(status -X PATCH "$base/admin/profile" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"username":"intruder","email":"intruder@example.com","current_password":"","new_password":""}') == 401 ]]
admin -f -X PATCH "$base/admin/profile" -H 'content-type: application/json' -d '{"username":"owner","email":"owner@example.com","current_password":"","new_password":""}' >/dev/null
session=$(admin -f "$base/admin/session")
[[ $(printf '%s' "$session" | jq -r .username) == owner ]]
[[ $(printf '%s' "$session" | jq -r .email) == owner@example.com ]]
[[ $(admin_status -X PATCH "$base/admin/profile" -H 'content-type: application/json' -d '{"username":"owner","email":"owner@example.com","current_password":"wrong","new_password":"newpassword123"}') == 403 ]]
# Graceful shutdown flushes a final batch smaller than ten records.
before_shutdown_count=$(wc -l < data/activity.jsonl | tr -d ' ')
curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/model-a","messages":[]}' >/dev/null
kill -TERM "$pid"
wait "$pid"
pid=
[[ $(wc -l < data/activity.jsonl | tr -d ' ') -eq $((before_shutdown_count + 1)) ]]
# The process-wide CLI override leaves compiled Extensions visible but prevents every Hook and runtime enablement.
"$binary" --no-extensions --addr "127.0.0.1:$port" >no-extensions.log 2>&1 & pid=$!
for _ in $(seq 1 50); do curl -sf "$base/healthz" >/dev/null && break; sleep .1; done
[[ $(curl -sS -c "$cookie" -o /dev/null -w '%{http_code}' -X POST "$base/admin/login" -H 'content-type: application/json' -d '{"username":"owner","email":null,"password":"password123","turnstile_token":""}') == 204 ]]
cli_extension=$(admin -f "$base/admin/extensions" | jq -c '.[] | select(.id == "request-defaults")')
[[ $(printf '%s' "$cli_extension" | jq -r '.enabled') == false ]]
[[ $(printf '%s' "$cli_extension" | jq -r '.runtime_configurable') == false ]]
[[ $(admin_status -X PATCH "$base/admin/extensions/request-defaults" -H 'content-type: application/json' -d '{"enabled":true}') == 409 ]]
cli_without_defaults=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/model-a","messages":[]}')
[[ $(printf '%s' "$cli_without_defaults" | jq -r .extra) == null ]]
[[ $(jq -r '.enabled["request-defaults"]' data/extensions.json) == true ]]
# Model listing serves the last successful discovery cache even when every live
# upstream is unavailable; refreshing models remains an explicit admin action.
kill "$upstream_pid"
wait "$upstream_pid" || true
upstream_pid=
cached_status=$(status "$base/v1/models" -H "Authorization: Bearer $unrestricted_secret")
if [[ $cached_status != 200 ]]; then
  echo "cached model listing returned HTTP $cached_status after upstream shutdown" >&2
  cat response.json >&2
  exit 1
fi
cached_models=$(<response.json)
[[ $(printf '%s' "$cached_models" | jq '[.data[] | select(.id == "multi/model-a")] | length') == 1 ]]
kill -TERM "$pid"
wait "$pid"
pid=
# Cross-file reference validation runs on the real startup path instead of allowing
# a manually corrupted route file to survive until an inference request fails.
cp data/routes.json data/routes.valid.json
cat >data/routes.json <<'JSON'
[{"pattern":"dangling-startup","targets":[{"provider_id":"missing-provider","endpoint_id":"missing-endpoint","api_key_id":"missing-key","upstream_model":"model","weight":100,"enabled":true}]}]
JSON
"$binary" --addr "127.0.0.1:$port" >dangling-configuration.log 2>&1 & pid=$!
for _ in $(seq 1 50); do
  if ! kill -0 "$pid" 2>/dev/null; then break; fi
  sleep .02
done
if kill -0 "$pid" 2>/dev/null; then
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  pid=
  echo "dangling configuration unexpectedly started" >&2
  exit 1
fi
if wait "$pid"; then
  pid=
  echo "dangling configuration unexpectedly exited successfully" >&2
  exit 1
fi
pid=
grep -q "model route 'dangling-startup' refers to unknown Provider 'missing-provider'" dangling-configuration.log
mv data/routes.valid.json data/routes.json
echo 'Authentication and routing E2E passed'
