#!/usr/bin/env python3

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class BackupSshContractTest(unittest.TestCase):
    def test_vm_client_uses_a_pinned_dedicated_key_and_jump_host(self):
        config = (ROOT / "ssh" / "joplin-backup.conf").read_text()

        self.assertIn("Host joplin-backup-nas", config)
        self.assertIn("HostName 192.168.5.170", config)
        self.assertIn("User joplin-backup", config)
        self.assertIn("ProxyJump joplin-backup-router", config)
        self.assertIn("Host joplin-backup-router", config)
        self.assertIn("HostName 192.168.3.226", config)
        self.assertIn("Port 61622", config)
        self.assertIn("User joplin-backup-jump", config)
        self.assertIn("IdentityFile /etc/joplin-server/ssh/joplin-backup-ed25519", config)
        self.assertIn("UserKnownHostsFile /etc/joplin-server/ssh/known_hosts", config)
        self.assertEqual(config.count("StrictHostKeyChecking yes"), 2)
        self.assertEqual(config.count("BatchMode yes"), 2)
        self.assertEqual(config.count("GlobalKnownHostsFile /dev/null"), 2)

    def test_nas_account_is_chrooted_to_internal_sftp(self):
        config = (ROOT / "nas" / "90-joplin-backup.conf").read_text()

        self.assertIn("Match User joplin-backup", config)
        self.assertIn("ChrootDirectory /volume1/Backups/joplin-server", config)
        self.assertIn("ForceCommand internal-sftp -d /repo", config)
        self.assertIn("AuthorizedKeysFile /etc/ssh/authorized_keys/joplin-backup", config)
        self.assertIn("PasswordAuthentication no", config)
        self.assertIn("KbdInteractiveAuthentication no", config)
        self.assertIn("AllowTcpForwarding no", config)
        self.assertIn("AllowStreamLocalForwarding no", config)
        self.assertIn("X11Forwarding no", config)
        self.assertIn("PermitTTY no", config)
        self.assertNotIn("PermitUserEnvironment", config)

    def test_router_key_allows_only_the_required_nas_forward(self):
        options = (ROOT / "router" / "joplin-backup-authorized-key-options").read_text().strip()
        config = (ROOT / "router" / "90-joplin-backup-jump.conf").read_text()

        self.assertIn("restrict", options)
        self.assertIn("port-forwarding", options)
        self.assertIn('permitopen="192.168.5.170:22"', options)
        self.assertIn('command="/usr/bin/false"', options)
        self.assertIn("Match User joplin-backup-jump", config)
        self.assertIn("AuthorizedKeysFile /etc/ssh/authorized_keys/joplin-backup-jump", config)
        self.assertIn("AuthenticationMethods publickey", config)
        self.assertIn("PasswordAuthentication no", config)
        self.assertIn("KbdInteractiveAuthentication no", config)
        self.assertIn("AllowAgentForwarding no", config)
        self.assertIn("AllowTcpForwarding local", config)
        self.assertIn("AllowStreamLocalForwarding no", config)
        self.assertIn("PermitOpen 192.168.5.170:22", config)
        self.assertIn("X11Forwarding no", config)
        self.assertIn("PermitTTY no", config)
        self.assertIn("PermitTunnel no", config)
        self.assertIn("ForceCommand /usr/bin/false", config)

    def test_documented_chroot_layout_has_safe_ownership_boundaries(self):
        readme = (ROOT / "README.md").read_text()

        self.assertIn("namei -l /volume1/Backups/joplin-server", readme)
        self.assertIn("root:root` mode `0755", readme)
        self.assertIn("joplin-backup:joplin-backup` mode `0700", readme)
        self.assertIn("/etc/ssh/authorized_keys/joplin-backup-jump", readme)
        self.assertIn("/etc/ssh/authorized_keys/joplin-backup", readme)
        self.assertIn("root:root` mode `0644", readme)
        self.assertIn("create, read, and delete a probe file", readme)


if __name__ == "__main__":
    unittest.main()
