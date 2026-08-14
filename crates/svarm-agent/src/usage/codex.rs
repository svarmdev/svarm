//! Codex subscription limits, read through `codex app-server`.
//!
//! Going through Codex's own app server means svarm never reads, stores, or refreshes an OpenAI
//! token: the CLI owns its credentials and answers on stdio. The framing is newline-delimited
//! JSON with no `jsonrpc` field, the server interleaves unsolicited notifications, and it refuses
//! work until `initialize` has been answered and `initialized` sent.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use serde::Deserialize;

use super::time::Timestamp;
use crate::{
    AgentKind,
    protocol::{UsageEvidence, UsageReport, UsageUnavailable, UsageUnavailableReason, UsageWindow},
};

const SOURCE: &str = "codex app-server account/rateLimits/read";
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const EXIT_GRACE: Duration = Duration::from_millis(500);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

const ID_INITIALIZE: u64 = 1;
const ID_ACCOUNT: u64 = 2;
const ID_RATE_LIMITS: u64 = 3;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RateLimitWindow {
    #[serde(rename = "usedPercent")]
    used_percent: Option<f64>,
    #[serde(rename = "resetsAt")]
    resets_at: Option<Timestamp>,
    #[serde(rename = "windowDurationMins")]
    window_duration_mins: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RateLimitSnapshot {
    primary: Option<RateLimitWindow>,
    secondary: Option<RateLimitWindow>,
    #[serde(rename = "planType")]
    plan_type: Option<String>,
    credits: Option<Credits>,
    #[serde(rename = "rateLimitReachedType")]
    rate_limit_reached_type: Option<String>,
    #[serde(rename = "spendControlReached")]
    spend_control_reached: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct Credits {
    balance: Option<String>,
    unlimited: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RateLimitsResult {
    #[serde(rename = "rateLimits")]
    rate_limits: Option<RateLimitSnapshot>,
}

/// What the exchange with the app server produced.
#[derive(Debug, Default)]
pub(crate) struct Exchange {
    pub initialized: bool,
    pub account: Option<serde_json::Value>,
    pub rate_limits: Option<RateLimitsResult>,
    /// A JSON-RPC error returned for one of our requests.
    pub error: Option<String>,
}

impl Exchange {
    fn complete(&self) -> bool {
        self.error.is_some() || (self.account.is_some() && self.rate_limits.is_some())
    }
}

pub(crate) fn probe() -> UsageReport {
    match run_exchange() {
        Ok(exchange) => interpret(&exchange),
        Err(evidence) => unavailable(
            UsageUnavailableReason::ProbeFailed,
            "Could not read usage from Codex. Try refreshing.",
            evidence,
        ),
    }
}

/// Request lines, in send order. Separated out so the wire format is asserted by a test rather
/// than only by a live server.
pub(crate) fn request_lines() -> Vec<String> {
    vec![
        format!(
            r#"{{"id":{ID_INITIALIZE},"method":"initialize","params":{{"clientInfo":{{"name":"svarm","title":"Svarm","version":"{}"}}}}}}"#,
            env!("CARGO_PKG_VERSION")
        ),
        r#"{"method":"initialized"}"#.to_owned(),
        format!(
            r#"{{"id":{ID_ACCOUNT},"method":"account/read","params":{{"refreshToken":false}}}}"#
        ),
        format!(r#"{{"id":{ID_RATE_LIMITS},"method":"account/rateLimits/read"}}"#),
    ]
}

fn run_exchange() -> Result<Exchange, String> {
    let mut command = Command::new(AgentKind::Codex.command());
    command.arg("app-server");
    crate::naming::prepare_generator_environment(&mut command);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start `codex app-server`: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "`codex app-server` gave no stdin".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "`codex app-server` gave no stdout".to_owned())?;

    // A blocking read on a wedged child would hold the worker forever, so lines arrive through a
    // channel and the deadline is enforced here.
    let (lines_tx, lines) = mpsc::sync_channel(64);
    let reader = thread::Builder::new()
        .name("svarm-usage-codex".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if lines_tx.send(line).is_err() {
                    break;
                }
            }
        })
        .map_err(|error| format!("could not start the reader thread: {error}"))?;

    let deadline = Instant::now() + PROBE_TIMEOUT;
    let result = drive(&mut stdin, &lines, deadline);

    // Closing stdin is what ends the server cleanly; only kill it if that is not enough.
    drop(stdin);
    let grace = Instant::now() + EXIT_GRACE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => break,
            Ok(None) if Instant::now() < grace => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let _ = reader.join();
    result
}

fn drive(
    stdin: &mut impl Write,
    lines: &Receiver<String>,
    deadline: Instant,
) -> Result<Exchange, String> {
    let mut requests = request_lines().into_iter();
    let mut exchange = Exchange::default();

    // `initialize` must be answered before anything else is accepted.
    let initialize = requests.next().expect("initialize request");
    write_line(stdin, &initialize)?;

    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| format!("`codex app-server` did not answer within {PROBE_TIMEOUT:?}"))?;
        let line = match lines.recv_timeout(remaining) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => {
                return Err(format!(
                    "`codex app-server` did not answer within {PROBE_TIMEOUT:?}"
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err("`codex app-server` closed before answering".to_owned());
            }
        };

        let was_initialized = exchange.initialized;
        absorb(&line, &mut exchange);
        if exchange.complete() {
            return Ok(exchange);
        }
        if exchange.initialized && !was_initialized {
            for request in requests.by_ref() {
                write_line(stdin, &request)?;
            }
        }
    }
}

fn write_line(stdin: &mut impl Write, line: &str) -> Result<(), String> {
    writeln!(stdin, "{line}")
        .map_err(|error| format!("could not write to `codex app-server`: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("could not write to `codex app-server`: {error}"))
}

/// Pure: fold one stdout line into the exchange.
///
/// Notifications carry no `id` and responses to anything svarm did not ask for carry a foreign
/// one; both are ignored so an unrelated server message cannot be mistaken for an answer.
pub(crate) fn absorb(line: &str, exchange: &mut Exchange) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let Some(id) = value.get("id").and_then(serde_json::Value::as_u64) else {
        return;
    };
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error");
        exchange.error = Some(format!("{SOURCE} → error: {message}"));
        return;
    }
    let Some(result) = value.get("result") else {
        return;
    };
    match id {
        ID_INITIALIZE => exchange.initialized = true,
        ID_ACCOUNT => exchange.account = Some(result.clone()),
        ID_RATE_LIMITS => {
            exchange.rate_limits = Some(serde_json::from_value(result.clone()).unwrap_or_default());
        }
        _ => {}
    }
}

/// Pure: turn a completed exchange into a report.
pub(crate) fn interpret(exchange: &Exchange) -> UsageReport {
    if let Some(error) = &exchange.error {
        return unavailable(
            UsageUnavailableReason::ProbeFailed,
            "Codex refused the usage request. Try refreshing.",
            error.clone(),
        );
    }

    if !signed_in(exchange.account.as_ref()) {
        return unavailable(
            UsageUnavailableReason::NotSignedIn,
            "Not signed in. Run `codex login`, then refresh.",
            "codex app-server account/read reported no signed-in account".to_owned(),
        );
    }

    let snapshot = exchange
        .rate_limits
        .as_ref()
        .and_then(|result| result.rate_limits.as_ref());
    let Some(snapshot) = snapshot else {
        return unavailable(
            UsageUnavailableReason::Unsupported,
            "Codex reported no usage limits for this account.",
            format!("{SOURCE} returned no rateLimits object"),
        );
    };

    let windows: Vec<UsageWindow> = [snapshot.primary.as_ref(), snapshot.secondary.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(read_window)
        .collect();

    if windows.is_empty() {
        return unavailable(
            UsageUnavailableReason::Unsupported,
            "Codex reported no usage limits for this account.",
            format!("{SOURCE} returned rateLimits with no primary or secondary window"),
        );
    }

    UsageReport::Available(UsageEvidence {
        plan: snapshot.plan_type.as_deref().map(plan_label),
        windows,
        notes: notes(snapshot),
        source: SOURCE.to_owned(),
    })
}

/// `account/read` shapes have changed across Codex releases, so accept any of the observed ways
/// of saying "there is an account" and treat only an outright absence as signed out.
fn signed_in(account: Option<&serde_json::Value>) -> bool {
    let Some(account) = account else {
        return false;
    };
    if account.is_null() {
        return false;
    }
    match account.get("account") {
        Some(serde_json::Value::Null) => false,
        Some(_) => true,
        // No nested `account` key: a non-empty object is itself the account.
        None => account.as_object().is_some_and(|map| !map.is_empty()),
    }
}

fn read_window(window: &RateLimitWindow) -> Option<UsageWindow> {
    let used = window.used_percent?;
    let mut usage = UsageWindow::from_percent(window_label(window.window_duration_mins), used);
    usage.resets_at_ms = window.resets_at.as_ref().and_then(Timestamp::to_unix_ms);
    Some(usage)
}

/// Name a window by the span it covers, because that is what a limit means to the user. Codex
/// reports the span rather than a name, and an account may report only one of the two windows.
fn window_label(minutes: Option<i64>) -> String {
    match minutes {
        Some(300) => "5-hour".to_owned(),
        Some(10_080) => "Weekly".to_owned(),
        Some(1_440) => "Daily".to_owned(),
        Some(minutes) if minutes > 0 && minutes % 10_080 == 0 => {
            format!("{}-week", minutes / 10_080)
        }
        Some(minutes) if minutes > 0 && minutes % 1_440 == 0 => format!("{}-day", minutes / 1_440),
        Some(minutes) if minutes > 0 && minutes % 60 == 0 => format!("{}-hour", minutes / 60),
        Some(minutes) if minutes > 0 => format!("{minutes}-minute"),
        _ => "Limit".to_owned(),
    }
}

fn notes(snapshot: &RateLimitSnapshot) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(credits) = &snapshot.credits {
        if credits.unlimited == Some(true) {
            notes.push("Credits: unlimited".to_owned());
        } else if let Some(balance) = credits.balance.as_deref().map(trim_balance) {
            notes.push(format!("Credits: {balance}"));
        }
    }
    if snapshot.spend_control_reached == Some(true) {
        notes.push("Spend limit reached".to_owned());
    }
    if let Some(reached) = &snapshot.rate_limit_reached_type {
        notes.push(format!("Limit reached: {}", reached.replace('_', " ")));
    }
    notes
}

/// Balances arrive with a long fractional tail ("402.3238200000"); show two decimals.
fn trim_balance(balance: &str) -> String {
    match balance.split_once('.') {
        Some((whole, fraction)) => {
            let fraction: String = fraction.chars().take(2).collect();
            format!("{whole}.{fraction}")
        }
        None => balance.to_owned(),
    }
}

fn plan_label(plan: &str) -> String {
    let mut characters = plan.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

fn unavailable(
    reason: UsageUnavailableReason,
    message: &str,
    evidence: impl Into<String>,
) -> UsageReport {
    UsageReport::Unavailable(UsageUnavailable {
        reason,
        message: message.to_owned(),
        evidence: evidence.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured verbatim from a live `codex app-server` on a Plus account.
    const LIVE_RATE_LIMITS: &str = r#"{"id":3,"result":{"rateLimits":{"limitId":"codex",
        "limitName":null,
        "primary":{"usedPercent":8,"windowDurationMins":10080,"resetsAt":1787201323},
        "secondary":null,
        "credits":{"hasCredits":true,"unlimited":false,"balance":"402.3238200000"},
        "individualLimit":null,"spendControlReached":false,"planType":"plus",
        "rateLimitReachedType":null},"rateLimitsByLimitId":{},"rateLimitResetCredits":
        {"availableCount":0,"credits":[]}}}"#;

    const LIVE_INITIALIZE: &str =
        r#"{"id":1,"result":{"userAgent":"svarm/0.2.0","codexHome":"/home/u/.codex"}}"#;
    const LIVE_NOTIFICATION: &str =
        r#"{"method":"remoteControl/status/changed","params":{"status":"disabled"}}"#;

    fn exchange_from(lines: &[&str]) -> Exchange {
        let mut exchange = Exchange::default();
        for line in lines {
            absorb(line, &mut exchange);
        }
        exchange
    }

    /// Captured verbatim from a live `codex app-server`, with the address removed.
    fn signed_in_account() -> &'static str {
        r#"{"id":2,"result":{"account":{"type":"chatgpt","email":"someone@example.com",
           "planType":"plus"},"requiresOpenaiAuth":true}}"#
    }

    #[test]
    fn the_request_lines_match_the_app_server_schema() {
        let lines = request_lines();
        assert_eq!(lines.len(), 4);
        // No `jsonrpc` field: the app-server envelope does not carry one.
        assert!(
            lines.iter().all(|line| !line.contains("jsonrpc")),
            "{lines:?}"
        );
        assert!(lines[0].contains(r#""method":"initialize""#));
        assert!(lines[0].contains(r#""clientInfo""#));
        assert_eq!(lines[1], r#"{"method":"initialized"}"#);
        assert!(lines[2].contains(r#""method":"account/read""#));
        // `account/rateLimits/read` takes no params.
        assert_eq!(lines[3], r#"{"id":3,"method":"account/rateLimits/read"}"#);
        for line in &lines {
            serde_json::from_str::<serde_json::Value>(line).expect("each request is valid JSON");
        }
    }

    #[test]
    fn the_live_response_yields_the_weekly_window() {
        let exchange = exchange_from(&[LIVE_INITIALIZE, signed_in_account(), LIVE_RATE_LIMITS]);
        assert!(exchange.initialized);
        assert!(exchange.complete());

        let UsageReport::Available(evidence) = interpret(&exchange) else {
            panic!("expected windows, got {:?}", interpret(&exchange));
        };
        assert_eq!(evidence.plan.as_deref(), Some("Plus"));
        assert_eq!(evidence.windows.len(), 1, "secondary was null");
        let window = &evidence.windows[0];
        assert_eq!(window.label, "Weekly");
        assert_eq!(window.used_tenths, 80);
        assert_eq!(window.whole_percent(), 8);
        assert_eq!(window.resets_at_ms, Some(1_787_201_323_000));
        assert_eq!(window.detail, None);
        assert_eq!(evidence.notes, ["Credits: 402.32"]);
    }

    #[test]
    fn both_windows_are_reported_when_the_account_has_them() {
        let both = r#"{"id":3,"result":{"rateLimits":{
            "primary":{"usedPercent":52,"windowDurationMins":300,"resetsAt":1787201323},
            "secondary":{"usedPercent":21,"windowDurationMins":10080,"resetsAt":1787301323},
            "planType":"pro"}}}"#;
        let exchange = exchange_from(&[LIVE_INITIALIZE, signed_in_account(), both]);
        let UsageReport::Available(evidence) = interpret(&exchange) else {
            panic!("expected windows");
        };
        let labels: Vec<_> = evidence.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5-hour", "Weekly"]);
        assert!(
            evidence
                .windows
                .iter()
                .all(|window| window.detail.is_none())
        );
    }

    #[test]
    fn windows_are_named_by_the_span_they_cover() {
        assert_eq!(window_label(Some(300)), "5-hour");
        assert_eq!(window_label(Some(10_080)), "Weekly");
        assert_eq!(window_label(Some(1_440)), "Daily");
        assert_eq!(window_label(Some(20_160)), "2-week");
        assert_eq!(window_label(Some(4_320)), "3-day");
        assert_eq!(window_label(Some(180)), "3-hour");
        assert_eq!(window_label(Some(45)), "45-minute");
        // An absent or nonsensical span must not invent one.
        assert_eq!(window_label(None), "Limit");
        assert_eq!(window_label(Some(0)), "Limit");
    }

    #[test]
    fn null_windows_report_unsupported_with_the_reason() {
        let empty = r#"{"id":3,"result":{"rateLimits":{"primary":null,"secondary":null}}}"#;
        let exchange = exchange_from(&[LIVE_INITIALIZE, signed_in_account(), empty]);
        let UsageReport::Unavailable(unavailable) = interpret(&exchange) else {
            panic!("expected unavailable");
        };
        assert_eq!(unavailable.reason, UsageUnavailableReason::Unsupported);
        assert!(unavailable.evidence.contains("no primary or secondary"));
    }

    #[test]
    fn a_missing_account_reports_not_signed_in() {
        for account in [
            r#"{"id":2,"result":null}"#,
            r#"{"id":2,"result":{"account":null}}"#,
        ] {
            let exchange = exchange_from(&[LIVE_INITIALIZE, account, LIVE_RATE_LIMITS]);
            let UsageReport::Unavailable(unavailable) = interpret(&exchange) else {
                panic!("expected unavailable for {account}");
            };
            assert_eq!(unavailable.reason, UsageUnavailableReason::NotSignedIn);
            assert!(unavailable.message.contains("codex login"));
        }
    }

    #[test]
    fn notifications_and_foreign_ids_are_ignored() {
        let mut exchange = exchange_from(&[
            LIVE_NOTIFICATION,
            r#"{"id":99,"result":{"rateLimits":{"primary":{"usedPercent":100}}}}"#,
            "not json at all",
            r#"{"id":3}"#,
        ]);
        assert!(!exchange.initialized);
        assert!(exchange.account.is_none());
        assert!(exchange.rate_limits.is_none());
        assert!(exchange.error.is_none());

        // The real answer still lands after the noise.
        absorb(LIVE_INITIALIZE, &mut exchange);
        absorb(LIVE_RATE_LIMITS, &mut exchange);
        assert!(exchange.initialized);
        assert!(exchange.rate_limits.is_some());
    }

    #[test]
    fn a_jsonrpc_error_becomes_a_probe_failure_carrying_the_message() {
        let exchange = exchange_from(&[
            LIVE_INITIALIZE,
            r#"{"id":3,"error":{"code":-32000,"message":"account is locked"}}"#,
        ]);
        assert!(
            exchange.complete(),
            "an error ends the exchange immediately"
        );
        let UsageReport::Unavailable(unavailable) = interpret(&exchange) else {
            panic!("expected unavailable");
        };
        assert_eq!(unavailable.reason, UsageUnavailableReason::ProbeFailed);
        assert!(unavailable.evidence.contains("account is locked"));
    }

    #[test]
    fn credit_balances_are_shortened_and_unlimited_is_stated() {
        assert_eq!(trim_balance("402.3238200000"), "402.32");
        assert_eq!(trim_balance("12"), "12");
        let unlimited = r#"{"id":3,"result":{"rateLimits":{
            "primary":{"usedPercent":1,"windowDurationMins":300},
            "credits":{"unlimited":true},"spendControlReached":true,
            "rateLimitReachedType":"rate_limit_reached"}}}"#;
        let exchange = exchange_from(&[LIVE_INITIALIZE, signed_in_account(), unlimited]);
        let UsageReport::Available(evidence) = interpret(&exchange) else {
            panic!("expected windows");
        };
        assert_eq!(
            evidence.notes,
            [
                "Credits: unlimited",
                "Spend limit reached",
                "Limit reached: rate limit reached"
            ]
        );
    }

    #[test]
    fn a_window_without_a_percentage_is_omitted_rather_than_shown_as_zero() {
        let partial = r#"{"id":3,"result":{"rateLimits":{
            "primary":{"windowDurationMins":300},
            "secondary":{"usedPercent":21,"windowDurationMins":10080}}}}"#;
        let exchange = exchange_from(&[LIVE_INITIALIZE, signed_in_account(), partial]);
        let UsageReport::Available(evidence) = interpret(&exchange) else {
            panic!("expected windows");
        };
        assert_eq!(evidence.windows.len(), 1);
        assert_eq!(evidence.windows[0].label, "Weekly");
    }
}
