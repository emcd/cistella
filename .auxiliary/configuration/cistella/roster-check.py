#!/usr/bin/env python3
"""Roster check for the flaky live-test quarantine tier.

Proves, at the exact tip and from execution output (never from
reading config): per-predicate exact-one live-eligible identity
verified against real `nextest list` selection (not a Python
substring approximation); informational-equals-roster; tier
disjointness; union against the independent baseline; ordinary
preservation. All identity comparisons use composite
(binary-id, test-name) identities; bare names never identify.

Discovery uses three different flag sets on purpose:
- baseline: NO profile flag (neutral) + --run-ignored=all +
  --ignore-default-filter (bypasses tier/roster/default
  filters). It is the authoritative universe.
- gating selection: -P live, --run-ignored=all, NO bypass —
  exactly the legs' flags. A real default-filter omission
  drops tests here while the baseline still sees them.
- flaky selection: -P flaky, --run-ignored=all, NO bypass.

Usage:
  roster-check.py <roster.toml> <inclusion -E> <exclusion -E>
      [--config-file <nextest.toml>]
  roster-check.py <roster.toml> GENERATE GENERATE
      [--config-file <nextest.toml>]   # print filter pair
  roster-check.py --self-test         # backend-free fixture controls

Exit 0 on success; non-zero with a diagnostic otherwise.
Requires Python >= 3.11 (tomllib).
"""

import json
import subprocess
import sys

try:
    import tomllib
except ImportError:
    sys.exit("roster-check requires Python >= 3.11 (tomllib)")


def fail(msg):
    print(f"roster-check FAILED: {msg}")
    raise SystemExit(1)


# ------------------------------------------------------------------
# Pure core (backend-free; exercised by --self-test fixtures).
# ------------------------------------------------------------------


def parse_roster(text, source="<roster>"):
    """Strict roster parse. The document must be exactly one table
    with a single `quarantined` key holding a list of strings.
    Unknown fields, wrong types, and duplicates fail loudly."""
    try:
        doc = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        fail(f"{source}: malformed TOML: {e}")
    if not isinstance(doc, dict):
        fail(f"{source}: top level must be a table")
    unknown = sorted(set(doc) - {"quarantined"})
    if unknown:
        fail(f"{source}: unknown fields rejected: {unknown}")
    if "quarantined" not in doc:
        fail(f"{source}: missing required `quarantined` array")
    names = doc["quarantined"]
    if not isinstance(names, list) or any(not isinstance(n, str) for n in names):
        fail(f"{source}: `quarantined` must be an array of strings")
    if len(names) != len(set(names)):
        dupes = sorted({n for n in names if names.count(n) > 1})
        fail(f"{source}: duplicate roster entries: {dupes}")
    for n in names:
        if not n or n != n.strip() or " " in n:
            fail(f"{source}: not a test name: {n!r}")
    return names


def check_predicate(name, selected):
    """A predicate's real nextest selection must resolve to
    exactly one live-eligible composite identity whose full test
    name equals the entry. Anything else fails: zero matches
    (stale), over-matches (substring collision, including an
    ordinary non-live test), non-live eligibility, or a
    non-full-name entry (leaf/partial). Returns the identity."""
    if len(selected) != 1:
        others = sorted(f"{b}::{t}" for b, t, _ in selected)
        fail(
            f"roster entry {name!r} matches {len(selected)} tests, "
            f"want exactly one: {others}"
        )
    binary, tname, ignored = selected[0]
    if tname != name:
        fail(
            f"roster entry {name!r} is not a full test name "
            f"(matched {binary}::{tname}); leaf/partial entries rejected"
        )
    if not ignored:
        fail(f"roster entry {name!r} is not live-eligible ({binary}::{tname})")
    return (binary, tname)


def check_disjoint(gating, flaky):
    overlap = gating & flaky
    if overlap:
        fail(f"tiers overlap: {sorted(f'{b}::{t}' for b, t in overlap)}")


def check_union(gating, flaky, baseline_live):
    union = gating | flaky
    if union != baseline_live:
        fail(
            "union != baseline live universe: "
            f"missing={sorted(f'{b}::{t}' for b, t in baseline_live - union)} "
            f"extra={sorted(f'{b}::{t}' for b, t in union - baseline_live)}"
        )


def check_ordinary_preserved(gating_all, baseline_all):
    gating_ord = {i for i in gating_all if not i[2]}
    baseline_ord = {i for i in baseline_all if not i[2]}
    if gating_ord != baseline_ord:
        missing = sorted(f"{b}::{t}" for b, t, _ in baseline_ord - gating_ord)[:10]
        fail(f"gating dropped ordinary tests: {missing}")


def filters_for(names):
    """Build the -E pair. Empty roster: an inclusion matching
    nothing (flaky leg runs --no-tests pass) and an exclusion
    matching everything (gating byte-identical)."""
    if names:
        inclusion = " or ".join(f"test({n})" for n in names)
    else:
        inclusion = "test(__no_such_test__)"
    return inclusion, f"not ({inclusion})"


# ------------------------------------------------------------------
# nextest interaction (needs cargo; not used by --self-test).
# ------------------------------------------------------------------


def run_list(config, profile_args, extra_args):
    cmd = ["cargo", "nextest", "list", "--config-file", config]
    cmd += profile_args + ["--run-ignored=all", "--message-format", "json"]
    cmd += extra_args
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout
    except subprocess.CalledProcessError as e:
        fail(f"nextest list failed: {e.stderr.strip().splitlines()[-1:] or e}")
    try:
        d = json.loads(out)
    except json.JSONDecodeError as e:
        fail(f"nextest list output is not JSON: {e}")
    suites = d.get("rust-suites")
    if not isinstance(suites, dict):
        fail("nextest list output missing rust-suites")
    identities = []  # (binary, name, ignored, selected)
    for _suite, body in suites.items():
        if not isinstance(body, dict):
            fail("nextest list suite entry is not an object")
        binary = body.get("binary-id")
        if not isinstance(binary, str):
            fail("nextest list entry missing binary-id")
        testcases = body.get("testcases")
        if not isinstance(testcases, dict):
            fail("nextest list entry missing testcases")
        for tname, tinfo in testcases.items():
            if not isinstance(tinfo, dict):
                fail(f"nextest list entry for {tname!r} is not an object")
            fm = tinfo.get("filter-match")
            if not isinstance(fm, dict) or not isinstance(fm.get("status"), str):
                fail(f"nextest list entry for {tname!r} missing filter-match.status")
            ignored = tinfo.get("ignored")
            if not isinstance(ignored, bool):
                fail(f"nextest list entry for {tname!r} missing ignored flag")
            if fm["status"] == "matches":
                identities.append((binary, tname, ignored))
    return identities


def selection_for(config, profile, filt):
    return run_list(config, ["-P", profile], ["-E", filt])


def discover_baseline(config):
    return run_list(config, [], ["--ignore-default-filter"])


# ------------------------------------------------------------------
# Commands.
# ------------------------------------------------------------------


def cmd_check(roster_path, inclusion, exclusion, config):
    with open(roster_path, "rb") as f:
        names = parse_roster(f.read().decode("utf-8"), roster_path)

    baseline = discover_baseline(config)
    baseline_live = {(b, t) for b, t, ign in baseline if ign}
    print(f"baseline: {len(baseline)} discovered, {len(baseline_live)} live-gated")

    # Per-predicate verification with REAL nextest selection, under
    # both tier profiles: each generated predicate must resolve to
    # exactly one live-eligible identity. Tier-2's ordinary-collision
    # and stale-entry fixtures live here, not in a Python approximation.
    expected = {}
    for name in names:
        found = None
        for profile in ("live", "flaky"):
            sel = selection_for(config, profile, f"test({name})")
            ident = check_predicate(name, sel)
            if found is None:
                found = ident
            elif found != ident:
                fail(f"roster entry {name!r} resolves differently per profile")
        expected[name] = found
    expected_set = set(expected.values())

    flaky = selection_for(config, "flaky", inclusion)
    flaky_set = {(b, t) for b, t, _ in flaky}
    # The informational selection must equal the roster: every
    # selected identity must be a rostered one and vice versa.
    if flaky_set != expected_set:
        fail(
            "informational selection != roster: "
            f"missing={sorted(f'{b}::{t}' for b, t in expected_set - flaky_set)} "
            f"extra={sorted(f'{b}::{t}' for b, t in flaky_set - expected_set)}"
        )

    gating = selection_for(config, "live", exclusion)
    gating_set = {(b, t) for b, t, _ in gating}
    check_disjoint(gating_set, flaky_set)
    gating_live = {(b, t) for b, t, ign in gating if ign}
    check_union(gating_live, flaky_set, baseline_live)
    check_ordinary_preserved(gating, baseline)

    n_ord = len([1 for _, _, ign in gating if not ign])
    print(
        f"roster-check OK: roster={len(names)} "
        f"gating-live={len(gating_live)} flaky-live={len(flaky_set)} "
        f"ordinary-preserved={n_ord}"
    )


def cmd_generate(roster_path):
    with open(roster_path, "rb") as f:
        names = parse_roster(f.read().decode("utf-8"), roster_path)
    inclusion, exclusion = filters_for(names)
    print(f"inclusion={inclusion}")
    print(f"exclusion={exclusion}")
    print(f"empty={'true' if not names else 'false'}")


def cmd_self_test():
    """Backend-free fixture controls: pure core only, no cargo."""
    cases = []

    def check(name, fn):
        try:
            fn()
        except SystemExit as e:
            cases.append((name, f"EXIT({e.code})"))
        else:
            cases.append((name, "PASS"))

    # parse_roster fixtures
    check("valid-double-quoted", lambda: parse_roster('quarantined = ["a::b"]\n'))
    check("valid-single-quoted", lambda: parse_roster("quarantined = ['a::b']\n"))
    check("valid-empty", lambda: parse_roster("quarantined = []\n"))
    check("reject-malformed", lambda: parse_roster("quarantined = [\n"))
    check("reject-unknown-field", lambda: parse_roster("quarantined = []\nother = 1\n"))
    check("reject-missing-key", lambda: parse_roster("x = []\n"))
    check("reject-non-list", lambda: parse_roster('quarantined = "a::b"\n'))
    check("reject-non-string", lambda: parse_roster("quarantined = [1]\n"))
    check("reject-duplicate", lambda: parse_roster('quarantined = ["a::b", "a::b"]\n'))
    check("reject-blank-name", lambda: parse_roster('quarantined = [""]\n'))
    check("reject-spaced-name", lambda: parse_roster('quarantined = ["a b"]\n'))

    # check_predicate fixtures (synthetic selections)
    live = lambda b, t: (b, t, True)
    dead = lambda b, t: (b, t, False)
    check("predicate-exact-one", lambda: check_predicate("m::t", [live("bin", "m::t")]))
    check("predicate-stale", lambda: check_predicate("m::t", []))
    check(
        "predicate-overmatch",
        lambda: check_predicate("m::t", [live("b1", "m::t"), live("b2", "m::t_x")]),
    )
    check(
        "predicate-ordinary-collision",
        lambda: check_predicate("m::t", [live("b1", "m::t"), dead("b2", "m::t_extra")]),
    )
    check(
        "predicate-non-live-only",
        lambda: check_predicate("m::t", [dead("bin", "m::t")]),
    )
    check(
        "predicate-leaf-rejected",
        lambda: check_predicate("t", [live("bin", "m::t")]),
    )
    check(
        "predicate-binary-identity",
        lambda: (
            check_predicate("m::t", [live("b1", "m::t")]),
            check_predicate("m::t", [live("b2", "m::t")]),
        ),
    )

    failures = []
    must_exit = {
        "valid-double-quoted",
        "valid-single-quoted",
        "valid-empty",
        "predicate-exact-one",
        "predicate-binary-identity",
    }
    for name, result in cases:
        want_pass = name in must_exit
        # must_exit cases must NOT raise; all others MUST raise SystemExit
        ok = (result == "PASS") == want_pass
        print(f"{'ok' if ok else 'FAIL'}: {name} -> {result}")
        if not ok:
            failures.append(name)
    if failures:
        fail(f"self-test failures: {failures}")
    print(f"self-test OK: {len(cases)} fixtures")


def main():
    args = sys.argv[1:]
    if args == ["--self-test"]:
        cmd_self_test()
        return
    config = ".auxiliary/configuration/nextest.toml"
    rest = []
    i = 0
    while i < len(args):
        if args[i] == "--config-file":
            config = args[i + 1]
            i += 2
        else:
            rest.append(args[i])
            i += 1
    if len(rest) == 2 and rest[1] == "GENERATE":
        cmd_generate(rest[0])
        return
    if len(rest) != 3:
        sys.exit(
            "usage: roster-check.py <roster> <inclusion> <exclusion> [--config-file P] | <roster> GENERATE GENERATE | --self-test"
        )
    cmd_check(rest[0], rest[1], rest[2], config)


if __name__ == "__main__":
    main()
