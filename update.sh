#!/usr/bin/env bash
# Host-side self-update script for Atlas.
#
# Triggered by the atlas-update.path systemd unit when the backend writes
# data/update-requested.  Pulls the latest commit, rebuilds the Docker image,
# and restarts the container — then removes the trigger file so the path unit
# does not fire again immediately.
#
# Runs as the 'alli' user (set in the .service unit).  Docker must be
# accessible to that user (i.e. they are in the docker group).

set -euo pipefail

APP_DIR="$(cd "$(dirname "$0")" && pwd)"
TRIGGER="$APP_DIR/data/update-requested"

cd "$APP_DIR"

echo "[atlas-update] $(date -Is) — update triggered"

git pull --ff-only

docker compose build --no-cache

docker compose up -d

rm -f "$TRIGGER"

echo "[atlas-update] $(date -Is) — done"
