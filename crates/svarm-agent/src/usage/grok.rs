//! Grok Build subscription limits.
//!
//! Source: `GET https://cli-chat-proxy.grok.com/v1/billing?format=credits` with the OIDC token
//! Grok stores in `~/.grok/auth.json`. Private interface, verified against grok-build and a live
//! account. svarm never refreshes this token: spending the same refresh token from a second
//! process can revoke the family and log the user out of grok.

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use serde::Deserialize;

use super::{
    http::{self, HttpOutcome},
    time::{Timestamp, parse_rfc3339_ms},
};
use crate::protocol::{
    UsageEvidence, UsageReport, UsageUnavailable, UsageUnavailableReason, UsageWindow,
};

const ENDPOINT: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const SOURCE: &str = "GET cli-chat-proxy.grok.com/v1/billing?format=credits";
const TIMEOUT: Duration = Duration::from_secs(15);
const TOKEN_AUTH: &str = "xai-grok-cli";
/// Value grok itself sends as `x-grok-client-version`. The proxy accepts this
/// from the installed CLI; we do not invent a svarm-specific version.
const CLIENT_VERSION: &str = "1.0.3";
const FIRST_PARTY_SCOPE: &str = "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828";
const LEGACY_SCOPE: &str = "https://accounts.x.ai/sign-in";

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Credentials {
    pub key: String,
    pub user_id: String,
    pub auth_mode: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Deserialize)]
struct BillingResponse {
    config: Option<BillingConfig>,
}

/// proto3 JSON omits zero scalars, so a `$0` Cent arrives as `{}`.
#[derive(Clone, Debug, Default, Deserialize)]
struct Cent {
    #[serde(default)]
    val: i64,
}

#[derive(Deserialize)]
struct UsagePeriod {
    #[serde(rename = "type")]
    period_type: Option<String>,
    end: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductUsage {
    product: Option<String>,
    usage_percent: Option<f64>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BillingConfig {
    credit_usage_percent: Option<f64>,
    current_period: Option<UsagePeriod>,
    monthly_limit: Option<Cent>,
    used: Option<Cent>,
    on_demand_cap: Option<Cent>,
    on_demand_used: Option<Cent>,
    prepaid_balance: Option<Cent>,
    billing_period_end: Option<String>,
    #[serde(default)]
    product_usage: Vec<ProductUsage>,
}

pub(crate) fn probe(home: &std::path::Path) -> UsageReport {
    let path = credentials_path(home);
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) => {
            return unavailable(
                UsageUnavailableReason::NotSignedIn,
                "Not signed in. Run `grok` and sign in, then refresh.",
                format!("~/.grok/auth.json could not be read: {error}"),
            );
        }
    };
    let credentials = match read_credentials(&contents) {
        Ok(credentials) => credentials,
        Err(evidence) => {
            return unavailable(
                UsageUnavailableReason::NotSignedIn,
                "Not signed in. Run `grok` and sign in, then refresh.",
                evidence,
            );
        }
    };
    let extra = [
        ("X-XAI-Token-Auth", TOKEN_AUTH),
        ("x-userid", credentials.user_id.as_str()),
        ("x-grok-client-version", CLIENT_VERSION),
    ];
    let outcome = http::get_json(ENDPOINT, &credentials.key, TIMEOUT, &extra);
    interpret(&credentials, &outcome, now_ms())
}

fn credentials_path(home: &std::path::Path) -> PathBuf {
    home.join(".grok/auth.json")
}

/// Pure: pull a usable grok.com session out of `auth.json`.
///
/// Matches grok's `lookup_auth`: prefer the first-party OIDC scope, fall back to
/// the legacy accounts-app scope, and reject `web_login` tokens outright.
pub(crate) fn read_credentials(contents: &str) -> Result<Credentials, String> {
    let store: BTreeMap<String, serde_json::Value> = serde_json::from_str(contents)
        .map_err(|error| format!("~/.grok/auth.json is not readable JSON: {error}"))?;

    let mut saw_web_login = false;
    for scope in [FIRST_PARTY_SCOPE, LEGACY_SCOPE] {
        let Some(value) = store.get(scope) else {
            continue;
        };
        let credentials: Credentials = serde_json::from_value(value.clone()).map_err(|error| {
            format!("~/.grok/auth.json entry {scope} is not a grok session: {error}")
        })?;
        if credentials
            .auth_mode
            .as_deref()
            .is_some_and(|mode| mode.eq_ignore_ascii_case("web_login"))
        {
            saw_web_login = true;
            continue;
        }
        if credentials.key.trim().is_empty() {
            return Err("~/.grok/auth.json has an empty access token".to_owned());
        }
        if credentials.user_id.trim().is_empty() {
            return Err("~/.grok/auth.json has no user id".to_owned());
        }
        return Ok(credentials);
    }

    if saw_web_login {
        return Err(
            "~/.grok/auth.json has only a legacy web_login token; run `grok` and sign in again"
                .to_owned(),
        );
    }
    Err("~/.grok/auth.json has no grok.com session".to_owned())
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
            let expired = expires_at_ms(credentials).is_some_and(|at| at <= now_ms);
            return unavailable(
                if expired {
                    UsageUnavailableReason::Expired
                } else {
                    UsageUnavailableReason::NotSignedIn
                },
                "Run `grok` to refresh, then refresh usage.",
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
                "Grok did not return usage. Try refreshing.",
                format!("{SOURCE} → {code} {}", http::evidence_snippet(body)),
            );
        }
        HttpOutcome::Transport(error) => {
            return unavailable(
                UsageUnavailableReason::ProbeFailed,
                "Could not reach Grok. Try refreshing.",
                format!("{SOURCE} failed: {error}"),
            );
        }
    };

    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(error) => {
            return unavailable(
                UsageUnavailableReason::ProbeFailed,
                "Grok returned something unreadable.",
                format!("{SOURCE} returned unparseable JSON: {error}"),
            );
        }
    };

    let response: BillingResponse = match serde_json::from_value(value.clone()) {
        Ok(response) => response,
        Err(error) => {
            return unavailable(
                UsageUnavailableReason::ProbeFailed,
                "Grok returned something unreadable.",
                format!("{SOURCE} returned an unexpected shape: {error}"),
            );
        }
    };

    let Some(config) = response.config else {
        return unavailable(
            UsageUnavailableReason::Unsupported,
            "Grok reported no usage windows for this account.",
            format!("{SOURCE} returned {}", describe_keys(&value)),
        );
    };

    let windows = read_windows(&config);
    if windows.is_empty() {
        return unavailable(
            UsageUnavailableReason::Unsupported,
            "Grok reported no usage windows for this account.",
            format!("{SOURCE} returned {}", describe_keys(&value)),
        );
    }

    UsageReport::Available(UsageEvidence {
        plan: None,
        windows,
        notes: notes(&config),
        source: SOURCE.to_owned(),
    })
}

fn read_windows(config: &BillingConfig) -> Vec<UsageWindow> {
    let resets_at_ms = reset_ms(config);
    let mut windows = Vec::new();

    if let Some(percent) = period_percent(config) {
        let mut window = UsageWindow::from_percent(period_label(config), percent);
        window.resets_at_ms = resets_at_ms;
        windows.push(window);
    }

    for product in &config.product_usage {
        let Some(name) = product.product.as_deref() else {
            continue;
        };
        // proto3 omits a 0.0 percent, so a named product without the field is 0%.
        let mut window =
            UsageWindow::from_percent(product_label(name), product.usage_percent.unwrap_or(0.0));
        window.resets_at_ms = resets_at_ms;
        windows.push(window);
    }

    windows
}

/// Prefer `creditUsagePercent`; fall back to `used / monthlyLimit` from the
/// deprecated billing shape. proto3 omits a 0.0 percent, so a described period
/// with neither field is 0%, not "no window".
fn period_percent(config: &BillingConfig) -> Option<f64> {
    if let Some(percent) = config.credit_usage_percent {
        return Some(percent);
    }
    if let (Some(used), Some(limit)) = (config.used.as_ref(), config.monthly_limit.as_ref())
        && limit.val > 0
    {
        return Some((used.val as f64) * 100.0 / (limit.val as f64));
    }
    described_period(config).then_some(0.0)
}

fn described_period(config: &BillingConfig) -> bool {
    config.current_period.is_some() || config.billing_period_end.is_some()
}

fn period_label(config: &BillingConfig) -> &'static str {
    let period_type = config
        .current_period
        .as_ref()
        .and_then(|period| period.period_type.as_deref());
    match period_type {
        Some(kind) if kind.contains("WEEKLY") => "Weekly",
        Some(kind) if kind.contains("MONTHLY") => "Monthly",
        Some(kind) if kind.contains("DAILY") => "Daily",
        _ if config.monthly_limit.is_some() => "Monthly",
        _ => "Limit",
    }
}

fn reset_ms(config: &BillingConfig) -> Option<u64> {
    config
        .current_period
        .as_ref()
        .and_then(|period| period.end.as_deref())
        .or(config.billing_period_end.as_deref())
        .and_then(parse_rfc3339_ms)
}

fn product_label(product: &str) -> String {
    match product {
        "GrokBuild" | "PRODUCT_GROK_BUILD" => "Grok Build".to_owned(),
        "GrokChat" | "PRODUCT_GROK_CHAT" => "Grok Chat".to_owned(),
        other => other.to_owned(),
    }
}

fn notes(config: &BillingConfig) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(prepaid) = &config.prepaid_balance
        && prepaid.val > 0
    {
        notes.push(format!("Prepaid: {}", format_cents(prepaid.val)));
    }
    if let Some(cap) = &config.on_demand_cap
        && cap.val > 0
    {
        let used = config
            .on_demand_used
            .as_ref()
            .map(|used| used.val)
            .unwrap_or(0);
        notes.push(format!(
            "On-demand: {} / {}",
            format_cents(used),
            format_cents(cap.val)
        ));
    }
    notes
}

fn format_cents(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.unsigned_abs();
    format!("{sign}${}.{:02}", cents / 100, cents % 100)
}

fn expires_at_ms(credentials: &Credentials) -> Option<u64> {
    credentials
        .expires_at
        .as_deref()
        .and_then(|text| Timestamp::Text(text.to_owned()).to_unix_ms())
}

fn describe_keys(value: &serde_json::Value) -> String {
    let root = value
        .get("config")
        .filter(|config| config.is_object())
        .unwrap_or(value);
    match root.as_object() {
        Some(map) if !map.is_empty() => {
            let keys = map.keys().take(6).cloned().collect::<Vec<_>>().join(", ");
            format!("an object with no known usage window (keys: {keys})")
        }
        Some(_) => "an empty object".to_owned(),
        None => format!("a {} rather than an object", kind_of(root)),
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
            key: "token".into(),
            user_id: "user-1".into(),
            auth_mode: Some("oidc".into()),
            expires_at: Some("2026-08-15T00:40:20Z".into()),
        }
    }

    /// Trimmed from a live 200 response, keeping the shape and observed values.
    const LIVE_BODY: &str = r#"{
        "config": {
            "currentPeriod": {
                "type": "USAGE_PERIOD_TYPE_WEEKLY",
                "start": "2026-08-12T18:21:01.790360+00:00",
                "end": "2026-08-19T18:21:01.790360+00:00"
            },
            "creditUsagePercent": 15.0,
            "onDemandCap": {"val": 0},
            "onDemandUsed": {"val": 0},
            "productUsage": [
                {"product": "GrokBuild", "usagePercent": 14.0},
                {"product": "GrokChat", "usagePercent": 1.0}
            ],
            "isUnifiedBillingUser": true,
            "prepaidBalance": {"val": 0},
            "topUpMethod": "TOP_UP_METHOD_SAVED_PAYMENT_METHOD",
            "billingPeriodStart": "2026-08-12T18:21:01.790360+00:00",
            "billingPeriodEnd": "2026-08-19T18:21:01.790360+00:00"
        }
    }"#;

    fn first_party(auth_mode: &str, key: &str) -> String {
        format!(
            r#"{{"{FIRST_PARTY_SCOPE}":{{"key":"{key}","user_id":"u1","auth_mode":"{auth_mode}","expires_at":"2026-08-15T00:40:20Z"}}}}"#
        )
    }

    #[test]
    fn the_live_response_yields_the_weekly_and_product_windows() {
        let report = interpret(&credentials(), &HttpOutcome::Ok(LIVE_BODY.into()), 1_000);
        let UsageReport::Available(evidence) = report else {
            panic!("expected windows, got {report:?}");
        };
        assert_eq!(evidence.plan, None);
        assert_eq!(evidence.source, SOURCE);
        assert!(evidence.notes.is_empty());

        let labels: Vec<_> = evidence.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Weekly", "Grok Build", "Grok Chat"]);
        assert_eq!(evidence.windows[0].used_tenths, 150);
        assert_eq!(evidence.windows[0].whole_percent(), 15);
        assert_eq!(evidence.windows[1].used_tenths, 140);
        assert_eq!(evidence.windows[2].used_tenths, 10);
        assert!(evidence.windows.iter().all(|w| w.resets_at_ms.is_some()));
    }

    #[test]
    fn proto3_omits_zero_cents_and_they_do_not_become_notes() {
        let body = r#"{"config":{"creditUsagePercent":3.0,"prepaidBalance":{},"onDemandCap":{}}}"#;
        let UsageReport::Available(evidence) =
            interpret(&credentials(), &HttpOutcome::Ok(body.into()), 1_000)
        else {
            panic!("expected a window");
        };
        assert_eq!(evidence.windows[0].used_tenths, 30);
        assert!(evidence.notes.is_empty());
    }

    #[test]
    fn deprecated_used_and_limit_are_the_fallback_when_percent_is_absent() {
        let body = r#"{"config":{
            "used":{"val":250},"monthlyLimit":{"val":1000},
            "billingPeriodEnd":"2026-09-01T00:00:00Z"
        }}"#;
        let UsageReport::Available(evidence) =
            interpret(&credentials(), &HttpOutcome::Ok(body.into()), 1_000)
        else {
            panic!("expected a window");
        };
        assert_eq!(evidence.windows.len(), 1);
        assert_eq!(evidence.windows[0].label, "Monthly");
        assert_eq!(evidence.windows[0].used_tenths, 250);
        assert!(evidence.windows[0].resets_at_ms.is_some());
    }

    #[test]
    fn new_fields_win_over_deprecated_ones() {
        let body = r#"{"config":{
            "creditUsagePercent":15.0,
            "currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","end":"2026-08-19T18:21:01Z"},
            "used":{"val":999},"monthlyLimit":{"val":1000},
            "billingPeriodEnd":"2026-09-01T00:00:00Z"
        }}"#;
        let UsageReport::Available(evidence) =
            interpret(&credentials(), &HttpOutcome::Ok(body.into()), 1_000)
        else {
            panic!("expected a window");
        };
        assert_eq!(evidence.windows[0].label, "Weekly");
        assert_eq!(evidence.windows[0].used_tenths, 150);
        assert_eq!(
            evidence.windows[0].resets_at_ms,
            parse_rfc3339_ms("2026-08-19T18:21:01Z")
        );
    }

    #[test]
    fn proto_product_names_are_shown_as_plain_labels() {
        let body = r#"{"config":{"productUsage":[
            {"product":"PRODUCT_GROK_BUILD","usagePercent":61.2}
        ]}}"#;
        let UsageReport::Available(evidence) =
            interpret(&credentials(), &HttpOutcome::Ok(body.into()), 1_000)
        else {
            panic!("expected a window");
        };
        assert_eq!(evidence.windows.len(), 1);
        assert_eq!(evidence.windows[0].label, "Grok Build");
        assert_eq!(evidence.windows[0].used_tenths, 612);
    }

    #[test]
    fn a_named_product_with_no_percent_is_zero_because_proto3_omits_it() {
        let body = r#"{"config":{
            "creditUsagePercent":8.0,
            "currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY"},
            "productUsage":[
                {"product":"GrokBuild"},
                {"product":"GrokChat","usagePercent":1.0}
            ]
        }}"#;
        let UsageReport::Available(evidence) =
            interpret(&credentials(), &HttpOutcome::Ok(body.into()), 1_000)
        else {
            panic!("expected windows");
        };
        let labels: Vec<_> = evidence.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Weekly", "Grok Build", "Grok Chat"]);
        assert_eq!(evidence.windows[1].used_tenths, 0);
        assert_eq!(evidence.windows[2].used_tenths, 10);
    }

    #[test]
    fn a_weekly_period_with_omitted_zero_percent_is_shown_as_zero() {
        // Trimmed from a live 200 at 0%: proto3 drops creditUsagePercent and
        // product usagePercent, but still describes the weekly period.
        let body = r#"{
            "config": {
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "start": "2026-08-19T18:21:01.790360+00:00",
                    "end": "2026-08-26T18:21:01.790360+00:00"
                },
                "onDemandCap": {},
                "onDemandUsed": {},
                "productUsage": [{"product": "GrokBuild"}],
                "isUnifiedBillingUser": true,
                "prepaidBalance": {},
                "topUpMethod": "TOP_UP_METHOD_SAVED_PAYMENT_METHOD",
                "billingPeriodStart": "2026-08-19T18:21:01.790360+00:00",
                "billingPeriodEnd": "2026-08-26T18:21:01.790360+00:00"
            }
        }"#;
        let UsageReport::Available(evidence) =
            interpret(&credentials(), &HttpOutcome::Ok(body.into()), 1_000)
        else {
            panic!("expected windows, got a missing-usage report");
        };
        let labels: Vec<_> = evidence.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Weekly", "Grok Build"]);
        assert!(
            evidence.windows.iter().all(|w| w.used_tenths == 0),
            "{evidence:?}"
        );
        assert!(evidence.windows.iter().all(|w| w.resets_at_ms.is_some()));
        assert!(evidence.notes.is_empty());
    }

    #[test]
    fn prepaid_and_on_demand_balances_become_notes_when_nonzero() {
        let body = r#"{"config":{
            "creditUsagePercent":1.0,
            "prepaidBalance":{"val":1250},
            "onDemandCap":{"val":5000},
            "onDemandUsed":{"val":300}
        }}"#;
        let UsageReport::Available(evidence) =
            interpret(&credentials(), &HttpOutcome::Ok(body.into()), 1_000)
        else {
            panic!("expected a window");
        };
        assert_eq!(
            evidence.notes,
            ["Prepaid: $12.50", "On-demand: $3.00 / $50.00"]
        );
    }

    #[test]
    fn an_unreadable_reset_leaves_the_reset_unknown_but_keeps_the_percentage() {
        let body = r#"{"config":{"creditUsagePercent":42.0,"currentPeriod":{"end":"whenever"}}}"#;
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
        let body = r#"{"config":{"isUnifiedBillingUser":true,"topUpMethod":"none"}}"#;
        let UsageReport::Unavailable(unavailable) =
            interpret(&credentials(), &HttpOutcome::Ok(body.into()), 1_000)
        else {
            panic!("expected unavailable");
        };
        assert_eq!(unavailable.reason, UsageUnavailableReason::Unsupported);
        assert!(
            unavailable.evidence.contains("isUnifiedBillingUser"),
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
        let expired_at = parse_rfc3339_ms("2026-08-15T00:40:20Z").unwrap();
        let UsageReport::Unavailable(expired) =
            interpret(&credentials(), &HttpOutcome::Unauthorized(401), expired_at)
        else {
            panic!("expected unavailable");
        };
        assert_eq!(expired.reason, UsageUnavailableReason::Expired);
        assert!(expired.evidence.contains("expired"));
        assert!(expired.message.contains("grok"));

        let UsageReport::Unavailable(refused) =
            interpret(&credentials(), &HttpOutcome::Unauthorized(403), 1)
        else {
            panic!("expected unavailable");
        };
        assert_eq!(refused.reason, UsageUnavailableReason::NotSignedIn);
        assert!(!refused.evidence.contains("expired"));
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
    fn credentials_prefer_the_first_party_scope_and_reject_web_login() {
        let parsed = read_credentials(&first_party("oidc", "abc")).expect("credentials");
        assert_eq!(parsed.key, "abc");
        assert_eq!(parsed.user_id, "u1");

        let mixed = format!(
            r#"{{"{FIRST_PARTY_SCOPE}":{{"key":"web","user_id":"u1","auth_mode":"web_login"}},"{LEGACY_SCOPE}":{{"key":"legacy","user_id":"u2","auth_mode":"oidc"}}}}"#
        );
        let fallback = read_credentials(&mixed).expect("legacy fallback");
        assert_eq!(fallback.key, "legacy");
        assert_eq!(fallback.user_id, "u2");

        assert!(read_credentials(&first_party("web_login", "abc")).is_err());
        assert!(read_credentials(r#"{"xai::api_key":{"key":"sk","user_id":"u"}}"#).is_err());
        assert!(read_credentials(&first_party("oidc", "  ")).is_err());
        assert!(read_credentials("not json").is_err());
    }

    #[test]
    fn interpretation_never_puts_the_token_into_a_report() {
        let secret = "xai-oat-SECRET";
        let credentials = Credentials {
            key: secret.into(),
            user_id: "user-1".into(),
            auth_mode: Some("oidc".into()),
            expires_at: None,
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
