# syntax=docker/dockerfile:1.7
#
# Multi-stage build for the OxDEX node binary.
#
# Stage 1 ("planner"): compute a dependency-only recipe with cargo-chef so
#                      Docker can cache the (slow) dependency build layer.
# Stage 2 ("builder"): cook the recipe, then build the workspace.
# Stage 3 ("runtime"): copy the static-ish binary into a minimal Debian
#                      slim image with only the runtime deps it actually
#                      needs (TLS roots, libgcc, libssl is NOT needed
#                      because we use rustls).

ARG RUST_VERSION=1.78
ARG DEBIAN_VERSION=bookworm

# ---------- planner ----------
FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS planner
WORKDIR /app
RUN cargo install cargo-chef --locked --version ^0.1
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---------- builder ----------
FROM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS builder
WORKDIR /app
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config \
 && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked --version ^0.1

# Cache deps
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json -p oxdex-node

# Build the actual binary
COPY . .
RUN cargo build --release -p oxdex-node \
 && strip target/release/oxdex-node

# ---------- runtime ----------
FROM debian:${DEBIAN_VERSION}-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 1000 oxdex \
 && useradd  --system --uid 1000 --gid oxdex --home /home/oxdex --create-home oxdex

WORKDIR /home/oxdex
COPY --from=builder /app/target/release/oxdex-node /usr/local/bin/oxdex-node
COPY --from=builder /app/crates/oxdex-storage/migrations /home/oxdex/migrations

# Sensible container defaults — override via `-e` / compose / k8s.
ENV RUST_LOG=info \
    OXDEX__SERVER__BIND=0.0.0.0:8080 \
    OXDEX_METRICS_PORT=9100

EXPOSE 8080 9100
USER oxdex

# `tini` reaps zombies + forwards SIGTERM cleanly to the node, which then
# triggers the cooperative auctioneer + Actix shutdown wired in main.rs.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/oxdex-node"]

