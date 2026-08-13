use std::{
    collections::HashSet,
    env,
    fs::{self, File},
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

pub(crate) fn available_harnesses() -> Vec<AgentKind> {
    let Some(path) = env::var_os("PATH") else {
        return Vec::new();
    };
    AgentKind::ALL
        .into_iter()
        .filter(|kind| command_available(kind.command(), &path))
        .collect()
}

fn command_available(command: &str, path: &std::ffi::OsStr) -> bool {
    env::split_paths(path).any(|directory| {
        let candidate = if directory.as_os_str().is_empty() {
            Path::new(".").join(command)
        } else {
            directory.join(command)
        };
        fs::metadata(candidate).is_ok_and(|metadata| {
            metadata.is_file() && {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            }
        })
    })
}

pub(crate) struct ConversationTracking {
    pub(crate) kind: AgentKind,
    pub(crate) initial_id: Option<String>,
    signal: Option<ConversationSignal>,
    leader_socket: Option<TempPath>,
}

impl ConversationTracking {
    pub(crate) fn new(kind: AgentKind, initial_id: Option<String>) -> Result<Self> {
        if kind == AgentKind::Grok {
            ensure_grok_conversation_hook()?;
        }
        Ok(Self {
            kind,
            initial_id,
            signal: kind
                .preassigns_conversation_id()
                .then(ConversationSignal::bind)
                .transpose()?,
            leader_socket: (kind == AgentKind::Grok)
                .then(grok_leader_socket)
                .transpose()?,
        })
    }

    #[cfg(test)]
    pub(crate) const fn without_signal(kind: AgentKind, initial_id: Option<String>) -> Self {
        Self {
            kind,
            initial_id,
            signal: None,
            leader_socket: None,
        }
    }

    pub(crate) fn configure(&self, command: &mut CommandBuilder) {
        if let Some(signal) = &self.signal {
            command.env(crate::CLAUDE_SIGNAL_ENV, signal.path.as_os_str());
        }
        if let Some(socket) = &self.leader_socket {
            command.args(["--leader-socket", &socket.0.to_string_lossy()]);
        }
    }
}

struct TempPath(PathBuf);

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn grok_leader_socket() -> Result<TempPath> {
    Ok(TempPath(std::env::temp_dir().join(format!(
        "svarm-grok-leader-{}-{}.sock",
        std::process::id(),
        new_uuid()?
    ))))
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
    pi_session_probe: Option<Mutex<PiSessionProbe>>,
    _leader_socket: Option<TempPath>,
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
        let conversation_id = kind
            .preassigns_conversation_id()
            .then(new_uuid)
            .transpose()?;
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
            leader_socket,
        } = conversation;
        let pi_session_probe = (kind == AgentKind::Pi)
            .then(|| PiSessionProbe::new(cwd))
            .flatten()
            .map(|mut probe| {
                probe.current = initial_id.clone();
                probe
            })
            .map(Mutex::new);
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
            pi_session_probe,
            _leader_socket: leader_socket,
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
        if let Some(probe) = &self.pi_session_probe
            && let Some(id) = probe
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .current_id()
        {
            *self
                .conversation_id
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(id);
        }
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

struct PiSessionProbe {
    root: PathBuf,
    cwd: PathBuf,
    known: HashSet<String>,
    current: Option<String>,
    current_modified: Option<std::time::SystemTime>,
}

impl PiSessionProbe {
    fn new(cwd: &Path) -> Option<Self> {
        let root = pi_session_directory()?;
        let known = session_files(&root, cwd)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        Some(Self {
            root,
            cwd: cwd.to_owned(),
            known,
            current: None,
            current_modified: None,
        })
    }

    fn current_id(&mut self) -> Option<String> {
        let candidates = session_files(&self.root, &self.cwd);
        if let Some(current) = &self.current
            && let Some((_, modified)) = candidates.iter().find(|(id, _)| id == current)
        {
            self.current_modified = Some(*modified);
        }

        let candidate = if self.current.is_none() {
            candidates
                .iter()
                .filter(|(id, _)| !self.known.contains(id))
                .max_by_key(|(_, modified)| *modified)
        } else {
            candidates
                .iter()
                .filter(|(id, modified)| {
                    Some(id) != self.current.as_ref()
                        && self
                            .current_modified
                            .is_none_or(|current| *modified > current)
                })
                .max_by_key(|(_, modified)| *modified)
        };
        if let Some((id, modified)) = candidate {
            self.current = Some(id.clone());
            self.current_modified = Some(*modified);
        }
        self.current.clone()
    }
}

fn pi_session_directory() -> Option<PathBuf> {
    std::env::var_os("PI_CODING_AGENT_SESSION_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("PI_CODING_AGENT_DIR")
                .filter(|path| !path.is_empty())
                .map(|path| PathBuf::from(path).join("sessions"))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".pi/agent/sessions"))
        })
}

fn session_files(root: &Path, cwd: &Path) -> Vec<(String, std::time::SystemTime)> {
    let mut directories = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                directories.push(path);
            } else if file_type.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
                && let Some((id, session_cwd)) = session_header(&path)
                && session_cwd == cwd
            {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                files.push((id, modified));
            }
        }
    }
    files
}

fn session_header(path: &Path) -> Option<(String, PathBuf)> {
    use std::io::BufRead;

    let line = std::io::BufReader::new(File::open(path).ok()?)
        .lines()
        .next()?
        .ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&line).ok()?;
    (value.get("type")?.as_str()? == "session").then_some(())?;
    let id = value.get("id")?.as_str()?;
    let cwd = value.get("cwd")?.as_str()?;
    crate::recognition::looks_like_uuid(id).then(|| (id.to_ascii_lowercase(), PathBuf::from(cwd)))
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
        AgentKind::Grok => {
            if let Some(conversation_id) = conversation_id {
                command.args(["--session-id", conversation_id]);
            }
            command.arg("--fullscreen");
        }
        AgentKind::Pi => {}
        AgentKind::OpenCode => {}
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
        AgentKind::Claude | AgentKind::Grok => command.args(["--resume", conversation_id]),
        AgentKind::Pi => command.args(["--session", conversation_id]),
        AgentKind::OpenCode => command.args(["--session", conversation_id]),
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

fn grok_home() -> Option<PathBuf> {
    std::env::var_os("GROK_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".grok"))
        })
}

fn ensure_grok_conversation_hook() -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    let Some(home) = grok_home() else {
        return Ok(());
    };
    write_grok_conversation_hook(&home)
}

fn write_grok_conversation_hook(home: &Path) -> Result<()> {
    let directory = home.join("hooks");
    let path = directory.join("svarm-conversation.json");
    if path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&directory)?;
    let executable = std::env::current_exe()?;
    let command = format!(
        "{} __conversation-hook",
        shell_quote(&executable.to_string_lossy())
    );
    let body = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "startup|resume|clear|compact",
                "hooks": [{ "type": "command", "command": command }]
            }]
        }
    });
    std::fs::write(path, serde_json::to_vec_pretty(&body)?)?;
    Ok(())
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

    #[test]
    fn command_availability_requires_an_executable_file_on_path() {
        assert!(command_available("sh", OsStr::new("/bin")));
        assert!(!command_available(
            "svarm-command-that-does-not-exist",
            OsStr::new("/bin")
        ));
    }

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

        let grok = agent_command(AgentKind::Grok, &cwd, Some(id)).unwrap();
        let grok_args = grok.get_argv();
        assert!(
            grok_args
                .windows(2)
                .any(|args| args == ["--session-id", id])
        );
        assert!(grok_args.iter().any(|arg| arg == "--fullscreen"));
        assert!(!grok_args.iter().any(|arg| arg == "--resume"));

        let grok = resume_agent_command(AgentKind::Grok, &cwd, id).unwrap();
        let grok_args = grok.get_argv();
        assert!(grok_args.windows(2).any(|args| args == ["--resume", id]));
        assert!(grok_args.iter().any(|arg| arg == "--fullscreen"));
        assert!(!grok_args.iter().any(|arg| arg == "--session-id"));

        let pi = resume_agent_command(AgentKind::Pi, &cwd, id).unwrap();
        assert!(
            pi.get_argv()
                .windows(2)
                .any(|args| args == ["--session", id])
        );
        assert_eq!(
            agent_command(AgentKind::Pi, &cwd, None).unwrap().get_argv(),
            &[std::ffi::OsString::from("pi")]
        );
    }

    #[test]
    fn pi_session_headers_are_read_only_when_they_are_valid() {
        let path = std::env::temp_dir().join(format!(
            "svarm-pi-session-{}-{}.jsonl",
            std::process::id(),
            new_uuid().unwrap()
        ));
        std::fs::write(
            &path,
            "{\"type\":\"session\",\"version\":3,\"id\":\"019ff1d3-375e-7a72-a176-c47497827e49\",\"cwd\":\"/tmp/project\"}\n",
        )
        .unwrap();
        assert_eq!(
            session_header(&path),
            Some((
                "019ff1d3-375e-7a72-a176-c47497827e49".into(),
                PathBuf::from("/tmp/project")
            ))
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn pi_session_probe_finds_a_new_session_for_its_working_directory() {
        let root = std::env::temp_dir().join(format!(
            "svarm-pi-probe-{}-{}",
            std::process::id(),
            new_uuid().unwrap()
        ));
        let path = root.join("--tmp-project--/20260813_019ff1d3-375e-7a72-a176-c47497827e49.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut probe = PiSessionProbe {
            root: root.clone(),
            cwd: PathBuf::from("/tmp/project"),
            known: HashSet::new(),
            current: None,
            current_modified: None,
        };
        assert_eq!(probe.current_id(), None);
        std::fs::write(
            &path,
            "{\"type\":\"session\",\"version\":3,\"id\":\"019ff1d3-375e-7a72-a176-c47497827e49\",\"cwd\":\"/tmp/project\"}\n",
        )
        .unwrap();
        assert_eq!(
            probe.current_id().as_deref(),
            Some("019ff1d3-375e-7a72-a176-c47497827e49")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opencode_commands_start_and_resume_the_requested_conversation() {
        let cwd = std::env::current_dir().unwrap();
        let id = "019ff1d3-375e-7a72-a176-c47497827e49";
        let opencode = agent_command(AgentKind::OpenCode, &cwd, None).unwrap();
        assert_eq!(opencode.get_argv(), &[std::ffi::OsString::from("opencode")]);
        let opencode = resume_agent_command(AgentKind::OpenCode, &cwd, id).unwrap();
        assert!(
            opencode
                .get_argv()
                .windows(2)
                .any(|args| args == ["--session", id])
        );
    }

    #[test]
    fn grok_isolates_its_leader_socket() {
        let tracking = ConversationTracking::new(AgentKind::Grok, None).unwrap();
        let cwd = std::env::current_dir().unwrap();
        let mut command = agent_command(AgentKind::Grok, &cwd, None).unwrap();
        tracking.configure(&mut command);
        let args = command.get_argv();
        let socket = args
            .windows(2)
            .find(|pair| pair[0] == "--leader-socket")
            .map(|pair| pair[1].to_string_lossy().into_owned())
            .expect("grok gets a private leader socket");
        assert!(socket.contains("svarm-grok-leader-"));
    }

    #[test]
    fn grok_hook_file_is_written_once_and_left_alone() {
        let home = std::env::temp_dir().join(format!(
            "svarm-grok-hooks-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_grok_conversation_hook(&home).unwrap();
        let path = home.join("hooks/svarm-conversation.json");
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("SessionStart"));
        assert!(first.contains("__conversation-hook"));
        assert!(first.contains("startup|resume|clear|compact"));
        std::fs::write(&path, "user-owned").unwrap();
        write_grok_conversation_hook(&home).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "user-owned");
        std::fs::remove_dir_all(&home).unwrap();
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
