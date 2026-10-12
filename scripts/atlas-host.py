#!/usr/bin/env python3
"""Run the installed Atlas binary with its service environment, without sourcing a shell file."""
import json
import os
from pathlib import Path
import subprocess
import sys

runtime = Path.home() / ".local/share/atlas"
environment = dict(os.environ)
for line in (runtime / "atlas.env").read_text().splitlines():
    key, value = line.split("=", 1)
    environment[key] = json.loads(value)
raise SystemExit(subprocess.call([str(runtime / "bin/atlas"), *sys.argv[1:]], env=environment))
