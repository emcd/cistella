#!/usr/bin/env python3
"""Roster check for the flaky live-test quarantine tier.

Proves, at the exact tip and from execution output (never from
reading config): per-predicate exact-one live-eligible identity
against the raw discovered set; informational-equals-roster;
tier disjointness; union against the full pre-change gating
selection; ordinary-test preservation.

Usage:
    roster-check.py <roster.toml> <inclusion -E> <exclusion -E> \
        [--config-file <nextest.toml>] [--profile <live>]

Exit 0 on success; non-zero with a diagnostic naming the
failing check.
"""

import json
import re
import subprocess
import sys


def run_nextest_list(config, profile, extra_args):
    cmd = [
        "cargo",
        "nextest",
        "list",
        "--config-file",
        config,
        "-P",
        profile,
        "--run-ignored=all",
        "--ignore-default-filter",
        "--message-format",
        "json",
    ] + extra_args
    out = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout
    tests = {}  # (binary, name) -> ignored
    d = json.loads(out)
    for _suite, body in d.get("rust-suites", {}).items():
        if not isinstance(body, dict):
            continue
        binary = body.get("binary-id", "?")
        for tname, tinfo in body.get("testcases", {}).items():
            if not isinstance(tinfo, dict):
                continue
            # Only SELECTED tests count: filter-match must be
            # "matches". Listed-but-filtered-out tests are not
            # in the selection, and must not satisfy any check.
            if tinfo.get("filter-match", {}).get("status") != "matches":
                continue
            tests[(binary, tname)] = bool(tinfo.get("ignored"))
    return tests


def roster_names(path):
    names = []
    for line in open(path).read().splitlines():
        line = line.split("#", 1)[0]
        for m in re.findall(r'"([^"]+)"', line):
            names.append(m)
    if len(names) != len(set(names)):
        dupes = sorted({n for n in names if names.count(n) > 1})
        fail(f"duplicate roster lines: {dupees}")
    return names


def filters_for(names):
    """Build the nextest -E selection pair. Verified semantics:
    plain test(<full-name>) matches exactly one test (names are
    unique and no live-gated name is a substring of another);
    an empty roster yields a match-nothing inclusion whose run
    exits 0 via --no-tests pass, and a match-everything
    exclusion byte-identical to the unfiltered gate."""
    if names:
        inclusion = " or ".join(f"test({n})" for n in names)
    else:
        inclusion = "test(__no_such_test__)"
    return inclusion, f"not ({inclusion})"


def fail(msg):
    print(f"roster-check FAILED: {msg}")
    sys.exit(1)


def main():
    args = sys.argv[1:]
    config = ".auxiliary/configuration/nextest.toml"
    profile = "live"
    rest = []
    i = 0
    while i < len(args):
        if args[i] == "--config-file":
            config = args[i + 1]
            i += 2
        elif args[i] == "--profile":
            profile = args[i + 1]
            i += 2
        else:
            rest.append(args[i])
            i += 1
    roster_path, inclusion, exclusion = rest
    if inclusion == "GENERATE" or exclusion == "GENERATE":
        names = roster_names(roster_path)
        inclusion, exclusion = filters_for(names)
        print(f"inclusion={inclusion}")
        print(f"exclusion={exclusion}")
        return
    names = roster_names(roster_path)

    baseline = run_nextest_list(config, profile, [])
    live = {k for k, ign in baseline.items() if ign}
    print(f"baseline: {len(baseline)} discovered, {len(live)} live-gated")

    # Per-predicate validation against the RAW discovered set:
    # exactly one composite identity, and it must be live-eligible.
    # Substring semantics of test() mean any extra container is an
    # overmatch, even an ordinary (non-live) test.
    for n in names:
        holders = [k for k in baseline if n in k[1]]
        if not holders:
            fail(f"roster entry matches zero tests: {n!r}")
        if len(holders) > 1:
            fail(f"roster entry over-matches {[f'{b}::{t}' for b, t in holders]}")
        if holders[0] not in live:
            fail(f"roster entry is not live-eligible: {n!r}")

    gating = run_nextest_list(config, profile, ["-E", exclusion])
    flaky = run_nextest_list(config, profile, ["-E", inclusion])
    gating_live = {k for k, ign in gating.items() if ign}
    flaky_live = {k for k, ign in flaky.items() if ign}
    # Compare by test name only: binary scoping is enforced by the
    # per-predicate check above (exactly one identity per entry).
    roster_names_set = set(names)
    flaky_names = {t for _, t in flaky_live}
    gating_names = {t for _, t in gating_live}

    if flaky_names != roster_names_set:
        fail(
            "informational selection != roster: "
            f"missing={sorted(roster_names_set - flaky_names)} "
            f"extra={sorted(flaky_names - roster_names_set)}"
        )
    if gating_names & flaky_names:
        fail(f"tiers overlap: {sorted(gating_names & flaky_names)}")
    baseline_live_names = {t for _, t in live}
    if gating_names | flaky_names != baseline_live_names:
        fail(
            "union != baseline live universe: "
            f"missing={sorted(baseline_live_names - gating_names - flaky_names)} "
            f"extra={sorted(gating_names | flaky_names - baseline_live_names)}"
        )
    # Ordinary (non-ignored) tests must be preserved in gating:
    # the exclusion must not silently remove ordinary coverage.
    baseline_ordinary = {t for (b, t), ign in baseline.items() if not ign}
    gating_ordinary = {t for (b, t), ign in gating.items() if not ign}
    if gating_ordinary != baseline_ordinary:
        fail(
            "gating dropped ordinary tests: "
            f"{sorted(baseline_ordinary - gating_ordinary)[:10]}"
        )
    print(
        f"roster-check OK: roster={len(names)} "
        f"gating-live={len(gating_names)} flaky-live={len(flaky_names)} "
        f"ordinary-preserved={len(gating_ordinary)}"
    )


if __name__ == "__main__":
    main()
