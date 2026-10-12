#!/usr/bin/env python3
"""Install the built Atlas binary and configure a service for the current host user."""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil


def read_env(path):
    values = {}
    if path.exists():
        for line in path.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            if key.startswith("ATLAS_"):
                parsed = shlex.split(value, comments=True)
                values[key] = " ".join(parsed)
    return values


def main():
    repo = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=repo / "target/debug/atlas")
    parser.add_argument("--origin", default="https://atlas.alastairrayner.co.uk")
    args = parser.parse_args()
    if not args.binary.is_file() or not (repo / "frontend/dist/index.html").is_file():
        parser.error("Build the backend binary and frontend before configuring the service.")
    values = read_env(repo / ".env")
    if not values.get("ATLAS_MASTER_KEY"):
        parser.error("Set ATLAS_MASTER_KEY in .env first; keep the existing key when upgrading.")
    runtime = Path.home() / ".local/share/atlas"
    (runtime / "bin").mkdir(parents=True, exist_ok=True)
    temporary = runtime / "bin/atlas.new"
    shutil.copy2(args.binary, temporary)
    temporary.chmod(0o755)
    temporary.replace(runtime / "bin/atlas")
    values.update({
        "ATLAS_DATABASE_URL": f"sqlite://{repo}/data/atlas.db?mode=rwc",
        "ATLAS_DATA_DIR": str(repo / "data"),
        "ATLAS_WORKSPACE_DIR": str(repo / "workspaces"),
        "ATLAS_STATIC_DIR": str(repo / "frontend/dist"),
        "ATLAS_BIND_ADDR": "127.0.0.1:8080",
        "ATLAS_ENV": "prod",
        "ATLAS_CORS_ALLOWED_ORIGINS": args.origin,
        "ATLAS_LOCAL_REPOSITORY": str(repo),
        "ATLAS_LOCAL_PROJECT_KEY": "ATLAS",
    })
    env_file = runtime / "atlas.env"
    # Open with restrictive permissions before writing any key material.
    descriptor = os.open(env_file, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.fchmod(descriptor, 0o600)
    with os.fdopen(descriptor, "w") as handle:
        for key, value in sorted(values.items()):
            handle.write(f"{key}={json.dumps(value)}\n")
    tools = runtime / "tools"
    executable_path = f"{tools}/rust/bin:{tools}/node/bin:{Path.home()}/.local/bin:/usr/local/bin:/usr/bin:/bin"
    service = Path.home() / ".config/systemd/user/atlas.service"
    service.parent.mkdir(parents=True, exist_ok=True)
    service.write_text(f"""[Unit]
Description=Atlas project tracker and host coding agents
After=network-online.target

[Service]
Type=simple
WorkingDirectory={str(repo).replace('%', '%%')}
EnvironmentFile={str(env_file).replace('%', '%%')}
Environment="PATH={executable_path}"
Environment=RUST_TEST_THREADS=8
Environment=CARGO_BUILD_JOBS=2
Environment=CARGO_INCREMENTAL=0
Environment=CARGO_PROFILE_DEV_DEBUG=0
Environment=CARGO_PROFILE_TEST_DEBUG=0
ExecStart={json.dumps(str(runtime / 'bin/atlas'))}
Restart=on-failure
RestartSec=3
KillMode=control-group
TimeoutStopSec=15
UMask=0077

[Install]
WantedBy=default.target
""")
    print(f"Installed binary and configured {service}")
    print("Start after stopping the old instance: systemctl --user daemon-reload && systemctl --user enable --now atlas")


if __name__ == "__main__":
    main()
