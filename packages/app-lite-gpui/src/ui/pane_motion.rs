//! Presentation-only pane transition. Durable targets stay in AppModel;
//! neither frame sampling nor interruption performs a repository mutation.
use crate::app::PaneState;
use gpui::{AnyElement, App, InteractiveElement, IntoElement, ParentElement, Styled, div, px};
use std::time::{Duration, Instant};

const DURATION: Duration = Duration::from_millis(200);

pub(super) struct PaneMotion {
    target: PaneState,
    from: [f32; 2],
    started: Option<Instant>,
}

pub(super) struct PaneFrame {
    pub sidebar: f32,
    pub list: f32,
    pub animating: bool,
}

fn widths(panes: PaneState) -> [f32; 2] {
    [
        if panes.sidebar_visible {
            f32::from(panes.sidebar_width)
        } else {
            0.0
        },
        if panes.list_visible {
            f32::from(panes.list_width)
        } else {
            0.0
        },
    ]
}

impl PaneMotion {
    pub fn new(target: PaneState) -> Self {
        Self {
            target,
            from: widths(target),
            started: None,
        }
    }

    fn sample(&self, now: Instant) -> ([f32; 2], bool) {
        let to = widths(self.target);
        let Some(started) = self.started else {
            return (to, false);
        };
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= DURATION {
            return (to, false);
        }
        let t = elapsed.as_secs_f32() / DURATION.as_secs_f32();
        let eased = 1.0 - (1.0 - t).powi(4);
        (
            [
                self.from[0] + (to[0] - self.from[0]) * eased,
                self.from[1] + (to[1] - self.from[1]) * eased,
            ],
            true,
        )
    }

    pub fn frame(&mut self, target: PaneState, now: Instant, reduced: bool) -> PaneFrame {
        if target != self.target {
            // A reversal begins at the *current displayed* value. Starting
            // again at either persisted endpoint would visibly jump.
            self.from = self.sample(now).0;
            self.target = target;
            self.started = (self.from != widths(target)).then_some(now);
        }
        if reduced {
            self.started = None;
        }
        let (value, animating) = self.sample(now);
        if !animating {
            self.started = None;
        }
        PaneFrame {
            sidebar: value[0],
            list: value[1],
            animating,
        }
    }
}

/// Keep the sidebar/card contents at their full width while the outside
/// occupied span reveals them. This avoids reflowing Card rows/proxy tiers
/// on every frame; the parent clip also bounds pointer hit testing.
pub(super) fn reveal(id: &'static str, occupied: f32, full: u16, child: AnyElement) -> AnyElement {
    let full = f32::from(full);
    div()
        .id(id)
        .relative()
        .w(px(occupied))
        .min_w(px(0.0))
        .h_full()
        .flex_none()
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left(px(occupied - full))
                .top(px(0.0))
                .bottom(px(0.0))
                .w(px(full))
                .min_w(px(full))
                .child(child),
        )
        .into_any_element()
}

pub(super) fn reduced_motion(cx: &App) -> bool {
    #[cfg(test)]
    {
        // A deterministic input at the platform boundary. Existing tests
        // stay immediate; motion tests opt into actual frame progression.
        cx.try_global::<super::tests::PaneMotionPreferenceForTest>()
            .is_none_or(|p| p.0)
    }
    #[cfg(all(not(test), target_os = "macos"))]
    {
        use cocoa::base::{BOOL, YES, id, nil};
        use objc::{class, msg_send, sel, sel_impl};
        let _ = cx;
        // Called on the GPUI main thread. NSWorkspace is a shared singleton,
        // and these getters transfer no ownership or system-setting writes.
        unsafe {
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            if workspace == nil {
                return true;
            }
            let reduce: BOOL = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
            reduce == YES
        }
    }
    #[cfg(all(not(test), not(target_os = "macos")))]
    {
        let _ = cx;
        true
    }
}
