use std::path::PathBuf;

use clap::{Parser, Subcommand};
use svarm_tui::AgentKind;

#[derive(Debug, Parser)]
#[command(version, about = "A small terminal multiplexer for coding agents")]
pub struct Cli {
    /// Attach only; never create a Svarm session.
    #[arg(long, conflicts_with = "new_session")]
    pub attach: bool,

    /// Always create a distinct Svarm session.
    #[arg(long, conflicts_with = "attach")]
    pub new_session: bool,

    /// Target a server-lifetime Svarm session ID.
    #[arg(long, requires = "attach")]
    pub workspace: Option<u64>,

    /// Deliberately disconnect an existing interactive client.
    #[arg(long, requires = "attach")]
    pub takeover: bool,

    /// Start one additional agent after opening the session.
    #[arg(short, long)]
    pub agent: Option<AgentKind>,

    /// Workspace path used for creation or attach filtering.
    pub path: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List running Svarm sessions.
    List,
    /// Stop one Svarm session.
    Stop {
        /// Target a server-lifetime session ID.
        #[arg(long)]
        workspace: Option<u64>,
        /// Confirm an unambiguous ID target without a terminal prompt.
        #[arg(long, requires = "workspace")]
        yes: bool,
        /// Filter sessions by canonical workspace path.
        path: Option<PathBuf>,
    },
    /// Inspect or stop the per-user Svarm server.
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    #[command(name = "__server", hide = true)]
    InternalServer,
}

#[derive(Debug, Subcommand)]
pub enum ServerCommand {
    /// Report server process and session status.
    Status,
    /// Stop every Svarm session and the server.
    Stop {
        /// Skip the terminal confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Run the server in the foreground for diagnostics.
    Run,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_launch_has_no_explicit_path_or_mode() {
        let cli = Cli::try_parse_from(["svarm"]).unwrap();
        assert!(!cli.attach);
        assert!(!cli.new_session);
        assert_eq!(cli.path, None);
        assert!(cli.command.is_none());
    }

    #[test]
    fn attach_by_id_and_takeover_are_explicit() {
        let cli = Cli::try_parse_from([
            "svarm",
            "--attach",
            "--workspace",
            "17",
            "--takeover",
            "--agent",
            "claude",
        ])
        .unwrap();
        assert_eq!(cli.workspace, Some(17));
        assert!(cli.takeover);
        assert_eq!(cli.agent, Some(AgentKind::Claude));
    }

    #[test]
    fn attach_and_new_session_conflict() {
        assert!(Cli::try_parse_from(["svarm", "--attach", "--new-session"]).is_err());
    }

    #[test]
    fn parses_control_commands() {
        let cli = Cli::try_parse_from(["svarm", "server", "stop", "--yes"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Server {
                command: ServerCommand::Stop { yes: true }
            })
        ));
        let cli = Cli::try_parse_from(["svarm", "stop", "--workspace", "2", "--yes"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Stop { yes: true, .. })));
        let cli = Cli::try_parse_from(["svarm", "__server"]).unwrap();
        assert!(matches!(cli.command, Some(Command::InternalServer)));
    }
}
