# Task 1 report — Evernote-style GPUI editor surface

Status: DONE_WITH_CONCERNS

## Implementation

- Added `native_editor::chrome`: fixed writing-column metrics, responsive deterministic toolbar placement and a standalone GPUI `TitleInput` with checked UTF-16 boundaries.
- Extended the shared catalogue with `InsertImage`, typed `ImagePath`, Chinese labels, icon descriptors and atomic argument validation. The command calls `EditorCore::insert_image_path`.
- Replaced the spike shell with a 1200x820 warm-neutral/white-page hierarchy, breadcrumb, title, metadata, one-row icon toolbar, narrow More fallback, labelled link field and native single-file image picker.
- Added owned 20x20 SVG assets and embedded them via the real `VelotypeAssets` source.

## RED to GREEN evidence

1. `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype chrome -- --nocapture` initially failed on missing `editor_chrome_metrics`, `toolbar_placement`, and `InsertImage`; after implementation, 18 matching tests pass.
2. `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype insert_image_command -- --nocapture` initially failed on missing `InsertImage`/`ImagePath`; after implementation, both real path-insertion/undo and atomic-error tests pass.
3. `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype title_input -- --nocapture` initially failed on missing `TitleInput`; after implementation, the CJK/emoji replacement, marked text, and surrogate-split rejection tests pass.

## Controller gates

- `chrome`: PASS, 18 tests.
- `shell_`: PASS, 12 tests.
- full bin suite with the specified one skip: PASS, 982 passed / 0 failed / 1 skipped.
- `--all-targets --no-run`: PASS.
- `cargo fmt --check`: PASS.
- `git diff --check`: PASS.
- `cargo build --manifest-path packages/app-lite-gpui/Cargo.toml --release`: PASS (optimized build completed in 2m09s).

## Real-window evidence

Release-window acceptance is PENDING: the final binary was launched with `--evernote-spike`, but this desktop session does not expose the standalone GPUI process to its accessibility automation surface, so no screenshot or interaction is represented as PASS. The behavior matrix has been updated accordingly.

## Self-review

- No Web runtime, copied Evernote asset, second body editor, or Task 7 drawable/memory threshold change was added.
- Image paste and drag/drop continue through their prior `EditorCore`/image-store paths; toolbar image selection only adds the typed native picker seam.
- More and the primary row resolve descriptors through the same catalogue. Invalid links now stay open with a visible error as required.

## Known concerns

- Real macOS Pinyin, native picker cancellation, drag/drop, and visual screenshot acceptance remain PENDING until the running Release build completes and the window can be automated.
- The repository's pre-existing warnings remain; no claim is made for `clippy -D warnings`.

## Round 1 review repairs

Status: DONE_WITH_CONCERNS

### Changes made

- Replaced the Link popover's inert `取消`/`应用` text with separately mounted GPUI hit targets (`evernote-link-cancel` and `evernote-link-apply`). Both route into the existing cancel/submit methods; invalid Apply retains the panel/error state, while valid Apply creates the actual catalogue transaction and restores body focus.
- Calculated toolbar placement once from `chrome.header_width` and passed that same placement into More. More now uses the shared overflow set and, when its trigger is near the right edge, clamps to `content_mask.right - 220`; it only becomes narrower when the mask itself is under 220pt.
- Completed the real title input path: shaped-line pointer placement/drag selection, grapheme-safe Left/Right/Home/End/Delete/Backspace, Cmd-A/C/X/V, 30pt semibold shaping, and Return/Down body-focus transfer. UTF-16 IME bridge validation remains checked at its platform boundary.
- Kept title and body on the same narrow writing-column left edge, changed the obsolete “wrapped toolbar” assertion to real one-row primary/More flex alignment, and rendered one-pixel separators only across occupied toolbar groups with neutral hover treatment.
- Split picker focus correctly through the prompt's shared completion seam: `Cancelled` is an explicit no-op that leaves prior focus alone; a real selected image transaction restores body focus through the active window handle.

### Round 1 RED to GREEN evidence

1. RED: `cargo test native_editor::chrome::tests::title_input_edits_by_grapheme_without_splitting_utf8_or_utf16 -- --exact` from `packages/app-lite-gpui` failed to compile because `TitleInput` had no `move_to_edge`, `delete_backward`, `move_horizontal`, `selected_text`, `delete_forward`, or `select_all`. GREEN: the same test passed, proving emoji and combining-grapheme edits do not split UTF-8/UTF-16 boundaries.
2. RED seam evidence: `cargo test shell_visible_link_buttons_keep_invalid_open_and_dispatch_cancel_or_apply -- --nocapture` initially failed at `Apply must be a real hit target`; after adding the mounted buttons it passed, including invalid retention, actual button cancellation, valid apply, and body focus.
3. GREEN shell seams: `cargo test --manifest-path Cargo.toml --bin velotype shell_ -- --nocapture` passed 16 tests, including 760pt primary+More membership, title click/edit/Return focus, title/body left alignment, picker focus divergence, More bounds, and live-scroll membership.

### Round 1 controller gates

- `cargo test --manifest-path Cargo.toml --bin velotype chrome -- --nocapture`: PASS, 19 tests.
- `cargo test --manifest-path Cargo.toml --bin velotype shell_ -- --nocapture`: PASS, 16 tests.
- `cargo test --manifest-path Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`: PASS, 987 passed / 0 failed / 1 filtered out.
- `cargo test --manifest-path Cargo.toml --all-targets --no-run`: PASS.
- `cargo fmt --check --manifest-path Cargo.toml`: PASS.
- `git diff --check`: PASS.
- `cargo build --manifest-path Cargo.toml --release`: PASS (optimized release target; final LTO link completed in 2m14s).

### Round 1 real-window evidence

- Launched the exact release binary as `target/release/velotype --evernote-spike`; its foreground process remained live until explicitly stopped after the evidence attempt.
- The desktop automation surface still did not enumerate a `velotype` app. A direct automation lookup returned `Invalid app: velotype`; therefore no screenshot or native interaction has been marked PASS. These real-window items remain PENDING, not inferred from shell tests.

### Round 1 self-review

- Verified the Round 1 diff changes only `spike_app.rs` and `native_editor/chrome.rs`; Task 7 fixture counts, memory thresholds, drawable-pool logic, image paste/drop paths, and measurement code were not changed.
- More and primary resolve their command descriptors through the same catalogue placement; Link buttons call the production submit/cancel methods rather than test helpers. Picker selection calls the same typed `InsertImage` catalogue transaction as the prompt.

## Round 2 review repairs

Status: DONE_WITH_CONCERNS

### Changes made

- Replaced the title's potentially reversed byte `Range` state with an always-ordered selection range plus an explicit anchor and direction flag. Platform `UTF16Selection` now reports that direction, while ordinary input, Cmd-V, IME marked replacement, painting, and byte replacement always operate on an ordered range.
- Fixed non-extending Left/Right so a nonempty title selection first collapses at the requested edge and returns; it only moves one grapheme on the next keypress.
- Made Cmd-X a true no-op for an empty title selection, preserving both text and clipboard. Nonempty title selections retain copy-and-delete behavior.
- Tightened the one-row toolbar to a nonshrinking 44pt flex item and made every 32pt command hit target an actual flex container, so the 20pt SVG child is centered.
- Strengthened Link Apply verification to assert exactly one history entry and a real URL mark. Picker completion now asserts cancellation leaves document/history/selection/focus identical and selection creates a structured image block, valid selection, one history entry, and body focus.

### Round 2 RED to GREEN evidence

1. RED: `cargo test native_editor::chrome::tests::title_input_reverse_selection_replaces_and_marks_without_reversed_ranges -- --exact` failed with `slice index starts at 10 but ends at 7` when Shift-Left formed a reversed range and the IME replacement path reached `String::replace_range`. GREEN: the same test passes after storing ordered range plus direction.
2. The title production-shell test now drives reverse selection through normal key events, ordinary input, Cmd-V, and `EntityInputHandler` IME mark/commit calls; it also demonstrates empty-selection Cmd-X preserves clipboard. The Link and picker shell seams now assert transaction-level effects rather than only focus/panel state.

### Round 2 controller gates

- `cargo test --manifest-path Cargo.toml --bin velotype chrome -- --nocapture`: PASS, 22 tests.
- `cargo test --manifest-path Cargo.toml --bin velotype shell_ -- --nocapture`: PASS, 16 tests.
- `cargo test --manifest-path Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`: PASS, 990 passed / 0 failed / 1 filtered out.
- `cargo test --manifest-path Cargo.toml --all-targets --no-run`: PASS.
- `cargo fmt --check --manifest-path Cargo.toml`: PASS.
- `git diff --check`: PASS.
- `cargo build --manifest-path Cargo.toml --release`: PASS (final optimized LTO link completed in 2m19s).

### Round 2 self-review and concerns

- The diff remains limited to the title/input shell and report; no Task 7 thresholds, drawable-pool settings, image cache budgets, or measurement workloads changed.
- Real-window status remains PENDING for the unchanged desktop automation limitation documented in Round 1; no screenshot or native interaction is claimed as PASS.

## Round 3 — asynchronous native-picker image repaint regression

Status: DONE_WITH_CONCERNS

### Root cause proved

The reproduced release symptom was not an image-path, catalogue, document transaction, resource, decode-cache, or body-focus failure. The native completion previously retained only `Entity<EditorCore>` and `CommandCatalogue`, then called `editor_cx.notify()`. `EditorCore` therefore changed and decoded the image, but the parent-owned `SpikeView` canvas/scroll surface received no notification to recompute its measured height and paint snapshot after that asynchronous callback. This exactly matches the manual evidence: the picker closed, the body caret returned, the note thumbnail changed, and stderr remained empty while the old current surface stayed clipped.

The fix retains a typed `WindowHandle<SpikeView>` before opening the platform picker. On completion it executes the existing selected-image transaction through the owning `SpikeView` context, then calls `Context<SpikeView>::notify()`. It does not use a global refresh, a note switch, or a nested `Entity::update` (the latter was experimentally rejected because GPUI panics when updating the view already being updated by `WindowHandle::update`). Cancellation remains an explicit no-op for editor data/focus and is only allowed to invalidate the shell harmlessly.

### Round 3 RED to GREEN evidence

1. RED on the `478deb054` behavior model: the new real asynchronous seam spawned a completion outside `SpikeView::update`, performed the valid picker insertion, then asserted that the owning shell had been notified. It failed with: `an async picker completion must notify its owning SpikeView so the current surface can relayout`. This was intentionally a parent canvas/scroll contract, not a document-only or mock assertion.
2. GREEN: `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype shell_picker_selection_immediately_paints_the_inserted_image_in_the_current_surface -- --nocapture` passed (`1 passed; 0 failed`). The production seam drives the actual spawned callback and typed window update, then proves one structured image block in the current render snapshot, one visible image, decoded cache state `Loaded`, increased scroll extent, increased `spike-editor-surface` height, and a real `SpikeView` notification.
3. The same seam uses the exact manual-acceptance asset: `/Users/kevinhao/Projects/joplin/.worktrees/joplin-lite-native-rust-mvp/packages/app-lite-gpui/assets/showcase/1.png`. It checks the file exists before dispatch and exercises the same `InsertImage` catalogue transaction as the native prompt.

### Round 3 controller gates

- Focused current-surface image seam: PASS, `1 passed; 0 failed`.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype image -- --nocapture`: PASS, `142 passed; 0 failed; 850 filtered out`.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype chrome -- --nocapture`: PASS, `22 passed; 0 failed`.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype shell_ -- --nocapture`: PASS, `17 passed; 0 failed`.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`: PASS, `991 passed; 0 failed; 1 filtered out`.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets --no-run`: PASS.
- `cargo fmt --check --manifest-path packages/app-lite-gpui/Cargo.toml`: PASS.
- `git diff --check`: PASS.
- `cargo build --manifest-path packages/app-lite-gpui/Cargo.toml --release`: PASS (optimized release build completed in 2m13s; pre-existing macro/unused warnings remain).

### Real-window evidence

- Baseline failure screenshots supplied with the reproduction remain preserved: `/tmp/joplin-lite-visible-mvp/fresh-known-inserted-front.png` and `/tmp/joplin-lite-visible-mvp/image-after-resize.png`.
- The repaired exact Release binary was freshly launched as `packages/app-lite-gpui/target/release/velotype --evernote-spike`; launch/foreground screenshots are preserved at `/tmp/joplin-lite-round3/release-launch.png` and `/tmp/joplin-lite-round3/release-front.png`.
- Native picker selection of the known image is PENDING, not PASS. In this shared desktop session the foreground-window contention prevented reliable accessibility automation of the unbundled `velotype` process; a later screenshot after an attempted toolbar click showed an unrelated foreground application rather than an attributable picker result. No visual acceptance is inferred from the seam tests.

### Round 3 self-review and known concerns

- The source diff is limited to `packages/app-lite-gpui/src/spike_app.rs`; it does not alter Task 7 image budgets, drawable-pool settings, fixtures, measurement limits, paste/drop behavior, or title/link/toolbar paths.
- The selected-image callback retains its typed parent ownership for the whole prompt lifetime and invokes the pre-existing transaction/focus logic exactly once. The regression test would fail under the old child-only notification semantics.
- Real native-picker/manual visual acceptance remains PENDING due to the desktop automation limitation above. This is the only promotion concern; no claim is made for `clippy -D warnings` because the repository retains pre-existing warnings.

## Round 4 — picker completion seam review repair

Status: DONE_WITH_CONCERNS

### Changes made

- Replaced the non-portable test image path with `PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/showcase/1.png")`.
- Captured the owning typed `WindowHandle<SpikeView>` directly from the toolbar click's `window.window_handle()` before opening the native prompt. If that exact window cannot be downcast, no picker is opened. The prompt no longer relies on global `active_window`.
- Extracted the actual post-await completion handoff into `deliver_image_picker_completion`. Both the production await and the tests' already-settled results use that same `AsyncApp -> WindowHandle<SpikeView>::update -> complete_image_picker_in_view` path. A closed window is safely discarded by the failed typed handle update; there is no nested update or global refresh.
- Moved cancellation and selection focus/data assertions onto the shared asynchronous completion entry. Cancelled completion keeps document semantic snapshot, undo depth, selection, and title focus unchanged; successful completion restores body focus and adds exactly the normal image transaction.

### Corrected root-cause statement and RED to GREEN evidence

Round 3's parent-notification observation is now treated as a demonstrated necessary repaint/relayout contract rather than as a claim that every real-world symptom had a single independently isolated cause. The regression test now checks user-visible current-surface behavior before notification count.

1. RED: after adding the shared completion test, the helper was deliberately run with the old child-only completion body (real `complete_image_picker` transaction but no `SpikeView` notification). `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype shell_picker_selection_immediately_paints_the_inserted_image_in_the_current_surface -- --nocapture` failed at `picker completion must schedule the current surface for painting`. This proves the old semantics reaches a document mutation but fails at the current visible paint/surface seam, not merely at an observer counter.
2. GREEN: after routing that exact shared helper through `complete_image_picker_in_view`, the same command passed (`1 passed; 0 failed`). It verifies current render membership and visibility, decoded image cache state, scroll growth, surface-height growth, and then the parent notification as supplementary evidence.
3. Shared asynchronous cancel/selection seam: `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype shell_picker_ -- --nocapture` passed (`2 passed; 0 failed`), including full cancellation no-op checks and selected-image body-focus/transaction checks through the production completion entry.

### Round 4 controller gates

- `chrome`: PASS.
- `shell_`: PASS.
- `image`: PASS.
- Full bin with the specified skip: PASS.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets --no-run`: PASS.
- `cargo fmt --check --manifest-path packages/app-lite-gpui/Cargo.toml`: PASS after standard formatting.
- `git diff --check`: PASS.
- `cargo build --manifest-path packages/app-lite-gpui/Cargo.toml --release`: PASS (optimized release build; existing warnings remain).

### Round 4 self-review and concerns

- No Task 7 threshold, image-budget, drawable-pool, fixture, paste/drop, or measurement change was made.
- The source contains one typed completion route used after real native prompt settlement and in both production-seam tests; it cannot open a panel without an owning typed spike window and does not duplicate insertion.
- Native-picker/manual visual acceptance remains PENDING under the existing desktop accessibility limitation; nothing in this round promotes it to PASS.

## Controller real-window acceptance after Round 4

Status: PARTIAL_PASS

- Launched the exact optimized binary at `packages/app-lite-gpui/target/release/velotype --evernote-spike`, brought that process to the foreground, clicked the production image command, and completed the native macOS file picker with `packages/app-lite-gpui/assets/showcase/1.png`.
- PASS: the selected image appeared immediately in the currently focused editor surface. No note switch, focus detour, window resize, or reload was used between picker confirmation and the visible result. Evidence: `/tmp/joplin-lite-visible-mvp/round4-image-immediate.png`.
- PASS: the caret remained usable directly after the image and accepted `after image typing`. Two screenshots sampled two seconds apart show the same image/text geometry and scroll position; their file hashes differ because cursor/system chrome are dynamic, so this is recorded as sampled layout stability rather than a general proof that every animation frame is flash-free. Evidence: `/tmp/joplin-lite-visible-mvp/round4-image-followup-ascii-1.png` and `/tmp/joplin-lite-visible-mvp/round4-image-followup-ascii-2.png`.
- The Release process was stopped after capture. Finder/Preview clipboard insertion, drag/drop, cross-image drag selection, narrow-window overflow, and a complete macOS Pinyin candidate-revision pass remain separate manual matrix items and are not promoted by this focused acceptance.
- Independent scoped review of `d4dccff30..93a469340` approved the picker-completion repair with zero Critical, Important, or Minor findings. The controller gates remain PASS: focused asynchronous picker seam, `chrome`, `shell_`, `image`, full bin (`991 passed; 0 failed; 1 filtered out`), all-target compilation, formatting, diff check, and optimized Release build.

## Round 5 — visible More/Link acceptance repair

Status: DONE_WITH_CONCERNS

### RED to GREEN evidence

1. RED — `shell_more_rows_have_chinese_labels_and_full_line_hit_targets_at_wide_and_narrow_widths` initially failed at `Heading 1 Chinese More label`: the mounted overflow entries exposed only their SVG hit boxes. The test checks every shared-catalogue overflow descriptor at 1200 pt and 760 pt, its Chinese `label_zh` bounds, full row bounds, 220-pt menu width, and 300-pt maximum height.
2. GREEN — the same test passed after rendering every More descriptor as a 36-pt full-width flex row with a 20-pt icon and the real `label_zh`; rows retain active/disabled styling. A first Green attempt exposed a 28-pt shrunken narrow row; adding `flex_none` fixed that visual layout defect. Final focused command: `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype shell_more_rows_have_chinese_labels_and_full_line_hit_targets_at_wide_and_narrow_widths -- --nocapture` — `1 passed; 0 failed`.
3. RED — `shell_link_popover_anchors_to_clicked_trigger_and_stays_inside_content_mask` first had no mounted popover selector, then exposed the fixed-left/fixed-top panel behavior. It opens the actual wide Link trigger and narrow More→Link row and checks the resulting panel bounds overlap the clicked trigger and stay inside the mounted root/content mask.
4. GREEN — Link now carries the actual mouse-down trigger point, clamps its 360-pt maximum width to the mask, and flips/clamps vertically. The panel is `absolute` (not subsequently overwritten by `relative`) and is a column flex layout. Focused command passed: `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype shell_link_popover_anchors_to_clicked_trigger_and_stays_inside_content_mask -- --nocapture` — `1 passed; 0 failed`.
5. RED — the strengthened `shell_visible_link_buttons_keep_invalid_open_and_dispatch_cancel_or_apply` used the mounted `evernote-link-apply` bounds and a real simulated click after entering `not-a-url`. Under the previous event layering, it failed at `mounted Apply must invoke URL validation`; no error state was set.
6. GREEN — `CommandCatalogue::execute(LinkUrl)` now accepts only absolute `http`/`https` URLs with a host. Link-open disables the editor-surface capture listener; a single full-window backdrop is placed below the panel solely for outside cancellation, while the panel remains the highest sibling and its actual input/Cancel/Apply controls own their own mouse handlers. The mounted click now retains the panel and input focus, renders `请输入有效 URL`, and leaves the semantic snapshot, undo depth, and selection unchanged. Valid Apply creates one real link transaction and Cancel restores body focus. Focused command passed: `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype shell_visible_link_buttons_keep_invalid_open_and_dispatch_cancel_or_apply -- --nocapture` — `1 passed; 0 failed`.

### Round 5 controller gates

- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype chrome -- --nocapture`: PASS, `22 passed; 0 failed`.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype shell_ -- --nocapture`: PASS, `19 passed; 0 failed`.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype -- --skip editor::selection::tests::cross_block_cut_writes_markdown_deletes_range_and_undo_restores`: PASS, `993 passed; 0 failed; 1 filtered out`.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets --no-run`: PASS.
- `cargo fmt --check --manifest-path packages/app-lite-gpui/Cargo.toml`: PASS.
- `git diff --check`: PASS.
- `cargo build --manifest-path packages/app-lite-gpui/Cargo.toml --release`: PASS (optimized LTO link; existing repository warnings remain).

### Round 5 self-review and concerns

- The product diff is confined to `spike_app.rs` and the shared-command URL validation. It does not change Task 7 thresholds, drawable settings, image budgets, fixtures, measurement workloads, paste/drop, or the typed picker completion route.
- More uses only the existing `CommandCatalogue` descriptors; no hard-coded duplicate command list or source-string detector was added.
- The Link dismissal/event flow has one outside-click owner (the backdrop). The panel is above that backdrop; the real input, Cancel, and Apply targets handle their own events. There is no global refresh, helper-only Apply assertion, or document mutation on invalid input.
- Existing real-window screenshots establishing the failures are preserved at `/tmp/joplin-lite-visible-mvp/acceptance2/08-more-open.png`, `/tmp/joplin-lite-visible-mvp/acceptance2/12-narrow-more.png`, and `/tmp/joplin-lite-visible-mvp/acceptance2/09-link-panel.png`. Fresh desktop re-acceptance of this follow-up is PENDING because this session did not perform another attributable native Release interaction; no visual PASS is inferred from automated seam results.

## Round 6 — More→Link state and short-mask containment repair

Status: DONE_WITH_CONCERNS

### RED to GREEN evidence

1. RED — `shell_more_to_link_dismisses_overflow_for_cancel_apply_and_outside` drove the mounted 760-pt More trigger and mounted Link row. On the previous branch it failed at `opening Link from More must close More`: `more_open` remained true after Link opened.
2. GREEN — the Link branch now clears `more_open` in the same `SpikeView` update that opens the popover. The real mounted test then covers More→Link→Cancel, More→Link→valid Apply, and More→Link→outside backdrop dismissal. Each finishes with `more_open == false` and no Link panel; Cancel/outside preserve selection and valid Apply creates exactly one history entry. Focused command: `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype shell_more_to_link_dismisses_overflow_for_cancel_apply_and_outside -- --nocapture` — `1 passed; 0 failed`.
3. RED — `shell_link_popover_clamps_normal_and_invalid_height_after_resize` opened the actual Link control, resized 820 pt to 112 pt high, and failed with a normal panel at `y=22`, `height=117.5` against a 112-pt root mask. This proves the former upward-only clamp still allowed bottom overflow.
4. GREEN — normal and invalid Link layouts use explicit 118-pt/134-pt natural heights, clamp top into `[mask_top, mask_bottom - panel_height]`, constrain height to the available mask, and use vertical scrolling when the mask is shorter. The same test covers normal resize, invalid-error resize, and opening More→Link directly while short. Focused command passed: `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype shell_link_popover_clamps_normal_and_invalid_height_after_resize -- --nocapture` — `1 passed; 0 failed`.
5. Mounted validation coverage — `shell_mounted_apply_rejects_parseable_disallowed_link_urls` uses the real Apply target for `mailto:note@example.com`, `file:///tmp/note`, and the parseable hostless `data:text/plain,no-host`. Each retains the panel, error, and URL focus, with unchanged document, history, and selection. Focused command passed: `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --bin velotype shell_mounted_apply_rejects_parseable_disallowed_link_urls -- --nocapture` — `1 passed; 0 failed`.

### Round 6 controller gates

- `chrome`: PASS, `22 passed; 0 failed`.
- `shell_`: PASS, `22 passed; 0 failed`.
- Full bin with the specified skip: PASS, `996 passed; 0 failed; 1 filtered out`.
- `cargo test --manifest-path packages/app-lite-gpui/Cargo.toml --all-targets --no-run`: PASS.
- `cargo fmt --check --manifest-path packages/app-lite-gpui/Cargo.toml`: PASS.
- `git diff --check`: PASS.
- `cargo build --manifest-path packages/app-lite-gpui/Cargo.toml --release`: PASS (optimized LTO link completed; existing repository warnings remain).

### Round 6 self-review and concerns

- The fix preserves the single backdrop/panel hierarchy from Round 5. It adds no helper-dispatch layer, overlay, or second event owner.
- The short-mask rule constrains the popover itself rather than moving the content mask, editor surface, Task 7 thresholds, or measurement path.
- Independent scoped review of `6512f51da..94168c807` approved the repair with no Critical or Important findings. One non-blocking Minor remains: the invalid-state 134-pt natural height is conservative and scroll-contained, but an actual very-short-window click after scrolling was not manually exercised.

### Round 6 exact Release acceptance

Status: PASS_WITH_MINOR_FOLLOWUP

- Wide window PASS: More renders full Chinese labels and full-row targets; Link opens at the clicked toolbar trigger and stays inside the note surface. Evidence: `/tmp/joplin-lite-visible-mvp/acceptance3/01-wide-more-labelled.png` and `/tmp/joplin-lite-visible-mvp/acceptance3/05-wide-link-anchored.png`.
- Real invalid Apply PASS: with the first line selected, entering `not-a-url` and clicking the mounted 应用 button kept the panel open, rendered the red invalid field plus `请输入有效 URL`, and did not replace the selected document text. Evidence: `/tmp/joplin-lite-visible-mvp/acceptance3/08-invalid-link-real-apply.png`.
- Real Cancel PASS: clicking the mounted 取消 button closed Link and restored the original first-line selection. Evidence: `/tmp/joplin-lite-visible-mvp/acceptance3/09-invalid-cancel-selection-restored.png`.
- Narrow 760-pt window PASS: More retained Chinese labels; More→Link atomically removed the overflow menu; both the mounted 取消 button and an outside-body click closed Link without leaving either panel behind, while the original selection remained. Evidence: `/tmp/joplin-lite-visible-mvp/acceptance3/11-narrow-more-labelled.png`, `/tmp/joplin-lite-visible-mvp/acceptance3/12-narrow-more-to-link.png`, `/tmp/joplin-lite-visible-mvp/acceptance3/13-narrow-cancel-no-stale-panel.png`, and `/tmp/joplin-lite-visible-mvp/acceptance3/14-narrow-outside-dismiss-no-stale-panel.png`.
- The exact Release process was stopped after capture. Manual very-short-window scrolling/clickability remains the single Minor follow-up; the mounted resize/containment test is PASS, so no clipping or stale state is inferred beyond what was exercised.
