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
//! sync until a manual sync succeeds. Like the reference clients' resume
//! from background, waking from sleep or a change of network (the local
//! address of the default route) reconnects at once and syncs, instead of
//! waiting for the silence timeout.

use super::*;
use app_lite_protocol::{SyncEvent, TransportError};
use futures::StreamExt;
use futures::channel::mpsc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const HELLO_TIMEOUT: Duration = Duration::from_secs(15);
/// More wall-clock time than this between two 5 s checks means the Mac
/// slept.
const WAKE_GAP: Duration = Duration::from_secs(30);

/// What the link watches besides the stream; replaced in tests.
pub(crate) trait LinkEnvironment {
    fn wall_clock(&self) -> std::time::SystemTime;
    /// Changes when the machine moves to another network; None offline.
    fn network(&self) -> Option<std::net::IpAddr>;
}

pub(crate) struct SystemEnvironment;

impl LinkEnvironment for SystemEnvironment {
    fn wall_clock(&self) -> std::time::SystemTime {
        std::time::SystemTime::now()
    }

    /// The source address the default route would use. A UDP connect
    /// only selects the route; nothing is sent.
    fn network(&self) -> Option<std::net::IpAddr> {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect("192.0.2.1:9").ok()?;
        socket.local_addr().ok().map(|address| address.ip())
    }
}
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
    config: Option<sync::SyncConfig>,
    environment: Box<dyn LinkEnvironment>,
    last_wall_clock: Option<std::time::SystemTime>,
    last_network: Option<Option<std::net::IpAddr>>,
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
            config: None,
            environment: Box::new(SystemEnvironment),
            last_wall_clock: None,
            last_network: None,
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
    /// Opens the link when off; a changed configuration replaces it.
    pub(super) fn ensure_event_link(&mut self, config: &sync::SyncConfig, cx: &mut Context<Self>) {
        if self.event_link.config.as_ref() != Some(config) {
            self.stop_event_link();
            self.event_link.reconnect.reset();
            self.event_link.config = Some(config.clone());
        }
        if self.event_link.state == LinkState::Off {
            self.open_event_link(cx);
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

    /// Reconnects and syncs at once after sleep or a network change. Called
    /// from the 5 s automatic-sync check.
    pub(super) fn check_link_environment(&mut self) {
        let wall = self.event_link.environment.wall_clock();
        let network = self.event_link.environment.network();
        let woke = self
            .event_link
            .last_wall_clock
            .and_then(|previous| wall.duration_since(previous).ok())
            .is_some_and(|gap| gap > WAKE_GAP);
        let moved = self
            .event_link
            .last_network
            .is_some_and(|previous| previous != network);
        self.event_link.last_wall_clock = Some(wall);
        self.event_link.last_network = Some(network);
        if woke || moved {
            self.stop_event_link();
            self.event_link.reconnect.reset();
            self.auto_sync.retry_at = None;
            self.auto_sync.backoff = None;
            // Makes the periodic pull due now.
            self.auto_sync.last_finished = None;
            #[cfg(test)]
            {
                self.event_link_recoveries += 1;
            }
        }
    }

    /// A manual sync: skip any backoff wait.
    pub(super) fn reconnect_event_link_now(&mut self) {
        if matches!(self.event_link.state, LinkState::Waiting) {
            self.stop_event_link();
        }
        self.event_link.reconnect.reset();
    }

    fn open_event_link(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.event_link.config.clone() else {
            return;
        };
        let generation = self.event_link.current.fetch_add(1, Ordering::AcqRel) + 1;
        self.event_link.state = LinkState::Connecting;
        self.event_link.retry = None;
        #[cfg(test)]
        {
            self.event_link_opens += 1;
        }
        let (sender, mut receiver) = mpsc::unbounded();
        let current = Arc::clone(&self.event_link.current);
        std::thread::spawn(move || {
            let stream = config
                .transport()
                .and_then(|transport| transport.open_events());
            let mut stream = match stream {
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
                        shell.on_link_message(generation, message, shell_cx)
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
                self.open_event_link(cx);
                false
            }
            // Retrying cannot fix credentials or an untrusted certificate.
            LinkMessage::Ended(TransportError::Unauthorized) => {
                self.pause_after_link_refusal(
                    "同步失败：服务器拒绝了凭据，请检查同步设置中的 token。".into(),
                    cx,
                );
                false
            }
            LinkMessage::Ended(TransportError::Permanent(reason)) => {
                self.pause_after_link_refusal(sync::refused_reason_message(&reason), cx);
                false
            }
            LinkMessage::Ended(_) => {
                self.event_link.current.fetch_add(1, Ordering::AcqRel);
                self.event_link.state = LinkState::Waiting;
                let delay = self.event_link.reconnect.next_delay(jitter());
                self.event_link.retry = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    let _ = this.update(cx, |shell, shell_cx| {
                        if shell.event_link.state == LinkState::Waiting {
                            shell.open_event_link(shell_cx);
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
    fn pause_after_link_refusal(&mut self, message: String, cx: &mut Context<Self>) {
        self.stop_event_link();
        self.auto_sync.paused = true;
        self.sync_status = sync::ShellSyncStatus::Failed(message);
        cx.notify();
    }

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

    pub(super) fn event_link_recoveries_for_test(&self) -> usize {
        self.event_link_recoveries
    }

    pub(super) fn set_link_environment_for_test(&mut self, environment: Box<dyn LinkEnvironment>) {
        self.event_link.environment = environment;
        self.event_link.last_wall_clock = None;
        self.event_link.last_network = None;
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
