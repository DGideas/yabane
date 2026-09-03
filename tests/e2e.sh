#!/usr/bin/env bash
set -euo pipefail
binary=${1:-target/debug/yabane}
binary=$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")
work=$(mktemp -d)
port=${YABANE_E2E_PORT:-18097}
pid=
cleanup() { [[ -n "$pid" ]] && kill "$pid" 2>/dev/null || true; rm -rf "$work"; }
trap cleanup EXIT
cd "$work"
YABANE_ADDR="127.0.0.1:$port" "$binary" >server.log 2>&1 & pid=$!
for _ in $(seq 1 50); do curl -sf "http://127.0.0.1:$port/healthz" >/dev/null && break; sleep .1; done
base="http://127.0.0.1:$port"
status() { curl -sS -o response.json -w '%{http_code}' "$@"; }
[[ $(status "$base/v1/models") == 401 ]]
[[ $(status -X POST "$base/admin/providers/missing/models/refresh") == 404 ]]
created=$(curl -sf -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"E2E unrestricted","expires_at":null,"provider_ids":[]}')
secret=$(printf '%s' "$created" | jq -r .secret)
id=$(printf '%s' "$created" | jq -r .api_key.id)
[[ $secret == sk-* ]]
! curl -sf "$base/admin/auth" | grep -Fq "$secret"
! grep -Fq "$secret" data/auth.json
[[ $(status "$base/v1/models" -H 'Authorization: Basic nope') == 401 ]]
[[ $(status "$base/v1/models" -H 'Authorization: Bearer sk-invalid') == 401 ]]
[[ $(status "$base/v1/models" -H "Authorization: Bearer $secret") == 200 ]]
curl -sf -X DELETE "$base/admin/auth/keys/$id" >/dev/null
[[ $(status "$base/v1/models" -H "Authorization: Bearer $secret") == 401 ]]
[[ $(status -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"expired","expires_at":1,"provider_ids":[]}') == 400 ]]
[[ $(status -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"bad scope","expires_at":null,"provider_ids":["missing"]}') == 400 ]]
for provider in allowed denied; do
  curl -sf -X POST "$base/admin/providers" -H 'content-type: application/json' -d "{\"id\":\"$provider\",\"name\":\"$provider\",\"endpoint\":{\"api_type\":\"openai_compatible\",\"base_url\":\"http://127.0.0.1:1/v1\",\"requires_api_key\":false,\"api_key\":null}}" >/dev/null
done
scoped=$(curl -sf -X POST "$base/admin/auth/keys" -H 'content-type: application/json' -d '{"note":"Scoped","expires_at":null,"provider_ids":["allowed"]}')
scoped_secret=$(printf '%s' "$scoped" | jq -r .secret)
[[ $(status -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $scoped_secret" -H 'content-type: application/json' -d '{"model":"denied/model","messages":[]}') == 403 ]]
[[ $(status -X POST "$base/v1/chat/completions" -H "Authorization: Bearer $scoped_secret" -H 'content-type: application/json' -d '{"model":"allowed/model","messages":[]}') == 502 ]]
curl -sf -X PATCH "$base/admin/auth" -H 'content-type: application/json' -d '{"enabled":false}' >/dev/null
[[ $(status "$base/v1/models") == 200 ]]
curl -sf -X PATCH "$base/admin/auth" -H 'content-type: application/json' -d '{"enabled":true}' >/dev/null
[[ $(status "$base/v1/models") == 401 ]]
echo 'Authentication E2E passed'
