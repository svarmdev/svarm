use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use svarm_agent::{
    Result, logging,
    paths::RuntimePaths,
    protocol::{ConnectionRole, Request, Response, SessionId, SessionSummary, StopSummary},
};
use svarm_tui::{InitialAgentRequest, InitialSession, StartupChoice};

mod cli;
mod client;
mod server_start;

use cli::{Cli, Command, ServerCommand};
use client::{ControlClient, Probe};

const NONINTERACTIVE_CHOICE_ERROR: &str =
    "session choice requires a terminal; use `--attach --session ID` or `--new-session`";

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
        Some(Command::Stop { session, yes }) => stop_session(&paths, session, yes),
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
    let initial_agent = InitialAgentRequest {
        kind: cli.agent,
        workspace: requested_workspace(&cli)?,
    };
    let target = if cli.new_session {
        InitialSession::Create
    } else if cli.attach {
        if let Some(id) = cli.session {
            InitialSession::Attach {
                session_id: SessionId(id),
                takeover: cli.takeover,
            }
        } else {
            let Some(target) = select_launch_target(sessions, false, cli.takeover)? else {
                return Ok(());
            };
            target
        }
    } else if sessions.is_empty() {
        InitialSession::Create
    } else {
        let Some(target) = select_launch_target(sessions, true, false)? else {
            return Ok(());
        };
        target
    };
    svarm_tui::run(initial_agent, paths.socket.clone(), target)
}

fn select_launch_target(
    sessions: Vec<SessionSummary>,
    allow_new: bool,
    takeover: bool,
) -> Result<Option<InitialSession>> {
    match discovery_route(&sessions, allow_new) {
        DiscoveryRoute::Create => return Ok(Some(InitialSession::Create)),
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
        return Err(NONINTERACTIVE_CHOICE_ERROR.into());
    }
    match svarm_tui::choose_session(sessions, allow_new)? {
        StartupChoice::Session(session_id) => Ok(Some(InitialSession::Attach {
            session_id,
            takeover,
        })),
        StartupChoice::NewSession => Ok(Some(InitialSession::Create)),
        StartupChoice::Cancel => Ok(None),
    }
}

fn requested_workspace(cli: &Cli) -> Result<Option<PathBuf>> {
    cli.path.as_deref().map(canonicalize).transpose()
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

fn stop_session(paths: &RuntimePaths, session: Option<u64>, yes: bool) -> Result<()> {
    if ControlClient::probe(&paths.socket)?.is_none() {
        return Err("no running Svarm sessions".into());
    }
    let sessions = running_sessions(paths)?;
    let target = if let Some(id) = session {
        sessions
            .iter()
            .find(|session| session.id == SessionId(id))
            .cloned()
            .ok_or("Svarm session was not found")?
    } else {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            print_session_summaries(&sessions);
            return Err("choose a session with `svarm stop --session ID`".into());
        }
        match svarm_tui::choose_session(sessions.clone(), false)? {
            StartupChoice::Session(id) => sessions
                .into_iter()
                .find(|session| session.id == id)
                .expect("chooser returns a listed session"),
            StartupChoice::Cancel => return Ok(()),
            StartupChoice::NewSession => unreachable!("stop chooser cannot create"),
        }
    };
    if !yes
        && !confirm(&format!(
            "Stop Svarm session {} and terminate {} running agents ({} total)?",
            target.id.0, target.running_agents, target.total_agents
        ))?
    {
        return Ok(());
    }
    let mut client = ControlClient::connect(&paths.socket, ConnectionRole::Control)?;
    match client.request(Request::StopSession {
        session_id: target.id,
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
    let status = match ControlClient::probe_socket(&paths.socket)? {
        Probe::None => {
            println!("Svarm server is not running");
            return Ok(());
        }
        Probe::Running(status) => *status,
        // Stopping is exactly what a user reaches for after an upgrade, so it has to work against
        // a server this build cannot talk to. Its own socket is unusable, so signal the process.
        Probe::Incompatible(reason) => return stop_incompatible_server(paths, yes, &reason),
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
    if !yes && !confirm("Stop the Svarm server and every agent it owns?")? {
        return Ok(());
    }
    let mut client = ControlClient::connect(&paths.socket, ConnectionRole::Control)?;
    match client.request(Request::StopServer { confirmed: true })? {
        Response::Stopped(summary) => print_stop_summary(summary),
        _ => return Err("Svarm server returned an invalid stop response".into()),
    }
    Ok(())
}

/// Terminates a server that cannot be asked to stop. The signal is the same one the server handles
/// gracefully when the terminal sends it, so agents are still shut down in order.
fn stop_incompatible_server(paths: &RuntimePaths, yes: bool, reason: &str) -> Result<()> {
    let Some(pid) = paths.read_pid() else {
        return Err(format!(
            "{reason}\nThe running server did not record its process, so it cannot be stopped \
             automatically."
        )
        .into());
    };
    println!("A Svarm server from a different build is running as process {pid}.");
    println!("{reason}");
    if !yes && !confirm("Stop it and every agent it owns?")? {
        return Ok(());
    }
    // SAFETY: kill with a valid signal has no preconditions; a stale PID simply fails.
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
        return Err(format!(
            "could not stop Svarm server process {pid}: {}",
            io::Error::last_os_error()
        )
        .into());
    }

    let deadline = SystemTime::now() + Duration::from_secs(5);
    while SystemTime::now() < deadline {
        if matches!(ControlClient::probe_socket(&paths.socket)?, Probe::None) {
            println!("Svarm server stopped.");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("Svarm server process {pid} did not exit").into())
}

fn running_sessions(paths: &RuntimePaths) -> Result<Vec<SessionSummary>> {
    let mut client = ControlClient::connect(&paths.socket, ConnectionRole::Control)?;
    match client.request(Request::ListSessions)? {
        Response::Sessions { sessions } => Ok(sessions),
        _ => Err("Svarm server returned an invalid session list".into()),
    }
}

fn print_session_summaries(sessions: &[SessionSummary]) {
    let now = unix_time_ms();
    for session in sessions {
        println!(
            "{}  {}  {}/{} running  {}",
            session.id.0,
            if session.attachment.is_some() {
                "attached"
            } else {
                "detached"
            },
            session.running_agents,
            session.total_agents,
            format_age(now.saturating_sub(session.last_user_activity_ms))
        );
    }
}

fn print_stop_summary(summary: StopSummary) {
    println!(
        "stopped {} sessions and {} agents{}{}",
        summary.session_count,
        summary.agent_count,
        if summary.server_stopped {
            "; server stopped"
        } else {
            ""
        },
        if summary.cleanup_errors == 0 {
            String::new()
        } else {
            format!("; {} cleanup errors", summary.cleanup_errors)
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
    path.canonicalize().map_err(|error| {
        format!(
            "could not resolve agent workspace {}: {error}",
            path.display()
        )
        .into()
    })
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
    fn noninteractive_choice_error_names_both_deterministic_remedies() {
        assert!(NONINTERACTIVE_CHOICE_ERROR.contains("--attach --session ID"));
        assert!(NONINTERACTIVE_CHOICE_ERROR.contains("--new-session"));
    }

    #[test]
    fn explicit_path_is_an_agent_workspace_even_for_id_attachment() {
        let workspace = std::env::current_dir().unwrap();
        let cli = Cli::try_parse_from([
            "svarm",
            "--attach",
            "--session",
            "7",
            workspace.to_str().unwrap(),
        ])
        .unwrap();

        assert_eq!(requested_workspace(&cli).unwrap(), Some(workspace));
    }

    fn summary(id: u64) -> SessionSummary {
        SessionSummary {
            id: SessionId(id),
            running_agents: 0,
            total_agents: 0,
            attachment: None,
            last_user_activity_ms: 0,
            revision: SessionRevision(0),
        }
    }
}
