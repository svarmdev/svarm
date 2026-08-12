use std::{
    fs::File,
    io::{self, Read},
    os::unix::net::UnixDatagram,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use portable_pty::{CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};

use crate::recognition::ConversationIdDetector;
use crate::{
    AgentId, AgentKind, ProcessExit, Result, SessionStatus, TerminalPalette,
    terminal_model::TerminalSnapshot, terminal_process::TerminalProcess,
};

/// Called when an agent's terminal changes so its owner can wake immediately.
pub type OutputNotifier = Arc<dyn Fn(AgentId) + Send + Sync>;

pub(crate) struct ConversationTracking {
    pub(crate) kind: AgentKind,
    pub(crate) initial_id: Option<String>,
    signal: Option<ConversationSignal>,
}

impl ConversationTracking {
    pub(crate) fn new(kind: AgentKind, initial_id: Option<String>) -> Result<Self> {
        Ok(Self {
            kind,
            initial_id,
            signal: (kind == AgentKind::Claude)
                .then(ConversationSignal::bind)
                .transpose()?,
        })
    }

    #[cfg(test)]
    pub(crate) const fn without_signal(kind: AgentKind, initial_id: Option<String>) -> Self {
        Self {
            kind,
            initial_id,
            signal: None,
        }
    }

    pub(crate) fn configure(&self, command: &mut CommandBuilder) {
        if let Some(signal) = &self.signal {
            command.env(crate::CLAUDE_SIGNAL_ENV, signal.path.as_os_str());
        }
    }
}

struct ConversationSignal {
    socket: UnixDatagram,
    path: PathBuf,
}

impl ConversationSignal {
    fn bind() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "svarm-conversation-{}-{}.sock",
            std::process::id(),
            new_uuid()?
        ));
        let socket = UnixDatagram::bind(&path)?;
        socket.set_nonblocking(true)?;
        Ok(Self { socket, path })
    }

    fn latest(&self) -> Option<String> {
        let mut latest = None;
        let mut bytes = [0; 64];
        loop {
            match self.socket.recv(&mut bytes) {
                Ok(length) => {
                    let id = std::str::from_utf8(&bytes[..length]).ok()?;
                    if crate::recognition::looks_like_uuid(id) {
                        latest = Some(id.to_ascii_lowercase());
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return latest,
                Err(_) => return latest,
            }
        }
    }
}

impl Drop for ConversationSignal {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) const SCROLLBACK_BYTES: usize = 25_000_000;

pub struct AgentSession {
    id: AgentId,
    kind: AgentKind,
    launch_directory: PathBuf,
    terminal: TerminalProcess,
    conversation_id: Arc<Mutex<Option<String>>>,
    conversation_signal: Option<ConversationSignal>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSnapshot {
    pub id: AgentId,
    pub kind: AgentKind,
    pub launch_directory: PathBuf,
    pub status: SessionStatus,
    pub output_generation: u64,
    pub read_error: Option<String>,
    pub exit: Option<ProcessExit>,
    pub conversation_id: Option<String>,
}

impl AgentSession {
    pub fn spawn(
        id: AgentId,
        kind: AgentKind,
        cwd: &Path,
        size: PtySize,
        palette: Option<TerminalPalette>,
    ) -> Result<Self> {
        let conversation_id = (kind == AgentKind::Claude).then(new_uuid).transpose()?;
        let mut command = agent_command(kind, cwd, conversation_id.as_deref())?;
        let tracking = ConversationTracking::new(kind, conversation_id)?;
        tracking.configure(&mut command);
        Self::spawn_command_with_conversation(id, cwd, size, command, palette, None, tracking)
    }

    pub(crate) fn spawn_command_with_conversation(
        id: AgentId,
        cwd: &Path,
        size: PtySize,
        command: CommandBuilder,
        palette: Option<TerminalPalette>,
        notify: Option<OutputNotifier>,
        conversation: ConversationTracking,
    ) -> Result<Self> {
        let ConversationTracking {
            kind,
            initial_id,
            signal,
        } = conversation;
        let notify = notify.map(|notify| Arc::new(move || notify(id)) as _);
        let conversation_id = Arc::new(Mutex::new(initial_id));
        let detected_id = conversation_id.clone();
        let detector = Arc::new(Mutex::new(ConversationIdDetector::new(kind)));
        let output_observer = Arc::new(move |bytes: &[u8]| {
            let mut detector = detector.lock().unwrap_or_else(|poison| poison.into_inner());
            if let Some(id) = detector.process(bytes) {
                *detected_id
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = Some(id);
            }
        });
        Ok(Self {
            id,
            kind,
            launch_directory: cwd.to_owned(),
            terminal: TerminalProcess::spawn_command_with_scrollback_bytes(
                command,
                cwd,
                size,
                palette,
                notify,
                Some(output_observer),
                SCROLLBACK_BYTES,
            )?,
            conversation_id,
            conversation_signal: signal,
        })
    }

    /// Where the agent is working right now, which is not the directory it was launched in once
    /// it moves itself into another checkout. `None` when the live directory cannot be observed.
    pub(crate) fn working_directory(&self) -> Option<PathBuf> {
        crate::cwd::of_process(self.terminal.foreground_process()?)
    }

    pub fn terminal_modes(&self) -> crate::protocol::TerminalModes {
        self.terminal.terminal_modes()
    }

    pub fn with_terminal<T>(&self, read: impl FnOnce(&TerminalSnapshot) -> T) -> T {
        self.terminal.with_terminal(read)
    }

    pub fn terminal_snapshot(&self) -> TerminalSnapshot {
        self.terminal.terminal_snapshot()
    }

    pub fn viewport(&self, requested: usize) -> TerminalSnapshot {
        self.terminal.viewport(requested)
    }

    pub fn set_terminal_palette(&self, palette: Option<TerminalPalette>) {
        self.terminal.set_terminal_palette(palette);
    }

    pub fn send(&self, bytes: &[u8]) -> Result<()> {
        self.terminal.send(bytes)
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.terminal.resize(rows, cols)
    }

    pub fn stop(&mut self) -> Result<()> {
        self.terminal.stop()
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        if let Some(id) = self
            .conversation_signal
            .as_ref()
            .and_then(ConversationSignal::latest)
        {
            *self
                .conversation_id
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(id);
        }
        SessionSnapshot {
            id: self.id,
            kind: self.kind,
            launch_directory: self.launch_directory.clone(),
            status: self.terminal.status(),
            output_generation: self.terminal.generation(),
            read_error: self.terminal.read_error(),
            exit: self.terminal.exit(),
            conversation_id: self
                .conversation_id
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone(),
        }
    }

    pub fn poll(&mut self) -> Result<SessionSnapshot> {
        self.terminal.poll()?;
        Ok(self.snapshot())
    }
}

pub(crate) fn agent_command(
    kind: AgentKind,
    cwd: &Path,
    conversation_id: Option<&str>,
) -> Result<CommandBuilder> {
    let mut command = CommandBuilder::new(kind.command());
    command.cwd(cwd);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env_remove("NO_COLOR");
    command.env("SVARM", "1");
    match kind {
        AgentKind::Codex => {
            command.args([
                "-c",
                r#"tui.terminal_title=["activity","run-state","thread-title"]"#,
            ]);
        }
        AgentKind::Claude => {
            command.env_remove("CLAUDECODE");
            command.env_remove("CLAUDE_CODE_DISABLE_TERMINAL_TITLE");
            if let Some(conversation_id) = conversation_id {
                command.args(["--session-id", conversation_id]);
            }
            command.args(["--settings", &claude_hook_settings()?]);
        }
    }
    Ok(command)
}

pub(crate) fn resume_agent_command(
    kind: AgentKind,
    cwd: &Path,
    conversation_id: &str,
) -> Result<CommandBuilder> {
    let mut command = agent_command(kind, cwd, None)?;
    match kind {
        AgentKind::Codex => command.args(["resume", conversation_id]),
        AgentKind::Claude => command.args(["--resume", conversation_id]),
    }
    Ok(command)
}

pub(crate) fn new_uuid() -> Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    ))
}

fn claude_hook_settings() -> Result<String> {
    let executable = std::env::current_exe()?;
    let command = format!(
        "{} __conversation-hook",
        shell_quote(&executable.to_string_lossy())
    );
    Ok(serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "startup|resume|clear|compact",
                "hooks": [{ "type": "command", "command": command }]
            }]
        }
    })
    .to_string())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn working_directory_follows_the_agent_out_of_its_launch_directory() {
        use std::time::{Duration, Instant};

        let launch = std::env::temp_dir();
        let moved = launch.join(format!("svarm-session-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&moved).unwrap();

        let mut command = CommandBuilder::new("sh");
        command.args(["-c", &format!("cd '{}' && exec cat", moved.display())]);
        command.cwd(&launch);
        let mut session = AgentSession::spawn_command_with_conversation(
            AgentId::new(1),
            &launch,
            PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
            command,
            None,
            None,
            ConversationTracking::without_signal(AgentKind::Codex, None),
        )
        .unwrap();

        let expected = moved.canonicalize().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && session.working_directory() != Some(expected.clone()) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(session.working_directory(), Some(expected));

        session.stop().unwrap();
        std::fs::remove_dir_all(&moved).unwrap();
    }

    #[test]
    fn native_agent_owns_its_terminal_colors() {
        let cwd = std::env::current_dir().unwrap();
        let command = agent_command(AgentKind::Codex, &cwd, None).unwrap();

        assert_eq!(command.get_env("TERM"), Some(OsStr::new("xterm-256color")));
        assert_eq!(command.get_env("COLORTERM"), Some(OsStr::new("truecolor")));
        assert_eq!(command.get_env("NO_COLOR"), None);
    }

    #[test]
    fn codex_reports_activity_and_thread_name_in_the_terminal_title() {
        let cwd = std::env::current_dir().unwrap();
        let command = agent_command(AgentKind::Codex, &cwd, None).unwrap();

        assert_eq!(
            command.get_argv(),
            &vec![
                std::ffi::OsString::from("codex"),
                std::ffi::OsString::from("-c"),
                std::ffi::OsString::from(
                    r#"tui.terminal_title=["activity","run-state","thread-title"]"#,
                ),
            ]
        );
    }

    #[test]
    fn provider_commands_start_and_resume_the_requested_conversation() {
        let cwd = std::env::current_dir().unwrap();
        let id = "019ff1d3-375e-7a72-a176-c47497827e49";
        let claude = agent_command(AgentKind::Claude, &cwd, Some(id)).unwrap();
        let claude_args = claude.get_argv();
        assert!(
            claude_args
                .windows(2)
                .any(|args| args == ["--session-id", id])
        );
        assert!(claude_args.iter().any(|arg| arg == "--settings"));

        let codex = resume_agent_command(AgentKind::Codex, &cwd, id).unwrap();
        assert!(
            codex
                .get_argv()
                .windows(2)
                .any(|args| args == ["resume", id])
        );
        let claude = resume_agent_command(AgentKind::Claude, &cwd, id).unwrap();
        assert!(
            claude
                .get_argv()
                .windows(2)
                .any(|args| args == ["--resume", id])
        );
    }

    #[test]
    fn generated_conversation_ids_are_valid_v4_uuids() {
        let id = new_uuid().unwrap();
        assert!(crate::recognition::looks_like_uuid(&id));
        assert_eq!(id.as_bytes()[14], b'4');
        assert!(matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
    }

    #[test]
    fn claude_hook_signal_delivers_the_latest_conversation_id() {
        let tracking = ConversationTracking::new(AgentKind::Claude, None).unwrap();
        let signal = tracking.signal.as_ref().unwrap();
        let first = "019ff1d3-375e-4a72-a176-c47497827e49";
        let second = "129ff1d3-375e-4a72-a176-c47497827e49";
        let sender = UnixDatagram::unbound().unwrap();
        sender.send_to(first.as_bytes(), &signal.path).unwrap();
        sender.send_to(second.as_bytes(), &signal.path).unwrap();

        assert_eq!(signal.latest().as_deref(), Some(second));
    }
}
