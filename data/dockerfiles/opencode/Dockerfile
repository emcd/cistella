# Example per-harness image — self-contained.
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171 AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends ncurses-bin \
    && rm -rf /var/lib/apt/lists/*

ARG TERMINFOS_SRC=data/terminfos/xterm-ghostty.terminfo
COPY ${TERMINFOS_SRC} /tmp/xterm-ghostty.terminfo
RUN tic -x -o /tmp/terminfo /tmp/xterm-ghostty.terminfo \
    && TERMINFO=/tmp/terminfo infocmp -x xterm-ghostty >/dev/null

FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ARG DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends ncurses-bin ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /tmp/terminfo /usr/share/terminfo

ARG OPENCODE_VERSION=1.18.23
# NOTE: https://opencode.ai/install is a mutable script; version pin ensures the
# binary version, but not script integrity. For production, use a versioned
# artifact URL with checksum/signature verification (e.g., GitHub release tarball
# + sha256) instead of the install script.
RUN if [ -n "$OPENCODE_VERSION" ]; then \
        curl -fsSL https://opencode.ai/install | bash -s -- --version "$OPENCODE_VERSION"; \
    else \
        echo "OPENCODE_VERSION is empty; refusing to install latest (pin required for reproducibility)" >&2; exit 1; \
    fi
