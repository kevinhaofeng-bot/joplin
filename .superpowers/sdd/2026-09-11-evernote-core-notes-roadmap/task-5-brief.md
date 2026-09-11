### Task 5: Make images and attachments a complete cross-layer transaction

**Files:**
- Modify: `packages/app-lite-gpui/src/native_editor/images.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/transaction.rs`
- Modify: `packages/app-lite-gpui/src/native_editor/core.rs`
- Modify: `packages/app-lite-gpui/src/app/note_session.rs`
- Modify: `packages/app-lite-gpui/src/ui/note_card.rs`
- Create: `packages/app-lite-gpui/src/ui/attachment_card.rs`
- Test: `packages/app-lite-gpui/src/app/image_flow_tests.rs`

**Interfaces:**
- Consumes: `ResourceStore::put`, `LibraryRepository::associate_resource`, current `Selection`.
- Produces: `NoteSession::insert_resource(ResourceImport, InsertIntent)` and `ThumbnailKey { resource_id, pixel_size, scale }`.

- [ ] **Step 1: Reproduce the reported stale-image bug as a test**

Mount the real AppModel, insert an image through the picker completion path, and assert in the same presentation cycle that: the active document contains the Image block; editor measured extent grows; the image cache has a pending/loaded key; and the selected card projection references the resource. The test must not select another note to pass.

- [ ] **Step 2: Lock image-boundary behavior**

Add real event-path tests for typing before and after an image, inserting two adjacent images, selecting across text-image-text, Backspace/Delete at both boundaries, undo/redo, IME composition beside an image, and long-image reflow. No sequence may hide typed text or move the document on alternating frames.

- [ ] **Step 3: Implement one insert transaction**

Import and validate the resource, insert at the saved DocPoint, update note-resource order, flush the note snapshot, emit one projection event, then schedule decode. On failure before association, roll back resource metadata; the content-addressed blob may remain for safe reuse/GC.

- [ ] **Step 4: Add attachment cards**

Images render inline. PDF, audio, video and other files render a filename/mime/size card with system preview/open actions. Attachment bytes never enter the document string or GPUI element state.

- [ ] **Step 5: Perform M1 acceptance and commit**

In a fresh temporary profile, create three notes; type Chinese titles/body; paste a screenshot; drag a JPEG; insert a PDF; type before and after images; switch rapidly; quit and reopen. Verify exact persistence and immediate display.

```bash
git add packages/app-lite-gpui/src packages/app-lite-core
git commit -m "Complete durable image and attachment flow"
```

M1 is complete only after this real Release flow passes. At this point the product becomes an actual minimal notes application; Tasks 6–10 expand it to the full core target.
