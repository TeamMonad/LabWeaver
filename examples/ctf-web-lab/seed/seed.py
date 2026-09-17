#!/usr/bin/env python3
"""Seed the deterministic Flag Vault database."""

from __future__ import annotations

import os
import sqlite3

DB_PATH = os.environ.get("CTF_DB_PATH", "/opt/ctf/data/app.db")
FLAG = "FLAG{ctf_web_sqli_admin_secret_v1}"


def main() -> int:
    os.makedirs(os.path.dirname(DB_PATH), exist_ok=True)
    if os.path.exists(DB_PATH):
        os.remove(DB_PATH)
    connection = sqlite3.connect(DB_PATH)
    connection.execute(
        "CREATE TABLE users ("
        "id INTEGER PRIMARY KEY, "
        "username TEXT NOT NULL UNIQUE, "
        "password TEXT NOT NULL, "
        "secret TEXT)"
    )
    connection.execute(
        "INSERT INTO users (username, password, secret) VALUES (?, ?, ?)",
        ("guest", "guest-password", None),
    )
    connection.execute(
        "INSERT INTO users (username, password, secret) VALUES (?, ?, ?)",
        ("admin", "b7f4c1e9d2a84f6c0e3b5d7a9c1f2e40", FLAG),
    )
    connection.commit()
    connection.close()
    print(f"seeded {DB_PATH}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
