use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

mod base64;

/// The terminal emulator behind every agent screen, re-exported so the interface parses and
/// renders exactly the state the agent side produced.
pub use vt100;

pub mod framing;
mod git;
pub mod input;
#[cfg(unix)]
pub mod ipc;
#[cfg(unix)]
pub mod logging;
mod manager;
#[cfg(unix)]
pub mod paths;
pub mod protocol;
mod recognition;
#[cfg(unix)]
pub mod server;
pub mod server_session;
mod session;
mod terminal;
mod terminal_process;

pub use manager::{AgentManager, pty_size};
pub use portable_pty::PtySize;
pub use session::{AgentSession, SessionSnapshot};
pub use terminal::{CursorStyle, TerminalPalette};
pub use terminal_process::{
    ProcessExit, Result, SessionStatus, TerminalNotifier, TerminalProcess, TerminalProcessSnapshot,
};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_supported_agents() {
        assert_eq!("codex".parse(), Ok(AgentKind::Codex));
        assert_eq!("claude-code".parse(), Ok(AgentKind::Claude));
        assert!("opencode".parse::<AgentKind>().is_err());
    }
}
