# Task 7 D2: ordinary terms search live attachment filenames

Status: bounded code acceptance, 2026-09-13. Implementation commits
`bc8cedbfe`, `a73dcc3d8`, `b05d49690`; this is not Task 7 or M2 acceptance.

The source/decision boundary is in `task-7-stage-d2-filename-terms-brief.md`.
Evernote 11.32.5 `48353__module-48353.js` maintains a distinct attachment
metadata FTS projection with insert/delete/update triggers. Its
`83028__module-83028.js::k` general-term union names *extracted attachment
searchText*, not metadata filename FTS; this native app's plain-word filename
search is an explicit usability enhancement, not a recovered Evernote rule.

Schema v9 adds disposable unicode61 and trigram filename projections keyed by
resource identity. Resource insert, title/deleted-time update, and hard delete
maintain them in the same SQLite transaction through triggers; v8-to-v9
rebuilds only live resource metadata and skips an unnecessary rewrite of all
v8 note bodies. Each ordinary term now ORs the existing note title/body FTS
candidate with the currently associated, non-deleted resource-filename FTS
candidate; negation encloses that union. Explicit `filename:`/`mime:` filters
retain D1 priority. When a positive ordinary term actually matches a filename,
`matched_resource` uses the same FTS grammar, query-term order, then relation
position/resource ID, so a body match for `cat` cannot falsely attribute
`education.pdf`.

TDD first proved the missing `ledger` filename-only search (0 hits before D2).
The follow-up cases cover Latin word boundaries and quoted filename phrases,
short/long CJK, negation, mixed body+filename terms, explicit-filter priority,
detach, Trash scope, title update, soft-delete/restore, hard-delete, reopen,
v8 rebuild without body rewrite, and query observer absence of canonical
HTML/body text/blob reads. Independent review of the final code found **0
Critical/Important**. It noted one deferred Minor: the public `purge_note`
entrypoint is not separately exercised with a filename query (the note-deletion
and resource hard-delete paths have other tests). This gap does not justify
holding the MVP search improvement, but must be covered before full M2 signoff.

Controller independently ran `app-lite-core --features test-support` **149/149**,
GPUI binary suite **1,308 passed / 0 failed / 1 documented donor test filtered**,
both crate formatting checks, diff check, and GPUI Release build, all exit 0.
Release binary SHA-256:
`b166f5e8fed353a94d503be99fe249feba7b8cab140ecfbf9ed8f6728e3be602`.
No populated Release UI search, real 1,662-note p50/p95/RSS/index-size
acceptance, extracted PDF text, image OCR, or personal-library migration is
claimed by this stage.
