//! Bounded inverse-operation history for the native document model.

use std::collections::VecDeque;

use super::model::{Document, DocumentError, Selection};
use super::transaction::{ApplyOutcome, Transaction, TransactionBatch};

#[derive(Clone, Debug)]
struct HistoryEntry {
    inverse: TransactionBatch,
    forward: TransactionBatch,
    before_selection: Selection,
    after_selection: Selection,
    bytes: usize,
}

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
        };
        self.used_bytes = self.used_bytes.saturating_add(bytes);
        self.undo.push_back(entry);
        self.trim_to_budget();
        Ok(outcome)
    }

    pub fn undo(&mut self, document: &mut Document) -> Result<Selection, DocumentError> {
        let Some(entry) = self.undo.back().cloned() else {
            return Err(DocumentError::HistoryEmpty);
        };
        let inverse_outcome = document.apply_batch(entry.inverse.clone())?;
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
        Ok(entry.before_selection)
    }

    pub fn redo(&mut self, document: &mut Document) -> Result<Selection, DocumentError> {
        let Some(entry) = self.redo.back().cloned() else {
            return Err(DocumentError::HistoryEmpty);
        };
        let redo_outcome = document.apply_batch(entry.forward.clone())?;
        let entry = self.redo.pop_back().ok_or(DocumentError::HistoryEmpty)?;
        let mut entry = entry;
        entry.inverse = redo_outcome.inverse;
        self.reprice_entry(&mut entry);
        self.current_selection = Some(entry.after_selection);
        self.undo.push_back(entry.clone());
        self.trim_to_budget();
        Ok(entry.after_selection)
    }

    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo.len()
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
