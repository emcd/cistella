#!/usr/bin/env bash
set -euo pipefail
# Build each example image and verify baked terminfo (no mount, no TERMINFO).
for d in data/dockerfiles/*; do
  [ -f "$d/Dockerfile" ] || continue
  name=$(basename "$d")
  tag="cistella/$name:example"
  echo "=== Building $tag from $d ==="
  podman build -f "$d/Dockerfile" -t "$tag" .
  echo "--- Verifying baked terminfo and opencode for $tag ---"
  # Use --userns=keep-id so HOME=/ and uid 1000 — opencode must be in /usr/local/bin
  expected_version=$(grep -E '^ARG OPENCODE_VERSION=' "$d/Dockerfile" | cut -d= -f2)
  podman run --rm --userns=keep-id -e TERM=xterm-ghostty "$tag" sh -c 'infocmp xterm-ghostty >/dev/null && [ "$(tput colors)" = "256" ] && opencode --version | grep -q "'"$expected_version"'" && echo ok'
  echo "=== $tag OK ==="
done
