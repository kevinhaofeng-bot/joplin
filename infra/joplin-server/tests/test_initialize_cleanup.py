#!/usr/bin/env python3

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


SOURCE = Path(__file__).resolve().parents[1] / "scripts" / "initialize.sh"


class InitializeCleanupTest(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        self.scripts = self.root / "scripts"
        self.bin_dir = self.root / "bin"
        self.scripts.mkdir()
        self.bin_dir.mkdir()
        shutil.copy2(SOURCE, self.scripts / "initialize.sh")
        (self.root / "compose.yaml").write_text("services: {}\n")
        (self.root / "compose.bootstrap.yaml").write_text("services: {}\n")
        (self.root / ".env").write_text("test-only\n")
        verify_script = self.scripts / "verify-config.sh"
        verify_script.write_text(
            "#!/usr/bin/env bash\n"
            "if [ \"${FAIL_VERIFY:-0}\" = 1 ]; then exit 1; fi\n"
            "if [ \"${SIGNAL_VERIFY:-0}\" = 1 ]; then kill -TERM \"$PPID\"; /bin/sleep 0.2; fi\n"
            "exit 0\n"
        )
        verify_script.chmod(0o755)
        (self.scripts / "bootstrap-admin.py").write_text("# test double\n")
        self.log_file = self.root / "docker.log"
        self.inspect_count = self.root / "inspect-count"

        self.write_executable(
            "id",
            "#!/usr/bin/env bash\n"
            "if [ \"${1:-}\" = -u ]; then printf '0\\n'; else exec /usr/bin/id \"$@\"; fi\n",
        )
        self.write_executable("sleep", "#!/usr/bin/env bash\nexit 0\n")
        self.write_executable("python3", "#!/usr/bin/env bash\nexit 0\n")
        self.write_executable(
            "docker",
            "#!/usr/bin/env bash\n"
            "printf '%s\\n' \"$*\" >>\"$DOCKER_LOG\"\n"
            "if [ \"${1:-}\" = inspect ]; then\n"
            "  count=0\n"
            "  [ ! -f \"$INSPECT_COUNT\" ] || count=$(cat \"$INSPECT_COUNT\")\n"
            "  count=$((count + 1))\n"
            "  printf '%s\\n' \"$count\" >\"$INSPECT_COUNT\"\n"
            "  if [ \"$count\" -eq \"$FAIL_INSPECT\" ]; then printf 'unhealthy\\n'; else printf 'healthy\\n'; fi\n"
            "fi\n",
        )

    def tearDown(self):
        self.temp_dir.cleanup()

    def write_executable(self, name, content):
        path = self.bin_dir / name
        path.write_text(content)
        path.chmod(0o755)

    def run_failure(self, fail_inspect=99, extra_env=None):
        env = os.environ.copy()
        env.update(
            {
                "PATH": f"{self.bin_dir}:{env['PATH']}",
                "DOCKER_LOG": str(self.log_file),
                "INSPECT_COUNT": str(self.inspect_count),
                "FAIL_INSPECT": str(fail_inspect),
                "JOPLIN_READINESS_ATTEMPTS": "1",
                "JOPLIN_ENV_FILE": str(self.root / ".env"),
            }
        )
        env.update(extra_env or {})
        return subprocess.run(
            ["bash", str(self.scripts / "initialize.sh")],
            capture_output=True,
            text=True,
            timeout=5,
            env=env,
        )

    def test_every_readiness_failure_stops_both_services(self):
        for fail_inspect in range(1, 5):
            with self.subTest(fail_inspect=fail_inspect):
                self.log_file.unlink(missing_ok=True)
                self.inspect_count.unlink(missing_ok=True)

                result = self.run_failure(fail_inspect)

                self.assertNotEqual(result.returncode, 0)
                log_lines = self.log_file.read_text().splitlines()
                self.assertIn("stop app db", log_lines[-1])

    def test_verify_failure_does_not_touch_an_existing_project(self):
        result = self.run_failure(extra_env={"FAIL_VERIFY": "1"})

        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.log_file.exists())

    def test_signal_during_verify_does_not_touch_an_existing_project(self):
        result = self.run_failure(extra_env={"SIGNAL_VERIFY": "1"})

        self.assertEqual(result.returncode, 143)
        self.assertFalse(self.log_file.exists())


if __name__ == "__main__":
    unittest.main()
