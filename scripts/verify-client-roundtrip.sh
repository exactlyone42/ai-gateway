#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
mode="${1:-all}"
database_url="${DATABASE_URL:?set DATABASE_URL to a disposable PostgreSQL database}"

if [ "${CLIENT_ROUNDTRIP_ALLOW_DATABASE_MUTATION:-}" != 1 ]; then
  echo "set CLIENT_ROUNDTRIP_ALLOW_DATABASE_MUTATION=1 for a disposable database" >&2
  exit 2
fi
case "${database_url,,}" in
  *test*|*validation*|*disposable*) ;;
  *) echo "DATABASE_URL must name a test, validation, or disposable database" >&2; exit 2 ;;
esac

runtime=$(mktemp -d "${TMPDIR:-/tmp}/apg-client-roundtrip.XXXXXX")
read -r mock_port gateway_port < <(python3 -c '
import socket
left = socket.socket()
right = socket.socket()
left.bind(("127.0.0.1", 0))
right.bind(("127.0.0.1", 0))
print(left.getsockname()[1], right.getsockname()[1])
')

case "$mode" in
  all|claude|codex) ;;
  *) echo "usage: $0 [all|claude|codex]" >&2; exit 2 ;;
esac

command -v python3 >/dev/null
command -v cargo >/dev/null
if [ "$mode" = all ] || [ "$mode" = claude ]; then command -v claude >/dev/null; fi
if [ "$mode" = all ] || [ "$mode" = codex ]; then command -v codex >/dev/null; fi
if [ "$mode" = all ] || [ "$mode" = claude ]; then
  case "$(claude --version)" in
    *2.1.306*) ;;
    *) echo "expected Claude Code 2.1.306; update the versioned evidence first" >&2; exit 1 ;;
  esac
fi
if [ "$mode" = all ] || [ "$mode" = codex ]; then
  case "$(codex --version)" in
    *0.153.0*) ;;
    *) echo "expected Codex CLI 0.153.0; update the versioned evidence first" >&2; exit 1 ;;
  esac
fi

mkdir -p "$runtime/captures" "$runtime/claude-home" "$runtime/codex-home" "$runtime/spool"

mock_pid=""
gateway_pid=""
cleanup() {
  if [ -n "$gateway_pid" ]; then kill "$gateway_pid" 2>/dev/null || true; fi
  if [ -n "$mock_pid" ]; then kill "$mock_pid" 2>/dev/null || true; fi
}
trap cleanup EXIT INT TERM

python3 "$root/scripts/client-roundtrip-mock.py" \
  --port "$mock_port" \
  --fixture-dir "$root/tests/fixtures/client-roundtrip" \
  --capture-dir "$runtime/captures" \
  >"$runtime/mock.log" 2>&1 &
mock_pid=$!
sleep 0.2
if ! kill -0 "$mock_pid" 2>/dev/null; then
  echo "fixture provider failed to start; see $runtime/mock.log" >&2
  exit 1
fi

cargo build --quiet --manifest-path "$root/Cargo.toml"
env \
  LISTEN_ADDRESS="127.0.0.1:$gateway_port" \
  DATABASE_URL="$database_url" \
  DATABASE_MAX_CONNECTIONS=5 \
  ADMIN_DATABASE_MAX_CONNECTIONS=1 \
  OPENAI_BASE_URL="http://127.0.0.1:$mock_port" \
  OPENAI_API_KEY=fixture-provider-key \
  ANTHROPIC_BASE_URL="http://127.0.0.1:$mock_port" \
  ANTHROPIC_API_KEY=fixture-provider-key \
  CREDENTIAL_HASH_KEY=fixture-hash-key-with-at-least-32-bytes \
  CREDENTIAL_ENVIRONMENT=dev \
  ENABLE_DEVELOPMENT_BOOTSTRAP=true \
  BOOTSTRAP_CLIENT_KEY=fixture-client-key \
  ALLOWED_MODELS=claude-test,company-default \
  LEDGER_SPOOL_DIR="$runtime/spool" \
  RUST_LOG=agent_pool_gateway=info \
  "$root/target/debug/agent-pool-gateway" >"$runtime/gateway.log" 2>&1 &
gateway_pid=$!

sleep 2
if ! kill -0 "$gateway_pid" 2>/dev/null; then
  echo "gateway failed to start; see $runtime/gateway.log" >&2
  exit 1
fi
curl --fail --silent "http://127.0.0.1:$gateway_port/health" >"$runtime/health.json"

if [ "$mode" = all ] || [ "$mode" = claude ]; then
  env \
    -u ANTHROPIC_API_KEY \
    -u CLAUDE_CODE_OAUTH_TOKEN \
    CLAUDE_CONFIG_DIR="$runtime/claude-home" \
    ANTHROPIC_BASE_URL="http://127.0.0.1:$gateway_port" \
    ANTHROPIC_AUTH_TOKEN=fixture-client-key \
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
    DISABLE_AUTOUPDATER=1 \
    DISABLE_TELEMETRY=1 \
    claude -p --model claude-test --max-turns 1 \
      "Reply with exactly W05_CLAUDE_CODE_OK and nothing else." \
      >"$runtime/claude.out"
  grep -Fxq 'W05_CLAUDE_CODE_OK' "$runtime/claude.out"
  diff -u \
    "$root/tests/fixtures/client-roundtrip/captures/claude-code-2.1.306.json" \
    "$runtime/captures/claude-code.json"
fi

if [ "$mode" = all ] || [ "$mode" = codex ]; then
  printf '%s\n' \
    'model = "company-default"' \
    'model_provider = "aigw"' \
    'model_reasoning_effort = "low"' \
    '' \
    '[model_providers.aigw]' \
    'name = "AIGW Fixture"' \
    "base_url = \"http://127.0.0.1:$gateway_port/v1\"" \
    'env_key = "AIGW_API_KEY"' \
    'wire_api = "responses"' \
    'requires_openai_auth = false' \
    >"$runtime/codex-home/config.toml"
  env \
    CODEX_HOME="$runtime/codex-home" \
    AIGW_API_KEY=fixture-client-key \
    codex exec --sandbox read-only --skip-git-repo-check \
      "Reply with exactly W06_CODEX_OK and nothing else." \
      >"$runtime/codex.out" 2>"$runtime/codex.err"
  grep -Fxq 'W06_CODEX_OK' "$runtime/codex.out"
  grep -Fq 'Model metadata for' "$runtime/codex.err"
  grep -Fq 'company-default' "$runtime/codex.err"
  grep -Fq 'Defaulting to fallback metadata' "$runtime/codex.err"
  if grep -Fq '/v1/models' "$runtime/gateway.log"; then
    echo "Codex 0.153.0 unexpectedly called gateway model discovery; update the contract" >&2
    exit 1
  fi
  diff -u \
    "$root/tests/fixtures/client-roundtrip/captures/codex-cli-0.153.0.json" \
    "$runtime/captures/codex-cli.json"
fi

echo "client roundtrip ok: $mode"
echo "sanitized captures: $runtime/captures"
