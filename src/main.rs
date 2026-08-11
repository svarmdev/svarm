use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use svarm_agent::{
    Result, logging,
    paths::RuntimePaths,
    protocol::{
        ConnectionRole, Request, Response, SessionId, SessionSummary, SessionTarget, StopSummary,
    },
};
use svarm_tui::{InitialSession, StartupChoice};

mod cli;
mod client;
mod server_start;

use cli::{Cli, Command, ServerCommand};
use client::ControlClient;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = RuntimePaths::discover()?;
    let internal_server = matches!(&cli.command, Some(Command::InternalServer));
    if !internal_server {
        let _ = logging::append(
            &paths.client_log,
            concat!("client starting version=", env!("CARGO_PKG_VERSION")),
        );
    }
    let result = match cli.command {
        Some(Command::InternalServer) => server_start::run_server(),
        Some(Command::Server {
            command: ServerCommand::Run,
        }) => server_start::run_server(),
        Some(Command::Server {
            command: ServerCommand::Status,
        }) => server_status(&paths),
        Some(Command::Server {
            command: ServerCommand::Stop { yes },
        }) => stop_server(&paths, yes),
        Some(Command::List) => list_sessions(&paths),
        Some(Command::Stop {
            workspace,
            yes,
            path,
        }) => stop_session(&paths, workspace, path, yes),
        None => launch(&paths, cli),
    };
    if !internal_server {
        let _ = logging::append(
            &paths.client_log,
            if result.is_ok() {
                "client stopped cleanly"
            } else {
                "client stopped after an error"
            },
        );
    }
    result
}

fn launch(paths: &RuntimePaths, cli: Cli) -> Result<()> {
    server_start::ensure_server(paths)?;
    let sessions = running_sessions(paths)?;
    let requested_path = canonicalize(cli.path.as_deref().unwrap_or_else(|| Path::new(".")))?;
    let target = if cli.new_session {
        InitialSession::Create(requested_path)
    } else if cli.attach {
        if let Some(id) = cli.workspace {
            InitialSession::Attach {
                session_id: SessionId(id),
                takeover: cli.takeover,
            }
        } else {
            let eligible = if cli.path.is_some() {
                sessions_for_path(sessions, &requested_path)
            } else {
                sessions
            };
            let Some(target) =
                select_launch_target(eligible, false, &requested_path, cli.takeover)?
            else {
                return Ok(());
            };
            target
        }
    } else if sessions.is_empty() {
        InitialSession::Create(requested_path)
    } else {
        let Some(target) = select_launch_target(sessions, true, &requested_path, false)? else {
            return Ok(());
        };
        target
    };
    svarm_tui::run(cli.agent, paths.socket.clone(), target)
}

fn select_launch_target(
    sessions: Vec<SessionSummary>,
    allow_new: bool,
    requested_path: &Path,
    takeover: bool,
) -> Result<Option<InitialSession>> {
    match discovery_route(&sessions, allow_new) {
        DiscoveryRoute::Create => {
            return Ok(Some(InitialSession::Create(requested_path.to_owned())));
        }
        DiscoveryRoute::Attach(session_id) => {
            return Ok(Some(InitialSession::Attach {
                session_id,
                takeover,
            }));
        }
        DiscoveryRoute::NoSessions => return Err("no running Svarm sessions".into()),
        DiscoveryRoute::Choose => {}
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        print_session_summaries(&sessions);
        return Err(
            "session choice requires a terminal; use `--attach --workspace ID` or `--new-session`"
                .into(),
        );
    }
    match svarm_tui::choose_session(sessions, allow_new)? {
        StartupChoice::Session(session_id) => Ok(Some(InitialSession::Attach {
            session_id,
            takeover,
        })),
        StartupChoice::NewSession => Ok(Some(InitialSession::Create(requested_path.to_owned()))),
        StartupChoice::Cancel => Ok(None),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiscoveryRoute {
    Create,
    Attach(SessionId),
    Choose,
    NoSessions,
}

fn discovery_route(sessions: &[SessionSummary], allow_new: bool) -> DiscoveryRoute {
    match (sessions, allow_new) {
        ([], true) => DiscoveryRoute::Create,
        ([], false) => DiscoveryRoute::NoSessions,
        ([session], false) => DiscoveryRoute::Attach(session.id),
        _ => DiscoveryRoute::Choose,
    }
}

fn list_sessions(paths: &RuntimePaths) -> Result<()> {
    if ControlClient::probe(&paths.socket)?.is_none() {
        println!("no running Svarm sessions");
        return Ok(());
    }
    let sessions = running_sessions(paths)?;
    if sessions.is_empty() {
        println!("no running Svarm sessions");
    } else {
        print_session_summaries(&sessions);
    }
    Ok(())
}

fn stop_session(
    paths: &RuntimePaths,
    workspace: Option<u64>,
    path: Option<PathBuf>,
    yes: bool,
) -> Result<()> {
    if ControlClient::probe(&paths.socket)?.is_none() {
        return Err("no running Svarm sessions".into());
    }
    let sessions = running_sessions(paths)?;
    let target = if let Some(id) = workspace {
        sessions
            .iter()
            .find(|session| session.id == SessionId(id))
            .cloned()
            .ok_or("Svarm session was not found")?
    } else {
        let path = canonicalize(path.as_deref().unwrap_or_else(|| Path::new(".")))?;
        let eligible = sessions_for_path(sessions, &path);
        match eligible.as_slice() {
            [session] => session.clone(),
            [] => return Err("no Svarm session uses that workspace path".into()),
            _ if !io::stdin().is_terminal() || !io::stdout().is_terminal() => {
                print_session_summaries(&eligible);
                return Err("multiple sessions match; use `svarm stop --workspace ID`".into());
            }
            _ => match svarm_tui::choose_session(eligible.clone(), false)? {
                StartupChoice::Session(id) => eligible
                    .into_iter()
                    .find(|session| session.id == id)
                    .expect("chooser returns an eligible session"),
                StartupChoice::Cancel => return Ok(()),
                StartupChoice::NewSession => unreachable!("stop chooser cannot create"),
            },
        }
    };
    if !yes {
        if !confirm(&format!(
            "Stop Svarm session {} at {} and terminate {} running agents ({} total)?",
            target.id.0,
            target.canonical_path.display(),
            target.running_agents,
            target.total_agents
        ))? {
            return Ok(());
        }
    }
    let mut client = ControlClient::connect(&paths.socket, ConnectionRole::Control)?;
    match client.request(Request::StopSession {
        target: SessionTarget::Id(target.id),
        confirmed: true,
    })? {
        Response::Stopped(summary) => print_stop_summary(summary),
        _ => return Err("Svarm server returned an invalid stop response".into()),
    }
    Ok(())
}

fn server_status(paths: &RuntimePaths) -> Result<()> {
    let Some(status) = ControlClient::probe(&paths.socket)? else {
        println!("Svarm server is not running");
        return Ok(());
    };
    println!("Svarm server is running");
    println!("pid: {}", status.process_id);
    println!("version: {}", status.application_version);
    println!("protocol: {}", status.protocol_version);
    println!("socket: {}", status.socket_path.display());
    println!("uptime: {}", format_age(status.uptime_ms));
    println!("sessions: {}", status.session_count);
    println!("clients: {}", status.client_count);
    Ok(())
}

fn stop_server(paths: &RuntimePaths, yes: bool) -> Result<()> {
    let Some(status) = ControlClient::probe(&paths.socket)? else {
        println!("Svarm server is not running");
        return Ok(());
    };
    let sessions = running_sessions(paths)?;
    let agents = sessions
        .iter()
        .map(|session| session.total_agents)
        .sum::<usize>();
    println!(
        "Stopping {} Svarm sessions containing {} agents.",
        status.session_count, agents
    );
    if !yes {
        if !confirm("Stop the Svarm server and every agent it owns?")? {
            return Ok(());
        }
    }
    let mut client = ControlClient::connect(&paths.socket, ConnectionRole::Control)?;
    match client.request(Request::StopServer { confirmed: true })? {
        Response::Stopped(summary) => print_stop_summary(summary),
        _ => return Err("Svarm server returned an invalid stop response".into()),
    }
    Ok(())
}

fn running_sessions(paths: &RuntimePaths) -> Result<Vec<SessionSummary>> {
    let mut client = ControlClient::connect(&paths.socket, ConnectionRole::Control)?;
    match client.request(Request::ListSessions)? {
        Response::Sessions { sessions } => Ok(sessions),
        _ => Err("Svarm server returned an invalid session list".into()),
    }
}

fn sessions_for_path(sessions: Vec<SessionSummary>, path: &Path) -> Vec<SessionSummary> {
    sessions
        .into_iter()
        .filter(|session| session.canonical_path == path)
        .collect()
}

fn print_session_summaries(sessions: &[SessionSummary]) {
    let now = unix_time_ms();
    for session in sessions {
        println!(
            "{}  {}  {}/{} running  {}  {}  {}",
            session.id.0,
            session.display_name,
            session.running_agents,
            session.total_agents,
            if session.attachment.is_some() {
                "attached"
            } else {
                "detached"
            },
            format_age(now.saturating_sub(session.last_user_activity_ms)),
            session.canonical_path.display()
        );
    }
}

fn print_stop_summary(summary: StopSummary) {
    println!(
        "stopped {} sessions and {} agents{}",
        summary.session_count,
        summary.agent_count,
        if summary.server_stopped {
            "; server stopped"
        } else {
            ""
        }
    );
}

fn confirm(prompt: &str) -> Result<bool> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "confirmation requires a terminal; use an unambiguous ID target with `--yes`".into(),
        );
    }
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if answer.trim().eq_ignore_ascii_case("y") {
        Ok(true)
    } else {
        Ok(false)
    }
}

fn canonicalize(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .map_err(|error| format!("could not resolve workspace {}: {error}", path.display()).into())
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn format_age(age_ms: u64) -> String {
    let seconds = age_ms / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use svarm_agent::protocol::SessionRevision;

    #[test]
    fn normal_start_creates_only_when_none_exist_and_otherwise_chooses() {
        assert_eq!(discovery_route(&[], true), DiscoveryRoute::Create);
        assert_eq!(discovery_route(&[summary(1)], true), DiscoveryRoute::Choose);
        assert_eq!(
            discovery_route(&[summary(1), summary(2)], true),
            DiscoveryRoute::Choose
        );
    }

    #[test]
    fn attach_only_selects_one_and_chooses_between_many() {
        assert_eq!(discovery_route(&[], false), DiscoveryRoute::NoSessions);
        assert_eq!(
            discovery_route(&[summary(7)], false),
            DiscoveryRoute::Attach(SessionId(7))
        );
        assert_eq!(
            discovery_route(&[summary(1), summary(2)], false),
            DiscoveryRoute::Choose
        );
    }

    #[test]
    fn path_filter_keeps_duplicate_path_sessions_distinct() {
        let shared = PathBuf::from("/tmp/shared");
        let mut first = summary(1);
        first.canonical_path = shared.clone();
        let mut second = summary(2);
        second.canonical_path = shared.clone();

        let eligible = sessions_for_path(vec![first, summary(3), second], &shared);
        assert_eq!(
            eligible
                .iter()
                .map(|session| session.id)
                .collect::<Vec<_>>(),
            vec![SessionId(1), SessionId(2)]
        );
        assert_eq!(discovery_route(&eligible, false), DiscoveryRoute::Choose);
    }

    fn summary(id: u64) -> SessionSummary {
        SessionSummary {
            id: SessionId(id),
            canonical_path: PathBuf::from(format!("/tmp/project-{id}")),
            display_name: format!("project-{id}"),
            running_agents: 0,
            total_agents: 0,
            attachment: None,
            last_user_activity_ms: 0,
            revision: SessionRevision(0),
        }
    }
}
