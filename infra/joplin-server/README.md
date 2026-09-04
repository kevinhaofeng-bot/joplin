# Joplin Server private infrastructure

This directory defines a private Joplin Server deployment for VM 101. It is a
local, reviewable artifact set; it does not deploy anything by itself.

## Network boundary

`compose.yaml` publishes no PostgreSQL port. Joplin HTTP binds only to
`192.168.3.3:22300`, so it is reachable only from the VM/PVE path. PVE exposes
the public TLS endpoint on `*:22300`: its dedicated systemd socat service
terminates TLS and forwards plain HTTP only to `192.168.3.3:22300`. Port 22300
is intentionally reused at two different hops; the PVE listener is public TLS
while the VM listener is private HTTP. No Joplin container listens directly on
a public interface.

The PVE service builds `/run/joplin-tls-proxy/server.pem` from the existing
trusted certificate and key. `/etc/joplin-server/tls-proxy.env` is root-only
and contains only the source paths (`TLS_CERT_SOURCE` and `TLS_KEY_SOURCE`),
not certificate contents. Before enabling it, confirm the installed socat
supports `min-version=TLS1.2`; its startup checks nonempty inputs and matching
certificate/key public keys before it creates the mode-0600 runtime PEM. The
certificate-watch path automatically observes the authoritative PVE certificate and key paths,
`/etc/pve/local/pveproxy-ssl.pem` and
`/etc/pve/local/pveproxy-ssl.key`, as well as the proxy source configuration.
It restarts only this unit; the restarted service reruns its nonempty and
public-key-match checks before replacing the runtime PEM.

The socat choice is a single-backend proxy only: it has no HAProxy health
checks or load balancing. This is acceptable for a private single-user service;
systemd restart policy and external HTTPS health checks provide supervision.

## Local configuration checks

Create a real VM `/srv/joplin-server/.env` from `env.example` with a generated
database password and a separate generated `DEFAULT_ADMIN_PASSWORD`. Do not
commit that file. Joplin Server applies `DEFAULT_ADMIN_PASSWORD` only during
first initialization of an empty database; set it before the first start so the
upstream `admin` default is never used, and manage an existing admin password
through Joplin rather than expecting a later environment change to replace it.
A local, non-secret validation can use a temporary env file with dummy values:

```bash
bash infra/joplin-server/scripts/verify-config.sh
docker compose --env-file /path/to/dummy.env -f infra/joplin-server/compose.yaml config
```

The production Compose directory is `/srv/joplin-server`; its database volume
is local VM storage. The NAS is never a live-database mount.

## Encrypted NAS backups

`backup.sh` is installed on VM 101 and run by `joplin-backup.timer` as root.
`/etc/joplin-server/backup.env` is root-only and supplies `RESTIC_REPOSITORY`
as an SFTP repository, `RESTIC_PASSWORD_FILE` as a mode-0600 root-only file,
and the location of the root-only Compose env file. The script streams a
custom-format `pg_dump` straight to a restic snapshot tagged `joplin-database`;
it does not write a persistent plaintext database dump. It separately streams
non-secret deployment metadata under `joplin-metadata`, so the restore drill
selects the database snapshot unambiguously and applies retention.
Before enabling the timer, deployment creates
`/srv/joplin-server/.restic-cache` as a root-only directory (for example,
`install -d -o root -g root -m 0700 /srv/joplin-server/.restic-cache`). The
unit sets `RESTIC_CACHE_DIR` to that path, avoiding a cache write under a
systemd-protected home directory.

## Isolated restore drill

`restore-drill.sh` creates a temporary root-only restore directory and uses
only `joplin-server-restore-drill`, `joplin-server-restore-network`,
`joplin-server-restore-postgres`, and `127.0.0.1:22301`. It generates a new
database password in memory, extracts only the explicitly tagged custom-format
database dump, imports it with `pg_restore --exit-on-error --no-owner
--no-privileges`, and bounds PostgreSQL readiness to 120 seconds. It then
checks the loopback health endpoint with its own 120-second deadline and tears down its separate project,
volume, network, temporary configuration, and plaintext restore material
through an EXIT trap. It never references the production database volume.

For the VM compose syntax gate without restic access or container startup, run
`RESTORE_CONFIG_ONLY=1 ./scripts/restore-drill.sh` as root. It creates only
temporary root-only generated files, runs `docker compose config --quiet`, and
exits through the cleanup trap.

## Scope boundary

This artifact set does not change the current WebDAV target, clients, PVE
80/443 mediation gateway, or remote servers. Deployment, TLS publication,
backup initialization, and restore execution are separate reviewed tasks.
