#!/usr/bin/env bash
set -euo pipefail
# Update pinned OPENCODE_VERSION in Dockerfile from latest GitHub release.
DOCKERFILE="data/dockerfiles/opencode/Dockerfile"
latest=$(curl -fsSL https://api.github.com/repos/anomalyco/opencode/releases/latest | sed -n 's/.*"tag_name": *"v\([^"]*\)".*/\1/p')
if [ -z "$latest" ]; then
  echo "Failed to fetch latest opencode version" >&2
  exit 1
fi
echo "Latest opencode: $latest"
# Dockerfile has: ARG OPENCODE_VERSION=...
if ! grep -q "^ARG OPENCODE_VERSION=" "$DOCKERFILE"; then
  echo "Pattern not found in $DOCKERFILE" >&2
  exit 1
fi
sed -i "s/^ARG OPENCODE_VERSION=.*/ARG OPENCODE_VERSION=$latest/" "$DOCKERFILE"
if ! grep -q "^ARG OPENCODE_VERSION=$latest" "$DOCKERFILE"; then
  echo "Failed to update $DOCKERFILE" >&2
  exit 1
fi
echo "Updated $DOCKERFILE to $latest"
