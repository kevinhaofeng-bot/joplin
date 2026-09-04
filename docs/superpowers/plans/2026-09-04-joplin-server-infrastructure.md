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
- `infra/joplin-server/compose.bootstrap.yaml`
- `infra/joplin-server/env.example`
- `infra/joplin-server/scripts/bootstrap-admin.py`
- `infra/joplin-server/scripts/initialize.sh`
- `infra/joplin-server/ssh/joplin-backup.conf`
- `infra/joplin-server/nas/90-joplin-backup.conf`
- `infra/joplin-server/router/joplin-backup-authorized-key-options`
- `infra/joplin-server/router/90-joplin-backup-jump.conf`
- `infra/joplin-server/scripts/backup.sh`
- `infra/joplin-server/scripts/maintenance.sh`
- `infra/joplin-server/scripts/restore-drill.sh`
- `infra/joplin-server/scripts/verify-config.sh`
- `infra/joplin-server/systemd/joplin-backup.service`
- `infra/joplin-server/systemd/joplin-backup.timer`
- `infra/joplin-server/systemd/joplin-maintenance.service`
- `infra/joplin-server/systemd/joplin-maintenance.timer`
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

**Task 1 initial-admin correction (2026-09-04):** Live authentication testing caught a version-boundary error before public TLS was enabled: stable image `server-v3.7.1` predates upstream support for `DEFAULT_ADMIN_PASSWORD`, so it ignored that variable and initialized the empty database with `admin/admin`. The containers were stopped, and the empty state was verified as one initial user and zero items. The corrected design removes the unsupported variable from the container, starts first initialization through a loopback-only Compose override, rotates the password through Joplin's own authenticated API, proves the default fails and the generated password succeeds, and only then recreates the private-LAN binding. **Cost:** first deployment has a short additional bootstrap/recreate phase and depends on the stable v3.7.1 admin API contract; it fails closed if neither the generated password nor the one-time upstream default authenticates.

**Task 1 deployment-placeholder gate (2026-09-04):** `verify-config.sh` accepts optional `DEPLOY_ENV_FILE` only for a root-run validation of a root:root mode-0600 regular non-symlink production `.env`. It reads password assignments without sourcing or printing them and rejects missing/duplicate, empty, `admin`, placeholder, and under-20-character `POSTGRES_PASSWORD` or `JOPLIN_ADMIN_PASSWORD` values. **Cost:** passwords must be generated in a simple literal dotenv-compatible form before deployment; rotation is handled separately by the fail-closed loopback bootstrap.

### Task 2: Deploy the private VM stack

- [x] Confirm ports, free space, Docker health, and existing containers again immediately before mutation.
- [x] Create `/srv/joplin-server` and root-only secrets without displaying them; install the reviewed Compose and scripts atomically; before any first `docker compose up`, run `DEPLOY_ENV_FILE=/srv/joplin-server/.env verify-config.sh` as root and require its placeholder/password gate to pass.
- [x] Pull the pinned images, start PostgreSQL first, wait healthy, then start Joplin Server.
- [x] Run the loopback-only administrator bootstrap, require both credential checks to pass, then recreate Joplin on its private-LAN binding.
- [x] Verify database migrations, container health, restart policy, no host 5432 listener, and HTTP readiness from the VM and PVE only.
- [x] Record non-secret image IDs, container health, and resource footprint.

**Task 2 healthcheck incident (2026-09-04):** The first controlled startup reached the app but its healthcheck sent `GET /api/ping` with loopback origin, which Joplin rejected as `Invalid origin: http://127.0.0.1:22300` (404). The app and database were safely stopped while preserving the volume. The probe now connects to loopback but derives the `Host` header from `new URL(process.env.APP_BASE_URL).host`, matching Joplin's origin validation. **Cost:** `APP_BASE_URL` must remain a syntactically valid URL; a malformed base URL now causes the healthcheck to fail rather than masking a routing configuration error.

**Task 2 live acceptance (2026-09-04):** Before replacement, both containers were stopped and the existing database was confirmed to contain one initial user and zero items. The corrected bootstrap artifacts passed independent review, 11 local tests, 11 VM tests, both production/bootstrap Compose parses, and the VM deployment contract 50 consecutive times. The root-only administrator secret was migrated by key name without printing or changing its value; the bootstrap app then ran only on `127.0.0.1:22300`, rotated through the stable Joplin API, proved `admin` returns 403 and the generated password returns 200, and was removed before the production app was recreated on `192.168.3.3:22300`. Both containers are healthy with `unless-stopped`; PostgreSQL has no host listener; PVE-to-VM `/api/ping` returns 200; public PVE 22300 remains closed; and the old WebDAV endpoint still returns its expected 401 challenge. The empty database remains one user and zero items/resources. Image IDs are app `sha256:20a912e7f3909aa4fe43b901b011a8bc4f3f091b95056738fc2e868338b3f929` and database `sha256:8ba5ca87c6a43b60d370510edff66a43c3c655961b68881590e06605335694cd`; the sampled footprint was about 337 MiB for the app and 26 MiB for PostgreSQL. **Cost:** the live server is intentionally empty and private; no real client has been migrated, and TLS/backup/restore remain later gates.

### Task 3: Publish the isolated trusted-TLS endpoint

- [x] Confirm the installed PVE socat supports `min-version=TLS1.2`; do not alter its currently broken apt state or install a proxy package.
- [x] Install a dedicated systemd socat listener on `22300`, using a root-only runtime PEM assembled from the existing PVE key/certificate and forwarding to `192.168.3.3:22300`.
- [x] Add a certificate path unit that restarts only this proxy after successful configuration validation.
- [x] Verify trusted certificate hostname, TLS 1.2/1.3, HTTP health, external access, and unchanged existing 80/443/8006/8080 listeners.

**Task 3 PEM assembly incident (2026-09-04):** The first systemd start reached the reviewed preflight but socat exited because the PVE certificate file has no trailing newline; direct concatenation joined `END CERTIFICATE` and `BEGIN PRIVATE KEY` on one line. Failure cleanup disabled the new units, moved their files into a root-only failure bundle, and left public 22300 closed. Runtime PEM assembly now inserts an explicit blank-line separator between the existing certificate and key. **Cost:** the runtime PEM contains one harmless extra newline; source certificate/key files remain unchanged.

**Task 3 live acceptance (2026-09-04):** The corrected unit passed its regression contract, independent review, a loopback socat probe, and live systemd validation. `joplin-tls-proxy.service` is enabled and running on PVE `0.0.0.0:22300` with a root:root mode-0600 runtime PEM; `joplin-tls-proxy-cert-watch.path` is enabled and waiting. Rewriting only the non-secret source-path env metadata triggered and completed a proxy restart, proving the watcher path. Mac-to-public-IP HTTPS returned Joplin's healthy ping with hostname verification; TLS 1.2 negotiated `ECDHE-RSA-AES256-GCM-SHA384`, TLS 1.3 negotiated `TLS_AES_256_GCM_SHA384`, and both chains verified for `yun.arielkevin.com`. Existing PVE listener address/program fingerprints on 80, 443, 8006, and 8080 remained unchanged, old WebDAV still returned 401, and both backend containers remained healthy. Sampled proxy memory was under 1 MiB. **Cost:** this is a single-backend TLS relay; it relies on systemd restart and the separate path watcher rather than active upstream load-balancer health checks.

### Task 4: Configure encrypted NAS backup

- [x] Create `/volume1/Backups/joplin-server` as a root-owned mode-0755 chroot whose full `namei -l` parent chain is root-owned and not group/other writable; create only `/repo` as mode-0700 and writable by `joplin-backup`; install both external public-key files as root:root mode-0644 so sshd can read them while the target users cannot replace them; add no daemon.
- [x] Create a dedicated VM-to-NAS SSH key and independent router jump account; prove only SFTP plus local TCP forwarding to `192.168.5.170:22` succeed, while shell/command, other TCP targets, remote TCP forwarding, and both stream-local directions fail.
- [x] Through the VM alias, create, read, and delete an SFTP probe inside `/repo` before repository initialization.
- [x] Initialize a restic repository using a generated root-only password file; run the backup script manually.
- [x] Install and enable the nightly systemd timer with randomized delay and failure-visible journal status.
- [x] Keep daily retention fast by separating weekly prune/check maintenance; serialize both services with one bounded flock under the shared `/srv/joplin-server` write boundary.
- [x] Verify `restic check`, snapshot contents, retention dry-run, repository permissions, and that no plaintext dump remains on VM or NAS.

**Task 4 SSH authentication incident (2026-09-04):** The first end-to-end SFTP probe reached the router but public-key authentication failed. Temporary DEBUG3 logging showed that sshd dropped to the target account before opening the external `AuthorizedKeysFile`, so a root:root mode-0600 public-key file was unreadable. Both external files are now root:root mode-0644: the service accounts can read the public keys but cannot replace them. The temporary debug drop-in was removed and the ordinary log level restored. **Cost:** these public keys are locally readable; no private key or restic password is exposed.

**Task 4 maintenance incident (2026-09-04):** An interactive repository inspection left one lock whose recorded PID no longer existed. The first systemd backup still wrote both snapshots, then failed closed at retention. `restic unlock` removed exactly that stale lock; subsequent runs finished successfully and the final lock count is zero. A daily `forget --prune` also took about 3 minutes 28 seconds over the two-hop SFTP path, while a daily backup without prune took about 40 seconds. **Cost:** unreferenced encrypted packs may remain until the weekly maintenance window; retention selection still runs after every daily backup.

**Ruling:** Keep the fast nightly backup and retention selection separate from a weekly prune plus full repository check, and serialize both with the same bounded flock inside `/srv/joplin-server`. **Cost:** physical space reclamation is delayed by at most one maintenance interval, and the weekly job takes several minutes because restic enumerates its repository over two SSH hops.

**Task 4 live acceptance (2026-09-04):** VM 101 runs restic 0.19.1 from the official SHA-256-verified amd64 binary. The dedicated VM key uses pinned ED25519 host keys and a system SSH include. Router account `joplin-backup-jump` permits only local TCP forwarding to `192.168.5.170:22`; shell/command, another TCP target, remote TCP forwarding, and local/remote stream-local forwarding all failed in live tests. The NAS chroot parent chain is root-owned mode-0755, only `/repo` is `joplin-backup:joplin-backup` mode-0700, and SFTP created, read, and removed probe files. Manual and sandboxed systemd backups succeeded; first lock creation and delayed execution behind an already-held lock were proven. Weekly prune plus `restic check` completed with `no errors were found`. Four retained snapshots cover the oldest and current database/metadata generations, the active lock count is zero, no plaintext dump remains, and both timers are enabled and active.

### Task 5: Prove isolated restore and acceptance

- [x] Restore the newest snapshot to a temporary root-only directory, validate checksums and dump structure, and start a separate PostgreSQL restore project.
- [x] Import the dump, start a separate Joplin Server bound only to `127.0.0.1` on a different port, and verify readiness plus database row counts.
- [x] Tear down the restore project and remove plaintext restore material while preserving logs/evidence without note content.
- [x] Re-run config checks, production health, TLS, backup repository check, active timer state, listener boundary, and old WebDAV health.
- [x] Append exact non-secret evidence to this plan and `infra/joplin-server/README.md`; commit as `docs: verify Joplin Server deployment`.

**Task 5 restore incident (2026-09-04):** The first isolated import failed with `did not find magic string in file header`, even though both the snapshot and a freshly generated dump began with `PGDMP`. A controlled, non-mutating comparison proved `pg_restore` succeeds when the custom archive is supplied on standard input without a filename but fails when `/dev/stdin` is passed as a filename in the container. The script now omits that filename and a regression test enforces the contract. **Cost:** restore still materializes one root-only temporary custom archive because `pg_restore` needs a repeatable input for the isolated drill; the EXIT trap removes it.

**Task 5 live acceptance (2026-09-04):** The corrected drill restored the newest tagged snapshot into the separate `joplin-server-restore-drill` project, volume, network, and `127.0.0.1:22301` endpoint. The isolated app became healthy and reported `users=1`, `items=0`, `item_resources=0`, and `files=1`, exactly matching production. The app, database, volume, network, generated env/Compose files, and plaintext archive were then removed. Final checks passed the deployment contract, found both production containers healthy, exposed only private VM `192.168.3.3:22300` with no 5432 or 22301 listener, verified public TLS 1.2 and 1.3 for `yun.arielkevin.com`, found both backup timers active with successful last service results, and confirmed the unchanged WebDAV endpoint still returns 401.

## Migration Gate (Not Part of This Plan)

Only after this plan passes: create the non-admin sync user, run a full export/import and attachment count comparison, connect one disposable client profile, observe at least one backup/restore cycle, then move real clients one at a time. WebDAV becomes read-only only after all clients and the Data API workflow are verified against Joplin Server.
