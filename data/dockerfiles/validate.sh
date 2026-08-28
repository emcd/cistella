#!/usr/bin/env bash
set -euo pipefail
# Build each example image and verify baked terminfo (no mount, no TERMINFO).
for d in data/dockerfiles/*; do
  [ -f "$d/Dockerfile" ] || continue
  name=$(basename "$d")
  tag="cistella/$name:example"
  echo "=== Building $tag from $d ==="
  podman build -f "$d/Dockerfile" -t "$tag" .
  echo "--- Verifying baked terminfo for $tag ---"
  podman run --rm -e TERM=xterm-ghostty "$tag" sh -c 'infocmp xterm-ghostty >/dev/null && [ "$(tput colors)" = "256" ] && echo ok'
  echo "=== $tag OK ==="
done
