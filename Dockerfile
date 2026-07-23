# syntax=docker/dockerfile:1
#
# brain-edge — HTTP/JSON edge for the Brain memory database.
#
# IMPORTANT — build context is the arc-labs PARENT dir, not this crate:
#   brain-edge's Cargo.toml has a path dependency on a SIBLING directory:
#       brain-db-sdk = { path = "../brain-sdk/rust" }
#   so a context scoped to brain-edge/ alone cannot see ../brain-sdk and the
#   build fails. Build from the parent so both crates are COPY-able:
#
#       cd /Users/dodo/Desktop/work/arc-labs
#       docker build -f brain-edge/Dockerfile -t brain-edge:latest .
#
#   (docker-compose.yml already sets `context: ..` for you.)

# ---- builder ---------------------------------------------------------------
FROM rust:1-bookworm AS builder

WORKDIR /build

# Preserve the relative layout the path dep expects: brain-edge/ next to
# brain-sdk/, so `../brain-sdk/rust` from inside brain-edge/ resolves.
COPY brain-edge/ ./brain-edge/
COPY brain-sdk/  ./brain-sdk/

WORKDIR /build/brain-edge
# --locked: build against the committed Cargo.lock so the image is reproducible
# and a drifting transitive dep can't silently change what ships.
RUN cargo build --release --locked --bin brain-edge

# ---- runtime ---------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# wget for the HEALTHCHECK (--spider), plus CA certs for any TLS the SDK does.
RUN apt-get update \
    && apt-get install -y --no-install-recommends wget ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Non-root runtime user.
RUN useradd --system --create-home --uid 10001 brain
USER brain

# Just the compiled binary — no toolchain, no source.
COPY --from=builder /build/brain-edge/target/release/brain-edge /usr/local/bin/brain-edge

# HTTP listen port (BRAIN_EDGE_LISTEN default 0.0.0.0:8080).
EXPOSE 8080

# Readiness probe against the edge's own /health/ready.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget --quiet --spider http://127.0.0.1:8080/health/ready || exit 1

ENTRYPOINT ["brain-edge"]
