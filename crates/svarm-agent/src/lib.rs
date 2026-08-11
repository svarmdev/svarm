use std::{fmt, str::FromStr};

mod manager;
mod session;
mod terminal;

pub use manager::{AgentManager, pty_size};
pub use portable_pty::PtySize;
pub use session::{
    AgentSession, ProcessExit, Result, SessionSnapshot, SessionStatus, TerminalSnapshot,
};
pub use terminal::TerminalPalette;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AgentId(u64);

impl AgentId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
