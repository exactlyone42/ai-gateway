#!/usr/bin/env bash
# Runs the gateway with sentinel secrets, exercises it, then scans everything
# it printed. A static grep cannot answer this question: the leak that matters
# is a value that arrives at runtime -- a key, a prompt, an address -- reaching
# the log through a field nobody thought about.
#
#   ./scripts/check-log-secrets.sh              # scan a real run
#   ./scripts/check-log-secrets.sh --selftest   # prove the scanner goes red
#
# --selftest appends one line containing a sentinel to the captured output and
# requires the scan to fail. A scanner nobody has seen go red is a rubber stamp.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

selftest=0
[ "${1:-}" = "--selftest" ] && selftest=1

# Distinctive so a hit cannot be a coincidence, and marked so a reader of the
# log knows immediately that the check failed rather than a real key leaked.
client_key="apg_sentinel_client_key_do_not_log"
openai_key="sk_sentinel_openai_key_do_not_log"
anthropic_key="sk_sentinel_anthropic_key_do_not_log"
body_token="sentinel_body_token_do_not_log"
person="sentinel.person@example.invalid"
prompt="sentinel_prompt_content_do_not_log"
credential_hash_key="sentinel_credential_hash_key_never_log_32"
retiring_hash_key="sentinel_retiring_hash_key_never_log_32"

port=18190
log="$(mktemp)"
trap 'rm -f "$log"' EXIT

cargo build --quiet
binary="${CARGO_TARGET_DIR:-target}/debug/agent-pool-gateway"

# The gateway runs its migrations at startup, so booting it for real now needs
# a real database. Docker does the waiting through the compose health check --
# a polling loop here would be the wrong shape and would hammer the port.
if [ -z "${DATABASE_URL:-}" ]; then
  if ! docker compose -f deploy/docker-compose.yml up -d --wait postgres >/dev/null 2>&1; then
    echo "cannot start the database, so this check did not run." >&2
    echo "Start it yourself and set DATABASE_URL, or make docker available." >&2
    # 3, not 0: "not verified" is not "passed".
    exit 3
  fi
  DATABASE_URL="postgres://gateway:gateway-dev@127.0.0.1:${POSTGRES_PORT:-15432}/gateway"
fi

DATABASE_URL="$DATABASE_URL" \
LISTEN_ADDRESS="127.0.0.1:$port" \
OPENAI_BASE_URL="http://127.0.0.1:1/" \
OPENAI_API_KEY="$openai_key" \
ANTHROPIC_BASE_URL="http://127.0.0.1:1/" \
ANTHROPIC_API_KEY="$anthropic_key" \
ENABLE_DEVELOPMENT_BOOTSTRAP="true" \
BOOTSTRAP_CLIENT_KEY="$client_key" \
CREDENTIAL_HASH_KEY="$credential_hash_key" \
CREDENTIAL_HASH_KEY_ID="v2" \
CREDENTIAL_RETIRING_HASH_KEYS="v1=$retiring_hash_key" \
ALLOWED_MODELS="company-default" \
  "$binary" >"$log" 2>&1 &
pid=$!
# shellcheck disable=SC2064
trap "kill $pid 2>/dev/null || true; rm -f '$log'" EXIT

# Wait for the startup line, then assert on the file rather than on the
# pipeline's status. `grep -m1` exits as soon as it matches, which can kill
# `tail` with SIGPIPE, and under `pipefail` that makes a successful wait look
# like a failure -- it did exactly that in CI while the startup line was
# sitting in the very output the error printed (measured 2026-09-09).
timeout 15 tail -f -n +1 "$log" | grep -q -m1 "gateway started" || true
if ! grep -q "gateway started" "$log"; then
  echo "the gateway did not start; output was:" >&2
  cat "$log" >&2
  exit 1
fi

base="http://127.0.0.1:$port"
body="{\"model\":\"company-default\",\"messages\":[{\"role\":\"user\",\"content\":\"$prompt $person token=$body_token\"}]}"

# These are two distinct local handler paths, each called once. Both upstreams
# point at the local closed port above; this check never probes a remote service.
curl -s -o /dev/null -X POST "$base/v1/messages" \
  -H "x-api-key: $client_key" -H 'anthropic-version: 2023-06-01' \
  -H 'content-type: application/json' -d "$body"
curl -s -o /dev/null -X POST "$base/v1/responses" \
  -H "authorization: Bearer $client_key" \
  -H 'content-type: application/json' \
  -d "{\"model\":\"company-default\",\"input\":\"$prompt $person token=$body_token\"}"

kill -INT "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true

if [ "$selftest" = 1 ]; then
  printf '{"body":"pretend a field carried it","token":"%s"}\n' "$client_key" >>"$log"
fi

failed=0
for secret in "$client_key" "$openai_key" "$anthropic_key" "$body_token" "$person" "$prompt" "$credential_hash_key" "$retiring_hash_key"; do
  if grep -nF "$secret" "$log" >/dev/null; then
    echo "secret in the log output: $secret"
    grep -nF "$secret" "$log" | sed 's/^/    /'
    failed=1
  fi
done

# One unparseable line makes a consumer treat the whole source as text, and a
# leaked value hidden in such a line would also escape a field-aware scan.
python3 - "$log" <<'PY'
import json
import sys

bad = []
records = []
with open(sys.argv[1], encoding="utf-8", errors="replace") as stream:
    for number, line in enumerate(stream, 1):
        if not line.strip():
            continue
        try:
            records.append(json.loads(line))
        except ValueError:
            bad.append((number, line.strip()[:120]))
if bad:
    for number, line in bad:
        print(f"not JSON, line {number}: {line}")
    sys.exit(1)

access = [record for record in records if record.get("event", {}).get("name") == "http.access"]
if not any(len(record.get("trace_id", "")) == 32 for record in access):
    print("no access log carried a trace id under the default no-exporter configuration")
    sys.exit(1)
PY

if [ "$failed" = 0 ]; then
  echo "no secrets in the log output"
fi
exit "$failed"
