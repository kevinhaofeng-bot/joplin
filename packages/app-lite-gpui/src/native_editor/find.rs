//! Ephemeral literal find-in-note state.
//!
//! Find is presentation state, not part of [`Document`], so changing a query
//! never creates a document revision or history entry.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use regex::RegexBuilder;

use super::model::{Document, NodeId};

/// Bound the query before regex compilation. Find is intentionally literal,
/// and a longer string is not useful as an interactive in-note query; the
/// limit also keeps a pasted payload from allocating an unbounded regex.
pub const MAX_FIND_QUERY_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindError {
    QueryTooLong { max_bytes: usize },
    MatcherUnavailable,
}

impl std::fmt::Display for FindError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueryTooLong { max_bytes } => {
                write!(formatter, "查找内容不能超过 {max_bytes} 字节")
            }
            Self::MatcherUnavailable => formatter.write_str("无法准备查找内容"),
        }
    }
}

impl std::error::Error for FindError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindMatch {
    pub node_id: NodeId,
    pub utf8_range: Range<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindSummary {
    pub total: usize,
    pub primary_index: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FindCursor {
    node_id: NodeId,
    index: usize,
}

#[derive(Clone, Debug)]
struct BlockMatches {
    revision: u64,
    matches: Vec<FindMatch>,
}

/// In-memory find state retained by one editor. Results are held per text
/// block, so an ordinary keystroke only replaces that block's compact match
/// vector; painting gets direct lookup for each visible node.
#[derive(Clone, Debug, Default)]
pub struct FindState {
    query: String,
    case_sensitive: bool,
    matcher: Option<regex::Regex>,
    blocks: HashMap<NodeId, BlockMatches>,
    text_order: Vec<NodeId>,
    total: usize,
    primary: Option<FindCursor>,
    #[cfg(test)]
    scanned_blocks: usize,
    #[cfg(test)]
    matcher_compiles: usize,
    #[cfg(test)]
    structural_membership_work: usize,
}

impl FindState {
    pub fn set_query(
        &mut self,
        document: &Document,
        query: &str,
        case_sensitive: bool,
    ) -> Result<(), FindError> {
        if query.is_empty() {
            self.clear();
            return Ok(());
        }
        if self.query == query && self.case_sensitive == case_sensitive {
            return Ok(());
        }
        if query.len() > MAX_FIND_QUERY_BYTES {
            return Err(FindError::QueryTooLong {
                max_bytes: MAX_FIND_QUERY_BYTES,
            });
        }
        // Compile before touching retained state. A matcher failure leaves the
        // currently displayed query/results intact so callers can surface an
        // error without the find UI appearing to lose its state.
        let matcher = literal_matcher(query, case_sensitive)?;
        self.query.clear();
        self.query.push_str(query);
        self.case_sensitive = case_sensitive;
        self.matcher = Some(matcher);
        #[cfg(test)]
        {
            self.matcher_compiles = self.matcher_compiles.saturating_add(1);
        }
        self.blocks.clear();
        self.text_order.clear();
        self.total = 0;
        self.primary = None;
        self.reconcile(document);
        self.primary = self.first_cursor();
        Ok(())
    }

    /// Refresh changed text blocks only. C1 excludes image alt text and
    /// attachment filenames, so atoms never manufacture editable ranges.
    pub fn reconcile(&mut self, document: &Document) {
        if self.query.is_empty() {
            return;
        }
        let old_primary = self.primary().cloned();
        let text_order_changed = !same_text_order(document, &self.text_order);
        if text_order_changed {
            self.text_order = document
                .blocks()
                .iter()
                .filter(|block| block.content.as_text().is_some())
                .map(|block| block.id)
                .collect();
        }

        for block in document.blocks() {
            let Some(text) = block.content.as_text() else {
                continue;
            };
            if self
                .blocks
                .get(&block.id)
                .is_some_and(|cached| cached.revision == block.revision)
            {
                continue;
            }
            let old_count = self
                .blocks
                .get(&block.id)
                .map_or(0, |cached| cached.matches.len());
            #[cfg(test)]
            {
                self.scanned_blocks = self.scanned_blocks.saturating_add(1);
            }
            let matcher = self
                .matcher
                .as_ref()
                .expect("non-empty find query always owns a compiled matcher");
            let matches = matcher
                .find_iter(text)
                .map(|matched| FindMatch {
                    node_id: block.id,
                    utf8_range: matched.start()..matched.end(),
                })
                .collect::<Vec<_>>();
            self.total = self
                .total
                .saturating_sub(old_count)
                .saturating_add(matches.len());
            self.blocks.insert(
                block.id,
                BlockMatches {
                    revision: block.revision,
                    matches,
                },
            );
        }

        // An ordinary text edit cannot remove a text node, so keep the hot
        // path to the one changed block above. Structural edits are rarer and
        // may discard cached nodes; only they need a complete total refresh.
        if text_order_changed {
            let retained_nodes = self.text_order.iter().copied().collect::<HashSet<_>>();
            #[cfg(test)]
            {
                self.structural_membership_work = self
                    .structural_membership_work
                    .saturating_add(self.blocks.len());
            }
            self.blocks
                .retain(|node_id, _| retained_nodes.contains(node_id));
            self.total = self
                .text_order
                .iter()
                .filter_map(|node_id| self.blocks.get(node_id))
                .map(|cached| cached.matches.len())
                .sum();
        }
        self.primary = old_primary
            .as_ref()
            .and_then(|previous| self.cursor_for_match(previous))
            .or_else(|| self.first_cursor());
    }

    pub fn next(&mut self) -> Option<&FindMatch> {
        let total = self.total;
        if total == 0 {
            self.primary = None;
            return None;
        }
        let index = self.primary_index().map_or(0, |index| (index + 1) % total);
        self.primary = self.cursor_for_global_index(index);
        self.primary()
    }

    pub fn previous(&mut self) -> Option<&FindMatch> {
        let total = self.total;
        if total == 0 {
            self.primary = None;
            return None;
        }
        let index = self
            .primary_index()
            .map_or(total.saturating_sub(1), |index| (index + total - 1) % total);
        self.primary = self.cursor_for_global_index(index);
        self.primary()
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.case_sensitive = false;
        self.matcher = None;
        self.blocks.clear();
        self.text_order.clear();
        self.total = 0;
        self.primary = None;
        #[cfg(test)]
        {
            self.scanned_blocks = 0;
            self.matcher_compiles = 0;
        }
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn summary(&self) -> FindSummary {
        FindSummary {
            total: self.total(),
            primary_index: self.primary_index(),
        }
    }

    pub fn primary(&self) -> Option<&FindMatch> {
        let cursor = self.primary?;
        self.blocks
            .get(&cursor.node_id)
            .and_then(|cached| cached.matches.get(cursor.index))
    }

    pub fn matches(&self) -> impl Iterator<Item = &FindMatch> {
        self.text_order.iter().flat_map(|node_id| {
            self.blocks
                .get(node_id)
                .into_iter()
                .flat_map(|cached| cached.matches.iter())
        })
    }

    /// Return only the sorted literal matches which overlap a shaped byte
    /// interval. Renderers use this for dense paragraphs so scrolling does
    /// not create geometry for every note-wide match each frame.
    pub fn matches_for_node_in_range(
        &self,
        node_id: NodeId,
        utf8_range: Range<usize>,
    ) -> impl Iterator<Item = (&FindMatch, bool)> {
        let primary = self.primary;
        self.blocks
            .get(&node_id)
            .into_iter()
            .flat_map(move |cached| {
                let first = cached
                    .matches
                    .partition_point(|found| found.utf8_range.end <= utf8_range.start);
                let last = cached
                    .matches
                    .partition_point(|found| found.utf8_range.start < utf8_range.end);
                cached
                    .matches
                    .get(first..last)
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(move |(index, found)| {
                        let index = first + index;
                        (found, primary == Some(FindCursor { node_id, index }))
                    })
            })
    }

    fn first_cursor(&self) -> Option<FindCursor> {
        self.text_order.iter().find_map(|node_id| {
            self.blocks.get(node_id).and_then(|cached| {
                (!cached.matches.is_empty()).then_some(FindCursor {
                    node_id: *node_id,
                    index: 0,
                })
            })
        })
    }

    fn primary_index(&self) -> Option<usize> {
        let primary = self.primary?;
        let mut index = 0usize;
        for node_id in &self.text_order {
            let cached = self.blocks.get(node_id)?;
            if *node_id == primary.node_id {
                return (primary.index < cached.matches.len()).then_some(index + primary.index);
            }
            index = index.saturating_add(cached.matches.len());
        }
        None
    }

    fn cursor_for_global_index(&self, mut index: usize) -> Option<FindCursor> {
        for node_id in &self.text_order {
            let cached = self.blocks.get(node_id)?;
            if index < cached.matches.len() {
                return Some(FindCursor {
                    node_id: *node_id,
                    index,
                });
            }
            index = index.saturating_sub(cached.matches.len());
        }
        None
    }

    fn cursor_for_match(&self, previous: &FindMatch) -> Option<FindCursor> {
        let cached = self.blocks.get(&previous.node_id)?;
        cached
            .matches
            .iter()
            .position(|current| current == previous)
            .or_else(|| {
                cached
                    .matches
                    .iter()
                    .position(|current| current.utf8_range.start >= previous.utf8_range.start)
            })
            .or_else(|| cached.matches.len().checked_sub(1))
            .map(|index| FindCursor {
                node_id: previous.node_id,
                index,
            })
    }

    #[cfg(test)]
    pub(crate) fn scanned_blocks_for_test(&self) -> usize {
        self.scanned_blocks
    }

    #[cfg(test)]
    pub(crate) fn matcher_compiles_for_test(&self) -> usize {
        self.matcher_compiles
    }

    #[cfg(test)]
    pub(crate) fn structural_membership_work_for_test(&self) -> usize {
        self.structural_membership_work
    }
}

fn same_text_order(document: &Document, previous: &[NodeId]) -> bool {
    let mut previous = previous.iter().copied();
    for block in document.blocks() {
        if block.content.as_text().is_some() && previous.next() != Some(block.id) {
            return false;
        }
    }
    previous.next().is_none()
}

fn literal_matcher(query: &str, case_sensitive: bool) -> Result<regex::Regex, FindError> {
    let mut builder = RegexBuilder::new(&regex::escape(query));
    builder.case_insensitive(!case_sensitive).unicode(true);
    builder.build().map_err(|_| FindError::MatcherUnavailable)
}
