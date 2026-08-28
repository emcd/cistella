#!/usr/bin/env bash
set -euo pipefail
# Shallow-clone latest Ghostty release and update data/terminfos.
tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT
echo "Cloning Ghostty..."
# Fetch latest tag (ghostty-org/ghostty is canonical; mitchellh redirects)
latest_tag=$(curl -fsSL https://api.github.com/repos/ghostty-org/ghostty/releases/latest | sed -n 's/.*"tag_name": *"\(.*\)".*/\1/p')
if [ -z "$latest_tag" ]; then
  echo "Failed to fetch Ghostty latest tag" >&2
  exit 1
fi
echo "Latest Ghostty: $latest_tag"
# Use ghostty-org/ghostty (mitchellh redirects, but API pin should be canonical)
git clone --depth 1 --branch "$latest_tag" https://github.com/ghostty-org/ghostty "$tmpdir/ghostty" 2>&1 | cat
# Ghostty terminfo source location varies; try common paths
src=""
for cand in "$tmpdir/ghostty/terminfo/xterm-ghostty" "$tmpdir/ghostty/src/terminfo/xterm-ghostty" "$tmpdir/ghostty/terminfo/src/xterm-ghostty"; do
  if [ -f "$cand" ]; then src="$cand"; break; fi
done
if [ -z "$src" ]; then
  echo "Ghostty source not found at expected paths for $latest_tag" >&2
  echo "Refusing to fall back to host infocmp — pin is required for reproducibility." >&2
  echo "If you need host-derived, run: TERMINFO=/usr/local/share/terminfo infocmp -x xterm-ghostty > data/terminfos/xterm-ghostty.terminfo" >&2
  exit 1
fi
# Validate and copy source to central location with version pin recorded
mkdir -p data/terminfos
# Prepend version header so data/terminfos/xterm-ghostty.terminfo is self-documenting
{ echo "# Ghostty $latest_tag (pinned, via update-ghostty-terminfo.sh)"; cat "$src"; } > data/terminfos/xterm-ghostty.terminfo
echo "$latest_tag" > data/terminfos/xterm-ghostty.version
echo "Updated data/terminfos/xterm-ghostty.terminfo from $latest_tag (version recorded in header and .version file)"
# Optionally verify compiled via container tic (ensures compatibility)
# podman run --rm -v "$PWD/data/terminfos:/src:ro" debian:bookworm-slim bash -c 'apt-get update && apt-get install -y ncurses-bin && tic -x -o /tmp/out /src/xterm-ghostty.terminfo && echo ok'
