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
    raise SystemExit(msg)


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


def check_predicate_raw(name, baseline_all):
    """Validate one roster entry against the RAW discovered set
    (every test in every binary — ignored or not — before any
    profile projection): exactly one composite identity whose
    full test name equals the entry, and it must be
    live-eligible. Stale, overmatching (including an
    ordinary-test collision), non-live, and leaf/partial
    entries fail here — profile filtering cannot hide them."""
    holders = sorted({(b, t, ign) for (b, t, ign) in baseline_all if name in t})
    if len(holders) != 1:
        fail(
            f"roster entry {name!r} matches {len(holders)} tests in raw set, "
            f"want exactly one: {[f'{b}::{t}' for b, t, _ in holders]}"
        )
    (binary, tname, live) = holders[0]
    if tname != name:
        fail(
            f"roster entry {name!r} is not a full test name "
            f"(matched {binary}::{tname}); leaf/partial names rejected"
        )
    if not live:
        fail(f"roster entry {name!r} is not live-eligible ({binary}::{tname})")
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


def effective_flaky_filter(inclusion, manual):
    """The flaky leg's effective selection: the roster
    inclusion, intersected with a manual dispatch filter when
    one is given. Documented route for deliberately targeting
    quarantined tests."""
    manual = (manual or "").strip()
    if manual:
        return f"( {inclusion} ) and ( {manual} )"
    return inclusion


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


def list_argv(config, profile, filt, bypass):
    """Pure constructor for every nextest listing invocation.
    profile=None means the neutral default profile (baseline
    only); bypass adds --ignore-default-filter. Pinned by
    self-test: baseline carries bypass and no -P, tier listings
    carry -P and no bypass, all carry the execution build
    flags."""
    cmd = base_list_cmd(config)
    if profile is not None:
        cmd += ["-P", profile]
    cmd += ["--run-ignored=all"]
    if bypass:
        cmd += ["--ignore-default-filter"]
    if filt is not None:
        cmd += ["-E", filt]
    cmd += ["--message-format", "json"]
    return cmd


def run_nextest_list(config, profile, filt, bypass):
    """Run one listing; return {(binary, name, ignored)} for
    SELECTED tests only (filter-match.status == "matches").
    Anything malformed fails instead of defaulting."""
    cmd = list_argv(config, profile, filt, bypass)
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

    def _run(self, profile, filt, bypass):
        argv = list_argv(self.config, profile, filt, bypass)
        self.calls.append(argv)
        return run_nextest_list(self.config, profile, filt, bypass)

    def baseline(self):
        return self._run(None, None, True)

    def selection(self, profile, filt):
        return self._run(profile, filt, False)

    def predicate(self, profile, name):
        return self._run(profile, f"test({name})", False)


def evaluate(lister, names, inclusion, exclusion):
    """Full orchestration over an injected listing backend.
    Returns evidence counts. Raises SystemExit on any failure."""
    baseline = lister.baseline()
    baseline_live = {(b, t) for b, t, ign in baseline if ign}
    print(f"baseline: {len(baseline)} discovered, {len(baseline_live)} live-gated")

    # 1. Raw-set predicate validation (before any profile
    # projection), then generated-predicate verification under
    # each tier profile. The raw check runs on the FULL
    # discovered set (ignored or not) so an ordinary-test
    # collision cannot hide behind a profile filter.
    expected = {}
    for name in names:
        holders = check_predicate_raw(name, baseline)
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


def find_final_summary(text):
    """Locate the final nextest summary region: the last line
    matching the Summary shape. Test stdout can spoof the words
    anywhere, but only the runner's final summary decides the
    outcome — earlier lookalikes are ignored. Returns
    (found, zero_tests) where zero_tests is read from that
    same final line."""
    stripped = re.sub(r"\x1b\[[0-9;]*m", "", text)
    last = None
    for line in stripped.splitlines():
        if re.search(r"Summary\s*\[", line):
            last = line
    if last is None:
        return False, False
    return True, bool(re.search(r"(^| )0 tests run", last))


def decide_flaky_for_log(log_path, exit_code, expect_empty):
    with open(log_path, errors="replace") as f:
        text = f.read()
    has_summary, zero_tests = find_final_summary(text)
    ok, reason = decide_flaky_exit(exit_code, has_summary, zero_tests, expect_empty)
    print(f"flaky leg decision: {'PASS (informational)' if ok else 'FAIL'}: {reason}")
    if not ok:
        raise SystemExit(1)


def split_workflow_jobs(text):
    """Split a workflow file into top-level jobs: {name: block}.
    Lightweight line scan (no yaml dependency): a job starts at
    a two-space `name:` line after the `jobs:` section."""
    jobs = {}
    current = None
    in_jobs = False
    for line in text.splitlines():
        if re.match(r"^jobs:\s*$", line):
            in_jobs = True
            continue
        m = re.match(r"^  ([A-Za-z0-9_/-]+):\s*$", line)
        if in_jobs and m:
            current = m.group(1)
            jobs[current] = []
            continue
        if current is not None:
            jobs[current].append(line)
    return {k: "\n".join(v) for k, v in jobs.items()}


def normalise_inputs(text):
    """Rewrite bracket-notation manual inputs to dot notation:
    inputs['filter'] and inputs["filter"] mean the same as
    inputs.filter in GitHub expressions, so the gate scans the
    normalised form and no spelling evades it."""
    return re.sub(r"inputs\s*\[\s*['\"](\w+)['\"]\s*\]", r"inputs.\1", text)


def job_display_template(job_id, block):
    """Effective check-run name template for a matrix job: a
    job-level `name:` override (four-space indent, before any
    step) wins; otherwise GitHub reports `job_id (platform)`."""
    head = block.split("steps:")[0] if "steps:" in block else block
    m = re.search(r"^    name:\s*(.+?)\s*$", head, re.M)
    if m:
        return m.group(1)
    if "matrix.platform" in block:
        return job_id + " (${{ matrix.platform }})"
    return job_id


def check_workflow_identities(text):
    """Pin the required-check identity invariant (tier-1 G1).
    The required `test` job must always execute the full
    live-minus-roster selection: its steps must never reference
    ANY manual input (all `inputs.` spellings, normalised), its
    job-level `if:` must never skip on diagnostics (a skipped
    required check reads as success), and its nextest selection
    must positively be exactly the roster exclusion. The
    `audit` job must not reference any manual filter. When the
    workflow defines a `filter` input, filtered gating
    diagnostics must live in a distinct job whose rendered check
    name collides with no other job. Raises SystemExit on any
    violation."""
    text = normalise_inputs(text)
    jobs = split_workflow_jobs(text)
    if "test" not in jobs:
        fail("workflow identity: no required `test` job found")
    test_block = jobs["test"]
    # Job-level `if:` lives before any step.
    head = test_block.split("steps:")[0] if "steps:" in test_block else test_block
    m = re.search(r"inputs\.(\w+)", head)
    if m:
        fail(
            "workflow identity: required `test` job is conditional on "
            f"inputs.{m.group(1)} (skipped required checks report success)"
        )
    # Only scan executable lines: strip `#` comments so prose
    # mentioning the input name cannot trip the gate.
    test_code = "\n".join(ln.split("#", 1)[0] for ln in test_block.splitlines())
    m = re.search(r"inputs\.(\w+)", test_code)
    if m:
        fail(
            "workflow identity: required `test` job references "
            f"inputs.{m.group(1)} (filtered runs must use the distinct "
            "diagnostic job)"
        )
    if not re.search(
        r"-E\s+['\"]\$\{\{\s*needs\.roster\.outputs\.exclusion\s*\}\}['\"]",
        test_code,
    ):
        fail(
            "workflow identity: required `test` job does not run "
            "exactly the roster exclusion"
        )
    if "audit" in jobs:
        audit_block = jobs["audit"]
        audit_head = (
            audit_block.split("steps:")[0] if "steps:" in audit_block else audit_block
        )
        for name in re.findall(r"inputs\.(\w+)", audit_head):
            if name != "full_audit":
                fail(
                    "workflow identity: `audit` job references "
                    f"inputs.{name} (audit always runs the complete set)"
                )
        audit_steps = (
            audit_block.split("steps:", 1)[1]
            if "steps:" in audit_block
            else audit_block
        )
        audit_code = "\n".join(ln.split("#", 1)[0] for ln in audit_steps.splitlines())
        m = re.search(r"inputs\.(\w+)", audit_code)
        if m:
            fail(
                "workflow identity: `audit` job references "
                f"inputs.{m.group(1)} (audit always runs the complete set)"
            )
    defines_filter_input = bool(re.search(r"^\s*filter:\s*$", text, re.M))
    if defines_filter_input:
        if "diagnostic" not in jobs:
            fail(
                "workflow identity: workflow defines a `filter` input "
                "but has no distinct `diagnostic` job"
            )
        diag_code = "\n".join(
            ln.split("#", 1)[0] for ln in jobs["diagnostic"].splitlines()
        )
        if "inputs.filter" not in diag_code:
            fail(
                "workflow identity: `diagnostic` job does not consume "
                "inputs.filter (the filtered run lives there, not in "
                "the required job)"
            )
        diag_template = job_display_template("diagnostic", jobs["diagnostic"])
        for other, block in jobs.items():
            if other == "diagnostic":
                continue
            if job_display_template(other, block) == diag_template:
                fail(
                    "workflow identity: `diagnostic` check name collides "
                    f"with `{other}` ({diag_template})"
                )


def cmd_check_workflow(path):
    with open(path, "r", encoding="utf-8") as f:
        text = f.read()
    check_workflow_identities(text)
    print(f"workflow identity OK: {path}")


def cmd_self_test():
    """Backend-free fixtures: pure core plus injected-backend
    orchestration. No cargo, no network."""
    total = [0]
    failed = []

    def check(name, fn, expect_pass=True, expect_msg=None):
        total[0] += 1
        got_msg = None
        try:
            fn()
            result = True
        except SystemExit as e:
            result = False
            got_msg = str(e)
        ok = result == expect_pass
        if ok and not expect_pass and expect_msg is not None:
            # The fixture must fail FOR THE STATED REASON, not merely
            # fail: an incidental later check rejecting the same
            # input does not prove the mechanism under test.
            ok = expect_msg in (got_msg or "")
            if not ok:
                print(f"    expected failure reason containing: {expect_msg!r}")
                print(f"    actual failure reason: {got_msg!r}")
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
    live = lambda b, t: {(b, t, True)}
    dead = lambda b, t: {(b, t, False)}
    check(
        "predicate-exact-one",
        lambda: check_predicate_raw("m::t", live("b", "m::t")),
        True,
    )
    check(
        "predicate-stale",
        lambda: check_predicate_raw("m::t", set()),
        False,
        "matches 0 tests in raw set",
    )
    check(
        "predicate-overmatch",
        lambda: check_predicate_raw("m::t", live("b1", "m::t") | live("b2", "m::t_x")),
        False,
        "matches 2 tests in raw set",
    )
    check(
        "predicate-ordinary-collision",
        lambda: check_predicate_raw(
            "m::t", live("b1", "m::t") | dead("b2", "m::t_extra")
        ),
        False,
        "matches 2 tests in raw set",
    )
    check(
        "predicate-non-live",
        lambda: check_predicate_raw("m::t", dead("b", "m::t")),
        False,
        "is not live-eligible",
    )
    check(
        "predicate-leaf",
        lambda: check_predicate_raw("t", live("b", "m::t")),
        False,
        "is not a full test name",
    )
    check(
        "predicate-cross-binary",
        lambda: check_predicate_raw("m::t", live("b1", "m::t") | live("b2", "m::t")),
        False,
        "matches 2 tests in raw set",
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
    check(
        "decide-spoofed-summary-uses-final",
        lambda: (
            find_final_summary(
                "some output\nSummary [ 0.1s] 3 passed\ntail line\n"
                "flaky (x) Test 2026 Z Summary [  0.2s] 5 tests run: 3 passed, 2 failed\n"
            )
            == (True, False)
            or _fail()
        ),
        True,
    )
    check(
        "decide-spoof-only-has-no-summary",
        lambda: (
            find_final_summary("some output\nno marker here\ntail\n") == (False, False)
            or _fail()
        ),
        True,
    )
    check(
        "decide-zero-read-from-final-line",
        lambda: (
            find_final_summary(
                "Summary [ 0.1s] 7 tests run\nflaky (x) Test Z Summary [  0.0s] 0 tests run: 0 passed\n"
            )
            == (True, True)
            or _fail()
        ),
        True,
    )
    check(
        "decide-ansi-stripped-before-match",
        lambda: (
            find_final_summary(
                "flaky (x) Test\x1b[33;1m     Summary\x1b[0m [  0.1s] 1 tests run: 1 passed\n"
            )
            == (True, False)
            or _fail()
        ),
        True,
    )

    # -- filters_for --
    check(
        "filters-empty",
        lambda: (
            filters_for([])
            == ("test(__no_such_test__)", "not (test(__no_such_test__))")
            or _fail()
        ),
        True,
    )
    check(
        "filters-pair",
        lambda: (
            filters_for(["a::b", "c::d"])
            == ("test(a::b) or test(c::d)", "not (test(a::b) or test(c::d))")
            or _fail()
        ),
        True,
    )
    check(
        "list_argv-baseline-bypasses-default-filters",
        lambda: (
            list_argv("C.toml", None, None, True)
            == [
                "cargo",
                "nextest",
                "list",
                "--config-file",
                "C.toml",
                "--release",
                "--locked",
                "--run-ignored=all",
                "--ignore-default-filter",
                "--message-format",
                "json",
            ]
            or _fail()
        ),
        True,
    )
    check(
        "list_argv-tier-carries-profile-no-bypass",
        lambda: (
            list_argv("C.toml", "live", "test(m::t)", False)
            == [
                "cargo",
                "nextest",
                "list",
                "--config-file",
                "C.toml",
                "--release",
                "--locked",
                "-P",
                "live",
                "--run-ignored=all",
                "-E",
                "test(m::t)",
                "--message-format",
                "json",
            ]
            or _fail()
        ),
        True,
    )
    check(
        "effective-filter-no-manual",
        lambda: effective_flaky_filter("test(a::b)", "") == "test(a::b)" or _fail(),
        True,
    )
    check(
        "effective-filter-intersects-manual",
        lambda: (
            effective_flaky_filter("test(a::b)", "test(c::d)")
            == "( test(a::b) ) and ( test(c::d) )"
            or _fail()
        ),
        True,
    )
    check(
        "decide-spoofed-summary-uses-final",
        lambda: (
            find_final_summary(
                "some output\n"
                "Summary [ 0.1s] 3 passed\n"
                "tail line\n"
                "flaky (x) Test 2026 Z Summary [  0.2s] 5 tests run: 3 passed, 2 failed\n"
            )[0]
            or _fail()
        ),
        True,
    )
    check(
        "decide-spoof-only-has-no-summary",
        lambda: (
            (not find_final_summary("some output\nno marker here\ntail\n")[0])
            or _fail()
        ),
        True,
    )
    check(
        "decide-zero-read-from-final-line",
        lambda: (
            find_final_summary(
                "Summary [ 0.1s] 3 passed\n"
                "flaky (x) Test 2026 Z Summary [  0.0s] 0 tests run: 0 passed\n"
            )
            == (True, True)
            or _fail()
        ),
        True,
    )

    # -- evaluate() with an injected fake backend (no cargo) --
    class FakeLister:
        """Canned discovery/selection. Selection honors the
        generated -E shape closely enough to prove the
        orchestration: exact full-name test() predicates over
        the raw set, then per-profile projection. Records the
        exact argv each call would have used, so the
        bypass/profile/build-flag composition is pinned, not
        just the boolean outcome."""

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
            self.recorded.append(list_argv("C.toml", None, None, True))
            return set(self._baseline)

        def predicate(self, profile, name):
            self.recorded.append(list_argv("C.toml", profile, f"test({name})", False))
            pool = {
                i
                for i in self._baseline
                if i[1] not in (self.live_df if profile == "live" else self.flaky_df)
            }
            return self._predicate_match(name, pool)

        def selection(self, profile, filt):
            self.recorded.append(list_argv("C.toml", profile, filt, False))
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
        out = evaluate(fl, names, inclusion, exclusion)
        return fl, out

    def e2e(names, baseline, live_df=(), flaky_df=()):
        inc, exc = filters_for(names)
        fl, _ = run_eval(names, baseline, inc, exc, live_df, flaky_df)
        return fl

    def has_flag(calls, want, without):
        return any(want in c and all(x not in c for x in without) for c in calls)

    base90 = [I("m::live1"), I("m::live2"), U("m::ord1")]
    check("e2e-empty-roster", lambda: e2e([], base90), True)
    check("e2e-one-entry", lambda: e2e(["m::live1"], base90), True)
    check(
        "e2e-stale",
        lambda: e2e(["m::gone"], base90),
        False,
        "matches 0 tests in raw set",
    )
    check(
        "e2e-ordinary-collision",
        lambda: e2e(["m::live1"], base90 + [U("m::live1_extra")]),
        False,
        "matches 2 tests in raw set",
    )
    check(
        "e2e-duplicate-identities",
        lambda: e2e(["m::live1"], [I("m::live1"), ("bin2", "m::live1", True)]),
        False,
        "matches 2 tests in raw set",
    )
    check(
        "e2e-default-filter-omission",
        lambda: e2e(["m::live1"], base90, live_df=("m::live2",)),
        False,
        "union != baseline",
    )
    check(
        "e2e-flaky-only-omission",
        lambda: e2e(
            ["m::live1"], [I("m::live1"), U("m::ord1")], flaky_df=("m::live1",)
        ),
        False,
        "does not select exactly itself under -P flaky",
    )

    def _e2e_overlap_only():
        # Gating selection ignores the exclusion: raw, predicate,
        # and informational checks all pass, so disjointness is
        # the ONLY failing check.
        class NoExclusion(FakeLister):
            def selection(self, profile, filt):
                if profile == "live":
                    return {i for i in self._baseline if i[1] not in self.live_df}
                return super().selection(profile, filt)

        return evaluate(
            NoExclusion(base90),
            ["m::live1"],
            "test(m::live1)",
            "not (test(m::live1))",
        )

    check(
        "e2e-overlap-only",
        lambda: _e2e_overlap_only(),
        False,
        "tiers overlap",
    )

    def _e2e_informational_mismatch_only():
        # Flaky selection drops a rostered test while raw and
        # per-profile predicate checks pass: informational
        # equality is the ONLY failing check.
        class DropFlaky(FakeLister):
            def selection(self, profile, filt):
                got = super().selection(profile, filt)
                if profile == "flaky":
                    got = {i for i in got if i[1] != "m::live1"}
                return got

        return evaluate(
            DropFlaky(base90),
            ["m::live1"],
            "test(m::live1)",
            "not (test(m::live1))",
        )

    check(
        "e2e-informational-mismatch-only",
        lambda: _e2e_informational_mismatch_only(),
        False,
        "informational selection != roster",
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

    def _e2e_argv_pinned_per_call_site():
        # F3: the recorded argv of every listing call proves the
        # bypass/profile composition — baseline bypasses without
        # -P; tier listings carry -P and no bypass. Mutating any
        # of these (e.g. adding --ignore-default-filter to a
        # tier, or -P to the baseline) must fail here.
        fl = FakeLister(base90)
        inc, exc = filters_for(["m::live1"])
        evaluate(fl, ["m::live1"], inc, exc)
        by_kind = {}
        for argv in fl.recorded:
            assert argv[:5] == [
                "cargo",
                "nextest",
                "list",
                "--config-file",
                "C.toml",
            ], argv
            assert "--release" in argv and "--locked" in argv, argv
            assert argv[-2:] == ["--message-format", "json"], argv
            assert argv.count("--run-ignored=all") == 1, argv
            if (
                argv[-4] == "--ignore-default-filter"
                or "--ignore-default-filter" in argv
            ):
                assert "-P" not in argv, argv
                by_kind.setdefault("baseline", []).append(argv)
            else:
                assert "-P" in argv, argv
                by_kind.setdefault("tier", []).append(argv)
        assert by_kind.get("baseline"), "baseline listing missing"
        assert len(by_kind.get("tier", [])) >= 3, by_kind.keys()
        # tier calls are exactly: 2 predicate checks + 2 selection checks
        return True

    def _e2e_production_argv_pinned():
        # F3, production side: the real Lister must emit the
        # exact same composition. Stub subprocess only (no
        # cargo); assert every recorded argv verbatim.
        canned = {
            "rust-suites": {
                "s": {
                    "binary-id": "b",
                    "testcases": {
                        "m::t": {
                            "kind": "test",
                            "ignored": True,
                            "filter-match": {"status": "matches"},
                        }
                    },
                }
            }
        }

        class _Out:
            stdout = json.dumps(canned)
            stderr = ""

        real_run = subprocess.run
        seen = []

        def fake_run(cmd, **kw):
            assert kw.get("capture_output") and kw.get("text") and kw.get("check"), kw
            seen.append(cmd)
            return _Out()

        subprocess.run = fake_run  # noqa: E402 -- test-only seam
        try:
            lst = Lister("C.toml")
            lst.baseline()
            lst.selection("live", "test(m::t)")
            lst.predicate("flaky", "m::t")
        finally:
            subprocess.run = real_run
        assert seen == [
            [
                "cargo",
                "nextest",
                "list",
                "--config-file",
                "C.toml",
                "--release",
                "--locked",
                "--run-ignored=all",
                "--ignore-default-filter",
                "--message-format",
                "json",
            ],
            [
                "cargo",
                "nextest",
                "list",
                "--config-file",
                "C.toml",
                "--release",
                "--locked",
                "-P",
                "live",
                "--run-ignored=all",
                "-E",
                "test(m::t)",
                "--message-format",
                "json",
            ],
            [
                "cargo",
                "nextest",
                "list",
                "--config-file",
                "C.toml",
                "--release",
                "--locked",
                "-P",
                "flaky",
                "--run-ignored=all",
                "-E",
                "test(m::t)",
                "--message-format",
                "json",
            ],
        ], seen
        return True

    check(
        "e2e-ordinary-loss",
        lambda: _e2e_ordinary_loss(),
        False,
        "gating dropped ordinary tests",
    )
    check(
        "e2e-command-builder-flags",
        lambda: _e2e_command_builder_flags(),
        True,
    )
    check(
        "e2e-argv-pinned-per-call-site",
        lambda: _e2e_argv_pinned_per_call_site(),
        True,
    )
    check(
        "e2e-production-argv-pinned",
        lambda: _e2e_production_argv_pinned(),
        True,
    )

    GOOD_WORKFLOW = """name: qa
on:
  workflow_dispatch:
    inputs:
      filter:
        description: 'x'
        required: false
        default: ''
jobs:
  test:
    runs-on: ${{ matrix.platform }}
    steps:
      - name: Test (live profile)
        run: |
          cargo nextest run -E '${{ needs.roster.outputs.exclusion }}' 2>&1 | tee suite.log
  diagnostic:
    if: github.event_name == 'workflow_dispatch' && inputs.filter != ''
    runs-on: ubuntu-latest
    steps:
      - name: Test (targeted diagnostic, non-gating)
        run: |
          cargo nextest run -E "( inputs.filter ) and exclusion" 2>&1 | tee diagnostic.log
  audit:
    if: inputs.full_audit == true
    runs-on: ubuntu-latest
    steps:
      - name: Test (full live audit)
        run: |
          cargo nextest run --no-fail-fast 2>&1 | tee audit.log
"""
    check(
        "workflow-good",
        lambda: check_workflow_identities(GOOD_WORKFLOW),
        True,
    )
    check(
        "workflow-required-references-filter",
        lambda: check_workflow_identities(
            GOOD_WORKFLOW.replace(
                "cargo nextest run -E '${{ needs.roster.outputs.exclusion }}'",
                'cargo nextest run -E "( inputs.filter ) and exclusion"',
            )
        ),
        False,
        "required `test` job references inputs.filter",
    )
    check(
        "workflow-required-bracket-step-evades",
        lambda: check_workflow_identities(
            GOOD_WORKFLOW.replace(
                "cargo nextest run -E '${{ needs.roster.outputs.exclusion }}'",
                "cargo nextest run -E \"( inputs['filter'] ) and exclusion\"",
            )
        ),
        False,
        "required `test` job references inputs.filter",
    )
    check(
        "workflow-required-bracket-if-evades",
        lambda: check_workflow_identities(
            GOOD_WORKFLOW.replace(
                "  test:\n    runs-on:",
                "  test:\n    if: inputs[\"filter\"] == ''\n    runs-on:",
            )
        ),
        False,
        "conditional on inputs.filter",
    )
    check(
        "workflow-required-wrong-selection",
        lambda: check_workflow_identities(
            GOOD_WORKFLOW.replace(
                "cargo nextest run -E '${{ needs.roster.outputs.exclusion }}'",
                "cargo nextest run -E 'something-else'",
            )
        ),
        False,
        "does not run exactly the roster exclusion",
    )
    check(
        "workflow-diagnostic-name-collision",
        lambda: check_workflow_identities(
            GOOD_WORKFLOW.replace(
                "  diagnostic:\n    if:",
                "  diagnostic:\n    name: test (${{ matrix.platform }})\n    if:",
            )
        ),
        False,
        "check name collides",
    )
    check(
        "workflow-diagnostic-not-consuming",
        lambda: check_workflow_identities(
            GOOD_WORKFLOW.replace("inputs.filter != ''", "inputs.other != ''").replace(
                "( inputs.filter )", "( inputs.other )"
            )
        ),
        False,
        "does not consume inputs.filter",
    )
    check(
        "workflow-required-skipped-on-filter",
        lambda: check_workflow_identities(
            GOOD_WORKFLOW.replace(
                "  test:\n    runs-on:",
                "  test:\n    if: inputs.filter == ''\n    runs-on:",
            )
        ),
        False,
        "conditional on inputs.filter",
    )

    def _without_diagnostic(text):
        out, skip = [], False
        for ln in text.splitlines():
            if ln.startswith("  diagnostic:"):
                skip = True
                continue
            if skip and ln.startswith("  audit:"):
                skip = False
            if not skip:
                out.append(ln)
        return "\n".join(out) + "\n"

    check(
        "workflow-missing-diagnostic",
        lambda: check_workflow_identities(_without_diagnostic(GOOD_WORKFLOW)),
        False,
        "no distinct `diagnostic` job",
    )
    check(
        "workflow-audit-references-filter",
        lambda: check_workflow_identities(
            GOOD_WORKFLOW.replace(
                "cargo nextest run --no-fail-fast",
                'cargo nextest run -E "( inputs.filter )" --no-fail-fast',
            )
        ),
        False,
        "`audit` job references inputs.filter",
    )

    passed = total[0] - len(failed)
    if failed:
        fail(f"self-test failures: {failed}")
    print(f"self-test OK: {passed} fixtures passed")


def main():
    args = sys.argv[1:]
    if args == ["--self-test"]:
        cmd_self_test()
        return
    if len(args) >= 1 and args[0] == "effective-subset":
        # roster-check.py effective-subset <roster> [--manual EXPR] [--config-file P]
        manual = ""
        rest = list(args[1:])
        if "--manual" in rest:
            i = rest.index("--manual")
            manual = rest[i + 1]
            rest = rest[:i] + rest[i + 2 :]
        config = ".auxiliary/configuration/nextest.toml"
        if "--config-file" in rest:
            i = rest.index("--config-file")
            config = rest[i + 1]
            rest = rest[:i] + rest[i + 2 :]
        if len(rest) != 1:
            sys.exit(
                "usage: roster-check.py effective-subset <roster> [--manual EXPR] [--config-file P]"
            )
        cmd_effective_subset(rest[0], manual, config)
        return
    if len(args) >= 1 and args[0] == "decide-flaky":
        cmd_decide_flaky(args[1:])
        return
    if len(args) == 2 and args[0] == "check-workflow":
        cmd_check_workflow(args[1])
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


def cmd_effective_subset(roster_path, manual, config):
    """Resolve the flaky leg's effective selection: the roster
    inclusion intersected with an optional manual dispatch
    filter, evaluated with the leg's actual flags. Prints the
    effective filter, whether it selects anything, and which
    rostered identities a manual filter omitted (explicit
    omission accounting so an empty diagnostic selection is
    distinguishable from an unexpected empty selection)."""
    with open(roster_path, "rb") as f:
        names = parse_roster(f.read().decode("utf-8"), roster_path)
    inclusion, _ = filters_for(names)
    effective = effective_flaky_filter(inclusion, manual or "")
    selected = run_nextest_list(config, "flaky", effective, False)
    selected_names = sorted({t for _, t, _ in selected})
    omitted = sorted(n for n in names if n not in selected_names)
    print(f"effective_filter={effective}")
    print(f"empty={'true' if not selected else 'false'}")
    print(f"omitted={','.join(omitted)}")
    print(f"selected={len(selected)}")


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
    decide_flaky_for_log(positional[0], exit_code, expect_empty)


def rest_is_generate(args):
    return (
        len(args) in (2, 3)
        and all(a == "GENERATE" for a in args[1:])
        and not args[0].startswith("-")
    )


if __name__ == "__main__":
    main()
