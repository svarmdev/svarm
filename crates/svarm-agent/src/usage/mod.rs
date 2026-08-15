//! Remaining subscription usage for the coding agents that publish it.
//!
//! Each provider is read through its own private interface, so every probe is isolated behind its
//! own parser and yields either windows the provider affirmatively reported or an explanation of
//! why it reported none. Probes run on a worker thread; the coordinator only drains results.

// Under test the transport is deliberately unreachable — probes must never launch a subprocess
// or open a socket — so the request-side entry points read as dead there. Their parsers, which
// carry the behaviour worth testing, are exercised by fixtures inside each module.
#[cfg_attr(test, allow(dead_code))]
mod claude;
#[cfg_attr(test, allow(dead_code))]
mod codex;
#[cfg_attr(test, allow(dead_code))]
mod grok;
#[cfg_attr(test, allow(dead_code))]
mod http;
mod time;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::mpsc::{Receiver, SyncSender, sync_channel},
    thread,
};

use crate::{
    AgentKind,
    protocol::{UsageOverview, UsageProviderReport, UsageReport},
};

/// How long a report stays fresh enough to skip re-probing on attach.
pub(crate) const USAGE_TTL_MS: u64 = 60_000;

/// The providers worth probing: installed, and known to publish limits.
pub(crate) fn usage_providers(available: &[AgentKind]) -> Vec<AgentKind> {
    AgentKind::ALL
        .into_iter()
        .filter(|kind| kind.reports_usage() && available.contains(kind))
        .collect()
}

pub(crate) struct UsageResult {
    pub kind: AgentKind,
    pub report: UsageReport,
}

/// Runs usage probes away from the server coordinator. One worker keeps the probes serial, so a
/// refresh never fires several subprocesses and HTTPS requests at the same instant.
pub(crate) struct UsageWorker {
    requests: SyncSender<AgentKind>,
    results: Receiver<UsageResult>,
}

impl UsageWorker {
    pub fn new() -> Self {
        let (requests, request_rx) = sync_channel::<AgentKind>(AgentKind::ALL.len());
        let (result_tx, results) = sync_channel(AgentKind::ALL.len());
        thread::Builder::new()
            .name("svarm-usage".into())
            .spawn(move || {
                while let Ok(kind) = request_rx.recv() {
                    let report = probe(kind);
                    if result_tx.send(UsageResult { kind, report }).is_err() {
                        break;
                    }
                }
            })
            .ok();
        Self { requests, results }
    }

    pub fn request(&self, kind: AgentKind) -> bool {
        self.requests.try_send(kind).is_ok()
    }

    pub fn try_result(&self) -> Option<UsageResult> {
        self.results.try_recv().ok()
    }
}

#[cfg(not(test))]
fn probe(kind: AgentKind) -> UsageReport {
    match kind {
        AgentKind::Claude => match crate::naming::home_directory() {
            Some(home) => claude::probe(std::path::Path::new(&home)),
            None => UsageReport::NotProbed,
        },
        AgentKind::Codex => codex::probe(),
        AgentKind::Grok => match crate::naming::home_directory() {
            Some(home) => grok::probe(std::path::Path::new(&home)),
            None => UsageReport::NotProbed,
        },
        _ => UsageReport::NotProbed,
    }
}

/// Tests must never launch a subprocess or reach the network. Each provider's parsing, which is
/// the part worth testing, is covered by fixtures in its own module.
#[cfg(test)]
fn probe(_kind: AgentKind) -> UsageReport {
    UsageReport::NotProbed
}

/// The server's view of usage: last report per provider, plus which probes are in flight.
///
/// Keeping the previous report while a probe runs is what lets the interface show the last known
/// numbers with a refreshing indicator instead of blanking on every refresh.
pub(crate) struct UsageCache {
    enabled: bool,
    worker: UsageWorker,
    observations: BTreeMap<AgentKind, (UsageReport, u64)>,
    in_flight: BTreeSet<AgentKind>,
}

impl UsageCache {
    /// Usage probes reach out to provider services, so they can be switched off entirely.
    pub fn from_environment() -> Self {
        Self::with_enabled(enabled_by(std::env::var("SVARM_USAGE").ok().as_deref()))
    }

    fn with_enabled(enabled: bool) -> Self {
        Self {
            enabled,
            worker: UsageWorker::new(),
            observations: BTreeMap::new(),
            in_flight: BTreeSet::new(),
        }
    }

    /// Schedule a probe for each provider not already being probed. Returns whether anything was
    /// scheduled, which is what makes the refreshing indicator worth broadcasting.
    pub fn refresh(&mut self, kinds: &[AgentKind]) -> bool {
        if !self.enabled {
            return false;
        }
        let mut scheduled = false;
        for kind in kinds {
            if self.in_flight.contains(kind) {
                continue;
            }
            if self.worker.request(*kind) {
                self.in_flight.insert(*kind);
                scheduled = true;
            }
        }
        scheduled
    }

    /// Refresh only what has gone stale, so repeated attaches do not stampede the providers.
    pub fn refresh_stale(&mut self, kinds: &[AgentKind], now_ms: u64) -> bool {
        let stale: Vec<AgentKind> = kinds
            .iter()
            .copied()
            .filter(|kind| match self.observations.get(kind) {
                Some((_, observed_at)) => now_ms.saturating_sub(*observed_at) >= USAGE_TTL_MS,
                None => true,
            })
            .collect();
        self.refresh(&stale)
    }

    /// Apply a finished probe. Returns whether the cache changed.
    pub fn absorb(&mut self, result: UsageResult, now_ms: u64) -> bool {
        self.in_flight.remove(&result.kind);
        self.observations
            .insert(result.kind, (result.report, now_ms));
        true
    }

    pub fn try_result(&self) -> Option<UsageResult> {
        self.worker.try_result()
    }

    /// The overview to send to clients, restricted to providers that are installed.
    pub fn overview(&self, installed: &[AgentKind]) -> UsageOverview {
        UsageOverview {
            providers: usage_providers(installed)
                .into_iter()
                .map(|kind| {
                    let observation = self.observations.get(&kind);
                    UsageProviderReport {
                        kind,
                        report: if self.enabled {
                            observation
                                .map(|(report, _)| report.clone())
                                .unwrap_or(UsageReport::NotProbed)
                        } else {
                            disabled_report()
                        },
                        observed_at_ms: observation.map(|(_, at)| *at),
                        refreshing: self.in_flight.contains(&kind),
                    }
                })
                .collect(),
        }
    }
}

fn disabled_report() -> UsageReport {
    UsageReport::Unavailable(crate::protocol::UsageUnavailable {
        reason: crate::protocol::UsageUnavailableReason::Unsupported,
        message: "Usage checks are switched off.".to_owned(),
        evidence: "SVARM_USAGE is set to off".to_owned(),
    })
}

fn enabled_by(value: Option<&str>) -> bool {
    !matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("0" | "off" | "false" | "no")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{UsageEvidence, UsageWindow};

    fn evidence(percent: f64) -> UsageReport {
        UsageReport::Available(UsageEvidence {
            plan: None,
            windows: vec![UsageWindow::from_percent("Weekly", percent)],
            notes: Vec::new(),
            source: "test".into(),
        })
    }

    fn percent_of(report: &UsageReport) -> Option<u16> {
        match report {
            UsageReport::Available(evidence) => Some(evidence.windows[0].used_tenths),
            _ => None,
        }
    }

    #[test]
    fn only_installed_providers_that_publish_limits_are_probed() {
        assert_eq!(
            usage_providers(&AgentKind::ALL),
            [AgentKind::Codex, AgentKind::Claude, AgentKind::Grok]
        );
        assert_eq!(
            usage_providers(&[AgentKind::Claude, AgentKind::Pi]),
            [AgentKind::Claude]
        );
        assert_eq!(
            usage_providers(&[AgentKind::Grok, AgentKind::OpenCode]),
            [AgentKind::Grok]
        );
        assert!(usage_providers(&[]).is_empty());
    }

    #[test]
    fn an_overview_lists_installed_providers_as_not_probed_before_any_result() {
        let cache = UsageCache::with_enabled(true);
        let overview = cache.overview(&[AgentKind::Claude, AgentKind::OpenCode]);
        assert_eq!(overview.providers.len(), 1);
        assert_eq!(overview.providers[0].kind, AgentKind::Claude);
        assert_eq!(overview.providers[0].report, UsageReport::NotProbed);
        assert_eq!(overview.providers[0].observed_at_ms, None);
        assert!(!overview.providers[0].refreshing);
    }

    #[test]
    fn a_probe_in_flight_keeps_the_previous_report_and_marks_it_refreshing() {
        let mut cache = UsageCache::with_enabled(true);
        cache.absorb(
            UsageResult {
                kind: AgentKind::Claude,
                report: evidence(42.0),
            },
            1_000,
        );

        cache.in_flight.insert(AgentKind::Claude);
        let overview = cache.overview(&[AgentKind::Claude]);
        let provider = &overview.providers[0];
        assert!(provider.refreshing, "the indicator must be set");
        assert_eq!(
            percent_of(&provider.report),
            Some(420),
            "the previous numbers must survive a refresh"
        );
        assert_eq!(provider.observed_at_ms, Some(1_000));
    }

    #[test]
    fn a_finished_probe_replaces_the_report_and_clears_the_indicator() {
        let mut cache = UsageCache::with_enabled(true);
        cache.in_flight.insert(AgentKind::Claude);
        cache.absorb(
            UsageResult {
                kind: AgentKind::Claude,
                report: evidence(17.0),
            },
            2_000,
        );

        let provider = &cache.overview(&[AgentKind::Claude]).providers[0];
        assert!(!provider.refreshing);
        assert_eq!(percent_of(&provider.report), Some(170));
        assert_eq!(provider.observed_at_ms, Some(2_000));
    }

    #[test]
    fn refresh_stale_skips_fresh_observations_but_not_old_ones() {
        let mut cache = UsageCache::with_enabled(true);
        cache.absorb(
            UsageResult {
                kind: AgentKind::Claude,
                report: evidence(1.0),
            },
            10_000,
        );

        // Well inside the TTL: nothing to do.
        assert!(!cache.refresh_stale(&[AgentKind::Claude], 10_000 + USAGE_TTL_MS - 1));
        assert!(!cache.overview(&[AgentKind::Claude]).providers[0].refreshing);

        // At the TTL boundary the observation has aged out.
        assert!(cache.refresh_stale(&[AgentKind::Claude], 10_000 + USAGE_TTL_MS));
        assert!(cache.overview(&[AgentKind::Claude]).providers[0].refreshing);
    }

    #[test]
    fn a_provider_already_being_probed_is_not_scheduled_twice() {
        let mut cache = UsageCache::with_enabled(true);
        assert!(cache.refresh(&[AgentKind::Claude]));
        assert!(!cache.refresh(&[AgentKind::Claude]));
    }

    #[test]
    fn switching_usage_off_stops_probes_and_says_so() {
        let mut cache = UsageCache::with_enabled(false);
        assert!(!cache.refresh(&[AgentKind::Claude]));
        assert!(!cache.refresh_stale(&[AgentKind::Claude], 0));

        let provider = &cache.overview(&[AgentKind::Claude]).providers[0];
        let UsageReport::Unavailable(unavailable) = &provider.report else {
            panic!("expected an explanation, got {:?}", provider.report);
        };
        assert!(unavailable.evidence.contains("SVARM_USAGE"));
    }

    #[test]
    fn only_an_explicit_off_value_disables_usage() {
        assert!(enabled_by(None));
        assert!(enabled_by(Some("1")));
        assert!(enabled_by(Some("")));
        for off in ["0", "off", "false", "no", " OFF "] {
            assert!(!enabled_by(Some(off)), "{off:?} should disable usage");
        }
    }
}
