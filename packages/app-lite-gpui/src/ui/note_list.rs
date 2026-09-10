//! Helpers shared by the real GPUI uniform list and its mounted tests.
//!
//! The list construction itself belongs to `LibraryShell`, because every card
//! click must travel through that shell's single action reducer.

use crate::app::ListViewMode;

pub const fn fixed_card_height(mode: ListViewMode) -> f32 {
    crate::ui::note_card::fixed_height(mode)
}

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
static CONSTRUCTED_ITEMS: AtomicUsize = AtomicUsize::new(0);

/// Called only for the range requested by GPUI's `uniform_list` processor.
/// Keeping the instrumentation next to the virtual-list seam makes a future
/// accidental eager iterator visible in the 1,662-card mounted test.
pub fn record_constructed_items(count: usize) {
    #[cfg(test)]
    CONSTRUCTED_ITEMS.fetch_add(count, Ordering::Relaxed);
    #[cfg(not(test))]
    let _ = count;
}

#[cfg(test)]
pub fn reset_constructed_items_for_test() {
    CONSTRUCTED_ITEMS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub fn constructed_items_for_test() -> usize {
    CONSTRUCTED_ITEMS.load(Ordering::Relaxed)
}
