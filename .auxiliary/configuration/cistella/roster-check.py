#!/usr/bin/env python3
"""Roster check for the flaky live-test quarantine tier.

Proves, at the exact tip and from execution output (never from
reading config): per-predicate exact-one live-eligible identity
verified against the raw discovered set AND real `nextest -E`
selection; informational-equals-roster; tier disjointness;
union against an independent baseline; ordinary-test
preservation. All identity comparisons use composite
(binary-id, test-name) identities.

Discovery discipline (deliberate):
- Baseline: NO cargo profile flag, --run-ignored=all,
  --ignore-default-filter (bypasses tier/roster/default
  filters). It is the authoritative universe, not a leg.
- Tier selections: the legs' actual profiles and flags
  (-P live / -P flaky, --run-ignored=all, execution build
  flags), NO bypass. A real default-filter omission fails
  the checks instead of passing silently.
- Every listing carries the execution's build flags
  (--release --locked): debug-profile binaries are not what
  CI runs.

Usage: roster-check.py <roster> <inclusion -E> <exclusion -E> [--config-file P]
       roster-check.py <roster> GENERATE [GENERATE]
       roster-check.py --self-test   # backend-free fixtures, no cargo
       roster-check.py decide-flaky <log> --exit-code N --expect-empty true|false
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
    whitespace. Unknown keys, wrong types, duplicates fail."""
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
    bad = sorted(
        {n for n in names if (not n) or (n != n.strip()) or any(c.isspace() for c in n)}
    )
    if bad:
        fail(f"{source}: not test names: {bad}")
    if len(set(names)) != len(names):
        dupes = sorted({n for n in names if names.count(n) > 1})
        fail(f"{source}: duplicate roster entries: {dupes}")
    return list(names)


def check_predicate_raw(name, baseline_live):
    """Validate one roster entry against the RAW discovered set
    (every live-gated test in every binary, before any profile
    projection): exactly one composite identity whose full test
    name equals the entry. Stale, overmatching (including an
    ordinary-test collision), non-live, and leaf/partial
    entries fail here — profile filtering cannot hide them."""
    holders = sorted({(b, t) for b, t in baseline_live if name in t})
    if len(holders) != 1:
        fail(
            f"roster entry {name!r} matches {len(holders)} live tests in raw set, "
            f"want exactly one: {[f'{b}::{t}' for b, t in holders]}"
        )
    (binary, tname) = holders[0]
    if tname != name:
        fail(
            f"roster entry {name!r} is not a full test name "
            f"(matched {binary}::{tname}); leaf/partial names rejected"
        )
    return (binary, tname)


def check_informational_equals(flaky_set, expected_set):
    if flaky_set != expected_set:
        fail(
            "informational selection != roster: "
            f"missing={sorted(f'{b}::{t}' for b, t in expected_set - flaky_set)} "
            f"extra={sorted(f'{b}::{t}' for b, t in flaky_set - expected_set)}"
        )


def check_disjoint(gating_set, flaky_set):
    overlap = gating_set & flaky_set
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


def decide_flaky_exit(exit_code, has_summary, zero_tests_run, expect_empty):
    """Classify a flaky-leg outcome. Informational test failures
    pass; anything else fails loud:
    - no final Summary: the run did not complete (infra, config,
      discovery, or build failure) — never informational;
    - zero tests with a non-empty expectation: unexpected empty
      selection — never informational;
    - exit 0 or 100 (TEST_RUN_FAILED): genuine test outcome;
    - any other code (1 unknown, 105 setup, signals, ...):
      not a test result, fail even with a Summary."""
    if not has_summary:
        return False, f"no final Summary (exit={exit_code}): run did not complete"
    if zero_tests_run and not expect_empty:
        return (
            False,
            "zero tests run with a non-empty expectation: unexpected empty selection",
        )
    if exit_code in (0, 100):
        return True, f"completed run (exit={exit_code}); failures are informational"
    return (
        False,
        f"non-test exit code {exit_code}: infra/config/setup failure, not a test result",
    )


# ------------------------------------------------------------------
# Listing backend (needs cargo; injected fakes in --self-test).
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


def run_nextest_list(config, profile_flags, extra_args):
    """Run one listing; return {(binary, name, ignored)} for
    SELECTED tests only (filter-match.status == "matches").
    Anything malformed fails instead of defaulting."""
    cmd = (
        base_list_cmd(config)
        + profile_flags
        + ["--run-ignored=all"]
        + extra_args
        + ["--message-format", "json"]
    )
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


class Lister:
    """Real backend: every listing goes through cargo nextest."""

    def __init__(self, config):
        self.config = config
        self.calls = []

    def baseline(self):
        self.calls.append(("baseline",))
        return run_nextest_list(self.config, [], ["--ignore-default-filter"])

    def selection(self, profile, filt):
        self.calls.append(("selection", profile, filt))
        return run_nextest_list(self.config, ["-P", profile], ["-E", filt])

    def predicate(self, profile, name):
        self.calls.append(("predicate", profile, name))
        return run_nextest_list(self.config, ["-P", profile], ["-E", f"test({name})"])


def evaluate(lister, names, inclusion, exclusion):
    """Full orchestration over an injected listing backend.
    Returns evidence counts. Raises SystemExit on any failure."""
    baseline = lister.baseline()
    baseline_live = {(b, t) for b, t, ign in baseline if ign}
    print(f"baseline: {len(baseline)} discovered, {len(baseline_live)} live-gated")

    # 1. Raw-set predicate validation (before any profile
    # projection), then generated-predicate verification under
    # each tier profile.
    expected = {}
    for name in names:
        holders = check_predicate_raw(name, {(b, t) for b, t, ign in baseline if ign})
        expected[name] = holders
        for profile in ("live", "flaky"):
            sel = {(b, t) for b, t, ign in lister.predicate(profile, name) if ign}
            if sel != {holders}:
                fail(
                    f"roster entry {name!r} does not select exactly itself "
                    f"under -P {profile}: {[f'{b}::{t}' for b, t in sorted(sel)]}"
                )

    # 2. Informational selection equals the roster.
    flaky = {(b, t) for b, t, _ in lister.selection("flaky", inclusion)}
    check_informational_equals(flaky, set(expected.values()))

    # 3. Gating selection, disjointness, union, ordinary preserved.
    gating = lister.selection("live", exclusion)
    gating_set = {(b, t) for b, t, _ in gating}
    check_disjoint(gating_set, flaky)
    check_union(
        {(b, t) for b, t, ign in gating if ign},
        flaky,
        baseline_live,
    )
    check_ordinary_preserved(gating, baseline)
    return {
        "roster": len(names),
        "gating_live": sum(1 for _, _, ign in gating if ign),
        "flaky_live": len(flaky),
        "ordinary_preserved": sum(1 for _, _, ign in gating if not ign),
    }


def cmd_check(roster_path, inclusion, exclusion, config):
    with open(roster_path, "rb") as f:
        names = parse_roster(f.read().decode("utf-8"), roster_path)
    ev = evaluate(Lister(config), names, inclusion, exclusion)
    print(
        f"roster-check OK: roster={ev['roster']} "
        f"gating-live={ev['gating_live']} flaky-live={ev['flaky_live']} "
        f"ordinary-preserved={ev['ordinary_preserved']}"
    )


def cmd_generate(roster_path):
    with open(roster_path, "rb") as f:
        names = parse_roster(f.read().decode("utf-8"), roster_path)
    inclusion, exclusion = filters_for(names)
    print(f"inclusion={inclusion}")
    print(f"exclusion={exclusion}")
    print(f"empty={'true' if not names else 'false'}")


def cmd_decide_flaky(log_path, exit_code, expect_empty):
    with open(log_path, errors="replace") as f:
        text = f.read()
    # Strip ANSI escapes first: CI prefixes every line and nextest
    # colorizes the Summary marker, so anchors must run on clean
    # text. The Summary shape is matched anywhere in the line.
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    has_summary = bool(re.search(r"Summary\s*\[", text))
    zero_tests = bool(re.search(r"(^| )0 tests run", text, re.MULTILINE))
    ok, reason = decide_flaky_exit(exit_code, has_summary, zero_tests, expect_empty)
    print(f"flaky leg decision: {'PASS (informational)' if ok else 'FAIL'}: {reason}")
    if not ok:
        raise SystemExit(1)


def cmd_self_test():
    """Backend-free fixtures: pure core plus injected-backend
    orchestration. No cargo, no network."""
    total = [0]
    failed = []

    def check(name, fn, expect_pass=True):
        total[0] += 1
        try:
            fn()
            result = True
        except SystemExit:
            result = False
        ok = result == expect_pass
        if not ok:
            failed.append(name)
        print(f"{'ok' if ok else 'FAIL'}: {name} -> {'PASS' if result else 'EXIT'}")

    def _fail():
        raise AssertionError("fixture condition not met")

    # -- parse_roster --
    check(
        "valid-double-quoted",
        lambda: parse_roster('quarantined = ["a::b"]\n', "t"),
        True,
    )
    check(
        "valid-single-quoted",
        lambda: parse_roster("quarantined = ['a::b']\n", "t"),
        True,
    )
    check("valid-empty", lambda: parse_roster("quarantined = []\n", "t"), True)
    check(
        "valid-multiline",
        lambda: parse_roster('quarantined = [\n  "a::b",\n]\n', "t"),
        True,
    )
    check("reject-malformed", lambda: parse_roster("quarantined = [\n", "t"), False)
    check(
        "reject-unknown-field",
        lambda: parse_roster("quarantined = []\nother = 1\n", "t"),
        False,
    )
    check("reject-missing-key", lambda: parse_roster("x = []\n", "t"), False)
    check("reject-non-list", lambda: parse_roster('quarantined = "a::b"\n', "t"), False)
    check("reject-non-string", lambda: parse_roster("quarantined = [1]\n", "t"), False)
    check(
        "reject-duplicate",
        lambda: parse_roster('quarantined = ["a::b", "a::b"]\n', "t"),
        False,
    )
    check("reject-blank", lambda: parse_roster('quarantined = [""]\n', "t"), False)
    check("reject-spaced", lambda: parse_roster('quarantined = ["a b"]\n', "t"), False)
    check(
        "reject-padded", lambda: parse_roster('quarantined = [" a::b"]\n', "t"), False
    )

    # -- check_predicate (pure) --
    live = lambda b, t: [(b, t, True)]
    dead = lambda b, t: [(b, t, False)]
    check(
        "predicate-exact-one",
        lambda: check_predicate_raw("m::t", {("b", "m::t")}),
        True,
    )
    check("predicate-stale", lambda: check_predicate_raw("m::t", set()), False)
    check(
        "predicate-overmatch",
        lambda: check_predicate_raw("m::t", {("b1", "m::t"), ("b2", "m::t_x")}),
        False,
    )
    check(
        "predicate-ordinary-collision",
        lambda: check_predicate_raw("m::t", {("b1", "m::t"), ("b2", "m::t_extra")}),
        False,
    )
    check("predicate-non-live", lambda: check_predicate_raw("m::t", set()), False)
    check("predicate-leaf", lambda: check_predicate_raw("t", {("b", "m::t")}), False)
    check(
        "predicate-cross-binary",
        lambda: check_predicate_raw("m::t", {("b1", "m::t"), ("b2", "m::t")}),
        False,
    )

    # -- decide_flaky_exit --
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
    check(
        "decide-unknown-code-fails",
        lambda: D(4, True, False, False)[0] and _fail(),
        True,
    )

    # -- filters_for --
    check(
        "filters-empty",
        lambda: filters_for([])
        == ("test(__no_such_test__)", "not (test(__no_such_test__))")
        or _fail(),
        True,
    )
    check(
        "filters-pair",
        lambda: filters_for(["a::b", "c::d"])
        == ("test(a::b) or test(c::d)", "not (test(a::b) or test(c::d))")
        or _fail(),
        True,
    )

    # -- evaluate() with an injected fake backend (no cargo) --
    class FakeLister:
        """Canned discovery/selection. Selection honors the
        generated -E shape closely enough to prove the
        orchestration: exact full-name test() predicates over
        the raw set, then per-profile projection."""

        def __init__(self, baseline, live_default_filter=(), flaky_default_filter=()):
            self._baseline = set(baseline)
            self.live_df = set(live_default_filter)
            self.flaky_df = set(flaky_default_filter)
            self.recorded = []

        def _names(self, idents):
            return {t for _, t, _ in idents}

        def _predicate_match(self, name, idents):
            return {(b, t, ign) for (b, t, ign) in idents if name in t}

        def baseline(self):
            self.recorded.append(("baseline",))
            return set(self._baseline)

        def predicate(self, profile, name):
            self.recorded.append(("predicate", profile, name))
            pool = {
                i
                for i in self._baseline
                if i[1] not in (self.live_df if profile == "live" else self.flaky_df)
            }
            return self._predicate_match(name, pool)

        def selection(self, profile, filt):
            self.recorded.append(("selection", profile, filt))
            import re as _re

            pats = _re.findall(r"test\(([^)]+)\)", filt)
            neg = filt.strip().startswith("not (")
            pool = {
                i
                for i in self._baseline
                if i[1] not in (self.live_df if profile == "live" else self.flaky_df)
            }
            if neg:
                return {i for i in pool if not any(p in i[1] for p in pats)}
            return {i for i in pool if any(p in i[1] for p in pats)}

    B = ("bin",)
    I = lambda t: ("bin", t, True)  # noqa: E731
    U = lambda t: ("bin", t, False)  # noqa: E731

    def run_eval(names, baseline, inclusion, exclusion, live_df=(), flaky_df=()):
        fl = FakeLister(baseline, live_df, flaky_df)
        return evaluate(fl, names, inclusion, exclusion)

    def e2e(names, baseline, live_df=(), flaky_df=()):
        inc, exc = filters_for(names)
        return run_eval(names, baseline, inc, exc, live_df, flaky_df)

    base90 = [I("m::live1"), I("m::live2"), U("m::ord1")]
    check("e2e-empty-roster", lambda: e2e([], base90), True)
    check("e2e-one-entry", lambda: e2e(["m::live1"], base90), True)
    check("e2e-stale", lambda: e2e(["m::gone"], base90), False)
    check(
        "e2e-ordinary-collision",
        lambda: e2e(["m::live1"], base90 + [U("m::live1_extra")]),
        False,
    )
    check(
        "e2e-duplicate-identities",
        lambda: e2e(["m::live1"], [I("m::live1"), ("bin2", "m::live1", True)]),
        False,
    )
    check(
        "e2e-default-filter-omission",
        lambda: e2e(["m::live1"], base90, live_df=("m::live2",)),
        False,
    )
    check(
        "e2e-flaky-only-omission",
        lambda: e2e(
            ["m::live1"], [I("m::live1"), U("m::ord1")], flaky_df=("m::live1",)
        ),
        False,
    )

    def _e2e_ordinary_loss():
        # Gating selection drops an ordinary test: preserved
        # check must fail.
        class DropOrdinary(FakeLister):
            def selection(self, profile, filt):
                got = super().selection(profile, filt)
                if profile == "live":
                    got = {i for i in got if i[1] != "m::ord1"}
                return got

        return evaluate(
            DropOrdinary(base90),
            [],
            "test(__no_such_test__)",
            "not (test(__no_such_test__))",
        )

    def _e2e_command_builder_flags():
        # Every listing carries the execution build flags; the
        # baseline bypasses tier/roster/default filters without a
        # profile; tier selections use their own profile.
        got = base_list_cmd("C.toml")
        assert got[:5] == ["cargo", "nextest", "list", "--config-file", "C.toml"], got
        assert "--release" in got and "--locked" in got, got
        return True

    check(
        "e2e-ordinary-loss",
        lambda: _e2e_ordinary_loss(),
        False,
    )
    check(
        "e2e-command-builder-flags",
        lambda: _e2e_command_builder_flags(),
        True,
    )

    passed = total[0] - len(failed)
    print(f"self-test OK: {passed} fixtures passed")
    if failed:
        fail(f"self-test failures: {failed}")


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
            "usage: roster-check.py <roster> <inclusion> <exclusion> [--config-file P] | <roster> GENERATE [GENERATE] | --self-test | decide-flaky <log> --exit-code N --expect-empty true|false"
        )
    cmd_check(rest[0], rest[1], rest[2], config)


def cmd_check(roster_path, inclusion, exclusion, config):
    with open(roster_path, "rb") as f:
        names = parse_roster(f.read().decode("utf-8"), roster_path)
    ev = evaluate(Lister(config), names, inclusion, exclusion)
    print(
        f"roster-check OK: roster={ev['roster']} "
        f"gating-live={ev['gating_live']} flaky-live={ev['flaky_live']} "
        f"ordinary-preserved={ev['ordinary_preserved']}"
    )


def cmd_generate(roster_path):
    with open(roster_path, "rb") as f:
        names = parse_roster(f.read().decode("utf-8"), roster_path)
    inclusion, exclusion = filters_for(names)
    print(f"inclusion={inclusion}")
    print(f"exclusion={exclusion}")
    print(f"empty={'true' if not names else 'false'}")


def cmd_decide_flaky(args):
    log_path = None
    exit_code = None
    expect_empty = None
    i = 0
    positional = []
    while i < len(args):
        if args[i] == "--exit-code":
            exit_code = int(args[i + 1])
            i += 2
        elif args[i] == "--expect-empty":
            if args[i + 1] not in ("true", "false"):
                sys.exit("decide-flaky: --expect-empty must be true|false")
            expect_empty = args[i + 1] == "true"
            i += 2
        else:
            positional.append(args[i])
            i += 1
    if len(positional) != 1 or exit_code is None or expect_empty is None:
        sys.exit(
            "usage: roster-check.py decide-flaky <log> --exit-code N --expect-empty true|false"
        )
    with open(positional[0], errors="replace") as f:
        text = f.read()
    # Strip ANSI escapes first: CI prefixes every line and nextest
    # colorizes the Summary marker, so anchors must run on clean
    # text. The Summary shape is matched anywhere in the line.
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    has_summary = bool(re.search(r"Summary\s*\[", text))
    zero_tests = bool(re.search(r"(^| )0 tests run", text, re.MULTILINE))
    ok, reason = decide_flaky_exit(exit_code, has_summary, zero_tests, expect_empty)
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
