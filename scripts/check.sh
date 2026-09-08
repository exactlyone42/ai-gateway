#!/usr/bin/env bash
# Quality gates. Run the cheap ones while iterating, the full set before a PR.
#
#   ./scripts/check.sh fmt         # rewrite formatting
#   ./scripts/check.sh gate        # fast: fmt-check + clippy on the library
#   ./scripts/check.sh loops       # loop-file discipline (sections, 200-line body)
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

case "${1:-gate}" in
  fmt)       fmt ;;
  fmt-check) fmt_check ;;
  gate)      fmt_check; clippy_fast; loops ;;
  loops)     loops ;;
  all|pre-push) fmt_check; clippy_full; test_all; loops ;;
  *) sed -n '2,9p' "$0"; exit 2 ;;
esac
