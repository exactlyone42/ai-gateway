#!/usr/bin/env bash
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
file="${1:-docs/client-compatibility.md}"
fixture_dir="tests/fixtures/client-roundtrip"

clients=(
  "Claude Code CLI"
  "Codex CLI"
  "Claude Desktop"
  "VS Code Copilot Chat"
  "Cursor"
  "Codex Desktop／IDE"
)
fields=(
  client version verification_date custom_endpoint protocol authentication
  streaming tools reasoning seat_subscription_behavior configuration_example
  known_limitations official_source
)
failed=0

for client in "${clients[@]}"; do
  section=$(awk -v heading="## $client" '
    $0 == heading { found = 1; next }
    found && /^## / { exit }
    found { print }
  ' "$file")
  if [ -z "$section" ]; then
    echo "$file: missing section '## $client'"
    failed=1
    continue
  fi

  for field in "${fields[@]}"; do
    count=$(printf '%s\n' "$section" | grep -c "^| $field |" || true)
    if [ "$count" -ne 1 ]; then
      echo "$file: $client field '$field' occurs $count times, expected 1"
      failed=1
    fi
  done

  if ! printf '%s\n' "$section" | grep -q '^| official_source |.*https://'; then
    echo "$file: $client has no official source URL"
    failed=1
  fi
  if ! printf '%s\n' "$section" | grep -Eq '證明|證據邊界'; then
    echo "$file: $client does not state what its evidence proves or cannot prove"
    failed=1
  fi
done

artifacts=(
  "scripts/client-roundtrip-mock.py"
  "scripts/check-client-captures.py"
  "scripts/verify-client-roundtrip.sh"
  "$fixture_dir/anthropic-response.sse"
  "$fixture_dir/openai-response.sse"
  "$fixture_dir/captures/claude-code-2.1.267.json"
  "$fixture_dir/captures/codex-cli-0.153.0.json"
  "docs/client-roundtrip-evidence.md"
)
for artifact in "${artifacts[@]}"; do
  if [ ! -f "$artifact" ]; then
    echo "missing client roundtrip artifact: $artifact"
    failed=1
  fi
done

if ! python3 scripts/check-client-captures.py; then failed=1; fi

if [ ! -x scripts/verify-client-roundtrip.sh ] || \
   [ ! -x scripts/client-roundtrip-mock.py ] || \
   [ ! -x scripts/check-client-captures.py ]; then
  echo "client roundtrip scripts must be executable"
  failed=1
fi

[ "$failed" = 0 ] && echo "client compatibility contract ok"
exit "$failed"
