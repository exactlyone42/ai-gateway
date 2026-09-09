#!/usr/bin/env bash
# Quality gates. Run the cheap ones while iterating, the full set before a PR.
#
#   ./scripts/check.sh fmt         # rewrite formatting
#   ./scripts/check.sh gate        # fast: fmt-check + clippy + loops + bans
#   ./scripts/check.sh loops       # loop-file discipline (sections, 200-line body)
#   ./scripts/check.sh bans        # banned constructs in production code
#   ./scripts/check.sh deadfiles   # .rs files no `mod` declaration reaches
#   ./scripts/check.sh selftest    # prove the gates above actually catch things
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
    # Every loop file must be reachable from the index, or it is invisible.
    grep -q "$(basename "$file")" loops/README.md || { echo "$file: not listed in loops/README.md"; failed=1; }
  done < <(find loops -name '*.loop.md' -not -name '_TEMPLATE*')

  [ "$failed" = 0 ] && echo "loop files ok"
  return "$failed"
}

# Print production lines as `path:line:text`, dropping test code.
#
# Test code is excluded two ways, because there are two kinds:
#   - `#[cfg(test)] mod ... { ... }` blocks, tracked by brace depth so a test
#     module in the MIDDLE of a file does not blind the rest of it.
#   - files under a `tests/` or `benches/` directory, which are already test
#     crates and carry no `#[cfg(test)]` marker at all.
prod_lines() {
  local f
  for f in "$@"; do
    case "$f" in */tests/*|*/benches/*) continue ;; esac
    awk -v file="$f" '
      # A #[cfg(test)] attribute arms the skip; the module line that follows
      # opens it. An attribute on a plain item (a `use ...;`) disarms instead.
      /^[[:space:]]*#\[cfg\(test\)\]/ { armed = 1; next }
      armed && /mod[[:space:]]/ && /\{/ {
        armed = 0
        depth = gsub(/\{/, "{") - gsub(/\}/, "}")
        if (depth > 0) intest = 1
        next
      }
      armed && /;[[:space:]]*$/ { armed = 0 }
      intest {
        depth += gsub(/\{/, "{") - gsub(/\}/, "}")
        if (depth <= 0) intest = 0
        next
      }
      { print file ":" FNR ":" $0 }
    ' "$f"
  done
}

# Production-code bans from docs/engineering-standards.md. Every rule that doc
# claims is machine-checked must exist here, and `selftest` proves each one
# actually fires. Pass roots as arguments; defaults to the real source tree.
bans() {
  step "banned constructs"
  local roots=("$@")
  [ "${#roots[@]}" -gt 0 ] || roots=(src crates)
  local failed=0 hits modrs sources=()

  while IFS= read -r f; do sources+=("$f"); done < <(
    find "${roots[@]}" -name '*.rs' -not -path '*/target/*' 2>/dev/null | sort
  )
  if [ "${#sources[@]}" = 0 ]; then
    echo "no Rust sources under ${roots[*]} -- nothing to check"
    return 0
  fi

  # 1. Rust 2018+ layout: no mod.rs anywhere.
  modrs=$(find "${roots[@]}" -name 'mod.rs' -not -path '*/target/*' 2>/dev/null || true)
  if [ -n "$modrs" ]; then
    echo "$modrs" | sed 's/^/mod.rs is banned (use <module>.rs + <module>\/): /'
    failed=1
  fi

  local prod
  prod=$(prod_lines "${sources[@]}")

  # 2. No panicking shortcuts on a production path. Errors travel up; they do
  #    not blow up in place.
  hits=$(printf '%s\n' "$prod" | grep -E \
    '\.unwrap\(\)|\.unwrap_err\(\)|\.expect\(|\.expect_err\(|panic!\(|todo!\(|unimplemented!\(|unreachable!\(|[^_a-zA-Z]assert(_eq|_ne)?!\(' \
    || true)
  if [ -n "$hits" ]; then
    echo "$hits" | sed 's/^/panicking shortcut on a production path: /'
    failed=1
  fi

  # 3. Model names live in configuration, never in code (spec section 20).
  #    A version digit is what separates a model id from a header name such as
  #    `x-claude-code-session-id`, which must stay allowed.
  hits=$(printf '%s\n' "$prod" | grep -Ei \
    '"[^"]*(claude|gpt|gemini|grok)-[a-z0-9.-]*[0-9][a-z0-9.-]*' || true)
  if [ -n "$hits" ]; then
    echo "$hits" | sed 's/^/hardcoded model name (put it in the model registry): /'
    failed=1
  fi

  # 4. The gateway speaks OTLP only; backend names belong in deploy/ and docs.
  #    Word-anchored: `temporary` must not read as `tempo`.
  hits=$(printf '%s\n' "$prod" | grep -Ei \
    '\b(grafana|prometheus|signoz|datadog|tempo|loki|jaeger)\b' || true)
  if [ -n "$hits" ]; then
    echo "$hits" | sed 's/^/observability backend named in gateway code (OTLP only): /'
    failed=1
  fi

  # 5. High-cardinality attributes must not become metric labels.
  #    rustfmt wraps these calls, so the window spans the following lines too.
  #    Boundary: a label name built at runtime is invisible here -- that stays a
  #    review item, and docs/engineering-standards.md says so.
  hits=$(printf '%s\n' "$prod" | awk '
    /counter|histogram|gauge|\.record\(|\.add\(/ { window = 6 }
    window > 0 {
      if ($0 ~ /"(request_id|trace_id|session_id|attempt_id|credential_id|user_id|device_id|agent_id)"/) print
      window--
    }
  ' || true)
  if [ -n "$hits" ]; then
    echo "$hits" | sed 's/^/high-cardinality metric label (trace and log only): /'
    failed=1
  fi

  [ "$failed" = 0 ] && echo "banned constructs ok"
  return "$failed"
}


# A .rs file that no `mod` declaration reaches is not compiled at all, so
# `cargo fmt --check`, `cargo clippy --all-targets` and `cargo test` never see
# it (measured 2026-09-08). Such a file can carry an `unwrap()` and a hardcoded
# model name into the repo with CI fully green -- the grep-based gates are the
# only thing standing in its way, so a dead file is a hole in every other gate.
#
# Boundary: this walks plain `mod name;` declarations from each crate root. It
# does not follow `#[path = "..."]` overrides or `mod` inside a macro.
deadfiles() {
  step "dead files"
  local roots=("$@")
  [ "${#roots[@]}" -gt 0 ] || roots=(src crates)

  local queue=() seen=" " f dir m cand failed=0 root
  for root in "${roots[@]}"; do
    while IFS= read -r f; do queue+=("$f"); done < <(
      find "$root" -path '*/target' -prune -o \
           \( -name 'main.rs' -o -name 'lib.rs' -o -name 'build.rs' \) -print 2>/dev/null
    )
  done

  while [ "${#queue[@]}" -gt 0 ]; do
    f="${queue[0]}"; queue=("${queue[@]:1}")
    case "$seen" in *" $f "*) continue ;; esac
    seen="$seen$f "
    # A crate root's modules are its siblings; any other module's children live
    # in a directory named after it.
    case "$f" in
      */main.rs|*/lib.rs|*/build.rs) dir="$(dirname "$f")" ;;
      *) dir="${f%.rs}" ;;
    esac
    while IFS= read -r m; do
      cand="$dir/$m.rs"
      [ -f "$cand" ] && queue+=("$cand")
    done < <(sed -nE 's/^[[:space:]]*(pub[[:space:]]+)?mod[[:space:]]+([a-z_][a-z0-9_]*)[[:space:]]*;.*/\2/p' "$f")
  done

  while IFS= read -r f; do
    case "$f" in */tests/*|*/benches/*|*/examples/*) continue ;; esac
    case "$seen" in *" $f "*) continue ;; esac
    echo "$f: no mod declaration reaches this file -- cargo never compiles it"
    failed=1
  done < <(find "${roots[@]}" -name '*.rs' -not -path '*/target/*' 2>/dev/null | sort)

  [ "$failed" = 0 ] && echo "dead files ok"
  return "$failed"
}

case "${1:-gate}" in
  fmt)       fmt ;;
  fmt-check) fmt_check ;;
  gate)      fmt_check; clippy_fast; loops; bans; deadfiles ;;
  loops)     loops ;;
  bans)      shift; bans "$@" ;;
  deadfiles) shift; deadfiles "$@" ;;
  selftest)  exec ./scripts/check-selftest.sh ;;
  all|pre-push) fmt_check; clippy_full; test_all; loops; bans; deadfiles; ./scripts/check-selftest.sh ;;
  *) sed -n '2,11p' "$0"; exit 2 ;;
esac
