use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

mod cwd;
pub mod framing;
mod git;
pub mod input;
#[cfg(unix)]
pub mod ipc;
#[cfg(unix)]
pub mod logging;
mod manager;
#[cfg(unix)]
mod naming;
#[cfg(unix)]
pub mod paths;
pub mod protocol;
mod recognition;
#[cfg(unix)]
pub mod server;
pub mod server_session;
mod session;
mod terminal;
mod terminal_backend;
pub mod terminal_model;
mod terminal_process;

pub use manager::{AgentManager, pty_size};
pub use portable_pty::PtySize;
pub use session::{AgentSession, SessionSnapshot};
pub use terminal::{CursorStyle, TerminalPalette};
pub use terminal_process::{
    ProcessExit, Result, SessionStatus, TerminalNotifier, TerminalProcess, TerminalProcessSnapshot,
};

const CLAUDE_SIGNAL_ENV: &str = "SVARM_CONVERSATION_SOCKET";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AgentId(u64);

impl AgentId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Codex,
    Claude,
}

impl AgentKind {
    pub const ALL: [Self; 2] = [Self::Codex, Self::Claude];

    pub const fn command(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

impl FromStr for AgentKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "codex" => Ok(Self::Codex),
            "claude" | "claude-code" => Ok(Self::Claude),
            _ => Err(format!("unsupported agent {value:?}; use codex or claude")),
        }
    }
}

pub fn claude_hook_session_id(input: &str) -> Option<String> {
    let input = serde_json::from_str::<serde_json::Value>(input).ok()?;
    let id = input.get("session_id")?.as_str()?;
    recognition::looks_like_uuid(id).then(|| id.to_ascii_lowercase())
}

pub fn send_claude_hook_session_id(input: &str) -> Result<()> {
    let Some(id) = claude_hook_session_id(input) else {
        return Ok(());
    };
    let Some(path) = std::env::var_os(CLAUDE_SIGNAL_ENV) else {
        return Ok(());
    };
    std::os::unix::net::UnixDatagram::unbound()?.send_to(id.as_bytes(), path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_supported_agents() {
        assert_eq!("codex".parse(), Ok(AgentKind::Codex));
        assert_eq!("claude-code".parse(), Ok(AgentKind::Claude));
        assert!("opencode".parse::<AgentKind>().is_err());
    }

    #[test]
    fn claude_hook_accepts_only_a_valid_session_id() {
        assert_eq!(
            claude_hook_session_id(
                r#"{"session_id":"019FF1D3-375E-7A72-A176-C47497827E49","source":"clear"}"#,
            )
            .as_deref(),
            Some("019ff1d3-375e-7a72-a176-c47497827e49")
        );
        assert_eq!(claude_hook_session_id(r#"{"session_id":"bad"}"#), None);
        assert_eq!(claude_hook_session_id("not json"), None);
    }
}
