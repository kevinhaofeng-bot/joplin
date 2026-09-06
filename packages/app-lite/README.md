# Joplin Lite 0.10.0

Joplin Lite is a macOS arm64 Tauri/WebKit client with an embedded official
Node 23.11 sidecar and Joplin 3.7 library. It uses an independent
`com.kevinhao.joplin-lite` profile and never opens the official Joplin
profile.

## Current MVP

- Folder, Tag, and Note CRUD with the official Joplin models.
- Default WYSIWYG editing, image paste, file attachments, and controlled
  attachment opening.
- Local note search.
- Joplin Server configuration, manual sync, and non-blocking background sync.
- Additive background JEX import.
- Release `.app` and DMG bundles containing the Node runtime and sidecar.

## Safety boundaries

- Sync passwords live only in the custom Keychain slot; ordinary startup does
  not read Keychain credentials.
- JEX input must be an absolute, regular, non-symlink `.jex` file no larger
  than 8 GiB. Import is additive, creates new IDs, is mutually exclusive with
  sync, and exposes no paths, content, or raw errors.
- Profile access, sidecar requests, attachment paths, and protocol errors are
  validated and fail closed. The official profile, unrelated user data, and
  credentials are outside the application boundary.

## Development and verification

From the repository root:

```bash
corepack yarn install --mode=skip-build
yarn workspace @joplin/app-lite-sync test
yarn workspace @joplin/app-lite-sync tsc
yarn workspace @joplin/app-lite test
yarn workspace @joplin/app-lite tsc
yarn workspace @joplin/app-lite build:web
cargo fmt --manifest-path packages/app-lite/src-tauri/Cargo.toml -- --check
cargo test --manifest-path packages/app-lite/src-tauri/Cargo.toml --locked
cargo clippy --manifest-path packages/app-lite/src-tauri/Cargo.toml --locked --all-targets --all-features -- -D warnings
```

The self-contained release build is opt-in:

```bash
yarn workspace @joplin/app-lite build:native:release
```

It produces the app and DMG under
`packages/app-lite/src-tauri/target/release/bundle/`. Release signing uses a
reproducible ad-hoc identity (`-`) for personal use only; there is no
Developer ID signing or notarization. Verify the app with:

```bash
codesign --verify --deep --strict --verbose=2 \
  "packages/app-lite/src-tauri/target/release/bundle/macos/Joplin Lite.app"
```

## Migration evidence

The isolated migration rehearsal imported a 1.17 GB JEX in about 10 seconds:

- 1,664 notes
- 31 folders
- 64 tags
- 4,153 linked resources

After restart, the imported data remained readable; metadata differences were
zero and total resource bytes matched. The source database contains 4,238
resources in total, while the JEX correctly contains only the 4,153 resources
referenced by exported notes.

## Known limitations

E2EE, OCR, and plugins are not included. The upstream JEX importer does not
yet clean its temporary directory on every failure path, and import is not an
all-or-nothing transaction. Memory usage remains above the 250 MiB target.
Release artifacts are not notarized, and a real full-server first upload has
not yet been performed.
