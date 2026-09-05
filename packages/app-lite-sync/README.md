# @joplin/app-lite-sync

This package provides the isolated NDJSON sidecar used by the Lite client
compatibility tests. It does not open a profile, access user data, or start
from the application.

## Clean bootstrap verification

After a fresh dependency install that deliberately skips package build hooks,
run this one command from the repository root:

```sh
corepack yarn workspace @joplin/app-lite-sync verify:clean
```

The command first probes the sidecar-local `sqlite3@5.1.6` binding and, only if
the `:memory:`/`SELECT 1` probe fails, runs that package's local
`@mapbox/node-pre-gyp install --fallback-to-build`. It then builds the three ignored upstream outputs needed by this sidecar
(`@joplin/fork-htmlparser2`, `@joplin/utils`, and `@joplin/lib`), then runs the
sidecar tests and TypeScript check. The build adds a small compilation cost,
but is required because `corepack yarn install --mode=skip-build` installs
dependencies without generating those package outputs. No network access or
real profile data is needed; generated JavaScript remains ignored and is never
committed.

The native prebuild channel may be used during dependency preparation. If it is
unavailable, set `npm_config_nodedir` to matching local Node headers and use the
local compiler for the offline source fallback; no machine-specific compiler
path is assumed. Profile-supervised launches receive the Rust-held sibling lease
through inherited FD 198 (`JOPLIN_LITE_PROFILE_LEASE_FD=198`). Ordinary
codec-only sidecars do not require or create a lease.

The ordinary package checks remain available as `yarn test` and `yarn tsc` when
the upstream outputs already exist.
