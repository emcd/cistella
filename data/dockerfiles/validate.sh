#!/usr/bin/env bash
set -euo pipefail
# Build each example image and verify baked terminfo (no mount, no TERMINFO).
for d in data/dockerfiles/*; do
  [ -f "$d/Dockerfile" ] || continue
  name=$(basename "$d")
  tag="localhost/cistella/$name:example"
  echo "=== Building $tag from $d ==="
  podman build -f "$d/Dockerfile" -t "$tag" .
  echo "--- Verifying baked terminfo and opencode for $tag ---"
  # Use --userns=keep-id (HOME would be / without driver); driver sets HOME=/home/cistella and mounts writable HOME
  expected_version=$(grep -E '^ARG OPENCODE_VERSION=' "$d/Dockerfile" | cut -d= -f2)
  podman run --rm --userns=keep-id -e TERM=xterm-ghostty -e HOME=/tmp "$tag" sh -c 'infocmp xterm-ghostty >/dev/null && [ "$(tput colors)" = "256" ] && opencode --version | grep -q "'"$expected_version"'" && echo ok'
  echo "--- Verifying binary discovery with HOME=/ (no writable HOME) ---"
  podman run --rm --userns=keep-id -e TERM=xterm-ghostty -e HOME=/ "$tag" sh -c 'command -v opencode | grep -q "/usr/local/bin/opencode" && test -x /usr/local/bin/opencode && echo ok'
  echo "=== $tag OK ==="
done
