#!/bin/sh
# Prints the exact-equality nextest -E expression for the smoke
# tier of tests/tiers.txt (one `test(=id)` alternation). Single
# source of truth shared by the tester gate and the pre-push
# hook: both invoke it inside a real shell (pre-commit
# `language: system` does NOT launch one, so the pipeline
# must never live inline in a hook entry). Bare `test(id)`
# is contains-match — exact predicates only.
set -eu
grep '^smoke ' tests/tiers.txt | cut -d\  -f2 | sed 's/.*/test(=&)/' | paste -s -d'|' -
