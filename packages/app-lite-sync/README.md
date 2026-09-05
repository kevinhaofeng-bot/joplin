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

The command builds the three ignored upstream outputs needed by this sidecar
(`@joplin/fork-htmlparser2`, `@joplin/utils`, and `@joplin/lib`), then runs the
sidecar tests and TypeScript check. The build adds a small compilation cost,
but is required because `corepack yarn install --mode=skip-build` installs
dependencies without generating those package outputs. No network access or
real profile data is needed; generated JavaScript remains ignored and is never
committed.

The ordinary package checks remain available as `yarn test` and `yarn tsc` when
the upstream outputs already exist.
