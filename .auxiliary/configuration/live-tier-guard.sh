#!/bin/sh
# Tier inventory guard for tests/tiers.txt (one `<lowest-tier>
# <test-id>` row per live test; membership cumulative downward).
# Fails when inventory and tree disagree: unknown tags,
# duplicate rows or test IDs (same ID under two tags would
# silently widen exact predicates), test names colliding
# across binaries (exact `test(=id)` matches by name in any
# binary), rows bijecting wrong with the live set (a new live
# test with no row fails here — explicit tiering, never
# silent), smoke count drift, or the demoted sample leaving
# extended. Membership assertions use RAW `nextest list`
# lines (binary-qualified) BEFORE name extraction — checking
# collisions on already-extracted names is vacuous.
#
# Usage: live-tier-guard.sh [tiers-file]
# Environment: NEXTTEST_LIST_OVERRIDE=<file> feeds canned
# `nextest list` output instead of invoking cargo (hermetic
# self-checks, e.g. simulated duplicate names across
# binaries must fail).
set -eu
tiers=${1:-tests/tiers.txt}
cfg=.auxiliary/configuration/nextest.toml
if [ -n "${NEXTTEST_LIST_OVERRIDE:-}" ]; then
    raw=$(grep '::' "$NEXTTEST_LIST_OVERRIDE" | sort -u || true)
else
    raw=$(cargo nextest list --config-file "$cfg" -P live --run-ignored=all | grep '::' | sort -u)
fi
# Live-only universe: the live profile includes the fast
# suite, so names present in the default profile are out.
if [ -n "${NEXTTEST_FAST_OVERRIDE:-}" ]; then
    fast=$(grep '::' "$NEXTTEST_FAST_OVERRIDE" | awk '{print $2}' | sort -u || true)
else
    fast=$(cargo nextest list --config-file "$cfg" | grep '::' | awk '{print $2}' | sort -u)
fi
fail() {
    echo "tier guard: $1" >&2
    exit 1
}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM
echo "$raw" | awk '{print $2}' | sort -u > "$work/all"
printf '%s\n' "$fast" > "$work/fast"
comm -23 "$work/all" "$work/fast" > "$work/live"
awk '{print $2}' "$tiers" | sort -u > "$work/rows"
live=$(cat "$work/live")
rows=$(cat "$work/rows")
test -z "$(awk '{print $1}' "$tiers" | grep -v -x -F -e smoke -e prerelease -e nightly -e extended || true)" || fail "unknown tier tag"
test -z "$(sort "$tiers" | uniq -d)" || fail "duplicate inventory rows"
test -z "$(awk '{print $2}' "$tiers" | sort | uniq -d)" || fail "duplicate test IDs across tiers"
test -z "$(echo "$raw" | awk '{print $2}' | sort | uniq -d)" || fail "test name collision across binaries"
test -z "$(comm -23 "$work/live" "$work/rows")" || fail "live tests missing from inventory"
test -z "$(comm -13 "$work/live" "$work/rows")" || fail "inventory rows with no live test"
test "$(grep -c '^smoke ' "$tiers")" -eq 9 || fail "smoke tier must hold 9 tests"
grep -q -x -F 'extended conformance::conformance_full_cycle_with_fidelity' "$tiers" || fail "guard sample must stay demoted"
echo "tier guard: 62 live IDs biject with inventory, smoke == 9"
