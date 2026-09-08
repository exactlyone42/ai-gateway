#!/usr/bin/env bash
# Quality gates. Run the cheap ones while iterating, the full set before a PR.
#
#   ./scripts/check.sh fmt         # rewrite formatting
#   ./scripts/check.sh gate        # fast: fmt-check + clippy on the library
#   ./scripts/check.sh loops       # loop-file discipline (sections, 200-line body)
#   ./scripts/check.sh bans        # banned constructs in production code
#   ./scripts/check.sh all         # everything CI runs
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

step() { printf '\n== %s\n' "$1"; }

fmt()        { step "cargo fmt";               cargo fmt --all; }
fmt_check()  { step "cargo fmt --check";       cargo fmt --all --check; }
clippy_fast(){ step "cargo clippy (lib)";      cargo clippy --workspace --all-features -- -D warnings; }
clippy_full(){ step "cargo clippy (all)";      cargo clippy --workspace --all-targets --all-features -- -D warnings; }
test_all()   { step "cargo test";              cargo test --workspace --all-features; }

# Machine-checkable loop-file rules. A loop file that drifts from the format is
# a loop file nobody can trust, so this is a gate and not a guideline.
loops() {
  step "loop files"
  local required=(目的 完成判準 已知 已做 已確定不可行 證據 來源 下一步)
  local failed=0 file section body

  while IFS= read -r file; do
    for section in "${required[@]}"; do
      grep -q "^## $section" "$file" || { echo "$file: missing section '## $section'"; failed=1; }
    done
    # Body = everything except the 已做 progress log, capped at 200 lines.
    body=$(awk '/^## 已做/{skip=1; next} /^## /{skip=0} !skip' "$file" | wc -l)
    if [ "$body" -gt 200 ]; then
      echo "$file: body is $body lines (limit 200) -- split the task"
      failed=1
    fi
    grep -q '^| 開始 |' "$file" || { echo "$file: missing 開始 row"; failed=1; }
    grep -q '^| 結束 |' "$file" || { echo "$file: missing 結束 row"; failed=1; }
  done < <(find loops -name '*.loop.md' -not -name '_TEMPLATE*')

  # Every loop file must be reachable from the index, or it is invisible.
  while IFS= read -r file; do
    grep -q "$(basename "$file")" loops/README.md || { echo "$file: not listed in loops/README.md"; failed=1; }
  done < <(find loops -name '*.loop.md' -not -name '_TEMPLATE*')

  [ "$failed" = 0 ] && echo "loop files ok"
  return "$failed"
}


# Production-code bans from docs/engineering-standards.md. Each rule here is a
# rule that doc claims is machine-checked, so the two must not drift.
#
# Test code is excluded by dropping everything from the first `#[cfg(test)]` to
# end of file, which is where this repo keeps its test modules.
bans() {
  step "banned constructs"
  local failed=0

  local sources=()
  while IFS= read -r f; do sources+=("$f"); done < <(
    find src crates -name '*.rs' -not -path '*/target/*' 2>/dev/null | sort
  )
  if [ "${#sources[@]}" = 0 ]; then
    echo "no Rust sources found -- nothing to check"
    return 0
  fi

  # 1. Rust 2018+ layout: no mod.rs anywhere.
  local modrs
  modrs=$(find src crates -name 'mod.rs' -not -path '*/target/*' 2>/dev/null || true)
  if [ -n "$modrs" ]; then
    echo "$modrs" | sed 's/^/mod.rs is banned (use <module>.rs + <module>\/): /'
    failed=1
  fi

  # 2. No panicking shortcuts on a production path.
  local hits
  hits=$(prod_grep "${sources[@]}" \
    | grep -E '\.unwrap\(\)|\.expect\(|panic!\(|todo!\(|unimplemented!\(' || true)
  if [ -n "$hits" ]; then
    echo "$hits" | sed 's/^/panicking shortcut on a production path: /'
    failed=1
  fi

  # 3. Model names live in configuration, never in code (spec section 20).
  hits=$(prod_grep "${sources[@]}" \
    | grep -E '"(claude|gpt|gemini|grok|o[0-9])-[a-z0-9.-]+"' || true)
  if [ -n "$hits" ]; then
    echo "$hits" | sed 's/^/hardcoded model name (put it in the model registry): /'
    failed=1
  fi

  # 4. The gateway speaks OTLP only; backend names belong in deploy/ and docs.
  hits=$(prod_grep "${sources[@]}" \
    | grep -iE 'grafana|prometheus|signoz|datadog|tempo' || true)
  if [ -n "$hits" ]; then
    echo "$hits" | sed 's/^/observability backend named in gateway code (OTLP only): /'
    failed=1
  fi

  # 5. High-cardinality attributes must not become metric labels.
  #    Boundary: this catches literal attribute keys on a metric call site. It
  #    does NOT catch a label name built at runtime -- that stays a review item.
  hits=$(prod_grep "${sources[@]}" \
    | grep -E '(counter|histogram|gauge|meter|\.record\(|\.add\()' \
    | grep -E '"(request_id|trace_id|session_id|attempt_id|credential_id|user_id|device_id|agent_id)"' || true)
  if [ -n "$hits" ]; then
    echo "$hits" | sed 's/^/high-cardinality metric label (trace and log only): /'
    failed=1
  fi

  [ "$failed" = 0 ] && echo "banned constructs ok"
  return "$failed"
}

# Print production lines only: each file with its `#[cfg(test)]` tail removed,
# prefixed with `path:line:` so the grep output stays addressable.
prod_grep() {
  local f
  for f in "$@"; do
    awk -v file="$f" '/#\[cfg\(test\)\]/ { exit } { print file ":" FNR ":" $0 }' "$f"
  done
}

case "${1:-gate}" in
  fmt)       fmt ;;
  fmt-check) fmt_check ;;
  gate)      fmt_check; clippy_fast; loops; bans ;;
  loops)     loops ;;
  bans)      bans ;;
  all|pre-push) fmt_check; clippy_full; test_all; loops; bans ;;
  *) sed -n '2,9p' "$0"; exit 2 ;;
esac
