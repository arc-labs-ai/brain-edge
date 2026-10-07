# syntax=docker/dockerfile:1
#
# brain-edge — HTTP/JSON edge for the Brain memory database.
#
# Build context is this crate:
#
#       docker build -t brain-edge:latest .
#
# It used to have to be the parent directory, because Cargo.toml carried a path
# dependency on a sibling checkout (`brain-db-sdk = { path = "../brain-sdk/rust" }`)
# that a context scoped here could not see. brain-db-sdk is on crates.io now, so
# the dependency resolves from the registry and the context is just this crate.

# ---- builder ---------------------------------------------------------------
FROM rust:1-bookworm AS builder

WORKDIR /build

COPY . .

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
COPY --from=builder /build/target/release/brain-edge /usr/local/bin/brain-edge

# HTTP listen port (BRAIN_EDGE_LISTEN default 0.0.0.0:8080).
EXPOSE 8080

# Readiness probe against the edge's own /health/ready.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget --quiet --spider http://127.0.0.1:8080/health/ready || exit 1

ENTRYPOINT ["brain-edge"]
