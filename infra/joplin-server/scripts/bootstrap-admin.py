#!/usr/bin/env python3

import argparse
import ipaddress
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import urlparse


ADMIN_EMAIL = "admin@localhost"
UPSTREAM_DEFAULT_PASSWORD = "admin"


def read_env_file(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line_number, raw_line in enumerate(path.read_text().splitlines(), start=1):
        line = raw_line.rstrip("\r")
        if not line or line.lstrip().startswith("#"):
            continue
        if "=" not in line:
            raise RuntimeError(f"invalid env entry at line {line_number}")
        key, value = line.split("=", 1)
        if key in values:
            raise RuntimeError(f"duplicate env key: {key}")
        values[key] = value
    return values


def request_json(base_url: str, host_header: str, method: str, path: str, body: dict, session_id: str = "") -> tuple[int, dict]:
    payload = json.dumps(body).encode()
    request = urllib.request.Request(f"{base_url.rstrip('/')}{path}", data=payload, method=method)
    request.add_header("Content-Type", "application/json")
    request.add_header("Host", host_header)
    if session_id:
        request.add_header("x-api-auth", session_id)

    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            response_body = response.read()
            return response.status, json.loads(response_body or b"{}")
    except urllib.error.HTTPError as error:
        error.read()
        return error.code, {}


def login(base_url: str, host_header: str, password: str) -> tuple[bool, dict]:
    status, body = request_json(
        base_url,
        host_header,
        "POST",
        "/api/sessions",
        {"email": ADMIN_EMAIL, "password": password},
    )
    if status == 403:
        return False, {}
    if status != 200 or not body.get("id") or not body.get("user_id"):
        raise RuntimeError(f"unexpected administrator login response status: {status}")
    return True, body


def bootstrap(env_file: Path, base_url: str) -> None:
    values = read_env_file(env_file)
    try:
        external_url = values["APP_BASE_URL"]
        target_password = values["JOPLIN_ADMIN_PASSWORD"]
    except KeyError as error:
        raise RuntimeError(f"missing required env key: {error.args[0]}") from None

    host_header = urlparse(external_url).netloc
    if not host_header:
        raise RuntimeError("APP_BASE_URL must be an absolute URL")

    bootstrap_url = urlparse(base_url)
    bootstrap_hostname = bootstrap_url.hostname or ""
    try:
        is_loopback = ipaddress.ip_address(bootstrap_hostname).is_loopback
    except ValueError:
        is_loopback = bootstrap_hostname == "localhost"
    if bootstrap_url.scheme != "http" or not is_loopback:
        raise RuntimeError("bootstrap base URL must use loopback HTTP")

    target_ok, _ = login(base_url, host_header, target_password)
    default_ok, default_session = login(base_url, host_header, UPSTREAM_DEFAULT_PASSWORD)

    if target_ok:
        if default_ok:
            raise RuntimeError("both the target and upstream default password authenticated")
        return

    if not default_ok:
        raise RuntimeError("neither the target nor upstream default password authenticated")

    status, _ = request_json(
        base_url,
        host_header,
        "PATCH",
        f"/api/users/{default_session['user_id']}",
        {"password": target_password},
        default_session["id"],
    )
    if status not in (200, 204):
        raise RuntimeError(f"administrator password update failed with status: {status}")

    default_still_works, _ = login(base_url, host_header, UPSTREAM_DEFAULT_PASSWORD)
    target_now_works, _ = login(base_url, host_header, target_password)
    if default_still_works or not target_now_works:
        raise RuntimeError("administrator password post-update verification failed")


def main() -> int:
    parser = argparse.ArgumentParser(description="Rotate the Joplin Server upstream administrator password")
    parser.add_argument("--env-file", required=True, type=Path)
    parser.add_argument("--base-url", default="http://127.0.0.1:22300")
    args = parser.parse_args()

    try:
        bootstrap(args.env_file, args.base_url)
    except Exception as error:
        print(f"Joplin administrator password bootstrap failed: {error}", file=sys.stderr)
        return 1

    print("Joplin administrator password bootstrap: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
