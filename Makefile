# Atlas — Makefile fallback.
#
# The justfile is the primary task runner; this mirrors its targets for anyone
# who does not want to install `just`. Keep the two in sync when adding targets.
#
#   make dev        backend + frontend, together
#   make check      the pre-PR gate: fmt-check + lint + test
#   make migrate-add NAME=create_cards
#
# ONE DIFFERENCE FROM THE JUSTFILE, ON PURPOSE: `just` loads .env automatically
# (`set dotenv-load`); make does not, and is not made to. `include .env` looks
# like the fix but silently misreads the file — make treats `#` as a comment
# mid-value and keeps quotes literally, so FOO="bar" becomes the 5-character
# string "bar" rather than bar. A fallback that reads config subtly differently
# from the primary runner is worse than one that plainly does not read it.
#
# So: with make, export the variables yourself, or pass them per-invocation:
#   set -a && . ./.env && set +a && make migrate
#   DATABASE_URL=sqlite://./data/atlas.db?mode=rwc make migrate

SHELL := bash
.SHELLFLAGS := -euo pipefail -c
.ONESHELL:

BACKEND_PKG := atlas
MIGRATIONS  := backend/migrations
DEFAULT_DB  := sqlite://./data/atlas.db?mode=rwc
DATABASE_URL ?= $(DEFAULT_DB)
export DATABASE_URL

.DEFAULT_GOAL := help

.PHONY: help dev dev-backend dev-frontend build build-backend build-frontend \
        test test-backend test-frontend lint lint-backend lint-frontend \
        fmt fmt-check check migrate migrate-add seed prepare gen-api clean

help:
	@grep -E '^[a-z-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

# ---------------------------------------------------------------- dev

dev: ## Run backend and frontend together; Ctrl-C stops both
	trap 'trap - INT TERM EXIT; kill 0' INT TERM EXIT
	$(MAKE) dev-backend &
	$(MAKE) dev-frontend &
	wait

dev-backend: ## Backend only, on :8080
	cargo run -p $(BACKEND_PKG)

dev-frontend: ## Frontend only, on :5173
	cd frontend && npm run dev

# ---------------------------------------------------------------- build

build: build-backend build-frontend ## Release build of both halves

build-backend:
	cargo build --workspace --release

build-frontend:
	cd frontend && npm run build

# ---------------------------------------------------------------- test

test: test-backend test-frontend ## Run every test suite

# No --all-targets: it silently excludes doctests. CI runs this exact command.
test-backend:
	cargo test --workspace

test-frontend:
	cd frontend && npm test -- --run

# ---------------------------------------------------------------- quality

lint: lint-backend lint-frontend ## Clippy (warnings denied) + tsc + eslint

lint-backend:
	cargo clippy --workspace --all-targets -- -D warnings

lint-frontend:
	cd frontend && npm run typecheck && npm run lint

fmt: ## Format everything in place
	cargo fmt --all
	cd frontend && npm run format --if-present

fmt-check: ## Verify formatting without writing
	cargo fmt --all --check
	cd frontend && npm run format:check --if-present

check: fmt-check lint test ## The pre-PR gate

# ---------------------------------------------------------------- database

migrate: ## Apply pending migrations
	mkdir -p data
	sqlx migrate run --source $(MIGRATIONS)

migrate-add: ## Create a reversible migration: make migrate-add NAME=create_cards
	@if [ -z "$(NAME)" ]; then \
		echo "NAME is required: make migrate-add NAME=create_cards" >&2; exit 1; \
	fi
	sqlx migrate add -r --source $(MIGRATIONS) $(NAME)

seed: ## Load the seed dataset (NOTE: --bin seed lands in Phase 2)
	mkdir -p data
	cargo run -p $(BACKEND_PKG) --bin seed

prepare: ## Refresh offline SQLx metadata in .sqlx/ — COMMIT the result
	cargo sqlx prepare --workspace -- --all-targets

# ---------------------------------------------------------------- codegen

gen-api: ## Regenerate the typed frontend client (needs `make dev-backend` running)
	cd frontend && npm run gen:api

# ---------------------------------------------------------------- housekeeping

clean:
	cargo clean
	rm -rf frontend/dist frontend/node_modules/.vite
