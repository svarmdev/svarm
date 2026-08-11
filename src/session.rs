use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use tui_term::vt100::Parser;

use crate::AgentKind;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionStatus {
    Running,
    Exited,
}

#[derive(Clone, Copy, Debug)]
pub struct TerminalPalette {
    foreground: [u16; 3],
    background: [u16; 3],
}

impl TerminalPalette {
    pub fn detect() -> Option<Self> {
        let palette = terminal_colorsaurus::color_palette(Default::default()).ok()?;
        Some(Self {
            foreground: [
                palette.foreground.r,
                palette.foreground.g,
                palette.foreground.b,
            ],
            background: [
                palette.background.r,
                palette.background.g,
                palette.background.b,
            ],
        })
    }

    fn response(self, slot: u8) -> Option<String> {
        let [red, green, blue] = match slot {
            10 => self.foreground,
            11 => self.background,
            _ => return None,
        };
        Some(format!(
            "\x1b]{slot};rgb:{red:04x}/{green:04x}/{blue:04x}\x1b\\"
        ))
    }
}

pub struct AgentSession {
    pub id: u64,
    pub kind: AgentKind,
    pub cwd: PathBuf,
    parser: Arc<Mutex<Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    status: SessionStatus,
    generation: Arc<AtomicU64>,
    read_error: Arc<Mutex<Option<String>>>,
}

impl AgentSession {
    pub fn spawn(
        id: u64,
        kind: AgentKind,
        cwd: &Path,
        size: PtySize,
        palette: Option<TerminalPalette>,
    ) -> Result<Self> {
        let command = agent_command(kind, cwd);
        Self::spawn_command(id, kind, cwd, size, command, palette)
    }

    fn spawn_command(
        id: u64,
        kind: AgentKind,
        cwd: &Path,
        size: PtySize,
        command: CommandBuilder,
        palette: Option<TerminalPalette>,
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
            10_000,
        )));
        let generation = Arc::new(AtomicU64::new(0));
        let read_error = Arc::new(Mutex::new(None));
        spawn_reader(
            reader,
            parser.clone(),
            writer.clone(),
            palette,
            generation.clone(),
            read_error.clone(),
        );

        Ok(Self {
            id,
            kind,
            cwd: cwd.to_path_buf(),
            parser,
            writer,
            master: pair.master,
            child,
            status: SessionStatus::Running,
            generation,
            read_error,
        })
    }

    pub(crate) fn parser(&self) -> MutexGuard<'_, Parser> {
        self.parser
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
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

    pub fn poll_status(&mut self) -> Result<SessionStatus> {
        if self.status == SessionStatus::Running && self.child.try_wait()?.is_some() {
            self.status = SessionStatus::Exited;
        }
        Ok(self.status)
    }

    pub const fn status(&self) -> SessionStatus {
        self.status
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.poll_status()? == SessionStatus::Running {
            self.child.kill()?;
            self.child.wait()?;
            self.status = SessionStatus::Exited;
        }
        Ok(())
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn read_error(&self) -> Option<String> {
        self.read_error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

fn agent_command(kind: AgentKind, cwd: &Path) -> CommandBuilder {
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

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    parser: Arc<Mutex<Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    palette: Option<TerminalPalette>,
    generation: Arc<AtomicU64>,
    read_error: Arc<Mutex<Option<String>>>,
) {
    thread::spawn(move || {
        let mut buffer = [0_u8; 16 * 1024];
        let mut color_queries = ColorQueryDetector::default();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    color_queries.process(&buffer[..count], |slot| {
                        let Some(response) = palette.and_then(|palette| palette.response(slot))
                        else {
                            return;
                        };
                        let mut writer = writer.lock().unwrap_or_else(|poison| poison.into_inner());
                        let _ = writer.write_all(response.as_bytes());
                        let _ = writer.flush();
                    });
                    parser
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .process(&buffer[..count]);
                    generation.fetch_add(1, Ordering::Release);
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

#[derive(Default)]
struct ColorQueryDetector {
    pending: Vec<u8>,
}

impl ColorQueryDetector {
    fn process(&mut self, bytes: &[u8], mut respond: impl FnMut(u8)) {
        const QUERIES: [(&[u8], u8); 4] = [
            (b"\x1b]10;?\x1b\\", 10),
            (b"\x1b]11;?\x1b\\", 11),
            (b"\x1b]10;?\x07", 10),
            (b"\x1b]11;?\x07", 11),
        ];

        self.pending.extend_from_slice(bytes);
        while let Some((position, pattern, slot)) = QUERIES
            .iter()
            .filter_map(|(pattern, slot)| {
                self.pending
                    .windows(pattern.len())
                    .position(|window| window == *pattern)
                    .map(|position| (position, *pattern, *slot))
            })
            .min_by_key(|(position, _, _)| *position)
        {
            respond(slot);
            self.pending.drain(..position + pattern.len());
        }

        const MAX_PARTIAL_QUERY_LEN: usize = 7;
        if self.pending.len() > MAX_PARTIAL_QUERY_LEN {
            self.pending
                .drain(..self.pending.len() - MAX_PARTIAL_QUERY_LEN);
        }
    }
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
            1,
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
            1,
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
        )
        .unwrap();

        session.stop().unwrap();
        assert_eq!(session.poll_status().unwrap(), SessionStatus::Exited);
    }

    #[test]
    fn terminal_palette_formats_osc_color_responses() {
        let palette = TerminalPalette {
            foreground: [0xaaaa, 0xbbbb, 0xcccc],
            background: [0x1111, 0x2222, 0x3333],
        };

        assert_eq!(
            palette.response(10).as_deref(),
            Some("\x1b]10;rgb:aaaa/bbbb/cccc\x1b\\")
        );
        assert_eq!(
            palette.response(11).as_deref(),
            Some("\x1b]11;rgb:1111/2222/3333\x1b\\")
        );
    }

    #[test]
    fn detects_split_osc_color_queries() {
        let mut detector = ColorQueryDetector::default();
        let mut slots = Vec::new();

        detector.process(b"before\x1b]10;?\x1b", |slot| slots.push(slot));
        detector.process(b"\\middle\x1b]11;?\x07after", |slot| slots.push(slot));

        assert_eq!(slots, [10, 11]);
    }
}
