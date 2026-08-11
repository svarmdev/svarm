use std::path::PathBuf;

use clap::Parser;

use crate::AgentKind;

#[derive(Debug, Parser)]
#[command(version, about = "A small terminal multiplexer for coding agents")]
pub struct Cli {
    /// Agent to open first: codex or claude.
    #[arg(short, long, default_value = "codex")]
    pub agent: AgentKind,

    /// Workspace in which agents are started.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_codex_in_current_directory() {
        let cli = Cli::try_parse_from(["svarm"]).unwrap();
        assert_eq!(cli.agent, AgentKind::Codex);
        assert_eq!(cli.path, PathBuf::from("."));
    }

    #[test]
    fn accepts_claude_and_a_workspace() {
        let cli = Cli::try_parse_from(["svarm", "--agent", "claude", "/tmp/project"]).unwrap();
        assert_eq!(cli.agent, AgentKind::Claude);
        assert_eq!(cli.path, PathBuf::from("/tmp/project"));
    }
}
