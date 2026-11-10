#!/usr/bin/env bash
# Quality gates. Run the cheap ones while iterating, the full set before a PR.
#
#   ./scripts/check.sh fmt         # rewrite formatting
#   ./scripts/check.sh gate        # fast: fmt-check + clippy + loops + bans + layout
#   ./scripts/check.sh loops       # loop-file discipline (sections, 200-line body)
#   ./scripts/check.sh bans        # banned constructs in production code
#   ./scripts/check.sh deadfiles   # .rs files no `mod` declaration reaches
#   ./scripts/check.sh layout      # root / src top-level directory allowlist
#   ./scripts/check.sh clients     # client compatibility document contract
#   ./scripts/check.sh frontend    # admin UI lint + types + tests + build
#   ./scripts/check.sh secrets     # run the gateway and scan its log for secrets
#   ./scripts/check.sh selftest    # prove the gates above actually catch things
#   ./scripts/check.sh all         # everything CI runs
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

step() { printf '\n== %s\n' "$1"; }

fmt()        { step "cargo fmt";               cargo fmt --all; }
fmt_check()  { step "cargo fmt --check";       cargo fmt --all --check; }
clippy_fast(){ step "cargo clippy (lib)";      cargo clippy --workspace --all-features -- -D warnings; }
clippy_full(){ step "cargo clippy (all)";      cargo clippy --workspace --all-targets --all-features -- -D warnings; }
test_all() {
  step "cargo test"
  if [ -z "${DATABASE_URL:-}" ]; then
    # Database-backed behavior must run in CI, not become an ignored test. The
    # compose health check owns waiting; this is one bounded service action and
    # never a retry loop against the database port.
    docker compose -f deploy/docker-compose.yml up -d --wait postgres >/dev/null
    export DATABASE_URL="postgres://gateway:gateway-dev@127.0.0.1:${POSTGRES_PORT:-15432}/gateway"
  fi
  cargo test --workspace --all-features
  cargo test --lib personal_tokens_survive_process_restart_and_revocation -- --ignored --test-threads=1
  python3 scripts/check-provider-compose.py
}

frontend() {
  step "admin frontend"
  if grep -RInE 'localStorage|sessionStorage' web/src; then
    echo "persistent browser storage is forbidden for Admin sessions"
    return 1
  fi
  (
    cd web
    npm run lint
    npm run typecheck
    npm run test
    npm run build
  )
}

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
  #    Matched on the attribute constructor itself, not on proximity to a
  #    metric call: the real code builds the attribute list first and records
  #    afterwards (measured 2026-09-09 -- a window that only looked forward
  #    from `.record(` never saw it).
  hits=$(printf '%s\n' "$prod" | grep -E \
    'KeyValue::new\("(request_id|trace_id|session_id|attempt_id|credential_id|user_id|device_id|agent_id)"' \
    || true)
  if [ -n "$hits" ]; then
    echo "$hits" | sed 's/^/high-cardinality metric label (trace and log only): /'
    failed=1
  fi

  # 6. No camelCase anywhere in Rust code -- identifiers or JSON key literals.
  #    A wire format that wants camelCase (the SRE log contract does) gets it
  #    from `#[serde(rename_all = "camelCase")]`, so the rename lives in one
  #    attribute instead of spreading foreign-looking names through the code.
  #    That attribute is the single allowed occurrence of the word.
  #    Match the code only, never the `path:line:` prefix: a temp directory
  #    named `tmp.al4IJAZBMd` matched the pattern and turned every line red
  #    (measured 2026-09-09, mktemp in the self-test).
  hits=$(printf '%s\n' "$prod" | grep -v 'rename_all[[:space:]]*=' | awk '
    {
      code = $0
      sub(/^[^:]*:[0-9]+:/, "", code)
      if (code ~ /(^|[^A-Za-z0-9_])[a-z][a-z0-9_]*[A-Z]/) print
    }' || true)
  if [ -n "$hits" ]; then
    echo "$hits" | sed 's/^/camelCase in Rust code (stay snake_case; let serde rename the wire format): /'
    failed=1
  fi

  # 8. Configuration reads go through `setting`, which treats an empty value
  #    as unset. That is what lets the deployment template forward every
  #    variable without holding a second copy of the defaults; a direct
  #    `env::var` silently opts out and turns a forwarded blank into a parse
  #    error at startup. Mark a deliberate raw read with `raw-env:` on the line
  #    above it, saying why empty is not the same as absent there.
  hits=$(printf '%s\n' "$prod" | awk '
    { code = $0; sub(/^[^:]*:[0-9]+:/, "", code)
      if (code ~ /env::var\(/ && previous !~ /raw-env:/) print
      previous = code }')
  if [ -n "$hits" ]; then
    echo "$hits" | sed 's/^/configuration read bypasses `setting` (mark it `raw-env:` if deliberate): /'
    failed=1
  fi

  # 7. Metric attributes come from an explicit allowlist, because the failure
  #    mode of a wrong one is a metrics backend that falls over days later,
  #    far from the commit that caused it. Rule 5 blocks the ids we already
  #    know are unbounded; this one requires every attribute to be argued for.
  local allow="scripts/metric-labels.txt"
  hits=$(printf '%s\n' "$prod" | awk -v allow="$allow" '
    BEGIN {
      while ((getline line < allow) > 0) {
        sub(/#.*/, "", line)
        gsub(/[ \t\r]/, "", line)
        if (line != "") permitted[line] = 1
      }
    }
    {
      rest = $0
      # A key that is not a literal cannot be checked at all, so it is
      # refused outright: a `const LABEL: &str = \"request_id\"` passed here
      # used to satisfy every rule while doing the exact forbidden thing
      # (measured 2026-09-09).
      # rustfmt puts the arguments on the next line when the call is long, so
      # the check has to carry over one line.
      code = $0
      sub(/^[^:]*:[0-9]+:/, "", code)
      if (pending) {
        pending = 0
        head = code
        sub(/^[[:space:]]+/, "", head)
        if (substr(head, 1, 1) != "\"") { print "  (key is not a literal) " held }
      }
      probe = code
      while (match(probe, /KeyValue::new\(/)) {
        after = substr(probe, RSTART + RLENGTH)
        sub(/[[:space:]]+$/, "", after)
        if (after == "") { pending = 1; held = $0 }
        else if (substr(after, 1, 1) != "\"") { print "  (key is not a literal) " $0 }
        probe = after
      }
      while (match(rest, /KeyValue::new\("[^"]+"/)) {
        key = substr(rest, RSTART + 15, RLENGTH - 16)
        if (!(key in permitted)) { print; break }
        rest = substr(rest, RSTART + RLENGTH)
      }
    }' || true)
  if [ -n "$hits" ]; then
    echo "$hits" | sed "s|^|metric attribute is not on the allowlist ($allow): |"
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
    done < <(sed -nE 's/^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+([a-z_][a-z0-9_]*)[[:space:]]*;.*/\3/p' "$f")
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

in_list() {
  local want="$1" item
  shift
  for item in "$@"; do [ "$item" = "$want" ] && return 0; done
  return 1
}

layout() {
  step "directory layout"
  local root_dirs=(.github .repo-policy .sqlx deploy docs loops migrations queries scripts src tests web)
  local root_files=(.dockerignore .env.example .gitattributes .gitignore AGENTS.md CLAUDE.md Cargo.lock Cargo.toml DEVELOPMENT.md Dockerfile LICENSE README.md build.rs)
  local src_dirs=(adapters admin orchestration routes services)
  local doc=docs/directory-structure.md
  local paths kind name failed=0

  if [ -n "${1:-}" ]; then
    paths="$(cat "$1")"
  else
    paths="$(git ls-files -co --exclude-standard | sort -u | grep -vxFf <(git ls-files -d) || true)"
  fi

  while read -r kind name; do
    if [ "$kind" = d ] && ! in_list "$name" "${root_dirs[@]}"; then
      echo "$name/: top-level directory is not on the allowlist -- add it here and to $doc in the same change"
      failed=1
    elif [ "$kind" = f ] && ! in_list "$name" "${root_files[@]}"; then
      echo "$name: file in the repository root is not on the allowlist -- implementation and notes do not go in the root"
      failed=1
    fi
  done < <(printf '%s\n' "$paths" | awk -F/ 'NF>1{print "d", $1} NF==1&&$0!=""{print "f", $1}' | sort -u)

  while read -r name; do
    if ! in_list "$name" "${src_dirs[@]}"; then
      echo "src/$name/: src top-level directory is not on the allowlist -- extend an existing module, or add it here and to the document in the same change"
      failed=1
    fi
  done < <(printf '%s\n' "$paths" | awk -F/ '$1=="src"&&NF>2{print $2}' | sort -u)

  while read -r name; do
    echo "$name: catch-all directory name (misc/temp/tmp/others/helpers/utils/common) -- name the responsibility instead"
    failed=1
  done < <(printf '%s\n' "$paths" | grep -E '(^|/)(misc|temp|tmp|others|helpers|utils|common)/' | sed -E 's#((^|.*/)(misc|temp|tmp|others|helpers|utils|common))/.*#\1#' | sort -u || true)

  for name in "${root_dirs[@]}"; do
    grep -qF "\`$name/\`" "$doc" || { echo "$doc: top-level directory '$name/' is not described"; failed=1; }
  done
  for name in "${src_dirs[@]}"; do
    grep -qF "\`src/$name/\`" "$doc" || { echo "$doc: 'src/$name/' is not described"; failed=1; }
  done

  [ "$failed" = 0 ] && echo "directory layout ok"
  return "$failed"
}

clients() {
  step "client compatibility"
  ./scripts/check-client-compatibility.sh
}

case "${1:-gate}" in
  fmt)       fmt ;;
  fmt-check) fmt_check ;;
  gate)      fmt_check; clippy_fast; loops; bans; deadfiles; layout; clients ;;
  loops)     loops ;;
  bans)      shift; bans "$@" ;;
  deadfiles) shift; deadfiles "$@" ;;
  layout)    shift; layout "$@" ;;
  clients)   clients ;;
  frontend)  frontend ;;
  secrets)   step "log secrets"; ./scripts/check-log-secrets.sh ;;
  selftest)  exec ./scripts/check-selftest.sh ;;
  all|pre-push)
    python3 - <<'PY'
import ast
from pathlib import Path
for path in Path('scripts').glob('*.py'):
    ast.parse(path.read_text(), filename=str(path))
PY
    python3 scripts/check-migrations.py
    python3 -m unittest discover -s scripts -p "test_deploy*.py"
    python3 -m unittest scripts/test_capture_backup_gateway_flags.py scripts/test_capture_restore_validation.py
    python3 -m unittest scripts/test_client_tool_roundtrip_fixture.py scripts/test_client_capture_validation.py scripts/test_client_compact_fixture.py scripts/test_client_slow_reader.py
    python3 -m unittest scripts/test_backup_capture.py scripts/test_backup_docker_orphan.py scripts/test_capture_backup.py scripts/test_capture_backup_freshness.py scripts/test_capture_backup_source_state.py scripts/test_capture_storage_identity.py scripts/test_capture_storage_identity_live.py scripts/test_capture_database_identity_live.py scripts/test_ledger_backup_source.py scripts/test_ledger_backup_source_live.py scripts/test_ledger_watchdog.py scripts/test_legacy_archive_stage.py scripts/test_legacy_backup_source.py scripts/test_legacy_backup_live.py scripts/test_capture_restore.py scripts/test_restore_ledger_backup.py scripts/test_restore_ledger_live.py scripts/test_erasure_witness.py
    if [[ "${RUN_ERASURE_WITNESS_DOCKER:-0}" == "1" ]]; then python3 scripts/test_remote_witness_tls.py; fi
    python3 -m unittest discover -s scripts -p "test_sync_codex_auth.py"
    frontend; fmt_check; clippy_full; test_all; loops; bans; deadfiles; layout; clients
    step "log secrets"; ./scripts/check-log-secrets.sh
    ./scripts/check-selftest.sh
    ;;
  *) sed -n '2,14p' "$0"; exit 2 ;;
esac
