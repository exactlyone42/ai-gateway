#!/usr/bin/env bash
# Sum Claude Code token usage for one task window.
#
# SOURCE DEPENDENCY: this reads Claude Code's own transcript files, which are an
# undocumented internal format. It is scaffolding, not the destination -- see
# "可攜性" in docs/task-ledger.md. Once the gateway serves our own traffic (W05),
# the ledger reads the gateway's usage tables instead and this file goes away.
#
# The loop file declares the window (start/end); this script measures what it
# cost. Usage rows are deduped by requestId because the transcript writes the
# same assistant message several times while streaming.
#
#   ./scripts/token-ledger.sh --slug <slug>            # window from loops/<slug>.loop.md
#   ./scripts/token-ledger.sh --since 2026-09-08T13:00 [--until ...]
#   ./scripts/token-ledger.sh --slug <slug> --md        # markdown table row
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJECT_GLOB="${CLAUDE_PROJECT_GLOB:-$HOME/.claude/projects/*agent-pool-gateway*}"

slug=""
since=""
until_ts=""
as_md=0

while [ $# -gt 0 ]; do
  case "$1" in
    --slug)  slug="${2:?--slug needs a value}"; shift 2 ;;
    --since) since="${2:?--since needs a value}"; shift 2 ;;
    --until) until_ts="${2:?--until needs a value}"; shift 2 ;;
    --md)    as_md=1; shift ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# Read the window out of the loop file table when only a slug is given.
if [ -n "$slug" ] && [ -z "$since" ]; then
  loop_file="$REPO_ROOT/loops/$slug.loop.md"
  [ -f "$loop_file" ] || { echo "no loop file: $loop_file" >&2; exit 1; }
  since="$(awk -F'|' '/^\| *開始 *\|/ {gsub(/[ `]/,"",$3); print $3; exit}' "$loop_file")"
  until_ts="$(awk -F'|' '/^\| *結束 *\|/ {gsub(/[ `]/,"",$3); print $3; exit}' "$loop_file")"
fi

is_iso() { case "$1" in [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T*) return 0 ;; *) return 1 ;; esac; }

is_iso "$since" || { echo "開始/--since must be ISO 8601 (YYYY-MM-DDThh:mm+08:00), got: ${since:-<empty>}" >&2; exit 2; }
# An unfinished task has a placeholder in 結束; measure up to now instead.
is_iso "$until_ts" || until_ts="$(date -u +%Y-%m-%dT%H:%M:%S%z)"

# Timestamps in the transcript are UTC with a trailing Z. Normalise both bounds
# so a plain string comparison inside jq is correct.
to_utc() { date -u -d "$1" +%Y-%m-%dT%H:%M:%S.000Z; }
since_utc="$(to_utc "$since")"
until_utc="$(to_utc "$until_ts")"

# shellcheck disable=SC2206 # word splitting on the glob is intended
files=( $PROJECT_GLOB/*.jsonl )
[ -e "${files[0]}" ] || { echo "no transcripts under $PROJECT_GLOB" >&2; exit 1; }

report="$(jq -s --arg since "$since_utc" --arg until "$until_utc" '
  [ .[]
    | select(.type == "assistant")
    | select(.timestamp >= $since and .timestamp <= $until)
    | select((.message.model // "") != "<synthetic>")
    | { rid:    (.requestId // .uuid),
        agent:  (.agentName // "main"),
        model:  (.message.model // "-"),
        u:      (.message.usage // {}) } ]
  | unique_by(.rid)
  | (group_by(.agent) | map({
        agent:       .[0].agent,
        model:       (map(.model) | unique | join(",")),
        calls:       length,
        input:       (map(.u.input_tokens // 0)                | add),
        cache_write: (map(.u.cache_creation_input_tokens // 0) | add),
        cache_read:  (map(.u.cache_read_input_tokens // 0)     | add),
        output:      (map(.u.output_tokens // 0)               | add),
        thinking:    (map(.u.output_tokens_details.thinking_tokens // 0) | add)
     })) as $rows
  | { rows: $rows,
      total: {
        calls:       ($rows | map(.calls)       | add // 0),
        input:       ($rows | map(.input)       | add // 0),
        cache_write: ($rows | map(.cache_write) | add // 0),
        cache_read:  ($rows | map(.cache_read)  | add // 0),
        output:      ($rows | map(.output)      | add // 0),
        thinking:    ($rows | map(.thinking)    | add // 0) } }
' "${files[@]}")"

billed=$(jq -r '.total | (.input + .cache_write + .cache_read + .output)' <<<"$report")

# Positive control. "No usage in this window" and "the reader stopped working"
# print the same zeros, so a zero is reported as a failure, never as a result.
# Exit 3 means "not measured", which is not the same as "measured zero".
if [ "$(jq -r '.total.calls' <<<"$report")" = 0 ] || [ "$billed" = 0 ]; then
  cat >&2 <<MSG
no usage records in this window -- refusing to report 0 as a measurement.
  window:  $since_utc .. $until_utc
  scanned: ${#files[@]} transcript file(s) under $PROJECT_GLOB
Check the window first. If the window is right, the transcript format changed
and this reader needs fixing; see 可攜性 in docs/task-ledger.md.
MSG
  exit 3
fi

if [ "$as_md" = 1 ]; then
  jq -r --arg slug "${slug:-ad-hoc}" --arg since "$since" --arg until "$until_ts" --argjson billed "$billed" '
    .total
    | "| \($slug) | \($since) | \($until) | \(.calls) | \(.input + .cache_write) | \(.cache_read) | \(.output) | \($billed) |"
  ' <<<"$report"
  exit 0
fi

printf 'window  %s .. %s\n' "$since_utc" "$until_utc"
printf 'agent            calls      input  cache_wr  cache_rd     output   thinking  model\n'
jq -r '.rows[] | [.agent, .calls, .input, .cache_write, .cache_read, .output, .thinking, .model] | @tsv' <<<"$report" \
  | awk -F'\t' '{printf "%-15s %6d %10d %9d %10d %10d %10d  %s\n", $1,$2,$3,$4,$5,$6,$7,$8}'
jq -r '.total | ["TOTAL", .calls, .input, .cache_write, .cache_read, .output, .thinking] | @tsv' <<<"$report" \
  | awk -F'\t' '{printf "%-15s %6d %10d %9d %10d %10d %10d\n", $1,$2,$3,$4,$5,$6,$7}'
printf 'billed tokens (input + cache_write + cache_read + output): %s\n' "$billed"
