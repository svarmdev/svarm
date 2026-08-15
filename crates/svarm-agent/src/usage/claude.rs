//! Claude Code subscription limits.
//!
//! Source: `GET https://api.anthropic.com/api/oauth/usage` with the OAuth access token Claude
//! Code stores in `~/.claude/.credentials.json`. Private interface, verified by observation.

use std::{path::PathBuf, time::Duration};

use serde::Deserialize;

use super::{
    http::{self, HttpOutcome},
    time::Timestamp,
};
use crate::protocol::{
    UsageEvidence, UsageReport, UsageUnavailable, UsageUnavailableReason, UsageWindow,
};

const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const SOURCE: &str = "GET api.anthropic.com/api/oauth/usage";
const TIMEOUT: Duration = Duration::from_secs(15);

/// The windows worth showing, in display order, with the name to show them under.
const WINDOWS: [(&str, &str); 4] = [
    ("five_hour", "5-hour"),
    ("seven_day", "Weekly"),
    ("seven_day_opus", "Weekly (Opus)"),
    ("seven_day_sonnet", "Weekly (Sonnet)"),
];

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Credentials {
    #[serde(rename = "accessToken")]
    pub access_token: String,
    /// Milliseconds since the epoch.
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<u64>,
    #[serde(rename = "subscriptionType")]
    pub subscription_type: Option<String>,
}

#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<Credentials>,
}

#[derive(Deserialize)]
struct Window {
    utilization: Option<f64>,
    resets_at: Option<Timestamp>,
}

pub(crate) fn probe(home: &std::path::Path) -> UsageReport {
    let path = credentials_path(home);
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) => {
            return unavailable(
                UsageUnavailableReason::NotSignedIn,
                "Not signed in. Run `claude` and sign in, then refresh.",
                format!("~/.claude/.credentials.json could not be read: {error}"),
            );
        }
    };
    let credentials = match read_credentials(&contents) {
        Ok(credentials) => credentials,
        Err(evidence) => {
            return unavailable(
                UsageUnavailableReason::NotSignedIn,
                "Not signed in. Run `claude` and sign in, then refresh.",
                evidence,
            );
        }
    };
    let outcome = http::get_json(ENDPOINT, &credentials.access_token, TIMEOUT, &[]);
    interpret(&credentials, &outcome, now_ms())
}

fn credentials_path(home: &std::path::Path) -> PathBuf {
    home.join(".claude/.credentials.json")
}

/// Pure: pull the OAuth block out of the credentials file.
pub(crate) fn read_credentials(contents: &str) -> Result<Credentials, String> {
    let file: CredentialsFile = serde_json::from_str(contents)
        .map_err(|error| format!("~/.claude/.credentials.json is not readable JSON: {error}"))?;
    let credentials = file
        .claude_ai_oauth
        .ok_or_else(|| "~/.claude/.credentials.json has no claudeAiOauth block".to_owned())?;
    if credentials.access_token.trim().is_empty() {
        return Err("~/.claude/.credentials.json has an empty access token".to_owned());
    }
    Ok(credentials)
}

/// Pure: turn a request outcome into a report. Every branch carries what was observed.
pub(crate) fn interpret(
    credentials: &Credentials,
    outcome: &HttpOutcome,
    now_ms: u64,
) -> UsageReport {
    let body = match outcome {
        HttpOutcome::Ok(body) => body,
        HttpOutcome::Unauthorized(code) => {
            let expired = credentials.expires_at.is_some_and(|at| at <= now_ms);
            return unavailable(
                if expired {
                    UsageUnavailableReason::Expired
                } else {
                    UsageUnavailableReason::NotSignedIn
                },
                "Sign in again with `claude`, then refresh.",
                if expired {
                    format!("{SOURCE} → {code}; the stored token had already expired")
                } else {
                    format!("{SOURCE} → {code}")
                },
            );
        }
        HttpOutcome::Status(code, body) => {
            return unavailable(
                UsageUnavailableReason::ProbeFailed,
                "Claude did not return usage. Try refreshing.",
                format!("{SOURCE} → {code} {}", http::evidence_snippet(body)),
            );
        }
        HttpOutcome::Transport(error) => {
            return unavailable(
                UsageUnavailableReason::ProbeFailed,
                "Could not reach Claude. Try refreshing.",
                format!("{SOURCE} failed: {error}"),
            );
        }
    };

    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(error) => {
            return unavailable(
                UsageUnavailableReason::ProbeFailed,
                "Claude returned something unreadable.",
                format!("{SOURCE} returned unparseable JSON: {error}"),
            );
        }
    };

    let windows = read_windows(&value);
    if windows.is_empty() {
        return unavailable(
            UsageUnavailableReason::Unsupported,
            "Claude reported no usage windows for this account.",
            format!("{SOURCE} returned {}", describe_keys(&value)),
        );
    }

    UsageReport::Available(UsageEvidence {
        plan: credentials
            .subscription_type
            .clone()
            .map(|plan| plan_label(&plan)),
        windows,
        notes: Vec::new(),
        source: SOURCE.to_owned(),
    })
}

/// Emit only the windows the payload actually contained, in the declared display order.
fn read_windows(value: &serde_json::Value) -> Vec<UsageWindow> {
    WINDOWS
        .iter()
        .filter_map(|(key, label)| {
            let window: Window = serde_json::from_value(value.get(key)?.clone()).ok()?;
            let utilization = window.utilization?;
            let mut usage = UsageWindow::from_percent(*label, utilization);
            usage.resets_at_ms = window.resets_at.as_ref().and_then(Timestamp::to_unix_ms);
            Some(usage)
        })
        .collect()
}

fn plan_label(subscription_type: &str) -> String {
    let mut characters = subscription_type.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

/// Name what came back, so an unrecognised payload can be diagnosed from the interface.
fn describe_keys(value: &serde_json::Value) -> String {
    match value.as_object() {
        Some(map) if !map.is_empty() => {
            let keys = map.keys().take(6).cloned().collect::<Vec<_>>().join(", ");
            format!("an object with no known usage window (keys: {keys})")
        }
        Some(_) => "an empty object".to_owned(),
        None => format!("a {} rather than an object", kind_of(value)),
    }
}

fn kind_of(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials() -> Credentials {
        Credentials {
            access_token: "token".into(),
            expires_at: Some(2_000),
            subscription_type: Some("max".into()),
        }
    }

    /// Trimmed from a real 200 response, keeping the shape and the observed values.
    const LIVE_BODY: &str = r#"{
        "five_hour": {"utilization": 42.0, "resets_at": "2026-08-15T00:00:00.366387+00:00",
                      "limit_dollars": null, "used_dollars": null},
        "seven_day": {"utilization": 17.0, "resets_at": "2026-08-19T22:00:00.366440+00:00"},
        "seven_day_opus": null,
        "seven_day_sonnet": null,
        "extra_usage": {"utilization": 12.68, "monthly_limit": 5000},
        "limits": [{"kind": "session", "percent": 42, "is_active": true}],
        "member_dashboard_available": false
    }"#;

    #[test]
    fn the_live_response_yields_the_five_hour_and_weekly_windows() {
        let report = interpret(&credentials(), &HttpOutcome::Ok(LIVE_BODY.into()), 1_000);
        let UsageReport::Available(evidence) = report else {
            panic!("expected windows, got {report:?}");
        };
        assert_eq!(evidence.plan.as_deref(), Some("Max"));
        assert_eq!(evidence.source, SOURCE);

        let labels: Vec<_> = evidence.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5-hour", "Weekly"]);
        assert_eq!(evidence.windows[0].used_tenths, 420);
        assert_eq!(evidence.windows[0].whole_percent(), 42);
        assert_eq!(evidence.windows[1].used_tenths, 170);
        // Both resets were RFC 3339 strings and must have been read.
        assert!(evidence.windows.iter().all(|w| w.resets_at_ms.is_some()));
        assert!(evidence.windows[0].resets_at_ms < evidence.windows[1].resets_at_ms);
    }

    #[test]
    fn per_model_windows_appear_when_the_account_has_them() {
        let body = r#"{"five_hour":{"utilization":1.0},
                       "seven_day_opus":{"utilization":8.5},
                       "seven_day_sonnet":{"utilization":14.0}}"#;
        let report = interpret(&credentials(), &HttpOutcome::Ok(body.into()), 1_000);
        let UsageReport::Available(evidence) = report else {
            panic!("expected windows");
        };
        let labels: Vec<_> = evidence.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5-hour", "Weekly (Opus)", "Weekly (Sonnet)"]);
        assert_eq!(evidence.windows[1].used_tenths, 85);
    }

    #[test]
    fn a_window_without_a_utilization_is_omitted_not_shown_as_zero() {
        let body = r#"{"five_hour":{"resets_at":"2026-08-15T00:00:00Z"},
                       "seven_day":{"utilization":17.0}}"#;
        let UsageReport::Available(evidence) =
            interpret(&credentials(), &HttpOutcome::Ok(body.into()), 1_000)
        else {
            panic!("expected windows");
        };
        assert_eq!(evidence.windows.len(), 1);
        assert_eq!(evidence.windows[0].label, "Weekly");
    }

    #[test]
    fn an_unreadable_reset_leaves_the_reset_unknown_but_keeps_the_percentage() {
        let body = r#"{"five_hour":{"utilization":42.0,"resets_at":"whenever"}}"#;
        let UsageReport::Available(evidence) =
            interpret(&credentials(), &HttpOutcome::Ok(body.into()), 1_000)
        else {
            panic!("expected a window");
        };
        assert_eq!(evidence.windows[0].used_tenths, 420);
        assert_eq!(evidence.windows[0].resets_at_ms, None);
    }

    #[test]
    fn a_payload_with_no_known_window_is_unsupported_and_names_what_it_saw() {
        let body = r#"{"something_else":1,"another":2}"#;
        let UsageReport::Unavailable(unavailable) =
            interpret(&credentials(), &HttpOutcome::Ok(body.into()), 1_000)
        else {
            panic!("expected unavailable");
        };
        assert_eq!(unavailable.reason, UsageUnavailableReason::Unsupported);
        assert!(
            unavailable.evidence.contains("something_else"),
            "{unavailable:?}"
        );
    }

    #[test]
    fn malformed_json_is_a_probe_failure_carrying_the_parse_error() {
        let UsageReport::Unavailable(unavailable) =
            interpret(&credentials(), &HttpOutcome::Ok("not json".into()), 1_000)
        else {
            panic!("expected unavailable");
        };
        assert_eq!(unavailable.reason, UsageUnavailableReason::ProbeFailed);
        assert!(unavailable.evidence.contains(SOURCE));
    }

    #[test]
    fn rejection_distinguishes_an_expired_token_from_a_missing_sign_in() {
        // Token expiry is in the past relative to `now`, so this is a stale session.
        let UsageReport::Unavailable(expired) =
            interpret(&credentials(), &HttpOutcome::Unauthorized(401), 9_999)
        else {
            panic!("expected unavailable");
        };
        assert_eq!(expired.reason, UsageUnavailableReason::Expired);
        assert!(expired.evidence.contains("expired"));

        let UsageReport::Unavailable(refused) =
            interpret(&credentials(), &HttpOutcome::Unauthorized(403), 1)
        else {
            panic!("expected unavailable");
        };
        assert_eq!(refused.reason, UsageUnavailableReason::NotSignedIn);
    }

    #[test]
    fn transport_and_status_failures_stay_probe_failures() {
        for outcome in [
            HttpOutcome::Transport("dns failure".into()),
            HttpOutcome::Status(500, "upstream exploded".into()),
        ] {
            let UsageReport::Unavailable(unavailable) = interpret(&credentials(), &outcome, 1_000)
            else {
                panic!("expected unavailable for {outcome:?}");
            };
            assert_eq!(unavailable.reason, UsageUnavailableReason::ProbeFailed);
        }
    }

    #[test]
    fn credentials_are_read_from_the_oauth_block_and_rejected_when_absent() {
        let parsed = read_credentials(
            r#"{"claudeAiOauth":{"accessToken":"abc","expiresAt":123,"subscriptionType":"max"}}"#,
        )
        .expect("credentials");
        assert_eq!(parsed.access_token, "abc");
        assert_eq!(parsed.expires_at, Some(123));
        assert_eq!(parsed.subscription_type.as_deref(), Some("max"));

        assert!(read_credentials(r#"{"mcpOAuth":{}}"#).is_err());
        assert!(read_credentials(r#"{"claudeAiOauth":{"accessToken":"  "}}"#).is_err());
        assert!(read_credentials("not json").is_err());
    }

    #[test]
    fn interpretation_never_puts_the_token_into_a_report() {
        let secret = "sk-ant-oat-SECRET";
        let credentials = Credentials {
            access_token: secret.into(),
            expires_at: None,
            subscription_type: None,
        };
        for outcome in [
            HttpOutcome::Unauthorized(401),
            HttpOutcome::Transport("dns failure".into()),
            HttpOutcome::Status(500, "upstream exploded".into()),
            HttpOutcome::Ok("{}".into()),
            HttpOutcome::Ok(LIVE_BODY.into()),
        ] {
            let rendered = format!("{:?}", interpret(&credentials, &outcome, 1_000));
            assert!(
                !rendered.contains(secret),
                "token leaked for {outcome:?}: {rendered}"
            );
        }
    }
}
