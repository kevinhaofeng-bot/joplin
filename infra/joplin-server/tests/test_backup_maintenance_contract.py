#!/usr/bin/env python3

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class BackupMaintenanceContractTest(unittest.TestCase):
    def test_daily_backup_does_not_run_expensive_prune(self):
        backup = (ROOT / "scripts" / "backup.sh").read_text()

        self.assertIn("restic forget --keep-daily 14 --keep-weekly 8 --keep-monthly 12", backup)
        self.assertNotIn("--prune", backup)

    def test_weekly_maintenance_prunes_and_checks_repository(self):
        maintenance = (ROOT / "scripts" / "maintenance.sh").read_text()

        self.assertIn("RESTIC_PASSWORD_FILE", maintenance)
        self.assertIn("require_root_owned_mode_600_file", maintenance)
        self.assertIn("restic prune", maintenance)
        self.assertIn("restic check", maintenance)

    def test_backup_and_maintenance_share_a_cross_process_lock(self):
        backup_unit = (ROOT / "systemd" / "joplin-backup.service").read_text()
        maintenance_unit = (ROOT / "systemd" / "joplin-maintenance.service").read_text()
        timer = (ROOT / "systemd" / "joplin-maintenance.timer").read_text()

        lock = "/srv/joplin-server/.restic-backup.lock"
        self.assertIn(lock, backup_unit)
        self.assertIn(lock, maintenance_unit)
        self.assertIn("/usr/bin/flock --wait 600", backup_unit)
        self.assertIn("/usr/bin/flock --wait 600", maintenance_unit)
        self.assertIn("OnCalendar=Sun *-*-* 05:30:00", timer)
        self.assertIn("RandomizedDelaySec=2h", timer)
        self.assertIn("Persistent=true", timer)


if __name__ == "__main__":
    unittest.main()
