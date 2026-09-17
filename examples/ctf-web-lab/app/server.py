#!/usr/bin/env python3
"""Deliberately vulnerable web service used by the CTF lab.

The service exposes a single login form, a proof submission form, and a health
endpoint. The login handler builds its SQL statement by string concatenation,
which is the vulnerability the student must exploit to read the seeded admin
secret. The submitted proof is written to the workspace so the platform
collector can snapshot it.
"""

from __future__ import annotations

import os
import sqlite3
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

DB_PATH = os.environ.get("CTF_DB_PATH", "/opt/ctf/data/app.db")
PROOF_PATH = os.environ.get("CTF_PROOF_PATH", "/workspace/student/flag.txt")
PORT = int(os.environ.get("CTF_PORT", "8080"))

PAGE = """<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Flag Vault</title></head>
<body>
<h1>Flag Vault</h1>
<p>Sign in to the operations console. Members receive a signed session note.</p>
<form method="get" action="/login">
  <label>Username <input name="u" autocomplete="off"></label>
  <label>Password <input name="p" autocomplete="off"></label>
  <button type="submit">Sign in</button>
</form>
<hr>
<p>Captured the flag? Submit it for scoring.</p>
<form method="post" action="/submit">
  <label>Flag <input name="flag" autocomplete="off"></label>
  <button type="submit">Submit proof</button>
</form>
</body>
</html>
"""


def lookup(user: str, password: str) -> list[tuple[str, str | None]]:
    connection = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True)
    try:
        cursor = connection.cursor()
        statement = (
            "SELECT username, secret FROM users "
            "WHERE username = '" + user + "' AND password = '" + password + "'"
        )
        return cursor.execute(statement).fetchall()
    finally:
        connection.close()


def write_proof(value: str) -> str:
    directory = os.path.dirname(PROOF_PATH)
    if directory:
        os.makedirs(directory, exist_ok=True)
    normalized = value.strip()
    with open(PROOF_PATH, "w", encoding="utf-8") as handle:
        handle.write(normalized + "\n")
    return normalized


class Handler(BaseHTTPRequestHandler):
    server_version = "FlagVault/1.0"

    def _send(self, status: int, body: str, content_type: str = "text/html; charset=utf-8") -> None:
        encoded = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def do_GET(self) -> None:  # noqa: N802 - stdlib handler name
        parsed = urlparse(self.path)
        params = parse_qs(parsed.query)
        if parsed.path == "/healthz":
            return self._send(200, "ok", "text/plain; charset=utf-8")
        if parsed.path == "/login":
            return self._login(params.get("u", [""])[0], params.get("p", [""])[0])
        if parsed.path in ("/", "/index.html"):
            return self._send(200, PAGE)
        return self._send(404, "not found", "text/plain; charset=utf-8")

    def do_POST(self) -> None:  # noqa: N802 - stdlib handler name
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length).decode("utf-8", "replace") if length else ""
        parsed = urlparse(self.path)
        params = parse_qs(body)
        if parsed.path == "/login":
            return self._login(params.get("u", [""])[0], params.get("p", [""])[0])
        if parsed.path == "/submit":
            return self._submit(params.get("flag", [""])[0])
        return self._send(404, "not found", "text/plain; charset=utf-8")

    def _login(self, user: str, password: str) -> None:
        try:
            rows = lookup(user, password)
        except sqlite3.Error as error:
            return self._send(500, f"query failed: {error}", "text/plain; charset=utf-8")
        if not rows:
            return self._send(401, "no matching account\n", "text/plain; charset=utf-8")
        lines = [f"{name}:{secret or ''}" for name, secret in rows]
        return self._send(200, "\n".join(lines) + "\n", "text/plain; charset=utf-8")

    def _submit(self, flag: str) -> None:
        stored = write_proof(flag)
        if not stored:
            return self._send(400, "empty submission\n", "text/plain; charset=utf-8")
        return self._send(200, f"proof recorded: {stored}\n", "text/plain; charset=utf-8")

    def log_message(self, format: str, *args: object) -> None:
        sys.stderr.write("flag-vault %s\n" % (format % args))


def main() -> int:
    if not os.path.exists(DB_PATH):
        sys.stderr.write(f"flag-vault: database missing at {DB_PATH}\n")
        return 1
    server = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    sys.stderr.write(f"flag-vault: listening on 0.0.0.0:{PORT}\n")
    server.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
