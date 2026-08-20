//! Background scheduling for harness release checks and native self-updates.
//!
//! The application model only receives completed observations. Clocks, network requests, and
//! child processes stay behind this runtime adapter.

use std::{
    collections::HashSet,
    sync::mpsc::{self, SyncSender},
    thread,
    time::{Duration, Instant},
};

use svarm_agent::{
    AgentKind,
    harness_update::{self, HarnessUpdated, HarnessVersion},
};

use crate::agents::ClientEvent;

const RESULT_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) enum HarnessUpdateEvent {
    Checked(Vec<(AgentKind, Result<HarnessVersion, String>)>),
    Updated {
        kind: AgentKind,
        result: Result<HarnessUpdated, String>,
    },
}

enum Request {
    Check(Vec<AgentKind>),
    Update(AgentKind),
}

pub(crate) struct HarnessUpdateRuntime {
    requests: mpsc::Sender<Request>,
    results: mpsc::Receiver<HarnessUpdateEvent>,
    checking: bool,
    updating: HashSet<AgentKind>,
    interval: Duration,
    next_check_at: Instant,
}

impl HarnessUpdateRuntime {
    pub fn new(
        events: SyncSender<ClientEvent>,
        interval_minutes: u64,
        now: Instant,
    ) -> Result<Self, String> {
        let (requests, receiver) = mpsc::channel();
        let (result_sender, results) = mpsc::channel();
        thread::Builder::new()
            .name("svarm-harness-updates".into())
            .spawn(move || worker(receiver, result_sender, events))
            .map_err(|error| format!("could not start harness update worker: {error}"))?;
        let interval = interval_duration(interval_minutes);
        Ok(Self {
            requests,
            results,
            checking: false,
            updating: HashSet::new(),
            interval,
            next_check_at: now + interval,
        })
    }

    pub fn request_check(&mut self, installed: &[AgentKind], now: Instant) -> bool {
        if self.checking || !self.updating.is_empty() || installed.is_empty() {
            return false;
        }
        if self
            .requests
            .send(Request::Check(installed.to_vec()))
            .is_err()
        {
            return false;
        }
        self.checking = true;
        self.next_check_at = now + self.interval;
        true
    }

    pub fn request_update(&mut self, kind: AgentKind) -> bool {
        if self.checking || !self.updating.insert(kind) {
            return false;
        }
        if self.requests.send(Request::Update(kind)).is_err() {
            self.updating.remove(&kind);
            return false;
        }
        true
    }

    pub fn finish(&mut self, event: &HarnessUpdateEvent) {
        match event {
            HarnessUpdateEvent::Checked(_) => self.checking = false,
            HarnessUpdateEvent::Updated { kind, .. } => {
                self.updating.remove(kind);
            }
        }
    }

    pub fn poll(&self) -> Vec<HarnessUpdateEvent> {
        self.results.try_iter().collect()
    }

    pub fn set_interval(&mut self, interval_minutes: u64, now: Instant) {
        self.interval = interval_duration(interval_minutes);
        self.next_check_at = now + self.interval;
    }

    pub fn tick(&mut self, installed: &[AgentKind], now: Instant) -> bool {
        if now < self.next_check_at {
            return false;
        }
        // Advancing the deadline even while another operation is active prevents a zero-timeout
        // retry loop. The next regular check will happen after one complete configured interval.
        self.next_check_at = now + self.interval;
        self.request_check(installed, now)
    }

    pub fn next_timeout(&self, now: Instant) -> Duration {
        let scheduled = self.next_check_at.saturating_duration_since(now);
        if self.checking || !self.updating.is_empty() {
            scheduled.min(RESULT_POLL_INTERVAL)
        } else {
            scheduled
        }
    }
}

fn worker(
    receiver: mpsc::Receiver<Request>,
    results: mpsc::Sender<HarnessUpdateEvent>,
    events: SyncSender<ClientEvent>,
) {
    while let Ok(request) = receiver.recv() {
        let event = match request {
            Request::Check(kinds) => HarnessUpdateEvent::Checked(
                kinds
                    .into_iter()
                    .map(|kind| (kind, harness_update::check(kind)))
                    .collect(),
            ),
            Request::Update(kind) => HarnessUpdateEvent::Updated {
                kind,
                result: harness_update::update(kind),
            },
        };
        if results.send(event).is_err() {
            break;
        }
        let _ = events.send(ClientEvent::HarnessUpdateReady);
    }
}

fn interval_duration(minutes: u64) -> Duration {
    Duration::from_secs(minutes.clamp(1, 525_600) * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(
        now: Instant,
    ) -> (
        HarnessUpdateRuntime,
        mpsc::Receiver<Request>,
        mpsc::Sender<HarnessUpdateEvent>,
    ) {
        let (requests, receiver) = mpsc::channel();
        let (result_sender, results) = mpsc::channel();
        (
            HarnessUpdateRuntime {
                requests,
                results,
                checking: false,
                updating: HashSet::new(),
                interval: Duration::from_secs(3_600),
                next_check_at: now + Duration::from_secs(3_600),
            },
            receiver,
            result_sender,
        )
    }

    #[test]
    fn interval_is_never_zero_and_untrusted_settings_are_bounded() {
        assert_eq!(interval_duration(0), Duration::from_secs(60));
        assert_eq!(
            interval_duration(u64::MAX),
            Duration::from_secs(525_600 * 60)
        );
    }

    #[test]
    fn startup_check_is_immediate_and_regular_checks_wait_one_interval() {
        let now = Instant::now();
        let (mut runtime, requests, _results) = runtime(now);

        assert!(runtime.request_check(&[AgentKind::Codex, AgentKind::Pi], now));
        assert!(matches!(
            requests.recv().unwrap(),
            Request::Check(kinds) if kinds == [AgentKind::Codex, AgentKind::Pi]
        ));
        runtime.finish(&HarnessUpdateEvent::Checked(Vec::new()));
        assert!(!runtime.tick(&[AgentKind::Codex], now + Duration::from_secs(3_599)));
        assert!(runtime.tick(&[AgentKind::Codex], now + Duration::from_secs(3_600)));
        assert!(matches!(
            requests.recv().unwrap(),
            Request::Check(kinds) if kinds == [AgentKind::Codex]
        ));
    }

    #[test]
    fn changing_the_interval_resets_the_deadline_without_catchup() {
        let now = Instant::now();
        let (mut runtime, _requests, _results) = runtime(now);

        runtime.set_interval(15, now + Duration::from_secs(10));

        assert_eq!(
            runtime.next_timeout(now + Duration::from_secs(10)),
            Duration::from_secs(15 * 60)
        );
    }

    #[test]
    fn completed_work_can_be_polled_even_if_its_wakeup_signal_is_consumed() {
        let now = Instant::now();
        let (runtime, _requests, results) = runtime(now);
        results
            .send(HarnessUpdateEvent::Checked(Vec::new()))
            .unwrap();

        assert!(matches!(
            runtime.poll().as_slice(),
            [HarnessUpdateEvent::Checked(results)] if results.is_empty()
        ));
    }
}
