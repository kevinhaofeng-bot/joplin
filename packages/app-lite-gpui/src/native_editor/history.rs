//! Bounded inverse-operation history for the native document model.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::model::{BlockContent, Document, DocumentError, Selection};
use super::transaction::{ApplyOutcome, Transaction, TransactionBatch};

#[derive(Clone, Debug)]
struct HistoryEntry {
    inverse: TransactionBatch,
    forward: TransactionBatch,
    before_selection: Selection,
    after_selection: Selection,
    bytes: usize,
    time: Instant,
}

/// Evernote's editor (prosemirror-history, `newGroupDelay` default 500 ms,
/// enabled in `apps/peso/plugins.ts:219`) joins a change to the previous undo
/// group when it follows within this delay at an adjacent position.
pub const TYPING_GROUP_DELAY: Duration = Duration::from_millis(500);

/// Undo/redo history bounded by both entry count and operation payload bytes.
/// Entries retain inverse and forward operations only; they never retain a
/// complete document snapshot.
#[derive(Clone, Debug)]
pub struct History {
    max_entries: usize,
    max_bytes: usize,
    used_bytes: usize,
    undo: VecDeque<HistoryEntry>,
    redo: VecDeque<HistoryEntry>,
    current_selection: Option<Selection>,
}

impl History {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            used_bytes: 0,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            current_selection: None,
        }
    }

    pub fn apply(
        &mut self,
        document: &mut Document,
        transaction: Transaction,
    ) -> Result<ApplyOutcome, DocumentError> {
        let before_selection = transaction
            .selection_hint()
            .or(self.current_selection)
            .unwrap_or_else(|| document.end_selection());
        self.apply_batch_with_selection(
            document,
            before_selection,
            TransactionBatch(vec![transaction]),
        )
    }

    /// Apply a structural operation with the editor's active selection.
    /// Structural transactions do not carry a selection of their own, so the
    /// caller supplies it explicitly instead of falling back to document end.
    pub fn apply_with_selection(
        &mut self,
        document: &mut Document,
        before_selection: Selection,
        transaction: Transaction,
    ) -> Result<ApplyOutcome, DocumentError> {
        self.apply_batch_with_selection(
            document,
            before_selection,
            TransactionBatch(vec![transaction]),
        )
    }

    /// Replace the latest provisional composition operation in place. The
    /// document owns the inverse/replacement rollback journal, so a failed
    /// candidate update cannot consume history or alter document revisions.
    /// Only the localized transaction payload is retained; the document and
    /// history are never cloned for this hot path.
    pub fn replace_last_with<F>(
        &mut self,
        document: &mut Document,
        before_selection: Selection,
        make_replacement: F,
    ) -> Result<ApplyOutcome, DocumentError>
    where
        F: FnOnce(&Document) -> Result<Transaction, DocumentError>,
    {
        let old_inverse = self
            .undo
            .back()
            .map(|entry| entry.inverse.clone())
            .ok_or(DocumentError::HistoryEmpty)?;
        let (outcome, replacement) =
            document.replace_after_inverse(old_inverse, before_selection, make_replacement)?;

        self.clear_redo();
        let entry = self.undo.back_mut().ok_or(DocumentError::HistoryEmpty)?;
        let old_bytes = entry.bytes;
        entry.inverse = outcome.inverse.clone();
        entry.forward = TransactionBatch(vec![replacement]);
        entry.before_selection = before_selection;
        entry.after_selection = outcome.selection;
        entry.time = Instant::now();
        entry.bytes = entry
            .inverse
            .estimated_bytes()
            .saturating_add(entry.forward.estimated_bytes());
        self.used_bytes = self
            .used_bytes
            .saturating_sub(old_bytes)
            .saturating_add(entry.bytes);
        self.current_selection = Some(outcome.selection);
        self.trim_to_budget();
        Ok(outcome)
    }

    /// Apply one user action made up of several model transactions as a
    /// single undoable history entry. The document already provides atomic
    /// batch rollback; this method keeps that batch atomic at the editor's
    /// history boundary as well.
    pub fn apply_batch_with_selection(
        &mut self,
        document: &mut Document,
        before_selection: Selection,
        batch: TransactionBatch,
    ) -> Result<ApplyOutcome, DocumentError> {
        document.validate_selection(before_selection)?;
        let forward = batch.clone();
        let outcome = document.apply_batch(batch)?;
        self.current_selection = Some(outcome.selection);

        // A no-op remains a valid transaction result but does not create an
        // entry that would make undo appear to change the document.
        if outcome.changed_nodes.is_empty() {
            return Ok(outcome);
        }

        self.clear_redo();
        let bytes = forward
            .estimated_bytes()
            .saturating_add(outcome.inverse.estimated_bytes());
        if self.max_entries == 0 || self.max_bytes == 0 || bytes > self.max_bytes {
            // The current edit has already committed.  Older inverse
            // operations are no longer safe to apply on top of it when the
            // edit cannot itself be represented within the budget.
            self.clear_undo();
            return Ok(outcome);
        }

        let entry = HistoryEntry {
            inverse: outcome.inverse.clone(),
            forward,
            before_selection,
            after_selection: outcome.selection,
            bytes,
            time: Instant::now(),
        };
        self.used_bytes = self.used_bytes.saturating_add(bytes);
        self.undo.push_back(entry);
        self.trim_to_budget();
        Ok(outcome)
    }

    pub fn undo(&mut self, document: &mut Document) -> Result<Selection, DocumentError> {
        Ok(self.undo_with_outcome(document)?.selection)
    }

    pub fn undo_with_outcome(
        &mut self,
        document: &mut Document,
    ) -> Result<ApplyOutcome, DocumentError> {
        let Some(entry) = self.undo.back().cloned() else {
            return Err(DocumentError::HistoryEmpty);
        };
        let inverse_outcome = document.apply_batch(entry.inverse.clone())?;
        let changed_nodes = inverse_outcome.changed_nodes.clone();
        let structural = inverse_outcome.structural;
        let structural_splices = inverse_outcome.structural_splices.clone();
        let numbering_ranges = inverse_outcome.numbering_ranges.clone();
        let inverse = inverse_outcome.inverse.clone();
        let estimated_bytes = inverse_outcome.estimated_bytes;
        let entry = self.undo.pop_back().ok_or(DocumentError::HistoryEmpty)?;
        let mut entry = entry;
        // The inverse application returns the exact post-edit block range,
        // including identities allocated by structural operations.  Retain
        // that localized inverse as the redo operation so a redo never
        // re-allocates a node id that later history entries reference.
        entry.forward = inverse_outcome.inverse;
        self.reprice_entry(&mut entry);
        self.current_selection = Some(entry.before_selection);
        self.redo.push_back(entry.clone());
        self.trim_to_budget();
        Ok(ApplyOutcome {
            selection: entry.before_selection,
            changed_nodes,
            structural,
            structural_splices,
            numbering_ranges,
            inverse,
            estimated_bytes,
            inserted_span: None,
        })
    }

    pub fn redo(&mut self, document: &mut Document) -> Result<Selection, DocumentError> {
        Ok(self.redo_with_outcome(document)?.selection)
    }

    pub fn redo_with_outcome(
        &mut self,
        document: &mut Document,
    ) -> Result<ApplyOutcome, DocumentError> {
        let Some(entry) = self.redo.back().cloned() else {
            return Err(DocumentError::HistoryEmpty);
        };
        let redo_outcome = document.apply_batch(entry.forward.clone())?;
        let changed_nodes = redo_outcome.changed_nodes.clone();
        let structural = redo_outcome.structural;
        let structural_splices = redo_outcome.structural_splices.clone();
        let numbering_ranges = redo_outcome.numbering_ranges.clone();
        let inverse = redo_outcome.inverse.clone();
        let estimated_bytes = redo_outcome.estimated_bytes;
        let entry = self.redo.pop_back().ok_or(DocumentError::HistoryEmpty)?;
        let mut entry = entry;
        entry.inverse = redo_outcome.inverse;
        self.reprice_entry(&mut entry);
        self.current_selection = Some(entry.after_selection);
        self.undo.push_back(entry.clone());
        self.trim_to_budget();
        Ok(ApplyOutcome {
            selection: entry.after_selection,
            changed_nodes,
            structural,
            structural_splices,
            numbering_ranges,
            inverse,
            estimated_bytes,
            inserted_span: None,
        })
    }

    /// Join the newest entry into the previous one when both only insert
    /// text, the newest starts where the previous ended, and it followed
    /// within [`TYPING_GROUP_DELAY`]. Called after plain typing and after an
    /// input-method commit, never while a composition is still provisional
    /// (its entry must stay separate for `replace_last_with`).
    pub fn coalesce_typing(&mut self) {
        let count = self.undo.len();
        if count < 2 {
            return;
        }
        let (previous, last) = (&self.undo[count - 2], &self.undo[count - 1]);
        let only_text = |batch: &TransactionBatch| {
            batch
                .0
                .iter()
                .all(|transaction| matches!(transaction, Transaction::InsertText { .. }))
        };
        if !only_text(&previous.forward)
            || !only_text(&last.forward)
            || previous.after_selection != last.before_selection
            || last.time.saturating_duration_since(previous.time) > TYPING_GROUP_DELAY
        {
            return;
        }
        let last = self.undo.pop_back().expect("counted two entries");
        let previous = self.undo.back_mut().expect("counted two entries");
        // Undo applies the newest inverse first, then the older one.
        let mut inverse = last.inverse.0;
        inverse.extend(previous.inverse.0.drain(..));
        previous.inverse = TransactionBatch(inverse);
        previous.forward.0.extend(last.forward.0);
        previous.after_selection = last.after_selection;
        previous.bytes = previous.bytes.saturating_add(last.bytes);
        previous.time = last.time;
    }

    /// Joins every entry recorded after `depth` into one, so a user action
    /// made of several transactions (a structured paste) undoes and redoes
    /// as one step. Redo replays the same transactions in the same order.
    pub fn merge_since(&mut self, depth: usize) {
        while self.undo.len() > depth + 1 {
            let last = self.undo.pop_back().expect("more than one entry");
            let previous = self.undo.back_mut().expect("more than one entry");
            // Undo applies the newest inverse first, then the older one.
            let mut inverse = last.inverse.0;
            inverse.extend(previous.inverse.0.drain(..));
            previous.inverse = TransactionBatch(inverse);
            previous.forward.0.extend(last.forward.0);
            previous.after_selection = last.after_selection;
            previous.bytes = previous.bytes.saturating_add(last.bytes);
            previous.time = last.time;
        }
    }

    #[cfg(test)]
    pub(crate) fn age_last_entry_for_test(&mut self, by: Duration) {
        if let Some(entry) = self.undo.back_mut() {
            entry.time = entry.time.checked_sub(by).unwrap_or(entry.time);
        }
    }

    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// The exact inverse batch that the next undo will apply. `EditorCore`
    /// uses this only to map a handful of pending external-resource
    /// selections; it never exposes history internals to UI callers.
    pub(crate) fn next_undo_batch_for_mapping(&self) -> Option<TransactionBatch> {
        self.undo.back().map(|entry| entry.inverse.clone())
    }

    /// The exact forward batch that the next redo will apply. See
    /// [`Self::next_undo_batch_for_mapping`].
    pub(crate) fn next_redo_batch_for_mapping(&self) -> Option<TransactionBatch> {
        self.redo.back().map(|entry| entry.forward.clone())
    }

    /// Locate the still-undoable optimistic resource insertion by its unique
    /// staged resource id.  A resource worker failure may arrive after later
    /// typing; `EditorCore` temporarily unwinds those later entries, removes
    /// this one entry, then reapplies the later entries against the original
    /// text.  Keeping this lookup inside History avoids exposing entries to
    /// UI code or trying to reconstruct a stale selection from a raw point.
    pub(crate) fn undo_depth_for_resource_insert(&self, resource_id: &str) -> Option<usize> {
        self.undo
            .iter()
            .rposition(|entry| {
                entry.forward.0.iter().any(|transaction| {
                    matches!(
                        transaction,
                        Transaction::InsertImage { resource_id: id, .. }
                            | Transaction::InsertAttachment { resource_id: id, .. }
                            if id == resource_id
                    )
                })
            })
            .map(|index| index.saturating_add(1))
    }

    /// Snapshot the original forward batches after an optimistic entry before
    /// temporarily unwinding history. `undo_with_outcome` intentionally
    /// rewrites redo payloads to exact local inverses; those rewritten batches
    /// are not portable across the resource inverse's structural splice.
    /// Replaying the original forward batches is the transaction-mapping
    /// analogue of ProseMirror's mapped transaction replay.
    pub(crate) fn forward_entries_after_depth(
        &self,
        depth: usize,
    ) -> Vec<(Selection, TransactionBatch)> {
        self.undo
            .iter()
            .skip(depth)
            .map(|entry| (entry.before_selection, entry.forward.clone()))
            .collect()
    }

    /// Drop the newest undo entry without applying it. Only for an entry
    /// the caller knows left the document as it was (a cancelled input
    /// method composition), so Undo is not spent on a step that does nothing.
    pub(crate) fn discard_last_noop(&mut self) -> Result<(), DocumentError> {
        let entry = self.undo.pop_back().ok_or(DocumentError::HistoryEmpty)?;
        self.used_bytes = self.used_bytes.saturating_sub(entry.bytes);
        Ok(())
    }

    /// Drop exactly the redo entry that represents a failed optimistic
    /// resource insertion.  It deliberately has no document effect: the
    /// caller has already applied its inverse and must prevent a future Redo
    /// from resurrecting a resource that never committed to SQLite.
    pub(crate) fn discard_next_redo(&mut self) -> Result<(), DocumentError> {
        let entry = self.redo.pop_back().ok_or(DocumentError::HistoryEmpty)?;
        self.used_bytes = self.used_bytes.saturating_sub(entry.bytes);
        Ok(())
    }

    /// If a person had manually undone the optimistic resource before its
    /// worker failed, its redo entry is no longer a legal action. Dropping
    /// redo is preferable to later recreating an undurable resource atom.
    pub(crate) fn discard_redo(&mut self) {
        self.clear_redo();
    }

    /// Every table step is a whole-table snapshot; dropping a resource from
    /// all of them leaves undo and redo as if it had never been inserted.
    pub(crate) fn forget_table_resource(&mut self, resource_id: &str) {
        let transactions = self
            .undo
            .iter_mut()
            .chain(self.redo.iter_mut())
            .flat_map(|entry| entry.inverse.0.iter_mut().chain(entry.forward.0.iter_mut()));
        for transaction in transactions {
            match transaction {
                Transaction::ReplaceTable { table, .. } => {
                    if let Some(stripped) = table.without_resource(resource_id) {
                        *table = std::sync::Arc::new(stripped);
                    }
                }
                // A table step's inverse restores the whole table block.
                Transaction::RestoreBlocks { blocks, .. } => {
                    for block in blocks {
                        if let BlockContent::Table(table) = &mut block.content
                            && let Some(stripped) = table.without_resource(resource_id)
                        {
                            *table = std::sync::Arc::new(stripped);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub fn len(&self) -> usize {
        self.undo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.undo.is_empty()
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    fn clear_redo(&mut self) {
        // The byte budget covers both undo and redo payloads.  Dropping redo
        // therefore releases its operation payload before recording a new
        // branch.
        while let Some(entry) = self.redo.pop_front() {
            self.used_bytes = self.used_bytes.saturating_sub(entry.bytes);
        }
    }

    fn clear_undo(&mut self) {
        while let Some(entry) = self.undo.pop_front() {
            self.used_bytes = self.used_bytes.saturating_sub(entry.bytes);
        }
    }

    fn reprice_entry(&mut self, entry: &mut HistoryEntry) {
        let old_bytes = entry.bytes;
        entry.bytes = entry
            .inverse
            .estimated_bytes()
            .saturating_add(entry.forward.estimated_bytes());
        self.used_bytes = self
            .used_bytes
            .saturating_sub(old_bytes)
            .saturating_add(entry.bytes);
    }

    fn trim_to_budget(&mut self) {
        while self.undo.len().saturating_add(self.redo.len()) > self.max_entries
            || self.used_bytes > self.max_bytes
        {
            let entry = self.undo.pop_front().or_else(|| self.redo.pop_front());
            let Some(entry) = entry else { break };
            self.used_bytes = self.used_bytes.saturating_sub(entry.bytes);
        }
    }
}
