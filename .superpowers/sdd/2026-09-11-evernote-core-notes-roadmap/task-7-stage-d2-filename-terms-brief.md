# Task 7 D2: ordinary-term attachment filename search

Status: implementation brief, 2026-09-13. Start from tagged/pushed
`joplin-lite-native-v0.15.0-search-find-snapshot` at `d4faf2e06` and preserve
the D1 live-resource boundary.

## Source and product decision

Readable Evernote 11.32.5 `main-readable/src/modules/48353__module-48353.js`
creates a distinct FTS5 projection for attachment label/MIME and maintains it
with insert/delete/update triggers. `83028__module-83028.js::k` unions note
title/content with **extracted attachment searchText**, while its
`resourceFileName`/`resourceMime` branches filter attachment metadata. The
reconstructed general-term branch does **not** prove that a plain search term
also searches filenames. This stage makes filename matching a deliberate
native usability improvement, not a claim of byte-for-byte Evernote behavior.

## Bounded architecture

1. Add a disposable, resource-owned filename FTS projection (Latin unicode61
   and CJK trigram) in schema v9, with transaction-local resource
   insert/update/soft-delete/delete maintenance and migration rebuild. Keep
   `resources` and note-resource relations authoritative; no blob/canonical
   body copy in the index. FTS row identity must be stable and resource-safe.
2. For each ordinary positive or negated search term, evaluate the union of
   existing note title/body candidates and **currently associated,
   non-deleted** filename candidates; keep existing grammar (Latin word/phrase,
   CJK substring including short-CJK fallback), bounded page, and note-level
   conjunction/negation. Do not make MIME types general terms.
3. Return deterministic `matched_resource` for a filename-only ordinary-term
   hit, while preserving D1 explicit `filename:`/`mime:` provenance priority.
   Keep note projection and filter/search APIs unchanged.
4. Do not implement OCR/PDF, attachment extracted text, saved searches,
   suggestions, or a user-library migration in this cut. No network or
   platform shell extractor in note-save transactions.

## Verification gate

First write a failing repository fixture where a filename term cannot be found
through body/title and check RED, then implement. Cover Latin word boundary,
quoted phrase, 1/2/3+ CJK, negative term, multiple attachments/stable
provenance, detach/soft-delete/purge, reopen, and v8-to-v9 migration rebuild.
Search observer must show no `notes.body_html`, `notes.body_text`, blob bytes,
or actual attachment byte reads during a query. Run focused and full core,
GPUI exact donor-skip suite, both crate fmt, diff check, and Release build;
independent review gates any acceptance. A 1,662-note latency/RSS pass remains
the separate M2 gate.
