//! The only `ureq` call site in svarm.
//!
//! Confined to one function so a transport change touches one place, and so probe interpretation
//! stays a pure function of [`HttpOutcome`] that tests can drive without a network.

use std::time::Duration;

/// What a probe request produced. Distinguishing "refused" from "broke" matters: the first is a
/// statement about the account, the second only about the attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HttpOutcome {
    Ok(String),
    /// 401 or 403: the provider rejected the credential.
    Unauthorized(u16),
    /// Any other non-success status, with a truncated body for evidence.
    Status(u16, String),
    /// The request never produced a response.
    Transport(String),
}

const BODY_EVIDENCE_CHARS: usize = 160;

/// GET `url` with a bearer token. The token is never placed in an error string.
///
/// `extra_headers` is for provider-specific identity the endpoint requires on
/// top of `Authorization` (Grok's `x-userid` and friends). Do not put the
/// bearer token here.
pub(crate) fn get_json(
    url: &str,
    bearer: &str,
    timeout: Duration,
    extra_headers: &[(&str, &str)],
) -> HttpOutcome {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(concat!("svarm/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();

    let mut request = agent
        .get(url)
        .header("Authorization", &format!("Bearer {bearer}"))
        .header("Accept", "application/json");
    for (name, value) in extra_headers {
        request = request.header(*name, *value);
    }
    let response = request.call();

    match response {
        Ok(mut response) => match response.body_mut().read_to_string() {
            Ok(body) => HttpOutcome::Ok(body),
            Err(error) => HttpOutcome::Transport(format!("could not read the response: {error}")),
        },
        Err(ureq::Error::StatusCode(code)) if matches!(code, 401 | 403) => {
            HttpOutcome::Unauthorized(code)
        }
        Err(ureq::Error::StatusCode(code)) => HttpOutcome::Status(code, String::new()),
        Err(error) => HttpOutcome::Transport(error.to_string()),
    }
}

/// Shorten a response body for use as evidence in the interface.
pub(crate) fn evidence_snippet(body: &str) -> String {
    let flattened = body.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&flattened, BODY_EVIDENCE_CHARS)
}

pub(crate) fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let kept: String = text.chars().take(limit.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_collapses_whitespace_and_bounds_length() {
        assert_eq!(evidence_snippet("{\n  \"a\": 1\n}"), "{ \"a\": 1 }");
        let long = "x".repeat(500);
        let snippet = evidence_snippet(&long);
        assert_eq!(snippet.chars().count(), BODY_EVIDENCE_CHARS);
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate_chars("héllo", 10), "héllo");
        assert_eq!(truncate_chars("héllo", 3), "hé…");
    }
}
