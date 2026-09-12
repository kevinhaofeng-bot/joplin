# Task 7 D1: current-resource filter and provenance

Status: bounded local acceptance, 2026-09-13. Code commits `577ab4c8c` and
`339b3f1ea`; this is not Task 7 or M2 completion.

## Source-to-behavior crosswalk

| Readable Evernote 11.32.5 evidence | Native implementation and explicit difference |
| --- | --- |
| `main-readable/src/modules/83028__module-83028.js`, `implementedFieldOperators` and `resourceMime` / `resourceFileName` cases: attachment metadata operators return distinct parent Note IDs through an Attachment join; unprefixed values compare case-insensitively for equality and prefixed values use `LIKE value%`. | `LibraryRepository::search` retains typed `filename:` / `mime:` filters as note-level predicates joined through the current note-resource relation. It rejects unassociated and soft-deleted resources. The existing native parser/query uses escaped, case-insensitive **contains** matching; exact/prefix source fidelity is still open and must not be claimed. |
| `main-readable/src/modules/48353__module-48353.js`: attachment label/MIME have a separate derived FTS table with insert/delete/update maintenance. `59009` and `98189` treat extracted attachment text separately. | This D1 cut filters authoritative resource metadata directly and returns a deterministic `ResourceId` for a matching attachment. It does **not** introduce attachment FTS, general-term filename matches, OCR/PDF extraction, or extracted-text search. Those remain later isolated work. |

For multiple filters, each predicate is evaluated at the note level; filename and
MIME may match different current attachments. `SearchHit.matched_resource` uses
the first filename filter when present, otherwise the first MIME filter, then
chooses by relation position and resource ID. This provenance rule is a native
product decision, not an observed Evernote algorithm. `hasattachment:true/false`
and result `attachment_count` use the same live-resource boundary. Search result
rows remain projections: no canonical HTML or blob-byte hydration.

## Verification and review

The D1 fixture covers multiple attachments, repeated filter combinations,
unassociated relations, soft-deleted resources, deterministic provenance, and
reopen. TDD initially failed because the provenance field was `None`; the
follow-up live-resource test initially failed because `hasattachment:true`
included a soft-deleted resource. The fixes make both tests pass.

Independent review first found one Important inconsistency in
`hasattachment:`/count. After `339b3f1ea`, re-review reported **0 Critical and
0 Important**. Controller independently ran the focused search suite 11/11,
complete `app-lite-core --features test-support` suite 140/140, GPUI binary
suite 1,308/1,308 with only the documented pre-existing donor SIGSEGV test
filtered, both crate formatting checks, staged diff check, and GPUI Release
build; all passed. The passing GPUI command used `--bin velotype`. An earlier
`--all-targets` invocation ran the same 1,308 tests successfully but then
exited 2 because the bench harness did not accept the forwarded `--skip` flag;
it is not counted as a passing command. This is code-level acceptance only. No
populated Release UI search, 1,662-note performance/RSS, OCR/PDF, or personal
library migration is claimed here.
