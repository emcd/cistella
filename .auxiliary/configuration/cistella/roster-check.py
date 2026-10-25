#!/usr/bin/env python3
"""Roster check for the flaky live-test quarantine tier.

Proves, at the exact tip and from execution output (never from
reading config): per-predicate exact-one live-eligible identity
verified against the raw discovered set with real `nextest
list -E` selections (not a Python substring approximation);
informational-equals-roster; tier disjointness; union against an
independent baseline; ordinary-test preservation.

Discovery discipline (deliberate asymmetries):
- Baseline: NO cargo profile flag, --run-ignored=all,
  --ignore-default-filter. Neutral with respect to tier and to
  any present-or-future default filter: it is the authoritative
  universe, not a leg selection.
- Tier selections: the legs' actual profiles (-P live / -P flaky)
  with NO bypass, so a real default-filter omission fails the
  checks instead of passing silently.
- All listings use the execution's build flags (--release
  --locked): debug-profile binaries are a different build and
  must not stand in for what CI actually runs.

Usage: roster-check.py <roster.toml> <inclusion -E> <exclusion -E>
       roster-check.py <roster.toml> GENERATE [GENERATE]
       roster-check.py --self-test   # backend-free fixtures
       roster-check.py decide-flaky <log> --exit-code N --roster-empty true|false
"""

import json
import re
import subprocess
import sys

try:
    import tomllib
except ImportError:
    sys.exit("roster-check requires Python >= 3.11 (tomllib)")

CONFIG = ".auxiliary/configuration/nextest.toml"


def fail(msg):
    print(f"roster-check FAILED: {msg}")
    raise SystemExit(1)


# ------------------------------------------------------------------
# Pure core (backend-free; exercised by --self-test).
# ------------------------------------------------------------------
def parse_roster(text, source="<roster>"):
    """Strict schema: exactly one top-level `quarantined` key
    holding a list of non-blank strings with no internal
    whitespace and no duplicates."""
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
    if any((not n) or (n != n.strip()) or any(c.isspace() for c in n) for n in names):
        bad = sorted(
            {
                n
                for n in names
                if (not n) or (n != n.strip()) or any(c.isspace() for c in n)
            }
        )
        fail(f"{source}: not test names: {bad}")
    if len(set(names)) != len(names):
        dupes = sorted({n for n in names if names.count(n) > 1})
        fail(f"{source}: duplicate roster entries: {dupes}")
    return list(names)


def check_predicate(name, selected):
    """A roster entry must resolve to exactly one live-eligible
    composite identity. `selected` is the real `nextest list -E`
    selection for `test(<name>)`: zero matches (stale), more
    than one (overmatch, including an ordinary-test collision),
    a non-live identity, or a non-full test name all fail."""
    if len(selected) != 1:
        fail(
            f"roster entry {name!r} matches {len(selected)} tests, want exactly one: "
            f"{sorted(f'{b}::{t}' for b, t, _ in selected)}"
        )
    binary, tname, ignored = selected[0]
    if tname != name:
        fail(
            f"roster entry {name!r} is not a full test name "
            f"(matched {binary}::{tname}); leaf/partial names rejected"
        )
    if not ignored:
        fail(f"roster entry {name!r} is not live-eligible ({binary}::{tname})")
    return (binary, tname)


def check_informational_equals(flaky_set, expected):
    if flaky_set != expected:
        fail(
            "informational selection != roster: "
            f"missing={sorted(f'{b}::{t}' for b, t in expected - flaky_set)} "
            f"extra={sorted(f'{b}::{t}' for b, t in flaky_set - expected)}"
        )


def check_disjoint(gating, flaky):
    overlap = gating & flaky
    if overlap:
        fail(f"tiers overlap: {sorted(f'{b}::{t}' for b, t in overlap)}")


def check_union(gating_live, flaky_live, baseline_live):
    union = gating_live | flaky_live
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
    """Build the -E selection pair. Empty roster: an inclusion
    matching nothing (flaky leg runs --no-tests pass) and an
    exclusion matching everything (gating byte-identical)."""
    if names:
        inclusion = " or ".join(f"test({n})" for n in names)
    else:
        inclusion = "test(__no_such_test__)"
    return inclusion, f"not ({inclusion})"


def decide_flaky_exit(exit_code, has_summary, zero_tests_run, roster_empty):
    """Classify a flaky-leg run outcome. Returns (ok, reason).
    Informational test failures pass; anything else fails loud:
    - no final Summary: the run did not complete (infra, config,
      discovery, or build failure) — never informational.
    - zero tests with a non-empty roster: unexpected empty
      selection — never informational.
    - documented nextest outcomes: 0 success and 100
      TEST_RUN_FAILED are genuine test results; setup/infra/
      unknown codes (105, 1, signals, ...) are not, even with
      a Summary present.
    """
    if not has_summary:
        return False, f"no final Summary (exit={exit_code}): run did not complete"
    if zero_tests_run and not roster_empty:
        return (
            False,
            "zero tests ran with a non-empty roster: unexpected empty selection",
        )
    if exit_code in (0, 100):
        return True, f"completed run (exit={exit_code}); failures are informational"
    return (
        False,
        f"non-test exit code {exit_code}: infra/config/setup failure, not a test result",
    )


# ------------------------------------------------------------------
# nextest interaction (needs cargo; not used by --self-test).
# ------------------------------------------------------------------
def base_list_cmd(config):
    """Command prefix shared by every listing: the execution's
    build flags, so selection is proved on the binaries CI runs."""
    return [
        "cargo",
        "nextest",
        "list",
        "--config-file",
        config,
        "--release",
        "--locked",
    ]


def run_nextest_list(config, extra_args):
    """Run one listing; return {(binary, name, ignored)} for
    SELECTED tests only (filter-match.status == "matches").
    Anything malformed fails instead of defaulting."""
    cmd = base_list_cmd(config) + extra_args + ["--message-format", "json"]
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout
    except subprocess.CalledProcessError as e:
        tail = (e.stderr or "").strip().splitlines()[-1:]
        fail(f"nextest list failed: {tail}")
    try:
        d = json.loads(out)
    except json.JSONDecodeError as e:
        fail(f"nextest list output is not JSON: {e}")
    suites = d.get("rust-suites")
    if not isinstance(suites, dict):
        fail("nextest list output missing rust-suites table")
    selected = set()
    for _suite, body in suites.items():
        if not isinstance(body, dict):
            fail("nextest list suite entry is not an object")
        binary = body.get("binary-id")
        if not isinstance(binary, str):
            fail("nextest list entry missing string binary-id")
        testcases = body.get("testcases")
        if not isinstance(testcases, dict):
            fail("nextest list entry missing testcases table")
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
                selected.add((binary, tname, ignored))
    return selected


def discover_baseline(config):
    """Independent authoritative universe: no cargo profile
    flag, --run-ignored=all, --ignore-default-filter."""
    return run_nextest_list(config, ["--run-ignored=all", "--ignore-default-filter"])


def selection_for(config, profile, filt):
    """One tier's actual selection: the leg's own profile and
    flags, NO bypass."""
    return run_nextest_list(config, ["-P", profile, "--run-ignored=all", "-E", filt])


# ------------------------------------------------------------------
# Commands.
# ------------------------------------------------------------------
def cmd_check(roster_path, inclusion, exclusion, config):
    with open(roster_path, "rb") as f:
        names = parse_roster(f.read().decode("utf-8"), roster_path)

    baseline = discover_baseline(config)
    baseline_live = {(b, t) for b, t, ign in baseline if ign}
    print(f"baseline: {len(baseline)} discovered, {len(baseline_live)} live-gated")

    # Per-predicate proof against RAW nextest selection (not a
    # Python substring approximation): each generated predicate
    # must resolve to exactly one live-eligible composite identity
    # under both tier profiles, before any projection.
    expected = {}
    for name in names:
        found = None
        for profile in ("live", "flaky"):
            sel = [
                (b, t, ign)
                for (b, t, ign) in selection_for(config, profile, f"test({name})")
            ]
            ident = check_predicate(name, sel)
            if found is None:
                found = ident
            elif found != ident:
                fail(f"roster entry {name!r} resolves differently per profile")
        expected[name] = found
    expected_set = set(expected.values())

    flaky = selection_for(config, "flaky", inclusion)
    flaky_set = {(b, t) for b, t, _ in flaky}
    check_informational_equals(flaky_set, expected_set)

    gating = selection_for(config, "live", exclusion)
    gating_set = {(b, t) for b, t, _ in gating}
    check_disjoint(gating_set, flaky_set)
    check_union(
        {(b, t) for b, t, ign in gating if ign},
        {(b, t) for b, t, ign in flaky if ign},
        baseline_live,
    )
    check_ordinary_preserved(gating, baseline)

    n_gating_live = sum(1 for _, _, ign in gating if ign)
    print(
        f"roster-check OK: roster={len(names)} "
        f"gating-live={n_gating_live} flaky-live={len(flaky_set)} "
        f"ordinary-preserved={sum(1 for _, _, ign in gating if not ign)}"
    )


def cmd_generate(roster_path):
    with open(roster_path, "rb") as f:
        names = parse_roster(f.read().decode("utf-8"), roster_path)
    inclusion, exclusion = filters_for(names)
    print(f"inclusion={inclusion}")
    print(f"exclusion={exclusion}")
    print(f"empty={'true' if not names else 'false'}")


def cmd_self_test():
    """Backend-free fixtures over the pure core (no cargo)."""
    cases = []

    def check(name, fn, expect_pass):
        try:
            fn()
            result = "PASS"
        except SystemExit:
            result = "EXIT"
        ok = (result == "PASS") == expect_pass
        cases.append(ok)
        print(f"{'ok' if ok else 'FAIL'}: {name} -> {result}")

    P = lambda *a: parse_roster(*a)  # noqa: E731
    # parse: valid forms
    check("valid-double-quoted", lambda: P('quarantined = ["a::b"]\n', "t"), True)
    check("valid-single-quoted", lambda: P("quarantined = ['a::b']\n", "t"), True)
    check("valid-empty", lambda: P("quarantined = []\n", "t"), True)
    check("valid-newlines", lambda: P('quarantined = [\n  "a::b",\n]\n', "t"), True)
    # parse: malformed/ill-typed/duplicates/blanks
    check("reject-malformed", lambda: P("quarantined = [\n", "t"), False)
    check(
        "reject-unknown-field", lambda: P("quarantined = []\nother = 1\n", "t"), False
    )
    check("reject-missing-key", lambda: P("x = []\n", "t"), False)
    check("reject-non-list", lambda: P('quarantined = "a::b"\n', "t"), False)
    check("reject-non-string", lambda: P("quarantined = [1]\n", "t"), False)
    check("reject-duplicate", lambda: P('quarantined = ["a::b", "a::b"]\n', "t"), False)
    check("reject-blank", lambda: P('quarantined = [""]\n', "t"), False)
    check("reject-spaced", lambda: P('quarantined = ["a b"]\n', "t"), False)
    check("reject-padded", lambda: P('quarantined = [" a::b"]\n', "t"), False)
    # predicate fixtures (synthetic composite identities)
    live = lambda b, t: (b, t, True)
    dead = lambda b, t: (b, t, False)
    check(
        "predicate-exact-one",
        lambda: check_predicate("m::t", [live("b", "m::t")]),
        True,
    )
    check("predicate-stale", lambda: check_predicate("m::t", []), False)
    check(
        "predicate-overmatch",
        lambda: check_predicate("m::t", [live("b1", "m::t"), live("b2", "m::t_x")]),
        False,
    )
    check(
        "predicate-ordinary-collision",
        lambda: check_predicate("m::t", [live("b1", "m::t"), dead("b2", "m::t_extra")]),
        False,
    )
    check(
        "predicate-non-live",
        lambda: check_predicate("m::t", [dead("b", "m::t")]),
        False,
    )
    check("predicate-leaf", lambda: check_predicate("t", [live("b", "m::t")]), False)
    check(
        "predicate-cross-binary-dup",
        lambda: check_predicate("m::t", [live("b1", "m::t"), live("b2", "m::t")]),
        False,
    )
    # set checks
    check(
        "informational-equals",
        lambda: check_informational_equals({("b", "m::t")}, {("b", "m::t")}),
        True,
    )
    check(
        "informational-missing",
        lambda: check_informational_equals(set(), {("b", "m::t")}),
        False,
    )
    check(
        "informational-extra",
        lambda: check_informational_equals(
            {("b", "m::t"), ("b", "m::x")}, {("b", "m::t")}
        ),
        False,
    )
    check("disjoint-ok", lambda: check_disjoint({("b", "m::t")}, {("b", "m::x")}), True)
    check(
        "disjoint-hit", lambda: check_disjoint({("b", "m::t")}, {("b", "m::t")}), False
    )
    check(
        "union-ok",
        lambda: check_union(
            {("b", "m::t")}, {("b", "m::x")}, {("b", "m::t"), ("b", "m::x")}
        ),
        True,
    )
    check(
        "union-gap",
        lambda: check_union({("b", "m::t")}, set(), {("b", "m::t"), ("b", "m::x")}),
        False,
    )
    check(
        "ordinary-preserved",
        lambda: check_ordinary_preserved(
            {("b", "m::t", True), ("b", "u::v", False)},
            {("b", "m::t", True), ("b", "u::v", False)},
        ),
        True,
    )
    check(
        "ordinary-dropped",
        lambda: check_ordinary_preserved(
            {("b", "m::t", True)}, {("b", "m::t", True), ("b", "u::v", False)}
        ),
        False,
    )
    # filters_for
    check(
        "filters-empty",
        lambda: (_ for _ in ()).throw(AssertionError())
        if filters_for([]) != ("test(__no_such_test__)", "not (test(__no_such_test__))")
        else None,
        True,
    )
    check(
        "filters-pair",
        lambda: (_ for _ in ()).throw(AssertionError())
        if filters_for(["a::b"]) != ("test(a::b)", "not (test(a::b))")
        else None,
        True,
    )
    # decide_flaky_exit classifier
    D = decide_flaky_exit
    check("decide-ok-pass", lambda: D(0, True, False, False)[0] or _fail(), True)
    check(
        "decide-rc100-informational",
        lambda: D(100, True, False, False)[0] or _fail(),
        True,
    )
    check(
        "decide-no-summary-fails",
        lambda: D(0, False, False, False)[0] and _fail(),
        True,
    )
    check(
        "decide-rc1-with-summary-fails",
        lambda: D(1, True, False, False)[0] and _fail(),
        True,
    )
    check(
        "decide-rc105-with-summary-fails",
        lambda: D(105, True, False, False)[0] and _fail(),
        True,
    )
    check(
        "decide-unexpected-empty-fails",
        lambda: D(0, True, True, False)[0] and _fail(),
        True,
    )
    check(
        "decide-empty-roster-zero-passes",
        lambda: D(0, True, True, True)[0] or _fail(),
        True,
    )
    check(
        "decide-rc130-with-summary-fails",
        lambda: D(130, True, False, False)[0] and _fail(),
        True,
    )

    if not all(c for c in cases):
        n_bad = sum(1 for c in cases if not c)
        fail(f"self-test: {n_bad} fixture(s) failed")


def _fail():
    raise AssertionError("expected condition not met")


def main():
    args = sys.argv[1:]
    if args == ["--self-test"]:
        cmd_self_test()
        return
    if len(args) >= 1 and args[0] == "decide-flaky":
        cmd_decide_flaky(args[1:])
        return
    if rest_is_generate(args):
        cmd_generate(args[0])
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
    if len(rest) != 3:
        sys.exit(
            "usage: roster-check.py <roster> <inclusion> <exclusion> [--config-file P] | <roster> GENERATE [GENERATE] | --self-test | decide-flaky <log> --exit-code N --roster-empty true|false"
        )
    if rest[0] == "decide-flaky":
        cmd_decide_flaky(rest[1:])
        return
    cmd_check(rest[0], rest[1], rest[2], config)


def cmd_decide_flaky(args):
    import re

    log_path = None
    exit_code = None
    roster_empty = None
    i = 0
    positional = []
    while i < len(args):
        if args[i] == "--exit-code":
            exit_code = int(args[i + 1])
            i += 2
        elif args[i] == "--roster-empty":
            if args[i + 1] not in ("true", "false"):
                sys.exit("decide-flaky: --roster-empty must be true|false")
            roster_empty = args[i + 1] == "true"
            i += 2
        else:
            positional.append(args[i])
            i += 1
    if len(positional) != 1 or exit_code is None or roster_empty is None:
        sys.exit(
            "usage: roster-check.py decide-flaky <log> --exit-code N --roster-empty true|false"
        )
    with open(positional[0], errors="replace") as f:
        text = f.read()
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    has_summary = bool(re.search(r"Summary\s*\[", text))
    zero_tests = bool(re.search(r"(^| )0 tests run", text, re.MULTILINE))
    ok, reason = decide_flaky_exit(exit_code, has_summary, zero_tests, roster_empty)
    print(f"flaky leg decision: {'PASS (informational)' if ok else 'FAIL'}: {reason}")
    if not ok:
        raise SystemExit(1)


def rest_is_generate(args):
    return (
        len(args) in (2, 3)
        and all(a == "GENERATE" for a in args[1:])
        and not args[0].startswith("-")
    )


if __name__ == "__main__":
    main()
