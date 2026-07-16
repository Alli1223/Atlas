# syntax=docker/dockerfile:1
#
# Atlas — multi-stage build.
#
#   docker build -t atlas .
#   docker run -p 8080:8080 -v atlas-data:/data atlas
#
# Three stages: node builds the SPA, rust builds the server, debian-slim runs it.
# Only the last stage ships — the toolchains stay behind.
#
# Image versions are pinned to a minor, and the builder/runtime pair are both
# Debian trixie on purpose: the binary is dynamically linked against glibc, so a
# builder and runtime on different Debian releases is a runtime loader failure.

# ---------------------------------------------------------------- 1. frontend

FROM node:22-trixie-slim AS frontend

WORKDIR /build

# Dependencies first: package*.json changes far less often than source, so the
# npm ci layer survives most rebuilds.
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci

COPY frontend/ ./
RUN npm run build

# ---------------------------------------------------------------- 2. backend

FROM rust:1.96-slim-trixie AS chef

# sqlx's `sqlite-bundled` feature compiles SQLite from source, so the builder
# needs a C toolchain regardless of how pure the Rust is.
#
# DL3008 (pin apt versions) is ignored deliberately: Debian's archive does not
# retain superseded versions, so a pinned `=version` build works until the next
# point release and then fails permanently. The base image tag is the pin.
# hadolint ignore=DL3008
RUN apt-get update \
    && apt-get install --no-install-recommends -y build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --version 0.1.77 --locked

WORKDIR /build

# cargo-chef exists to solve one problem: `COPY . .` before `cargo build` means
# every source edit rebuilds every dependency. The planner distils Cargo.toml
# into a recipe, and cooking that recipe is a cache layer keyed on dependencies
# alone — so editing a handler recompiles the handler, not all 400 crates.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY . .
# There is no database at image-build time, so the query macros must check
# against the committed .sqlx/ metadata. This is why `just prepare` output is
# committed — without it, this line fails.
ENV SQLX_OFFLINE=true
RUN cargo build --release --bin atlas \
    && strip target/release/atlas

# ---------------------------------------------------------------- 3. runtime

FROM debian:trixie-slim AS runtime

# ca-certificates: outbound TLS to the GitHub and Gemini APIs.
# curl:            the healthcheck below.
# git:             the agent runner clones repositories into /workspaces.
# hadolint ignore=DL3008
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates curl git \
    && rm -rf /var/lib/apt/lists/*

# Fixed uid/gid, not a system-assigned one: the numbers show up in bind-mount
# ownership on the host, so they need to be stable across rebuilds.
RUN groupadd --gid 10001 atlas \
    && useradd --uid 10001 --gid 10001 --create-home --shell /usr/sbin/nologin atlas

WORKDIR /app

COPY --from=builder  /build/target/release/atlas  /usr/local/bin/atlas
COPY --from=frontend /build/dist                  /app/static

# /data holds the SQLite database plus its -wal and -shm sidecars; /workspaces
# holds agent repo clones. BOTH MUST BE VOLUMES. A container filesystem is
# ephemeral, so an unmounted /data means the entire database — every project,
# card, and comment — is destroyed on `docker rm`. docker-compose.yml wires them.
#
# Deliberately not `VOLUME /data`: that directive silently creates anonymous
# volumes on plain `docker run`, which accumulate unnamed and get pruned by
# someone doing housekeeping. An explicit -v in compose is safer and visible.
RUN mkdir -p /data /workspaces && chown -R atlas:atlas /data /workspaces /app

USER atlas

# Every variable the process reads is ATLAS_-prefixed: the config loader is
# `figment::Env::prefixed("ATLAS_")`, and it *silently ignores* anything else.
# An unprefixed DATABASE_URL or a shortened ATLAS_BIND is not an error — it is
# a no-op that leaves the compiled-in default in place, which is exactly how
# this container previously ended up binding 127.0.0.1 (unreachable through
# `-p`) and crashing on a relative ./data/atlas.db that WORKDIR /app lacks.
# Keep these names in sync with .env.example and backend/src/config.rs.
#
# The database URL is absolute (three slashes) so it lands on the /data volume
# rather than under WORKDIR. ATLAS_DATA_DIR does not move it: they are
# independent settings.
ENV ATLAS_STATIC_DIR=/app/static \
    ATLAS_DATA_DIR=/data \
    ATLAS_WORKSPACE_DIR=/workspaces \
    ATLAS_DATABASE_URL="sqlite:///data/atlas.db?mode=rwc" \
    ATLAS_BIND_ADDR=0.0.0.0:8080 \
    RUST_LOG=info

EXPOSE 8080

# start-period covers migrations running on boot; until they finish the process
# is up but not yet listening, and that is not a failure.
HEALTHCHECK --interval=30s --timeout=3s --start-period=20s --retries=3 \
    CMD curl -fsS http://localhost:8080/healthz || exit 1

ENTRYPOINT ["/usr/local/bin/atlas"]
