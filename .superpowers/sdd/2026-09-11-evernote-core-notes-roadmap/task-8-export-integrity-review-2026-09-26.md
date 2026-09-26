# Task 8 cumulative export integrity review

Reviewer: `/root/export_snapshot_review`, GPT-6 Sol. Base `a338ae328`; uncommitted cumulative changes in readable_export.rs, repository.rs, export_restore.rs.

## Initial verdict

Spec requirements for consistent metadata snapshot, history-only resources, v1 refusal and durable notebook/resource metadata are met. Quality: Needs fixes, 0 Critical / 1 Important / 0 new Minor.

Important: repository.rs:1026-1027 and readable_export.rs:441-442,1084-1101 opened source blobs without a captured-size bound, then copied to EOF. A growing physical file could cause unbounded I/O/staging disk growth before final mismatch. Reuse `open_verified_with_limit` and bound copy to captured size plus one.

Strengths: deterministic snapshot and mutex-release tests, fail-closed restoration with hash/size checks before target publication. Reviewer did not repeat test suites. Source reread cannot be proved by a code diff; controller directly checked local `11354__enex-exporter.js:232-294` and mapped metadata/attachment export separately from our additional integrity guarantees.

## Fix round 1

Original implementer changed only captured-size propagation and copy bounds plus covering tests. Report: `task-8-snapshot-report-2026-09-26.md`, section Independent review round 1. Controller independently ran 9/9 export unit and 16/16 export/restore integration tests successfully.

Scoped reviewer verdict: ADDRESSED, no new breakage or out-of-scope observations. Evidence: repository.rs:1026-1033 passes captured size to limited-open; readable_export.rs:441-449,1091-1121 stops at size+1 before writing extra bytes; export_restore.rs:122-157 and readable_export.rs:1305-1321 cover oversized physical blobs and endless input. Approved for the bounded integrity slice, not whole Task 8 or GUI acceptance.
