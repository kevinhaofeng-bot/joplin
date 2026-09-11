//! Deterministic local-save timing for one retained native note session.
//!
//! SQLite remains synchronous and authoritative in `app-lite-core`; this
//! coordinator only decides *when* the session writes a readable journal or
//! compacts it into a canonical snapshot.  Keeping time behind this small
//! seam gives production a monotonic clock while tests advance without sleeps.

use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const JOURNAL_DELAY: Duration = Duration::from_millis(100);
pub const SETTLED_SNAPSHOT_DELAY: Duration = Duration::from_millis(500);
pub const HARD_SNAPSHOT_DELAY: Duration = Duration::from_secs(15);

pub(crate) trait SaveClock: Send + Sync {
    fn now(&self) -> Duration;
}

#[derive(Debug)]
pub(crate) struct SystemSaveClock {
    origin: Instant,
}

impl Default for SystemSaveClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl SaveClock for SystemSaveClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// Test-only-in-practice controllable monotonic clock. It is kept available
/// to sibling crate tests rather than smuggling sleeps or wall-clock hooks
/// through production code.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct ManualSaveClock {
    now: Mutex<Duration>,
}

#[cfg(test)]
impl ManualSaveClock {
    pub(crate) fn advance(&self, elapsed: Duration) {
        let mut now = self.now.lock().expect("manual save clock mutex poisoned");
        *now = now.saturating_add(elapsed);
    }
}

#[cfg(test)]
impl SaveClock for ManualSaveClock {
    fn now(&self) -> Duration {
        *self.now.lock().expect("manual save clock mutex poisoned")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SaveState {
    Clean,
    Dirty,
    Journaling,
    Snapshotting,
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlushReason {
    NoteSwitch,
    WindowClose,
    Quit,
    Delete,
    ManualSync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveWork {
    Journal { generation: i64 },
    Snapshot { generation: i64 },
}

/// Generation-aware timing state. A session owns one coordinator; its
/// generation is intentionally local rather than a global timestamp, which
/// prevents an old timer callback from being mistaken for a newer session's
/// content.
pub(crate) struct SaveCoordinator {
    clock: Arc<dyn SaveClock>,
    state: SaveState,
    generation: i64,
    dirty_started_at: Option<Duration>,
    last_edit_at: Option<Duration>,
    journaled_generation: Option<i64>,
}

impl SaveCoordinator {
    pub(crate) fn new(clock: Arc<dyn SaveClock>) -> Self {
        Self {
            clock,
            state: SaveState::Clean,
            generation: 0,
            dirty_started_at: None,
            last_edit_at: None,
            journaled_generation: None,
        }
    }

    pub(crate) fn state(&self) -> SaveState {
        self.state.clone()
    }

    pub(crate) fn mark_dirty(&mut self) -> i64 {
        let now = self.clock.now();
        self.generation = self.generation.saturating_add(1).max(1);
        if self.dirty_started_at.is_none() {
            self.dirty_started_at = Some(now);
        }
        self.last_edit_at = Some(now);
        self.state = SaveState::Dirty;
        self.generation
    }

    /// A recovered journal contains the entire current document. It is dirty
    /// until a normal snapshot compacts it, but it must not be needlessly
    /// re-journaled before that first snapshot.
    pub(crate) fn restore_journaled(&mut self, generation: i64) {
        let now = self.clock.now();
        self.generation = self.generation.max(generation).max(1);
        self.dirty_started_at = Some(now);
        self.last_edit_at = Some(now);
        self.journaled_generation = Some(self.generation);
        self.state = SaveState::Dirty;
    }

    pub(crate) fn due_work(&self) -> Option<SaveWork> {
        if matches!(self.state, SaveState::Clean | SaveState::Failed(_)) {
            return None;
        }
        let now = self.clock.now();
        let generation = self.generation;
        let dirty_started_at = self.dirty_started_at?;
        let last_edit_at = self.last_edit_at?;
        if self.journaled_generation != Some(generation)
            && now.saturating_sub(last_edit_at) >= JOURNAL_DELAY
        {
            return Some(SaveWork::Journal { generation });
        }
        if now.saturating_sub(last_edit_at) >= SETTLED_SNAPSHOT_DELAY
            || now.saturating_sub(dirty_started_at) >= HARD_SNAPSHOT_DELAY
        {
            return Some(SaveWork::Snapshot { generation });
        }
        None
    }

    pub(crate) fn begin(&mut self, work: SaveWork) -> bool {
        let generation = match work {
            SaveWork::Journal { generation } | SaveWork::Snapshot { generation } => generation,
        };
        if generation != self.generation || matches!(self.state, SaveState::Failed(_)) {
            return false;
        }
        self.state = match work {
            SaveWork::Journal { .. } => SaveState::Journaling,
            SaveWork::Snapshot { .. } => SaveState::Snapshotting,
        };
        true
    }

    pub(crate) fn journaled(&mut self, generation: i64) {
        if generation == self.generation {
            self.journaled_generation = Some(generation);
            self.state = SaveState::Dirty;
        }
    }

    pub(crate) fn snapshotted(&mut self, generation: i64) {
        if generation == self.generation {
            self.state = SaveState::Clean;
            self.dirty_started_at = None;
            self.last_edit_at = None;
            self.journaled_generation = None;
        } else {
            self.state = SaveState::Dirty;
        }
    }

    pub(crate) fn fail(&mut self, error: impl Into<String>) {
        self.state = SaveState::Failed(error.into());
    }

    pub(crate) fn force_snapshot(&mut self) -> Option<SaveWork> {
        if matches!(self.state, SaveState::Clean) {
            return None;
        }
        let work = SaveWork::Snapshot {
            generation: self.generation,
        };
        self.begin(work).then_some(work)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_schedule_journals_then_settles_or_hits_the_hard_maximum() {
        let clock = Arc::new(ManualSaveClock::default());
        let mut save = SaveCoordinator::new(clock.clone());
        let generation = save.mark_dirty();
        clock.advance(Duration::from_millis(99));
        assert_eq!(save.due_work(), None);
        clock.advance(Duration::from_millis(1));
        let journal = SaveWork::Journal { generation };
        assert_eq!(save.due_work(), Some(journal));
        assert!(save.begin(journal));
        save.journaled(generation);
        clock.advance(Duration::from_millis(399));
        assert_eq!(save.due_work(), None);
        clock.advance(Duration::from_millis(1));
        assert_eq!(save.due_work(), Some(SaveWork::Snapshot { generation }));
        assert!(save.begin(SaveWork::Snapshot { generation }));
        save.snapshotted(generation);
        assert_eq!(save.state(), SaveState::Clean);
    }
}
