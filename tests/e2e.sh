#!/usr/bin/env bash
set -euo pipefail
binary=${1:-target/debug/yabane}
binary=$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")
work=$(mktemp -d)
port=${YABANE_E2E_PORT:-18097}
upstream_port=$((port + 1))
pid=
upstream_pid=
cleanup() { [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true; [[ -n "$upstream_pid" ]] && kill "$upstream_pid" 2>/dev/null || true; rm -rf "$work"; }
trap cleanup EXIT
cd "$work"
TURNSTILE_SECRET="${TURNSTILE_TEST_SECRET:-1x0000000000000000000000000000000AA}" "$binary" --addr "127.0.0.1:$port" >server.log 2>&1 & pid=$!
for _ in $(seq 1 50); do curl -sf "http://127.0.0.1:$port/healthz" >/dev/null && break; sleep .1; done
base="http://127.0.0.1:$port"
cookie="$work/cookie.txt"
turnstile_config=$(curl -fsS "$base/admin/turnstile-config")
[[ $(printf '%s' "$turnstile_config" | jq -r .enabled) == true ]]
[[ $(printf '%s' "$turnstile_config" | jq -r .site_key) == 1x00000000000000000000AA ]]
setup_status=$(curl -sS -c "$cookie" -o response.json -w '%{http_code}' -X POST "$base/admin/setup" -H 'content-type: application/json' -d '{"username":"admin","email":"admin@example.com","password":"password123"}')
[[ $setup_status == 204 ]]
cat >upstream.py <<'PY'
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import sys

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        endpoint = self.headers.get('authorization', 'Bearer unknown').removeprefix('Bearer ')
        models = {'one': ['model-a', 'shared'], 'two': ['model-b', 'shared']}.get(endpoint, [])
        body = json.dumps({'object': 'list', 'data': [{'id': model} for model in models]}).encode()
        self.send_response(200); self.send_header('content-type', 'application/json'); self.end_headers(); self.wfile.write(body)
    def do_POST(self):
        length = int(self.headers.get('content-length', 0)); request = json.loads(self.rfile.read(length))
        endpoint = self.headers.get('authorization', 'Bearer unknown').removeprefix('Bearer ')
        body = json.dumps({'endpoint': endpoint, 'model': request['model'], 'headers': {'x-provider': self.headers.get('x-provider'), 'x-endpoint': self.headers.get('x-endpoint')}, 'extra': request.get('extra'), 'endpoint_extra': request.get('endpoint_extra'), 'usage': {'prompt_tokens': 1200, 'completion_tokens': 300, 'prompt_tokens_details': {'cached_tokens': 200}, 'cost': 0.0042}}).encode()
        self.send_response(200); self.send_header('content-type', 'application/json'); self.end_headers(); self.wfile.write(body)
    def log_message(self, *_): pass

ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), Handler).serve_forever()
PY
python3 upstream.py "$upstream_port" & upstream_pid=$!
status() { curl -sS -o response.json -w '%{http_code}' "$@"; }
admin() { curl -sS -b "$cookie" "$@"; }
admin_status() { curl -sS -b "$cookie" -o response.json -w '%{http_code}' "$@"; }
[[ $(status "$base/v1/models") == 401 ]]
[[ $(admin_status -X POST "$base/admin/providers/missing/models/refresh") == 404 ]]
created=$(admin -f -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"E2E unrestricted","expires_at":null,"provider_ids":[]}')
secret=$(printf '%s' "$created" | jq -r .secret)
id=$(printf '%s' "$created" | jq -r .api_key.id)
[[ $secret == sk-* ]]
admin -f "$base/admin/auth" | grep -Fq "$secret"
grep -Fq "$secret" data/auth.json
[[ $(status "$base/v1/models" -H 'Authorization: Basic nope') == 401 ]]
[[ $(status "$base/v1/models" -H 'Authorization: Bearer sk-invalid') == 401 ]]
[[ $(status "$base/v1/models" -H "Authorization: Bearer $secret") == 200 ]]
admin -f -X DELETE "$base/admin/auth/keys/$id" >/dev/null
[[ $(status "$base/v1/models" -H "Authorization: Bearer $secret") == 401 ]]
[[ $(admin_status -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"expired","expires_at":1,"provider_ids":[]}') == 400 ]]
[[ $(admin_status -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"bad scope","expires_at":null,"provider_ids":["missing"]}') == 400 ]]
[[ $(admin_status -X POST "$base/admin/providers" -H 'content-type: application/json' -d '{"id":"bad-proxy","name":"Bad proxy","endpoint":{"api_type":"openai_compatible","base_url":"http://127.0.0.1:1/v1","socks5_proxy":"http://127.0.0.1:1080","requires_api_key":false,"api_key":null}}') == 400 ]]
for provider in allowed denied; do
  admin -f -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"$provider\",\"name\":\"$provider\",\"endpoint\":{\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:1/v1\",\"requires_api_key\":false,\"api_key\":null}}" >/dev/null
done
admin -f -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"multi\",\"name\":\"Multi endpoint\",\"endpoint\":{\"id\":\"one\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true,\"api_key\":\"one\"}}" >/dev/null
admin -f -X POST "$base/admin/providers/multi/endpoints" -H 'content-type: application/json' -d "{\"id\":\"two\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true,\"api_key\":\"two\"}" >/dev/null
admin -f -X PATCH "$base/admin/providers/multi/endpoints/two" -H 'content-type: application/json' -d "{\"id\":\"two\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1/\",\"socks5_proxy\":null,\"requires_api_key\":true}" >/dev/null
[[ $(admin -f "$base/admin/providers" | jq -r '.[] | select(.id == "multi") | .endpoints[] | select(.id == "two") | .base_url') == "http://127.0.0.1:$upstream_port/v1" ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi/endpoints/two" -H 'content-type: application/json' -d "{\"id\":\"renamed\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true}") == 400 ]]
# The mock identifies endpoints from their bearer credentials.
# Discovery and inference must map unique models to the endpoint that reported them.
admin -f -X POST "$base/admin/providers/multi/models/refresh" >/dev/null
providers_json=$(admin -f "$base/admin/providers")
[[ $(printf '%s' "$providers_json" | jq -r '.[] | select(.id == "multi") | .model_endpoints["model-a"][0]') == one ]]
[[ $(printf '%s' "$providers_json" | jq -r '.[] | select(.id == "multi") | .model_endpoints["model-b"][0]') == two ]]
[[ $(printf '%s' "$providers_json" | jq -r '.[] | select(.id == "multi") | .model_endpoints["shared"] | join(",")') == one,two ]]
# Shared models use the first configured compatible endpoint by default and accept an explicit preference.
shared_default=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $(admin -f -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"Shared model setup","expires_at":null,"provider_ids":[]}' | jq -r .secret)" -H 'content-type: application/json' -d '{"model":"multi/shared","messages":[]}')
[[ $(printf '%s' "$shared_default" | jq -r .endpoint) == one ]]
admin -f -X PATCH "$base/admin/providers/multi/model-endpoint-preferences" -H 'content-type: application/json' -d '{"preferences":[{"model":"shared","api_type":"openai_compatible","endpoint_id":"two"}]}' >/dev/null
providers_json=$(admin -f "$base/admin/providers")
[[ $(printf '%s' "$providers_json" | jq -r '.[] | select(.id == "multi") | .model_endpoint_preferences[0].endpoint_id') == two ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi/model-endpoint-preferences" -H 'content-type: application/json' -d '{"preferences":[{"model":"model-a","api_type":"openai_compatible","endpoint_id":"two"}]}') == 400 ]]
scoped=$(admin -f -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"Scoped","expires_at":null,"provider_ids":["allowed"]}')
scoped_secret=$(printf '%s' "$scoped" | jq -r .secret)
[[ $(status -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $scoped_secret" -H 'content-type: application/json' -d '{"model":"denied/model","messages":[]}') == 403 ]]
[[ $(status -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $scoped_secret" -H 'content-type: application/json' -d '{"model":"allowed/model","messages":[]}') == 502 ]]
unrestricted=$(admin -f -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"Multi endpoint","expires_at":null,"provider_ids":[]}')
unrestricted_secret=$(printf '%s' "$unrestricted" | jq -r .secret)
preferred_shared=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/shared","messages":[]}')
[[ $(printf '%s' "$preferred_shared" | jq -r .endpoint) == two ]]
model_a=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/model-a","messages":[]}')
model_b=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/model-b","messages":[]}')
[[ $(printf '%s' "$model_a" | jq -r .endpoint) == one ]]
[[ $(printf '%s' "$model_b" | jq -r .endpoint) == two ]]
[[ $(printf '%s' "$model_a" | jq -r .model) == model-a ]]
admin -f -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"extra_headers":{"x-provider":"yes"},"extra_body":{"extra":"provider"},"defaults_endpoint_ids":["one"]}' >/dev/null
[[ $(admin_status -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"extra_headers":{"authorization":"unsafe"},"extra_body":{}}') == 400 ]]
[[ $(admin_status -X PATCH "$base/admin/providers/multi" -H 'content-type: application/json' -d '{"extra_headers":{"bad header":"unsafe"},"extra_body":{}}') == 400 ]]
route_payload='{"pattern":"friendly-model","targets":[{"provider_id":"multi","endpoint_id":"one","api_key_id":"default","upstream_model":"model-a","weight":1},{"provider_id":"multi","endpoint_id":"two","api_key_id":"default","upstream_model":"model-b","weight":1}]}'
[[ $(admin_status -X POST "$base/admin/routes" -H 'content-type: application/json' -d '{"pattern":"bad-prefixed-model","targets":[{"provider_id":"multi","endpoint_id":"one","api_key_id":"default","upstream_model":"multi/model-a","weight":1}]}') == 400 ]]
admin -f -X POST "$base/admin/routes" -H 'content-type: application/json' -d "$route_payload" >/dev/null
friendly_one=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"friendly-model","messages":[]}')
friendly_two=$(curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"friendly-model","messages":[]}')
[[ $(printf '%s' "$friendly_one" | jq -r .endpoint) == one ]]
[[ $(printf '%s' "$friendly_two" | jq -r .endpoint) == two ]]
[[ $(printf '%s' "$friendly_one" | jq -r .extra) == provider ]]
[[ $(printf '%s' "$friendly_one" | jq -r '.headers["x-provider"]') == yes ]]
[[ $(printf '%s' "$friendly_two" | jq -r .extra) == null ]]
[[ $(printf '%s' "$friendly_two" | jq -r '.headers["x-provider"]') == null ]]
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
admin -f -X POST "$base/admin/routes" -H 'content-type: application/json' -d '{"pattern":"temporary-route","targets":[{"provider_id":"multi","endpoint_id":"two","api_key_id":"routed-temporary","upstream_model":"model-b","weight":1}]}' >/dev/null
admin -f -X DELETE "$base/admin/providers/multi/endpoints/two/keys/routed-temporary" >/dev/null
[[ $(admin -f "$base/admin/routes" | jq '[.[] | select(.pattern == "temporary-route")] | length') == 0 ]]
# Deleting an endpoint removes its keys, discovery availability, and exact route destinations.
admin -f -X POST "$base/admin/providers/multi/keys" -H 'content-type: application/json' -d '{"endpoint_id":"two","name":"Endpoint deletion route","secret":"endpoint-deletion-route","weight":10}' >/dev/null
admin -f -X POST "$base/admin/routes" -H 'content-type: application/json' -d '{"pattern":"endpoint-deletion-route","targets":[{"provider_id":"multi","endpoint_id":"two","api_key_id":"endpoint-deletion-route","upstream_model":"model-b","weight":1}]}' >/dev/null
admin -f -X DELETE "$base/admin/providers/multi/endpoints/two" >/dev/null
[[ $(admin -f "$base/admin/providers" | jq '[.[] | select(.id == "multi") | .endpoints[] | select(.id == "two")] | length') == 0 ]]
[[ $(admin -f "$base/admin/providers" | jq -r '.[] | select(.id == "multi") | has("model_endpoints") and (.model_endpoints | has("model-b") | not)') == true ]]
[[ $(admin -f "$base/admin/routes" | jq '[.[] | select(.pattern == "endpoint-deletion-route")] | length') == 0 ]]
[[ $(admin_status -X DELETE "$base/admin/providers/multi/endpoints/missing") == 404 ]]
# Deleting a Provider removes every route destination that refers to it.
admin -f -X POST "$base/admin/routes" -H 'content-type: application/json' -d '{"pattern":"provider-deletion-route","targets":[{"provider_id":"multi","endpoint_id":"one","api_key_id":"default","upstream_model":"model-a","weight":1}]}' >/dev/null
admin -f -X DELETE "$base/admin/providers/denied" >/dev/null
admin -f -X DELETE "$base/admin/providers/multi" >/dev/null
[[ $(admin -f "$base/admin/routes" | jq '[.[] | select(.pattern == "provider-deletion-route")] | length') == 0 ]]
# Recreate the Provider needed by the remaining Activity checks.
admin -f -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"multi\",\"name\":\"Multi endpoint\",\"endpoint\":{\"id\":\"one\",\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:$upstream_port/v1\",\"socks5_proxy\":null,\"requires_api_key\":true,\"api_key\":\"one\"}}" >/dev/null
# Ten completed requests trigger an Activity JSONL flush without waiting for the timer.
for _ in $(seq 1 6); do
  curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/model-a","messages":[]}' >/dev/null
done
[[ $(wc -l < data/activity.jsonl | tr -d ' ') -eq 10 ]]
stats=$(admin -f "$base/admin/activity/stats?since=0")
[[ $(printf '%s' "$stats" | jq -r .requests) -ge 10 ]]
[[ $(printf '%s' "$stats" | jq -r .input_tokens) -ge 4800 ]]
logs=$(admin -f "$base/admin/activity/logs?since=0")
[[ $(printf '%s' "$logs" | jq 'length') -ge 4 ]]
stats=$(admin -f "$base/admin/activity/stats?since=0")
[[ $(printf '%s' "$stats" | jq -r '.cost > 0') == true ]]
logs=$(admin -f "$base/admin/activity/logs?since=0")
[[ $(printf '%s' "$logs" | jq '[.[] | select(.cost == 0.0042)] | length') -ge 4 ]]
# Activity exports are portable metadata snapshots. Imports deduplicate by source instance and request ID.
admin -f "$base/admin/activity/export" > activity-export.json
[[ $(jq -r .format activity-export.json) == yabane-activity ]]
[[ $(jq -r .version activity-export.json) == 1 ]]
[[ $(jq -r '.instance_id | length' activity-export.json) == 32 ]]
export_count=$(jq '.records | length' activity-export.json)
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
[[ $(admin_status -X POST "$base/admin/activity/import" -H 'content-type: application/json' -d '{"format":"unknown","version":1,"records":[]}') == 400 ]]
# Management API keys call control endpoints without a browser session.
management=$(admin -f -X POST "$base/admin/management-keys" -H 'content-type: application/json' -d '{"name":"E2E","expires_at":null}')
management_secret=$(printf '%s' "$management" | jq -r .secret)
[[ $management_secret == yab_mgmt_* ]]
[[ $(status "$base/admin/activity/stats?since=0" -H "Authorization: Bearer $management_secret") == 200 ]]
[[ $(status "$base/admin/providers" -H "Authorization: Bearer $management_secret") == 200 ]]
# Management keys cannot touch the profile or management-key endpoints.
[[ $(status -X PATCH "$base/admin/profile" -H "Authorization: Bearer $management_secret" -H 'content-type: application/json' -d '{"username":"x","email":"x@example.com"}') == 401 ]]
management_id=$(printf '%s' "$management" | jq -r .api_key.id)
admin -f -X DELETE "$base/admin/management-keys/$management_id" >/dev/null
[[ $(status "$base/admin/providers" -H "Authorization: Bearer $management_secret") == 401 ]]
# Live API docs and the embedded OpenAPI spec are public.
[[ $(curl -sS -o /dev/null -w '%{http_code}' "$base/docs") == 200 ]]
[[ $(curl -sS "$base/openapi.json" | jq -r .openapi) == 3.0.3 ]]
admin -f -X PATCH "$base/admin/auth" -H 'content-type: application/json' -d '{"enabled":false}' >/dev/null
[[ $(status "$base/v1/models") == 200 ]]
admin -f -X PATCH "$base/admin/auth" -H 'content-type: application/json' -d '{"enabled":true}' >/dev/null
[[ $(status "$base/v1/models") == 401 ]]
admin -f -X POST "$base/admin/logout" >/dev/null
[[ $(admin_status "$base/admin/providers") == 401 ]]
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
curl -sf -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $unrestricted_secret" -H 'content-type: application/json' -d '{"model":"multi/model-a","messages":[]}' >/dev/null
kill -TERM "$pid"
wait "$pid"
pid=
[[ $(wc -l < data/activity.jsonl | tr -d ' ') -eq 15 ]]
echo 'Authentication and routing E2E passed'
