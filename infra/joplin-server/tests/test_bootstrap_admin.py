#!/usr/bin/env python3

import json
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "bootstrap-admin.py"
TARGET_PASSWORD = "correct-horse-battery-staple-2026"


class FakeJoplinHandler(BaseHTTPRequestHandler):
    password = "admin"
    expected_host = "notes.example.test:22300"
    events = []

    def log_message(self, _format, *_args):
        return

    def send_json(self, status, value):
        payload = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def read_json(self):
        length = int(self.headers.get("Content-Length", "0"))
        return json.loads(self.rfile.read(length))

    def do_POST(self):
        if self.path != "/api/sessions" or self.headers.get("Host") != self.expected_host:
            self.send_json(400, {"error": "bad request"})
            return
        body = self.read_json()
        supplied = body.get("password", "")
        self.events.append(("login", supplied))
        if body.get("email") != "admin@localhost" or supplied != type(self).password:
            self.send_json(403, {"error": "invalid credentials"})
            return
        self.send_json(200, {"id": "bootstrap-session", "user_id": "admin-user"})

    def do_PATCH(self):
        if (
            self.path != "/api/users/admin-user"
            or self.headers.get("Host") != self.expected_host
            or self.headers.get("x-api-auth") != "bootstrap-session"
        ):
            self.send_json(403, {"error": "forbidden"})
            return
        body = self.read_json()
        self.events.append(("patch", body.get("password", "")))
        type(self).password = body["password"]
        self.send_json(200, {})


class BootstrapAdminTest(unittest.TestCase):
    def setUp(self):
        FakeJoplinHandler.password = "admin"
        FakeJoplinHandler.events = []
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), FakeJoplinHandler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.temp_dir = tempfile.TemporaryDirectory()
        self.env_file = Path(self.temp_dir.name) / ".env"
        self.env_file.write_text(
            "APP_BASE_URL=https://notes.example.test:22300\n"
            f"JOPLIN_ADMIN_PASSWORD={TARGET_PASSWORD}\n"
        )

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        self.temp_dir.cleanup()

    def run_script(self, base_url=None):
        return subprocess.run(
            [
                "python3",
                str(SCRIPT),
                "--env-file",
                str(self.env_file),
                "--base-url",
                base_url or f"http://127.0.0.1:{self.server.server_port}",
            ],
            capture_output=True,
            text=True,
            timeout=10,
        )

    def test_rotates_upstream_default_and_verifies_both_credentials(self):
        result = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "Joplin administrator password bootstrap: PASS\n")
        self.assertNotIn(TARGET_PASSWORD, result.stdout + result.stderr)
        self.assertEqual(FakeJoplinHandler.password, TARGET_PASSWORD)
        self.assertEqual(
            FakeJoplinHandler.events,
            [
                ("login", TARGET_PASSWORD),
                ("login", "admin"),
                ("patch", TARGET_PASSWORD),
                ("login", "admin"),
                ("login", TARGET_PASSWORD),
            ],
        )

    def test_is_idempotent_when_target_password_is_already_active(self):
        FakeJoplinHandler.password = TARGET_PASSWORD

        result = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(FakeJoplinHandler.password, TARGET_PASSWORD)
        self.assertFalse(any(event[0] == "patch" for event in FakeJoplinHandler.events))
        self.assertNotIn(TARGET_PASSWORD, result.stdout + result.stderr)

    def test_fails_closed_when_neither_expected_password_authenticates(self):
        FakeJoplinHandler.password = "unknown-existing-password"

        result = self.run_script()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("neither the target nor upstream default password authenticated", result.stderr)
        self.assertNotIn(TARGET_PASSWORD, result.stdout + result.stderr)
        self.assertFalse(any(event[0] == "patch" for event in FakeJoplinHandler.events))

    def test_refuses_to_send_bootstrap_credentials_to_a_non_loopback_url(self):
        result = self.run_script("http://notes.example.test:22300")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("base URL must use loopback", result.stderr)
        self.assertNotIn(TARGET_PASSWORD, result.stdout + result.stderr)
        self.assertEqual(FakeJoplinHandler.events, [])


if __name__ == "__main__":
    unittest.main()
