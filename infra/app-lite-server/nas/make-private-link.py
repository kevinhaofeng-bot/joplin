#!/usr/bin/env python3
"""Provision a separate notes-only FRP STCP pair, without editing existing FRP.

Usage: make-private-link.py <existing authorized Mac frpc.toml> <NEW private dir>
No credentials are printed. Copy only nas-frpc.json to the NAS secret mount.
The visitor endpoint is https://localhost:18787, with the NAS certificate pinned.
"""
import json
import os
from pathlib import Path
import secrets
import sys
import tomllib

source = Path(sys.argv[1])
destination = Path(sys.argv[2])
configuration = tomllib.loads(source.read_text())
if (configuration.get("serverAddr"), configuration.get("serverPort")) != ("126.77.164.34", 7749):
    raise SystemExit("existing FRP server does not match the authorized server")
token = configuration.get("auth", {}).get("token")
if not isinstance(token, str) or not token:
    raise SystemExit("existing token authentication is required")
# mkdir, not exist_ok: never overwrite a deployed key/config/symlink.
destination.mkdir(mode=0o700)
key = secrets.token_hex(32)
base = {
    "serverAddr": "126.77.164.34", "serverPort": 7749,
    "auth": {"method": "token", "token": token},
    "transport": {"tls": {"enable": True}},
    "log": {"to": "console", "level": "warn"},
}
nas = dict(base, proxies=[{
    "name": "joplin-lite-nas-sync", "type": "stcp", "secretKey": key,
    "localIP": "127.0.0.1", "localPort": 8787,
    "transport": {"useEncryption": True, "useCompression": False},
}])
visitor = dict(base, visitors=[{
    "name": "joplin-lite-sync-visitor", "type": "stcp",
    "serverName": "joplin-lite-nas-sync", "secretKey": key,
    "bindAddr": "127.0.0.1", "bindPort": 18787,
    "transport": {"useEncryption": True, "useCompression": False},
}])
for name, value in [("nas-frpc.json", nas), ("visitor.json", visitor)]:
    fd = os.open(destination / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as output:
        json.dump(value, output, ensure_ascii=True, indent=2)
        output.write("\n")
print("Separate private-link configurations created; no existing FRP file changed")
