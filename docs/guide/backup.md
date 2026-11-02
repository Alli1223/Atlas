# Backups and restore

## What a backup is

A backup is two files in `$ATLAS_DATA_DIR/backups/`:

- `atlas-YYYYMMDDTHHMMSSZ.db` — the database, written with SQLite's `VACUUM INTO`: one
  consistent, compacted copy taken from a single read transaction while Atlas keeps running.
  (Copying `atlas.db` by hand while Atlas runs is **not** a backup: in WAL mode the main file
  alone can be stale or torn.)
- `atlas-YYYYMMDDTHHMMSSZ-uploads.tar.gz` — every attachment, generated image and avatar.

The master key is never in a backup. Keep it somewhere else.

## When

- **Automatically**, whenever the newest backup is older than `ATLAS_BACKUP_INTERVAL_HOURS`
  (default 24; `0` turns the schedule off). The newest `ATLAS_BACKUP_KEEP` (default 7) are kept.
- **On demand**: *Admin settings → Backups → Back up now*, `POST /api/v1/admin/backups`, or
  `atlas backup` on the server (safe while the server runs).

Copy them off the machine: the admin page and `GET /api/v1/admin/backups/{name}/{database|uploads}`
download either part.

## Restore

Restore is deliberately offline — swapping the database under a running server would hand its
open connections a different file.

```sh
docker compose stop atlas          # or stop the service
atlas backups                      # list them
atlas restore atlas-20261101T020000Z
docker compose start atlas
```

`restore` refuses while anything still holds the database open. It replaces the database
(removing any stale `-wal`/`-shm`), moves the current `uploads/` aside to
`uploads.before-<name>/`, and unpacks the archive. Delete the set-aside folder once you are happy.

## One project instead of the whole instance

*Project → Import / export → Download the project* writes a JSON bundle (`atlas.project/1`):
configuration, workflows with their gates, tags, components, milestones, field layout,
automation rules, card templates, and every card with its comments and links. *Create a
project from a bundle* restores it under a new key, on this instance or another.
