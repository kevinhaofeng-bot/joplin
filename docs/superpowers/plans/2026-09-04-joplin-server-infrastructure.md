# Joplin Server Personal Infrastructure Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deploy a pinned, private-by-default Joplin Server and PostgreSQL stack on Japan VM 101, publish it through a separate trusted-TLS endpoint, back it up encrypted to the UGREEN NAS, and prove an isolated restore without changing the current WebDAV sync target.

**Architecture:** Joplin Server `3.7.1` and PostgreSQL 16 run in one Docker Compose project on VM 101. PostgreSQL has no host port and Joplin's plain HTTP port is reachable only from the VM/PVE path. PVE terminates TLS on the unused public port `22300` using a dedicated systemd-managed socat proxy: it combines the existing trusted certificate and key into a root-only runtime PEM, listens with `OPENSSL-LISTEN:22300`, and forwards to the VM at `192.168.3.3:22300`. Nightly `pg_dump` plus server metadata are streamed into a password-protected restic repository on the RAID10 NAS over SSH. Restore tests use a separate Compose project, database volume, network, and loopback-only port. The existing PVE WebDAV service and all current clients remain unchanged until a later migration gate.

**Pinned images:** `joplin/server:3.7.1` (`linux/amd64` digest `sha256:b9666df06e7e2db20aeb961d2aca19e20664b985ead96995ecd32f9d720f002c`) and `postgres:16.10-bookworm` (`linux/amd64` digest `sha256:94f23d40fdaf5e60cb2fd8a98c22f02a7b8724949f310d95a0ddf075e8c8b208`).

**Current authority:** `/Users/kevinhao/servers.md`, verified 2026-09-04. VM 101 has Docker 27.4.1, Compose 2.32.1, 6.4 GiB available RAM, 32 GiB filesystem free, and 99 GiB unallocated in its volume group. The NAS has about 14 TiB free and restic 0.14.0. PVE ports 80/443 are an existing frozen mediation gateway; port 22300 was verified end-to-end unused and reachable from the Mac using a temporary listener.

**Ruling:** Use the already-installed PVE socat as a dedicated systemd TLS proxy instead of HAProxy because HAProxy/nginx/stunnel are absent and PVE apt is currently unusable (`flAbsPath on /var/lib/dpkg/status failed - realpath (22)`). The proxy combines the existing certificate and key into a root-only runtime PEM, listens on `22300`, and forwards to VM 101. **Cost:** socat has no HAProxy health checks or load balancing; this is acceptable for one private user because systemd restart policy and external health checks provide the required single-backend supervision. Do not repair apt or alter PVE during this plan.

## Global Constraints

- Do not stop, modify, or replace PVE `wsgidav.service`; do not change any Joplin client sync target in this phase.
- Do not disturb the existing FRP 80/443 mediation gateway, PVE panel, VM containers, Apache, MySQL, Stego, Signal, or NAS file services.
- Never commit or print passwords, tokens, database dumps, note data, TLS private keys, restic credentials, or generated `.env` files.
- PostgreSQL must not publish port 5432. The Joplin application must not publish an unencrypted public port.
- Use `/srv/joplin-server` on VM local ext4 storage. NAS stores only encrypted backup repository data, never the live database.
- Every configuration replacement is staged, syntax-checked, backed up when replacing an existing file, and applied atomically.
- The restore drill must use separate names, volumes, network, credentials, and loopback-only ports. It must never attach to or overwrite production volumes.
- Leave the deployed service running only after local health, TLS, backup, and restore checks all pass.

## Files to Add

- `infra/joplin-server/compose.yaml`
- `infra/joplin-server/env.example`
- `infra/joplin-server/scripts/backup.sh`
- `infra/joplin-server/scripts/restore-drill.sh`
- `infra/joplin-server/scripts/verify-config.sh`
- `infra/joplin-server/systemd/joplin-backup.service`
- `infra/joplin-server/systemd/joplin-backup.timer`
- `infra/joplin-server/systemd/joplin-tls-proxy.service`
- `infra/joplin-server/systemd/joplin-tls-proxy-cert-watch.path`
- `infra/joplin-server/systemd/joplin-tls-proxy-cert-watch.service`
- `infra/joplin-server/README.md`

---

### Task 1: Create and test infrastructure artifacts

- [x] Write failing static contract checks for pinned images, internal-only PostgreSQL, non-public application binding, health checks, secret-free examples, restic stdin handling, and isolated restore names.
- [x] Add the Compose, socat TLS proxy, backup, restore-drill, verification, systemd, and documentation files.
- [x] Run `bash infra/joplin-server/scripts/verify-config.sh` and `docker compose --env-file` validation with generated dummy secrets.
- [x] Run ShellCheck when available, `git diff --check`, and a repository secret-pattern scan limited to the new files.
- [x] Commit as `feat: define Joplin Server infrastructure`.

**Task 1 local verification note (2026-09-04):** `verify-config.sh` was written first and failed against the missing `compose.yaml`, then passed after the artifacts were added; its expanded contract also caught and drove fixes for Joplin egress, tagged custom-format database snapshots, restore-volume key isolation, and bounded restore readiness. Bash syntax checks, Ruby YAML boundary assertions, a scoped secret-pattern scan, and `git diff --check` passed. This workstation does not have `docker`, `shellcheck`, or `systemd-analyze` installed, so the generated-dummy `docker compose --env-file ... config` check cannot run locally and this third checkbox remains open for a VM 101 temporary-directory gate. No local tool installation or remote connection was attempted.

**Task 1 review hardening (2026-09-04):** New static contracts were first run red for the fixed private application bind, then passed after the recovery artifacts exported every generated restore boundary into their temporary env and ran `docker compose config --quiet` before any startup. `RESTORE_CONFIG_ONLY=1` is a VM-safe config gate: it needs no restic access and starts no containers. Backup and restore now reject sourced/read sensitive files unless they are regular, non-symlink, root-owned mode-0600 files. The backup unit writes its restic cache only under root-only `/srv/joplin-server/.restic-cache`; the certificate path unit watches both authoritative PVE certificate sources and the proxy env. **Cost:** config-only still needs the VM's Docker Compose CLI, and the real restore/health verification remains a later isolated VM gate; no remote connection was attempted here.

**Task 1 VM configuration gate (2026-09-04):** VM 101 ran the gate in an automatically cleaned `/tmp/joplin-config.*` directory: `verify-config.sh` passed; Docker Compose 2.32.1 production `config --quiet` passed with generated dummy values; and `sudo env RESTORE_CONFIG_ONLY=1 restore-drill.sh` passed without accessing restic or starting containers. The only output was tar's macOS provenance xattr-ignore notice, which did not affect the configuration gate.

**Task 1 initial-admin hardening (2026-09-04):** The local Joplin Server source confirms `DEFAULT_ADMIN_PASSWORD` is supported and applies the configured default only on first startup. Compose now requires a generated `DEFAULT_ADMIN_PASSWORD` before startup and the example has only a deployment placeholder, preventing use of the upstream `admin` default on a fresh database. **Cost:** changing this environment variable after initialization does not rotate an existing admin password; that remains an authenticated Joplin administrative action.

**Task 1 deployment-placeholder gate (2026-09-04):** `verify-config.sh` accepts optional `DEPLOY_ENV_FILE` only for a root-run validation of a root:root mode-0600 regular non-symlink production `.env`. It reads password assignments without sourcing or printing them and rejects missing/duplicate, empty, `admin`, placeholder, and under-20-character `POSTGRES_PASSWORD` or `DEFAULT_ADMIN_PASSWORD` values. **Cost:** passwords must be generated in a simple literal dotenv-compatible form before deployment; this gate deliberately does not attempt to rotate initialized credentials.

### Task 2: Deploy the private VM stack

- [ ] Confirm ports, free space, Docker health, and existing containers again immediately before mutation.
- [ ] Create `/srv/joplin-server` and root-only secrets without displaying them; install the reviewed Compose and scripts atomically; before any first `docker compose up`, run `DEPLOY_ENV_FILE=/srv/joplin-server/.env verify-config.sh` as root and require its placeholder/password gate to pass.
- [ ] Pull the pinned images, start PostgreSQL first, wait healthy, then start Joplin Server.
- [ ] Verify database migrations, container health, restart policy, no host 5432 listener, and HTTP readiness from the VM and PVE only.
- [ ] Record non-secret image IDs, container health, and resource footprint.

### Task 3: Publish the isolated trusted-TLS endpoint

- [ ] Confirm the installed PVE socat supports `min-version=TLS1.2`; do not alter its currently broken apt state or install a proxy package.
- [ ] Install a dedicated systemd socat listener on `22300`, using a root-only runtime PEM assembled from the existing PVE key/certificate and forwarding to `192.168.3.3:22300`.
- [ ] Add a certificate path unit that restarts only this proxy after successful configuration validation.
- [ ] Verify trusted certificate hostname, TLS 1.2/1.3, HTTP health, external access, and unchanged existing 80/443/8006/8080 listeners.

### Task 4: Configure encrypted NAS backup

- [ ] Create a dedicated NAS directory under `/volume1/Backups/joplin-server` with restrictive ownership and no new daemon.
- [ ] Create a dedicated VM-to-NAS SSH key restricted to the backup target or the narrowest available command boundary.
- [ ] Initialize a restic repository using a generated root-only password file; run the backup script manually.
- [ ] Install and enable the nightly systemd timer with randomized delay and failure-visible journal status.
- [ ] Verify `restic check`, snapshot contents, retention dry-run, repository permissions, and that no plaintext dump remains on VM or NAS.

### Task 5: Prove isolated restore and acceptance

- [ ] Restore the newest snapshot to a temporary root-only directory, validate checksums and dump structure, and start a separate PostgreSQL restore project.
- [ ] Import the dump, start a separate Joplin Server bound only to `127.0.0.1` on a different port, and verify readiness plus database row counts.
- [ ] Tear down the restore project and remove plaintext restore material while preserving logs/evidence without note content.
- [ ] Re-run config checks, production health, TLS, backup repository check, active timer state, listener boundary, and old WebDAV health.
- [ ] Append exact non-secret evidence to this plan and `infra/joplin-server/README.md`; commit as `docs: verify Joplin Server deployment`.

## Migration Gate (Not Part of This Plan)

Only after this plan passes: create the non-admin sync user, run a full export/import and attachment count comparison, connect one disposable client profile, observe at least one backup/restore cycle, then move real clients one at a time. WebDAV becomes read-only only after all clients and the Data API workflow are verified against Joplin Server.
