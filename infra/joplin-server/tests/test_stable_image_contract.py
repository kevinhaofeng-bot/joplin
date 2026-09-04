#!/usr/bin/env python3

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class StableImageContractTest(unittest.TestCase):
    def test_static_verifier_has_no_pipefail_grep_short_circuit(self):
        verifier = (ROOT / "scripts" / "verify-config.sh").read_text()

        self.assertNotIn("printf '%s\\n' \"$db_block\" | grep -Fq", verifier)
        self.assertNotIn("printf '%s\\n' \"$app_block\" | grep -Fq", verifier)

    def test_server_371_does_not_receive_the_unsupported_admin_env_key(self):
        compose = (ROOT / "compose.yaml").read_text()
        env_example = (ROOT / "env.example").read_text()

        self.assertNotIn("DEFAULT_ADMIN_PASSWORD", compose)
        self.assertNotIn("DEFAULT_ADMIN_PASSWORD", env_example)
        self.assertIn("JOPLIN_ADMIN_PASSWORD=__GENERATE_AT_DEPLOYMENT__", env_example)

    def test_bootstrap_override_exposes_the_default_only_on_loopback(self):
        override = (ROOT / "compose.bootstrap.yaml").read_text()

        self.assertIn("!override", override)
        self.assertIn('"127.0.0.1:22300:22300"', override)
        self.assertNotIn('"192.168.3.3:22300:22300"', override)

    def test_initializer_rotates_before_starting_the_production_binding(self):
        initializer = (ROOT / "scripts" / "initialize.sh").read_text()

        verify_at = initializer.index('DEPLOY_ENV_FILE="$env_file" "$verify_script"')
        cleanup_arm_at = initializer.index("cleanup_required=1", verify_at)
        bootstrap_up_at = initializer.index('"${bootstrap_compose[@]}" up -d db app')
        rotate_at = initializer.index('python3 "$bootstrap_script"')
        production_up_at = initializer.rindex('up -d db app')
        self.assertLess(verify_at, bootstrap_up_at)
        self.assertLess(verify_at, cleanup_arm_at)
        self.assertLess(cleanup_arm_at, bootstrap_up_at)
        self.assertLess(bootstrap_up_at, rotate_at)
        self.assertLess(rotate_at, production_up_at)
        self.assertIn("trap cleanup EXIT", initializer)
        self.assertIn("trap 'exit 130' INT", initializer)
        self.assertIn("trap 'exit 143' TERM", initializer)


if __name__ == "__main__":
    unittest.main()
