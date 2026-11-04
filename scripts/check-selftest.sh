#!/usr/bin/env bash
# Self-test for the `bans` gate.
#
# A gate nobody has seen go red is a rubber stamp. This runs every ban rule
# against a fixture that should trip it, and against a fixture that should NOT,
# then fails if fewer assertions ran than expected -- because a half-executed
# suite printing "0 failed" is the classic false green.
#
# When you add a rule to `bans`, add both assertions here and bump the count.
set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

EXPECTED_ASSERTS=33

ran=0
failed=0
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# fixture <relative-path> <<'RS' ... RS
fixture() {
  local path="$work/$1"
  mkdir -p "$(dirname "$path")"
  cat > "$path"
}

reset() { rm -rf "${work:?}"/*; }

run_bans() { ./scripts/check.sh bans "$work" 2>&1; }
run_deadfiles() { ./scripts/check.sh deadfiles "$work" 2>&1; }

# Which gate the next expectation runs. Defaults to bans.
gate_cmd=run_bans

# The rule must fire, and its message must name the rule -- an exit code alone
# does not prove the RIGHT rule caught it.
expect_red() {
  local name="$1" want="$2" out status
  ran=$((ran + 1))
  set +e; out="$($gate_cmd)"; status=$?; set -e
  if [ "$status" = 0 ]; then
    echo "FAIL [$name]: expected a violation, gate passed"
    failed=$((failed + 1))
  elif ! printf '%s' "$out" | grep -qF "$want"; then
    echo "FAIL [$name]: fired, but not with '$want'"
    printf '%s\n' "$out" | sed 's/^/       /'
    failed=$((failed + 1))
  else
    echo "ok   [$name]"
  fi
}

expect_green() {
  local name="$1" out status
  ran=$((ran + 1))
  set +e; out="$($gate_cmd)"; status=$?; set -e
  if [ "$status" != 0 ]; then
    echo "FAIL [$name]: expected a pass, gate fired"
    printf '%s\n' "$out" | sed 's/^/       /'
    failed=$((failed + 1))
  else
    echo "ok   [$name]"
  fi
}

printf '== bans self-test\n'

# ---- rule 2: panicking shortcuts -------------------------------------------
for construct in \
  'let _ = none().unwrap();' \
  'let _ = none().expect("x");' \
  'panic!("x");' \
  'todo!();' \
  'unreachable!();' \
  'assert!(true);' \
  'let _ = err().unwrap_err();'
do
  reset
  fixture a.rs <<RS
fn f() { $construct }
RS
  expect_red "panicking: $construct" "panicking shortcut"
done

# The same construct inside a test module must NOT fire. Without this the rule
# could be matching everything and still look correct above.
reset
fixture a.rs <<'RS'
fn f() -> u8 { 1 }

#[cfg(test)]
mod tests {
    #[test]
    fn t() { let _ = Some(1u8).unwrap(); }
}
RS
expect_green "test module is exempt"

# A test module in the MIDDLE of a file must not blind the production code that
# follows it.
reset
fixture a.rs <<'RS'
#[cfg(test)]
mod tests {
    #[test]
    fn t() { assert!(true); }
}

fn after_the_test_module() { let _ = none().unwrap(); }
RS
expect_red "code after a test module is still scanned" "panicking shortcut"

# A whole test crate carries no #[cfg(test)] marker at all.
reset
fixture tests/integration.rs <<'RS'
#[test]
fn t() { let _ = none().unwrap(); }
RS
expect_green "tests/ directory is exempt"

# ---- rule 3: hardcoded model names -----------------------------------------
reset
fixture a.rs <<'RS'
const M: &str = "claude-sonnet-4-6";
RS
expect_red "model name" "hardcoded model name"

# A provider-prefixed id inside the string must still be caught.
reset
fixture a.rs <<'RS'
const M: &str = "anthropic/claude-sonnet-4-5";
RS
expect_red "model name behind a provider prefix" "hardcoded model name"

# The Claude Code attribution headers are not model names.
reset
fixture a.rs <<'RS'
const H: &str = "x-claude-code-session-id";
const G: &str = "x-claude-code-parent-agent-id";
RS
expect_green "claude-code header names are allowed"

# ---- rule 4: observability backends ----------------------------------------
reset
fixture a.rs <<'RS'
const U: &str = "http://prometheus:9090";
RS
expect_red "observability backend" "observability backend"

# `temporary` must not read as `tempo`; retry code says this word constantly.
reset
fixture a.rs <<'RS'
const E: &str = "temporary upstream failure, retrying";
RS
expect_green "the word temporary is allowed"

# ---- rule 5: high-cardinality metric labels --------------------------------
reset
fixture a.rs <<'RS'
fn f(m: Meter) { m.u64_counter("x").add(1, &[KeyValue::new("request_id", id)]); }
RS
expect_red "metric label, one line" "high-cardinality metric label"

# rustfmt wraps this call, so the rule must span lines.
reset
fixture a.rs <<'RS'
fn f(m: Meter) {
    m.u64_counter("gateway_requests_total").add(
        1,
        &[KeyValue::new("user_id", uid)],
    );
}
RS
expect_red "metric label, wrapped across lines" "high-cardinality metric label"

# A low-cardinality label is the whole point of metrics. It has to be one the
# allowlist (rule 7) also accepts, or this fixture would prove nothing about
# rule 5 -- it would just trip the other rule.
reset
fixture a.rs <<'RS'
fn f(m: Meter) {
    m.u64_counter("gateway_requests_total").add(
        1,
        &[KeyValue::new("http.route", route)],
    );
}
RS
expect_green "low-cardinality metric label is allowed"

# ---- rule 6: no camelCase ---------------------------------------------------
reset
fixture a.rs <<'RS'
fn f() { let requestId = 1; }
RS
expect_red "camelCase identifier" "camelCase in Rust code"

# A JSON key written by hand is the same violation -- that is how the wire
# format leaks back into the code.
reset
fixture a.rs <<'RS'
const KEY: &str = "statusCode";
RS
expect_red "camelCase JSON key literal" "camelCase in Rust code"

# The sanctioned way to reach a camelCase wire format: snake_case fields,
# PascalCase types, one serde attribute.
reset
fixture a.rs <<'RS'
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub request_id: String,
    pub status_code: u16,
}
RS
expect_green "serde rename_all is the allowed exception"

# The real code shape: attributes are collected first, recorded later. A rule
# that only looked forward from the record call missed this entirely.
reset
fixture a.rs <<'RS'
fn f(m: Meter, id: String) {
    let mut attributes = vec![KeyValue::new("http.route", route)];
    attributes.push(KeyValue::new("request_id", id));
    m.f64_histogram("http.server.request.duration")
        .build()
        .record(1.0, &attributes);
}
RS
expect_red "metric label collected before the record call" "high-cardinality metric label"

# A key that is not a literal cannot be checked, so it is refused: this is how
# a const smuggled a banned attribute past every rule.
reset
fixture a.rs <<'RS'
const LABEL: &str = "request_id";

fn f(m: Meter, id: String) {
    m.f64_histogram("http.server.request.duration")
        .build()
        .record(1.0, &[KeyValue::new(LABEL, id)]);
}
RS
expect_red "metric attribute key from a constant" "not on the allowlist"

# ---- rule 7: metric attribute allowlist -------------------------------------
# `model` is not on the allowlist and is not one of rule 5's known ids, so this
# proves the allowlist itself fires rather than the blocklist.
reset
fixture a.rs <<'RS'
fn f(m: Meter, model: String) {
    m.f64_histogram("http.server.request.duration")
        .build()
        .record(1.0, &[KeyValue::new("model", model)]);
}
RS
expect_red "metric attribute off the allowlist" "not on the allowlist"

reset
fixture a.rs <<'RS'
fn f(m: Meter, route: String) {
    m.f64_histogram("http.server.request.duration")
        .build()
        .record(1.0, &[KeyValue::new("http.route", route)]);
}
RS
expect_green "allowlisted metric attribute is fine"

# ---- rule 1: module layout --------------------------------------------------
reset
fixture thing/mod.rs <<'RS'
pub fn f() {}
RS
expect_red "mod.rs" "mod.rs is banned"

# ---- a clean tree passes ----------------------------------------------------
reset
fixture a.rs <<'RS'
pub fn f(x: Option<u8>) -> Result<u8, Error> { x.ok_or(Error::Missing) }
RS
expect_green "clean tree"

# ---- dead files -------------------------------------------------------------
# Measured 2026-09-08: an orphan .rs file passes cargo fmt --check, clippy
# --all-targets AND cargo test, because cargo never compiles it. So this gate is
# what stops such a file from carrying anything past every other gate.
gate_cmd=run_deadfiles

reset
fixture main.rs <<'RS'
fn main() {}
RS
fixture orphan.rs <<'RS'
pub fn f() {}
RS
expect_red "orphan file no mod reaches" "no mod declaration reaches"

# The mirror control: a properly declared file must pass, or the rule is just
# "every new file is red", which catches nothing useful.
reset
fixture main.rs <<'RS'
mod declared;

fn main() {}
RS
fixture declared.rs <<'RS'
pub fn f() {}
RS
expect_green "declared file is reachable"

reset
fixture main.rs <<'RS'
pub(crate) mod declared;
fn main() {}
RS
fixture declared.rs <<'RS'
pub fn f() {}
RS
expect_green "crate-visible module is reachable"

gate_cmd=run_bans

printf '\n'
# ---- rule 8: configuration reads bypassing `setting` -----------------------
reset
fixture a.rs <<'RS'
fn f() -> Option<String> { env::var("SOME_SETTING").ok() }
RS
expect_red "unmarked configuration read" "bypasses"

reset
fixture a.rs <<'RS'
fn f() -> Option<String> {
    // raw-env: empty is a decision here, not an omission.
    env::var("SOME_SETTING").ok()
}
RS
expect_green "a marked configuration read is allowed"

# The marker only counts on the line above, so a trailing one must not pass.
reset
fixture a.rs <<'RS'
fn f() -> Option<String> {
    env::var("SOME_SETTING").ok()
    // raw-env: too late to excuse the line above.
}
RS
expect_red "marker below the read does not excuse it" "bypasses"

if [ "$ran" != "$EXPECTED_ASSERTS" ]; then
  echo "FAIL: $ran assertions ran, expected $EXPECTED_ASSERTS -- the suite did not fully execute"
  exit 1
fi
if [ "$failed" != 0 ]; then
  echo "FAIL: $failed of $ran assertions failed"
  exit 1
fi
echo "bans self-test ok: $ran/$EXPECTED_ASSERTS assertions passed"
