//! Version checks and native self-updates for supported coding-agent harnesses.
//!
//! Process execution and release-service access live here at the agent boundary. Callers receive
//! plain observations and decide how to schedule or present them.

use std::{
    process::{Command, Stdio},
    time::Duration,
};

use semver::Version;
use serde::Deserialize;

use crate::AgentKind;

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const ERROR_CHARS: usize = 240;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessVersion {
    pub kind: AgentKind,
    pub current: String,
    pub latest: String,
    pub update_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessUpdated {
    pub kind: AgentKind,
    pub previous: String,
    pub current: String,
}

pub fn check(kind: AgentKind) -> Result<HarnessVersion, String> {
    if kind == AgentKind::Grok {
        return check_grok();
    }
    let current = current_version(kind)?;
    let latest = fetch_latest_release(kind)?;
    let update_available = newer(&latest, &current)?;
    Ok(HarnessVersion {
        kind,
        current,
        latest,
        update_available,
    })
}

pub fn update(kind: AgentKind) -> Result<HarnessUpdated, String> {
    let previous = current_version(kind)?;
    let output = Command::new(kind.command())
        .args(update_args(kind))
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not start {} updater: {error}", kind.label()))?;
    if !output.status.success() {
        let evidence = command_evidence(&output.stderr, &output.stdout);
        return Err(if evidence.is_empty() {
            format!("{} updater exited with {}", kind.label(), output.status)
        } else {
            format!("{} update failed: {evidence}", kind.label())
        });
    }
    let current = current_version(kind)?;
    if current == previous {
        return Err(format!(
            "{} updater completed, but the installed version is still {current}",
            kind.label()
        ));
    }
    Ok(HarnessUpdated {
        kind,
        previous,
        current,
    })
}

fn current_version(kind: AgentKind) -> Result<String, String> {
    let output = Command::new(kind.command())
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run `{} --version`: {error}", kind.command()))?;
    if !output.status.success() {
        let evidence = command_evidence(&output.stderr, &output.stdout);
        return Err(if evidence.is_empty() {
            format!(
                "`{} --version` exited with {}",
                kind.command(),
                output.status
            )
        } else {
            format!("could not read {} version: {evidence}", kind.label())
        });
    }
    parse_version_output(&String::from_utf8_lossy(&output.stdout))
        .or_else(|| parse_version_output(&String::from_utf8_lossy(&output.stderr)))
        .ok_or_else(|| format!("{} returned an unreadable version", kind.label()))
}

fn fetch_latest_release(kind: AgentKind) -> Result<String, String> {
    let (repository, prefix) = release_source(kind)
        .ok_or_else(|| format!("{} has no release metadata source", kind.label()))?;
    let url = format!("https://api.github.com/repos/{repository}/releases/latest");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .user_agent(concat!("svarm/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut response = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|error| format!("could not check {} releases: {error}", kind.label()))?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("could not read {} release metadata: {error}", kind.label()))?;
    parse_release(&body, prefix)
        .map_err(|error| format!("could not read latest {} release: {error}", kind.label()))
}

fn check_grok() -> Result<HarnessVersion, String> {
    let output = Command::new(AgentKind::Grok.command())
        .args(["update", "--check", "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not start Grok update check: {error}"))?;
    if !output.status.success() {
        let evidence = command_evidence(&output.stderr, &output.stdout);
        return Err(if evidence.is_empty() {
            format!("Grok update check exited with {}", output.status)
        } else {
            format!("Grok update check failed: {evidence}")
        });
    }
    parse_grok_check(&String::from_utf8_lossy(&output.stdout))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrokCheck {
    current_version: String,
    latest_version: Option<String>,
    update_available: bool,
    error: Option<String>,
}

fn parse_grok_check(json: &str) -> Result<HarnessVersion, String> {
    let check: GrokCheck = serde_json::from_str(json)
        .map_err(|error| format!("Grok returned unreadable update JSON: {error}"))?;
    if let Some(error) = check.error.filter(|error| !error.trim().is_empty()) {
        return Err(format!("Grok update check failed: {}", sanitize(&error)));
    }
    let latest = check
        .latest_version
        .unwrap_or_else(|| check.current_version.clone());
    Version::parse(&check.current_version)
        .map_err(|error| format!("Grok returned an invalid current version: {error}"))?;
    Version::parse(&latest)
        .map_err(|error| format!("Grok returned an invalid latest version: {error}"))?;
    Ok(HarnessVersion {
        kind: AgentKind::Grok,
        current: check.current_version,
        latest,
        update_available: check.update_available,
    })
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
}

fn parse_release(json: &str, prefix: &str) -> Result<String, String> {
    let release: GithubRelease =
        serde_json::from_str(json).map_err(|error| format!("invalid JSON: {error}"))?;
    let version = release
        .tag_name
        .strip_prefix(prefix)
        .ok_or_else(|| format!("unexpected tag {:?}", release.tag_name))?;
    Version::parse(version).map_err(|error| format!("invalid version {version:?}: {error}"))?;
    Ok(version.to_owned())
}

fn parse_version_output(output: &str) -> Option<String> {
    output.split_whitespace().find_map(|word| {
        let candidate = word.trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '.' | '-' | '+')
        });
        let candidate = candidate.strip_prefix('v').unwrap_or(candidate);
        Version::parse(candidate)
            .ok()
            .map(|version| version.to_string())
    })
}

fn newer(latest: &str, current: &str) -> Result<bool, String> {
    let latest = Version::parse(latest)
        .map_err(|error| format!("latest version {latest:?} is invalid: {error}"))?;
    let current = Version::parse(current)
        .map_err(|error| format!("installed version {current:?} is invalid: {error}"))?;
    Ok(latest > current)
}

const fn release_source(kind: AgentKind) -> Option<(&'static str, &'static str)> {
    match kind {
        AgentKind::Codex => Some(("openai/codex", "rust-v")),
        AgentKind::Claude => Some(("anthropics/claude-code", "v")),
        AgentKind::Pi => Some(("earendil-works/pi", "v")),
        AgentKind::OpenCode => Some(("anomalyco/opencode", "v")),
        AgentKind::Grok => None,
    }
}

const fn update_args(kind: AgentKind) -> &'static [&'static str] {
    match kind {
        AgentKind::Codex | AgentKind::Claude | AgentKind::Grok => &["update"],
        AgentKind::Pi => &["update", "--self"],
        AgentKind::OpenCode => &["upgrade"],
    }
}

fn command_evidence(primary: &[u8], fallback: &[u8]) -> String {
    let primary = sanitize(&String::from_utf8_lossy(primary));
    let evidence = if primary.is_empty() {
        sanitize(&String::from_utf8_lossy(fallback))
    } else {
        primary
    };
    truncate(&evidence, ERROR_CHARS)
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || character.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let kept: String = value.chars().take(limit.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_supported_version_shape() {
        for (output, expected) in [
            ("codex-cli 0.147.0\n", "0.147.0"),
            ("2.1.235 (Claude Code)\n", "2.1.235"),
            ("grok 1.0.5 (5115b46bc9) [stable]\n", "1.0.5"),
            ("pi v0.83.0\n", "0.83.0"),
            ("1.15.11\n", "1.15.11"),
            ("codex-cli 0.148.0-alpha.2\n", "0.148.0-alpha.2"),
        ] {
            assert_eq!(parse_version_output(output).as_deref(), Some(expected));
        }
        assert_eq!(parse_version_output("not a version"), None);
    }

    #[test]
    fn parses_provider_release_tag_prefixes() {
        assert_eq!(
            parse_release(r#"{"tag_name":"rust-v0.148.0"}"#, "rust-v").unwrap(),
            "0.148.0"
        );
        assert_eq!(
            parse_release(r#"{"tag_name":"v2.1.235"}"#, "v").unwrap(),
            "2.1.235"
        );
        assert!(parse_release(r#"{"tag_name":"rusty-v8-v150.4.0"}"#, "rust-v").is_err());
    }

    #[test]
    fn grok_errors_are_not_mistaken_for_up_to_date() {
        let error = parse_grok_check(
            r#"{"currentVersion":"1.0.5","latestVersion":null,"updateAvailable":false,"error":"channel fetch failed"}"#,
        )
        .unwrap_err();
        assert!(error.contains("channel fetch failed"));

        assert_eq!(
            parse_grok_check(
                r#"{"currentVersion":"1.0.5","latestVersion":"1.0.6","updateAvailable":true,"error":null}"#,
            )
            .unwrap(),
            HarnessVersion {
                kind: AgentKind::Grok,
                current: "1.0.5".into(),
                latest: "1.0.6".into(),
                update_available: true,
            }
        );
    }

    #[test]
    fn version_comparison_handles_prereleases() {
        assert!(newer("1.0.0", "1.0.0-alpha.1").unwrap());
        assert!(!newer("1.0.0-alpha.1", "1.0.0").unwrap());
        assert!(!newer("1.0.0", "1.0.0").unwrap());
    }

    #[test]
    fn every_harness_has_a_fixed_native_update_command() {
        assert_eq!(update_args(AgentKind::Codex), ["update"]);
        assert_eq!(update_args(AgentKind::Claude), ["update"]);
        assert_eq!(update_args(AgentKind::Grok), ["update"]);
        assert_eq!(update_args(AgentKind::Pi), ["update", "--self"]);
        assert_eq!(update_args(AgentKind::OpenCode), ["upgrade"]);
    }

    #[test]
    fn every_non_grok_harness_has_an_official_stable_release_source() {
        assert_eq!(
            release_source(AgentKind::Codex),
            Some(("openai/codex", "rust-v"))
        );
        assert_eq!(
            release_source(AgentKind::Claude),
            Some(("anthropics/claude-code", "v"))
        );
        assert_eq!(
            release_source(AgentKind::Pi),
            Some(("earendil-works/pi", "v"))
        );
        assert_eq!(
            release_source(AgentKind::OpenCode),
            Some(("anomalyco/opencode", "v"))
        );
        assert_eq!(release_source(AgentKind::Grok), None);
    }
}
