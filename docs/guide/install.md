# Installing and running Atlas

Atlas is one server binary and a folder of static files. It keeps everything in a single
SQLite database plus an `uploads/` directory, both under `ATLAS_DATA_DIR`.

## Docker (recommended)

```sh
cp .env.example .env            # edit: at least ATLAS_MASTER_KEY, see below
mkdir -p data workspaces && sudo chown -R 10001:10001 data workspaces
docker compose up -d            # http://localhost:8080
```

`./data` **must** be a mounted volume — it holds the database, uploads and backups.
`./workspaces` holds the agent's repository clones.

## From source

```sh
cd frontend && npm ci && npm run build && cd ..
cargo build --release
ATLAS_STATIC_DIR=frontend/dist ./target/release/atlas
```

Migrations run on every start, before the server accepts a request; there is no separate
migration step in production. `GET /healthz` reports `{"status":"ok","version":…,"db":"ok"}`
and queries both database pools — use it for container health checks and to confirm which
version is running.

## First login

The first start seeds one administrator, **`Admin` / `Admin`**, and forces a password change
before anything else works. Other users are created with `POST /api/v1/users` (see the API
docs at `/docs`; an admin users page is not built yet). Instance roles are admin, member and
viewer, and each project grants its own owner/member/viewer roles on top.

## The master key

API keys and tokens (GitHub, Anthropic, Gemini, SMTP) are encrypted at rest with
`ATLAS_MASTER_KEY` (32 random bytes, base64):

```sh
openssl rand -base64 32
```

Without it the vault is disabled and integrations cannot be configured. **Back the key up
separately from the database** — a backup without its key cannot decrypt a single credential,
which is the point.

## Behind a reverse proxy

Run Atlas on `127.0.0.1:8080` behind Caddy or nginx terminating TLS, and set:

| Variable | Why |
| --- | --- |
| `ATLAS_PUBLIC_URL=https://atlas.example.com` | Links in emails, GitHub webhook installation, and it turns on HSTS |
| `ATLAS_CORS_ALLOWED_ORIGINS=https://atlas.example.com` | The origin check on every write |
| `ATLAS_TRUSTED_PROXIES=127.0.0.1` | Believe `X-Forwarded-For` only from your proxy (`*` = any peer, `none` = never) |

Forward websockets for `/api/v1/ws` (live board updates and notifications). In nginx:
`proxy_set_header Upgrade $http_upgrade; proxy_set_header Connection "upgrade";`.

## Security headers

Every response carries `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
`Referrer-Policy`, `Cross-Origin-Opener-Policy`, `Permissions-Policy` and a
`Content-Security-Policy` that allows only Atlas's own origin (the SPA's inline theme script is
admitted by its hash, computed from the served `index.html` at startup). HSTS is sent only when
`ATLAS_PUBLIC_URL` is `https://`.
