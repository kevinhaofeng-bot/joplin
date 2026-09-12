# Task 7 Stage B2c final-HEAD Release smoke — 2026-09-13

Scope: local, disposable-profile visual/interaction check of B2c after independent code re-review. It is not an IME, populated-library search, performance, or M2 acceptance.

- HEAD: `a2559a14c`. `cargo build --release --quiet` in `packages/app-lite-gpui` exited 0. SHA-256 of `/Users/kevinhao/Projects/joplin/.shared-target/release/velotype`: `6e21821b75f8df5e00076ed1061f15a3c54e195030526051e4739fac1285154a`.
- Launched that executable with `JOPLIN_LITE_PROFILE=/tmp/joplin-lite-b2c-final.3kXVVp`. No existing `velotype` process was running before launch. The empty profile showed the three-pane library and first-note call-to-action; screenshot: `/tmp/joplin-lite-b2c-final.3kXVVp/initial.png`.
- Real macOS Cmd-K opened a focused, dim-backdrop Search palette; screenshot: `/tmp/joplin-lite-b2c-final.3kXVVp/search.png`. Escape closed it and Cmd-N created one note, visible in the list; screenshot: `/tmp/joplin-lite-b2c-final.3kXVVp/new-note.png`.
- Cmd-Q exited the app process with code 0. `sqlite3 /tmp/joplin-lite-b2c-final.3kXVVp/library.sqlite 'PRAGMA integrity_check; SELECT count(*) FROM notes;'` returned `ok` and `1`.

The disposable profile remains under `/tmp`; no personal profile, Evernote/Joplin data, server, or NAS was opened or mutated. This smoke does not establish typed-content persistence, Chinese IME composition, populated SearchRoute results/selection, image/editing interaction, or a 1,662-note latency/RSS budget. Mounted tests and the independent code review provide those narrower code-path checks separately. No new release artifact was deployed or pushed.
