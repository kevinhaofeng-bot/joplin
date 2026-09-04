#!/usr/bin/env python3

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class RestoreDrillContractTest(unittest.TestCase):
    def test_custom_archive_is_read_from_stdin_without_dev_stdin_filename(self):
        script = (ROOT / "scripts" / "restore-drill.sh").read_text()

        self.assertNotIn("/dev/stdin", script)
        self.assertIn("--dbname=joplin_restore < \"$RESTORE_ROOT/database.dump\"", script)
        self.assertIn("restore_table_counts_begin", script)
        self.assertIn("SELECT 'users=' || count(*) FROM users", script)
        self.assertIn("SELECT 'items=' || count(*) FROM items", script)
        self.assertIn("SELECT 'item_resources=' || count(*) FROM item_resources", script)
        self.assertIn("SELECT 'files=' || count(*) FROM files", script)
        self.assertIn("restore_table_counts_end", script)


if __name__ == "__main__":
    unittest.main()
