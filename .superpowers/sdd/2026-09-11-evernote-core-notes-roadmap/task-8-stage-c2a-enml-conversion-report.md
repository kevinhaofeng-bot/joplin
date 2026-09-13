# Task 8 Stage C2a — pure ENML conversion report

Status: **DONE_WITH_CONCERNS**, limited to synthetic, bounded ENML → existing
`CanonicalDocument`. This is not staging, importing, profile mutation, export,
sync, UI work, real-library validation, or final Task 8 acceptance.

## Source → native behavior → tests

| Source directly re-read | C2a behavior | Synthetic test |
| --- | --- | --- |
| Evernote reconstructed `main-readable/src/modules/36364__note-import-mutation-import-enex-file-import-file-from-url.js` uses strict SAX and separates parsed note content/resources from the mutation | Pure function consumes one ENML string plus an explicit same-note verified resource map; returns document/HTML/search/resource IDs or blocker, without storage dependency | Chinese text→image→text and PDF golden |
| Evernote renderer `chunks/9093.js::916042` sanitizer allows `en-media` hash/type and `en-todo` checked, but also permits HTML semantics such as font/color/size/sub/sup/table | Media and representable checklist states are converted; semantics absent from the canonical model are blocked, not treated as safely imported merely because Evernote sanitizer accepts them | table, font/color, sub, inline todo blockers |
| Evernote common-editor `resource.ts::getAttributeResourceFromElement` normalizes media hash and can fall back to MIME before resource detail arrives | Only an explicit same-note, uniquely verified MD5/MIME candidate resolves; missing, ambiguous or mismatched references block | missing/cross-note/ambiguous media cases |
| Open-source Joplin `packages/lib/import-enex-html-gen.ts::enexXmlToHtml_` emits inline checkbox input and tracks unmatched attachments (independent reference, **not** Evernote reverse-engineered source) | `en-todo` is accepted only as the first child of every `li` in a `ul` that maps exactly to canonical Checklist; generic inline todo blocks. PDF can be an attachment block in source order; text-adjacent inline PDF blocks. | checked/unchecked golden, inline todo blocker |

## RED → GREEN and commands

1. RED: `cargo test --test enml_convert` failed to compile because `convert_enml`, `VerifiedEnmlResource` and `EnmlFidelityBlocker` did not exist.
2. GREEN: the converter and first two tests passed. A subsequent RED case showed `https://host:bad/x` could pass a weaker converter URL check even though the canonical parser would drop its link mark. GREEN: converter now calls the canonical model's existing `valid_link` predicate and rejects whitespace.
3. Further RED cases found custom ENML processing instructions were silently dropped, a declared `ISO-8859-1` body was accepted as UTF-8, `en-todo checked="maybe"` was silently treated as unchecked, and `<!DOCTYPE en-notebook>` was accepted by a prefix check. GREEN blocks PI, accepts at most one XML 1.0 UTF-8 declaration and one exact `en-note` external DOCTYPE without internal subset, allows only `true`/`false`/default-false checkbox values, and intentionally removes only comments (as Evernote's sanitizer does). Expanded focused tests cover exact canonical HTML, search text and ordered resource IDs; H2/underline/strike/highlight/ordered list; malformed XML; inline todo; unsupported table/font/sub/style; unsafe URL; and missing/ambiguous media. Final focused result: `cargo test --test enml_convert` **4 passed**.

The converter accepts at most 4 MiB of ENML, 50,000 XML nodes and depth 64. It validates XML nesting, text entities and attributes, and builds a temporary XML tree only inside those bounds. Rendering allowlists a conservative subset and sends generated HTML through the existing `CanonicalDocument::parse_html`, then derives canonical HTML, search text and ordered resource IDs from the resulting document. It creates no second editor model, files, database connection or resource bytes.

## Remaining fidelity limits

Nested block layouts, arbitrary CSS/style, tables, font/color/size, superscript/subscript, embedded non-image media in inline flow, mixed/ordered checklists and non-list `en-todo` are deliberate typed blockers with source paths. Image dimensions and unverified resource metadata are not guessed. Duplicate source tag/resource occurrences remain the later staging report's responsibility; this pure body converter does not deduplicate or claim to audit them. Callers must retain raw ENML and provide the verified same-note resource map. No real user archive/profile was read.

Final commands after the last DOCTYPE correction: `cargo fmt`,
`cargo test --test enml_convert` (**4 passed**), `cargo fmt --check`
(clean), `git diff --check` (clean), and `cargo test --quiet` (all groups
passed: 67 unit, 4 document, 16 ENEX, 4 ENML conversion, 27 JEX,
15 migration, 8 organization, 7 repository flow, 7 resource-store tests;
empty/doc groups passed). This is still pending independent review.
