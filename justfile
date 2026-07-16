# Atlas — task runner. https://just.systems
#
#   just            list every target
#   just dev        backend + frontend, together
#   just check      the pre-PR gate: fmt-check + lint + test
#
# A Makefile with the same target names exists for anyone without `just`.
#
# Assumptions, all in one place so they are cheap to correct:
#   - Cargo workspace root is ./, with member ./backend, package name `atlas`.
#   - Frontend is ./frontend (npm). Vite dev server :5173, backend :8080.
#   - Migrations live in ./backend/migrations.
#   - `sqlx-cli` is installed for migrate/prepare:
#       cargo install sqlx-cli --version 0.9.0 --no-default-features --features sqlite
#     (no --locked — sqlx 0.9 removed Cargo.lock from the crate; see docs/research/rust-stack.md)

set shell := ["bash", "-euo", "pipefail", "-c"]
set dotenv-load := true

backend_pkg := "atlas"
migrations := "backend/migrations"
default_db := "sqlite://./data/atlas.db?mode=rwc"

# `just` with no arguments lists what is available.
_default:
    @just --list --unsorted

# ---------------------------------------------------------------- dev

# Run backend and frontend together; Ctrl-C stops both.
dev:
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'trap - INT TERM EXIT; kill 0' INT TERM EXIT
    just dev-backend &
    just dev-frontend &
    wait

# Backend only, on :8080.
dev-backend:
    cargo run -p {{backend_pkg}}

# Frontend only, on :5173 (proxies /api to the backend).
dev-frontend:
    cd frontend && npm run dev

# ---------------------------------------------------------------- build

# Release build of both halves.
build: build-backend build-frontend

build-backend:
    cargo build --workspace --release

build-frontend:
    cd frontend && npm run build

# ---------------------------------------------------------------- test

test: test-backend test-frontend

# No --all-targets: it silently excludes doctests. CI runs this exact command.
test-backend:
    cargo test --workspace

test-frontend:
    cd frontend && npm test -- --run

# ---------------------------------------------------------------- quality

# Clippy with warnings denied, plus the frontend's eslint.
lint: lint-backend lint-frontend

lint-backend:
    cargo clippy --workspace --all-targets -- -D warnings

lint-frontend:
    cd frontend && npm run typecheck && npm run lint

# Format everything in place.
fmt:
    cargo fmt --all
    cd frontend && npm run format --if-present

# Verify formatting without writing. CI runs this.
fmt-check:
    cargo fmt --all --check
    cd frontend && npm run format:check --if-present

# The pre-PR gate. Everything CI checks, in the order that fails fastest.
check: fmt-check lint test

# ---------------------------------------------------------------- database

# `mode=rwc` creates the database file but NOT its parent directory, so a clean
# clone needs data/ to exist first or sqlx fails with "unable to open database file".
[doc("Apply pending migrations to the dev database.")]
migrate:
    mkdir -p data
    DATABASE_URL="${DATABASE_URL:-{{default_db}}}" sqlx migrate run --source {{migrations}}

# Create a new reversible migration: `just migrate-add create_cards`
migrate-add NAME:
    sqlx migrate add -r --source {{migrations}} {{NAME}}

# NOTE: `--bin seed` does not exist yet — Phase 2 (auth) adds it along with the
# default admin, and Phase 4/18 add tag presets and templates. Until then this
# target fails with "no bin target named seed". Left wired up deliberately so the
# name is settled and the quickstart in README.md does not have to change later.
[doc("Load the seed dataset (default admin, project templates, tag presets).")]
seed:
    mkdir -p data
    DATABASE_URL="${DATABASE_URL:-{{default_db}}}" cargo run -p {{backend_pkg}} --bin seed

# CI builds with SQLX_OFFLINE=true and has no database, so a stale .sqlx/ is a red build.
[doc("Refresh the offline SQLx metadata in .sqlx/ — COMMIT the result.")]
prepare:
    DATABASE_URL="${DATABASE_URL:-{{default_db}}}" cargo sqlx prepare --workspace -- --all-targets

# ---------------------------------------------------------------- codegen

# Deliberately an explicit committed step, not a build-time plugin: codegen inside
# `vite build` would make builds nondeterministic and need a live backend in CI.
# Commit the regenerated schema — that is what turns "a Rust DTO changed and the
# frontend silently broke" into a red build.
#
# REQUIRES A RUNNING BACKEND: the frontend's `gen:api` script reads the schema from
# http://127.0.0.1:8080/api/openapi.json, so start `just dev-backend` first. Note the
# raw document is served *outside* /api/docs (which is the Swagger UI) precisely so the
# UI's `/{*rest}` catch-all cannot shadow it — see api::OPENAPI_JSON_PATH.
[doc("Regenerate the typed frontend client from the backend's OpenAPI schema.")]
gen-api:
    cd frontend && npm run gen:api

# ---------------------------------------------------------------- housekeeping

clean:
    cargo clean
    rm -rf frontend/dist frontend/node_modules/.vite
