use std::{
    io::{Read, Write},
    path::Path,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use serde::{Deserialize, Serialize};
use vt100::Parser;

use crate::{
    AgentId, AgentKind,
    terminal::{
        ColorQueryDetector, CursorStyle, CursorStyleDetector, TerminalPalette,
        color_query_responses,
    },
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Called from an agent's reader thread every time new output lands, so the owner can wake up
/// immediately instead of discovering the output on its next poll.
pub type OutputNotifier = Arc<dyn Fn(AgentId) + Send + Sync>;

/// Svarm shows only the active screen: nothing sets a scrollback offset, and frames transport the
/// visible grid alone, so retained rows could never be displayed. They would still be copied on
/// every frame, which is why the buffer is empty. Raise this if scrollback becomes navigable.
const SCROLLBACK_ROWS: usize = 0;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Exited,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessExit {
    pub code: u32,
    pub signal: Option<String>,
    pub success: bool,
}

impl From<portable_pty::ExitStatus> for ProcessExit {
    fn from(status: portable_pty::ExitStatus) -> Self {
        Self {
            code: status.exit_code(),
            signal: status.signal().map(str::to_owned),
            success: status.success(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TerminalSnapshot {
    screen: vt100::Screen,
}

impl TerminalSnapshot {
    pub const fn screen(&self) -> &vt100::Screen {
        &self.screen
    }
}

pub struct AgentSession {
    id: AgentId,
    kind: AgentKind,
    parser: Arc<Mutex<Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    status: SessionStatus,
    exit: Option<ProcessExit>,
    terminal_palette: Arc<Mutex<Option<TerminalPalette>>>,
    generation: Arc<AtomicU64>,
    read_error: Arc<Mutex<Option<String>>>,
    cursor_style: Arc<Mutex<CursorStyle>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSnapshot {
    pub id: AgentId,
    pub kind: AgentKind,
    pub status: SessionStatus,
    pub output_generation: u64,
    pub read_error: Option<String>,
    pub exit: Option<ProcessExit>,
}

impl AgentSession {
    pub fn spawn(
        id: AgentId,
        kind: AgentKind,
        cwd: &Path,
        size: PtySize,
        palette: Option<TerminalPalette>,
    ) -> Result<Self> {
        let command = agent_command(kind, cwd);
        Self::spawn_command(id, kind, cwd, size, command, palette, None)
    }

    pub(crate) fn spawn_command(
        id: AgentId,
        kind: AgentKind,
        cwd: &Path,
        size: PtySize,
        command: CommandBuilder,
        palette: Option<TerminalPalette>,
        notify: Option<OutputNotifier>,
    ) -> Result<Self> {
        if !cwd.is_dir() {
            return Err(format!(
                "workspace does not exist or is not a directory: {}",
                cwd.display()
            )
            .into());
        }

        let pair = NativePtySystem::default().openpty(size)?;
        let reader = pair.master.try_clone_reader()?;
        let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(Parser::new(
            size.rows.max(1),
            size.cols.max(1),
            SCROLLBACK_ROWS,
        )));
        let generation = Arc::new(AtomicU64::new(0));
        let read_error = Arc::new(Mutex::new(None));
        let terminal_palette = Arc::new(Mutex::new(palette));
        let cursor_style = Arc::new(Mutex::new(CursorStyle::default()));
        spawn_reader(
            reader,
            ReaderState {
                id,
                parser: parser.clone(),
                writer: writer.clone(),
                terminal_palette: terminal_palette.clone(),
                generation: generation.clone(),
                read_error: read_error.clone(),
                cursor_style: cursor_style.clone(),
                notify,
            },
        );

        Ok(Self {
            id,
            kind,
            parser,
            writer,
            master: pair.master,
            child,
            status: SessionStatus::Running,
            exit: None,
            terminal_palette,
            generation,
            read_error,
            cursor_style,
        })
    }

    /// The cursor the agent last asked for. Frames carry it because the emulated screen cannot.
    pub fn cursor_style(&self) -> CursorStyle {
        *self
            .cursor_style
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn parser(&self) -> MutexGuard<'_, Parser> {
        self.parser
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn terminal_snapshot(&self) -> TerminalSnapshot {
        TerminalSnapshot {
            screen: self.parser().screen().clone(),
        }
    }

    /// Reads the live screen in place. Callers that only need to measure or serialize it should
    /// use this rather than [`Self::terminal_snapshot`]: copying the screen is the most expensive
    /// thing on the output path, and doing it under the lock stalls the agent's reader thread.
    pub fn with_screen<T>(&self, read: impl FnOnce(&vt100::Screen) -> T) -> T {
        read(self.parser().screen())
    }

    pub fn set_terminal_palette(&self, palette: Option<TerminalPalette>) {
        *self
            .terminal_palette
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = palette;
    }

    pub fn send(&self, bytes: &[u8]) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        writer.write_all(bytes)?;
        writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let size = PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        };
        self.master.resize(size)?;
        self.parser().screen_mut().set_size(size.rows, size.cols);
        Ok(())
    }

    fn poll_status(&mut self) -> Result<SessionStatus> {
        if self.status == SessionStatus::Running
            && let Some(status) = self.child.try_wait()?
        {
            self.record_exit(status);
        }
        Ok(self.status)
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.poll_status()? == SessionStatus::Running {
            self.child.kill()?;
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if let Some(status) = self.child.try_wait()? {
                    self.record_exit(status);
                    break;
                }
                if Instant::now() >= deadline {
                    return Err("agent did not exit after forced termination".into());
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(())
    }

    fn record_exit(&mut self, status: portable_pty::ExitStatus) {
        self.status = SessionStatus::Exited;
        self.exit = Some(status.into());
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn read_error(&self) -> Option<String> {
        self.read_error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            id: self.id,
            kind: self.kind,
            status: self.status,
            output_generation: self.generation(),
            read_error: self.read_error(),
            exit: self.exit.clone(),
        }
    }

    pub fn poll(&mut self) -> Result<SessionSnapshot> {
        self.poll_status()?;
        Ok(self.snapshot())
    }
}

pub(crate) fn agent_command(kind: AgentKind, cwd: &Path) -> CommandBuilder {
    let mut command = CommandBuilder::new(kind.command());
    command.cwd(cwd);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env_remove("NO_COLOR");
    command.env("SVARM", "1");
    if kind == AgentKind::Claude {
        command.env_remove("CLAUDECODE");
    }
    command
}

impl Drop for AgentSession {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// The state an agent's reader thread shares with its session: everything the output stream can
/// change, plus the channels it answers and reports on.
struct ReaderState {
    id: AgentId,
    parser: Arc<Mutex<Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    terminal_palette: Arc<Mutex<Option<TerminalPalette>>>,
    generation: Arc<AtomicU64>,
    read_error: Arc<Mutex<Option<String>>>,
    cursor_style: Arc<Mutex<CursorStyle>>,
    notify: Option<OutputNotifier>,
}

fn spawn_reader(mut reader: Box<dyn Read + Send>, state: ReaderState) {
    let ReaderState {
        id,
        parser,
        writer,
        terminal_palette,
        generation,
        read_error,
        cursor_style,
        notify,
    } = state;
    thread::spawn(move || {
        let mut buffer = [0_u8; 16 * 1024];
        let mut color_queries = ColorQueryDetector::default();
        let mut cursor_styles = CursorStyleDetector::default();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    for response in color_query_responses(
                        &mut color_queries,
                        *terminal_palette
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner()),
                        &buffer[..count],
                    ) {
                        let mut writer = writer.lock().unwrap_or_else(|poison| poison.into_inner());
                        let _ = writer.write_all(response.as_bytes());
                        let _ = writer.flush();
                    }
                    if let Some(style) = cursor_styles.process(&buffer[..count]) {
                        *cursor_style
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner()) = style;
                    }
                    parser
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .process(&buffer[..count]);
                    generation.fetch_add(1, Ordering::Release);
                    if let Some(callback) = &notify {
                        callback(id);
                    }
                }
                Err(error) => {
                    *read_error
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner()) = Some(error.to_string());
                    break;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn native_agent_owns_its_terminal_colors() {
        let cwd = std::env::current_dir().unwrap();
        let command = agent_command(AgentKind::Codex, &cwd);

        assert_eq!(command.get_env("TERM"), Some(OsStr::new("xterm-256color")));
        assert_eq!(command.get_env("COLORTERM"), Some(OsStr::new("truecolor")));
        assert_eq!(command.get_env("NO_COLOR"), None);
    }

    #[test]
    fn captures_output_from_a_real_pty() {
        let cwd = std::env::current_dir().unwrap();
        let mut command = CommandBuilder::new("sh");
        command.args(["-c", "printf svarm"]);
        command.cwd(&cwd);
        let mut session = AgentSession::spawn_command(
            AgentId::new(1),
            AgentKind::Codex,
            &cwd,
            PtySize {
                rows: 10,
                cols: 40,
                pixel_width: 0,
                pixel_height: 0,
            },
            command,
            None,
            None,
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while !session.parser().screen().contents().contains("svarm") && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }

        assert!(session.parser().screen().contents().contains("svarm"));
        while session.poll_status().unwrap() == SessionStatus::Running && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(session.poll_status().unwrap(), SessionStatus::Exited);
    }

    #[test]
    fn stopping_a_session_reaps_its_process() {
        let cwd = std::env::current_dir().unwrap();
        let mut command = CommandBuilder::new("sh");
        command.args(["-c", "exec sleep 60"]);
        command.cwd(&cwd);
        let mut session = AgentSession::spawn_command(
            AgentId::new(1),
            AgentKind::Codex,
            &cwd,
            PtySize {
                rows: 10,
                cols: 40,
                pixel_width: 0,
                pixel_height: 0,
            },
            command,
            None,
            None,
        )
        .unwrap();

        session.stop().unwrap();
        assert_eq!(session.poll_status().unwrap(), SessionStatus::Exited);
    }
}
