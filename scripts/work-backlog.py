#!/usr/bin/env python3
"""Launch Atlas backlog tickets sequentially through the normal authenticated API."""
import argparse
import getpass
import http.cookiejar
import json
import os
import time
import urllib.error
import urllib.request


class Atlas:
    def __init__(self, url):
        self.url = url.rstrip("/")
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def request(self, method, path, body=None):
        request = urllib.request.Request(self.url + "/api/v1" + path, method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json", "Origin": self.url})
        try:
            with self.opener.open(request, timeout=60) as response:
                raw = response.read()
                return json.loads(raw) if raw else None
        except urllib.error.HTTPError as error:
            problem = json.loads(error.read())
            raise RuntimeError(problem.get("detail", f"HTTP {error.code}")) from error


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="https://atlas.alastairrayner.co.uk")
    parser.add_argument("--project", default="ATLAS")
    parser.add_argument("--provider", choices=["codex", "claude", "antigravity"], default="codex")
    parser.add_argument("--limit", type=int, default=1)
    parser.add_argument("--continuous", action="store_true", help="Keep picking tickets until the backlog is empty; stop on failure.")
    args = parser.parse_args()
    if args.limit < 1:
        parser.error("--limit must be positive")
    api = Atlas(args.url)
    api.request("POST", "/auth/login", {
        "username": os.environ.get("ATLAS_USERNAME") or input("Atlas username: "),
        "password": os.environ.get("ATLAS_PASSWORD") or getpass.getpass("Atlas password: "),
    })
    seen = set()
    active = None
    try:
        while args.continuous or len(seen) < args.limit:
            board = api.request("GET", f"/projects/{args.project}/board")
            cards = [card for column in board["columns"] if column["status"]["category"] == "todo"
                for card in column["cards"] if card["key"] not in seen]
            if not cards:
                print("No remaining backlog tickets.")
                return
            card = cards[0]
            active = api.request("POST", f"/cards/{card['key']}/agent-sessions", {"provider": args.provider})
            print(f"Started {card['key']} with {args.provider}: {active['id']}", flush=True)
            deadline = time.monotonic() + 31 * 60
            while active["status"] == "running":
                if time.monotonic() > deadline:
                    api.request("POST", f"/agent-sessions/{active['id']}/cancel")
                    raise RuntimeError("Run exceeded the waiting limit; cancellation requested.")
                time.sleep(3)
                active = api.request("GET", f"/agent-sessions/{active['id']}")
            print(f"{card['key']}: {active['status']}", flush=True)
            if active["status"] != "completed":
                raise RuntimeError(active.get("errorMessage") or "Review this ticket before continuing.")
            seen.add(card["key"])
            active = None
            # The terminal outcome is written before completion actions. Wait for publication.
            time.sleep(5)
    except KeyboardInterrupt:
        if active and active["status"] == "running":
            api.request("POST", f"/agent-sessions/{active['id']}/cancel")
            print("Cancellation requested.")
    finally:
        api.request("POST", "/auth/logout")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, urllib.error.URLError) as error:
        raise SystemExit(str(error))
