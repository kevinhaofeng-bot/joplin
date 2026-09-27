//! Keeps one change-notification stream open to the sync server
//! (`GET /v1/events`) so remote edits arrive in seconds instead of at the
//! next 5-minute pull. A notification is only a hint: the sync that follows
//! still pulls from the local cursor, so anything missed while the link was
//! down arrives with the next hint or the periodic pull.
//!
//! Weak-network rules, after the stego-reader clients' connection
//! controller: one link at a time, told apart by generation so a
//! superseded reader's events are ignored and its thread exits; `hello`
//! must arrive within 15 s; the reader calls a stream dead after
//! `EVENTS_SILENCE_TIMEOUT` without a byte; reconnects back off 3, 5, 10,
//! 30, 60 s plus up to 1 s jitter, and reset only on `hello`; the server's
//! scheduled `bye` reconnects at once; refused credentials pause automatic
//! sync until a manual sync succeeds.

use super::*;
use app_lite_protocol::client::HttpTransport;
use app_lite_protocol::{SyncEvent, TransportError};
use futures::StreamExt;
use futures::channel::mpsc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const HELLO_TIMEOUT: Duration = Duration::from_secs(15);
const RECONNECT_STEPS_SECONDS: [u64; 5] = [3, 5, 10, 30, 60];

/// Reconnect delays: the fixed steps, the last repeated, with up to 1 s of
/// jitter from the second attempt so devices that lost the same link do not
/// return in lockstep.
#[derive(Default, Debug)]
pub(crate) struct Reconnect {
    attempts: usize,
}

impl Reconnect {
    pub(crate) fn next_delay(&mut self, jitter: f64) -> Duration {
        let step = RECONNECT_STEPS_SECONDS[self.attempts.min(RECONNECT_STEPS_SECONDS.len() - 1)];
        let jitter = if self.attempts > 0 {
            Duration::from_secs_f64(jitter.clamp(0.0, 1.0))
        } else {
            Duration::ZERO
        };
        self.attempts += 1;
        Duration::from_secs(step) + jitter
    }

    pub(crate) fn reset(&mut self) {
        self.attempts = 0;
    }
}

fn jitter() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| {
            f64::from(elapsed.subsec_micros() % 1000) / 1000.0
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkState {
    Off,
    Connecting,
    Connected,
    Waiting,
}

pub(crate) struct EventLink {
    /// Shared with reader threads; a thread whose generation is no longer
    /// current stops at its next event or heartbeat.
    current: Arc<AtomicU64>,
    state: LinkState,
    reconnect: Reconnect,
    task: Option<Task<()>>,
    retry: Option<Task<()>>,
    last_event: Option<Instant>,
}

impl Default for EventLink {
    fn default() -> Self {
        Self {
            current: Arc::new(AtomicU64::new(0)),
            state: LinkState::Off,
            reconnect: Reconnect::default(),
            task: None,
            retry: None,
            last_event: None,
        }
    }
}

impl Drop for EventLink {
    fn drop(&mut self) {
        self.current.fetch_add(1, Ordering::AcqRel);
    }
}

enum LinkMessage {
    Event(SyncEvent),
    Ended(TransportError),
}

impl LibraryShell {
    pub(super) fn ensure_event_link(&mut self, url: &str, token: &str, cx: &mut Context<Self>) {
        if self.event_link.state == LinkState::Off {
            self.open_event_link(url.to_owned(), token.to_owned(), cx);
        }
    }

    pub(super) fn stop_event_link(&mut self) {
        if self.event_link.state != LinkState::Off {
            self.event_link.current.fetch_add(1, Ordering::AcqRel);
            self.event_link.state = LinkState::Off;
            self.event_link.task = None;
            self.event_link.retry = None;
        }
    }

    /// A manual sync: skip any backoff wait.
    pub(super) fn reconnect_event_link_now(&mut self) {
        if matches!(self.event_link.state, LinkState::Waiting) {
            self.stop_event_link();
        }
        self.event_link.reconnect.reset();
    }

    fn open_event_link(&mut self, url: String, token: String, cx: &mut Context<Self>) {
        let generation = self.event_link.current.fetch_add(1, Ordering::AcqRel) + 1;
        self.event_link.state = LinkState::Connecting;
        self.event_link.retry = None;
        #[cfg(test)]
        {
            self.event_link_opens += 1;
        }
        let (sender, mut receiver) = mpsc::unbounded();
        let current = Arc::clone(&self.event_link.current);
        let (thread_url, thread_token) = (url.clone(), token.clone());
        std::thread::spawn(move || {
            let transport = HttpTransport::new(&thread_url, &thread_token);
            let mut stream = match transport.open_events() {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = sender.unbounded_send(LinkMessage::Ended(error));
                    return;
                }
            };
            loop {
                let message = match stream.next_event() {
                    Ok(event) => LinkMessage::Event(event),
                    Err(error) => LinkMessage::Ended(error),
                };
                let ended = matches!(message, LinkMessage::Ended(_));
                if current.load(Ordering::Acquire) != generation
                    || sender.unbounded_send(message).is_err()
                    || ended
                {
                    return;
                }
            }
        });
        self.event_link.task = Some(cx.spawn(async move |this, cx| {
            let hello_deadline = cx.background_executor().timer(HELLO_TIMEOUT);
            let mut hello_deadline = std::pin::pin!(futures::FutureExt::fuse(hello_deadline));
            loop {
                let message = futures::select_biased! {
                    message = receiver.next() => message,
                    _ = hello_deadline => {
                        let waiting = this
                            .update(cx, |shell, _| shell.event_link.state == LinkState::Connecting)
                            .unwrap_or(false);
                        if !waiting {
                            continue;
                        }
                        Some(LinkMessage::Ended(TransportError::Retryable("no hello".into())))
                    }
                };
                let Some(message) = message else { return };
                let keep = this
                    .update(cx, |shell, shell_cx| {
                        shell.on_link_message(generation, message, &url, &token, shell_cx)
                    })
                    .unwrap_or(false);
                if !keep {
                    return;
                }
            }
        }));
    }

    /// Returns whether this generation's reader is still wanted.
    fn on_link_message(
        &mut self,
        generation: u64,
        message: LinkMessage,
        url: &str,
        token: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.event_link.current.load(Ordering::Acquire) != generation {
            return false;
        }
        self.event_link.last_event = Some(Instant::now());
        match message {
            LinkMessage::Event(SyncEvent::Hello { head }) => {
                self.event_link.state = LinkState::Connected;
                self.event_link.reconnect.reset();
                // The server answered: an earlier unreachable-server backoff
                // no longer applies.
                self.auto_sync.retry_at = None;
                self.auto_sync.backoff = None;
                self.note_remote_head(head, cx);
                true
            }
            LinkMessage::Event(SyncEvent::Changed { head }) => {
                self.note_remote_head(head, cx);
                true
            }
            LinkMessage::Event(SyncEvent::Bye) => {
                self.event_link.state = LinkState::Off;
                self.open_event_link(url.to_owned(), token.to_owned(), cx);
                false
            }
            LinkMessage::Ended(TransportError::Unauthorized) => {
                self.stop_event_link();
                self.auto_sync.paused = true;
                self.sync_status = sync::ShellSyncStatus::Failed(
                    "同步失败：服务器拒绝了凭据，请检查同步设置中的 token。".into(),
                );
                cx.notify();
                false
            }
            LinkMessage::Ended(_) => {
                self.event_link.current.fetch_add(1, Ordering::AcqRel);
                self.event_link.state = LinkState::Waiting;
                let delay = self.event_link.reconnect.next_delay(jitter());
                let (url, token) = (url.to_owned(), token.to_owned());
                self.event_link.retry = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    let _ = this.update(cx, |shell, shell_cx| {
                        if shell.event_link.state == LinkState::Waiting {
                            shell.open_event_link(url, token, shell_cx);
                        }
                    });
                }));
                cx.notify();
                false
            }
        }
    }

    /// A head below the cursor means a server restored from a backup; the
    /// sync reconciles that too. Our own uploads leave the cursor at the
    /// head, so their notification causes no sync.
    fn note_remote_head(&mut self, head: u64, cx: &mut Context<Self>) {
        self.auto_sync.announced_head = Some(head);
        if self.remote_changes_announced(cx) {
            self.auto_sync_tick(cx);
        }
    }

    pub(super) fn event_link_text(&self) -> String {
        match self.event_link.state {
            LinkState::Off => "实时通知：未连接".into(),
            LinkState::Connecting => "实时通知：正在连接…".into(),
            LinkState::Connected => "实时通知：已连接，其他设备的修改会在几秒内同步过来".into(),
            LinkState::Waiting => format!(
                "实时通知：连接中断，第 {} 次重连等待中；期间每 5 分钟仍会同步",
                self.event_link.reconnect.attempts
            ),
        }
    }
}

#[cfg(test)]
impl LibraryShell {
    pub(super) fn event_link_state_for_test(&self) -> LinkState {
        self.event_link.state
    }

    pub(super) fn event_link_opens_for_test(&self) -> usize {
        self.event_link_opens
    }
}

#[cfg(test)]
mod tests {
    use super::Reconnect;
    use std::time::Duration;

    #[test]
    fn reconnect_delays_follow_the_steps_with_jitter_and_reset() {
        let mut reconnect = Reconnect::default();
        let delays: Vec<Duration> = (0..7).map(|_| reconnect.next_delay(0.5)).collect();
        let seconds: Vec<f64> = delays.iter().map(Duration::as_secs_f64).collect();
        assert_eq!(seconds, vec![3.0, 5.5, 10.5, 30.5, 60.5, 60.5, 60.5]);
        reconnect.reset();
        assert_eq!(reconnect.next_delay(0.9), Duration::from_secs(3));
    }
}
