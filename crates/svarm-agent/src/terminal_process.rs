use std::{
    ffi::{OsStr, OsString},
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
use vt100::{MouseProtocolEncoding, MouseProtocolMode, Parser};

use crate::{
    protocol::{MouseEncoding, MouseProtocol, TerminalModes},
    terminal::{
        ColorQueryDetector, ControlDetector, CursorStyle, KeyboardState, Recognized,
        TerminalPalette, color_query_responses,
    },
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Called whenever output, EOF, or a read error means the owner should inspect the process.
pub type TerminalNotifier = Arc<dyn Fn() + Send + Sync>;

/// The visible screen is all Svarm renders. Retaining undisplayable scrollback would make every
/// prepared tool snapshot more expensive without adding a user-visible capability.
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

pub struct TerminalProcessSnapshot {
    pub screen: vt100::Screen,
    pub cursor_style: CursorStyle,
    pub status: SessionStatus,
    pub exit: Option<ProcessExit>,
    pub read_error: Option<String>,
    pub generation: u64,
    pub modes: TerminalModes,
}

pub struct TerminalProcess {
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
    keyboard: Arc<Mutex<KeyboardState>>,
}

impl TerminalProcess {
    pub fn spawn(
        program: &OsStr,
        args: &[OsString],
        cwd: &Path,
        size: PtySize,
        palette: Option<TerminalPalette>,
        notify: Option<TerminalNotifier>,
    ) -> Result<Self> {
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.cwd(cwd);
        Self::spawn_command(command, cwd, size, palette, notify)
    }

    pub(crate) fn spawn_command(
        command: CommandBuilder,
        cwd: &Path,
        size: PtySize,
        palette: Option<TerminalPalette>,
        notify: Option<TerminalNotifier>,
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
        let keyboard = Arc::new(Mutex::new(KeyboardState::default()));
        spawn_reader(
            reader,
            ReaderState {
                parser: parser.clone(),
                writer: writer.clone(),
                terminal_palette: terminal_palette.clone(),
                generation: generation.clone(),
                read_error: read_error.clone(),
                cursor_style: cursor_style.clone(),
                keyboard: keyboard.clone(),
                notify,
            },
        );

        Ok(Self {
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
            keyboard,
        })
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

    pub fn set_terminal_palette(&self, palette: Option<TerminalPalette>) {
        *self
            .terminal_palette
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = palette;
    }

    pub fn snapshot(&self) -> TerminalProcessSnapshot {
        let screen = self.parser().screen().clone();
        TerminalProcessSnapshot {
            modes: self.modes(&screen),
            screen,
            cursor_style: self.cursor_style(),
            status: self.status,
            exit: self.exit.clone(),
            read_error: self.read_error(),
            generation: self.generation(),
        }
    }

    pub fn poll(&mut self) -> Result<SessionStatus> {
        if self.status == SessionStatus::Running
            && let Some(status) = self.child.try_wait()?
        {
            self.record_exit(status);
        }
        Ok(self.status)
    }

    pub fn stop(&mut self) -> Result<()> {
        if self.poll()? == SessionStatus::Running {
            self.child.kill()?;
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if let Some(status) = self.child.try_wait()? {
                    self.record_exit(status);
                    break;
                }
                if Instant::now() >= deadline {
                    return Err("terminal process did not exit after forced termination".into());
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(())
    }

    pub fn with_screen<T>(&self, read: impl FnOnce(&vt100::Screen) -> T) -> T {
        read(self.parser().screen())
    }

    pub(crate) fn keyboard_disambiguates(&self) -> bool {
        self.keyboard
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .disambiguates()
    }

    pub(crate) fn cursor_style(&self) -> CursorStyle {
        *self
            .cursor_style
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub(crate) fn read_error(&self) -> Option<String> {
        self.read_error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub(crate) fn status(&self) -> SessionStatus {
        self.status
    }

    pub(crate) fn exit(&self) -> Option<ProcessExit> {
        self.exit.clone()
    }

    fn parser(&self) -> MutexGuard<'_, Parser> {
        self.parser
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn modes(&self, screen: &vt100::Screen) -> TerminalModes {
        TerminalModes {
            keyboard_disambiguate: self.keyboard_disambiguates(),
            application_cursor: screen.application_cursor(),
            application_keypad: screen.application_keypad(),
            bracketed_paste: screen.bracketed_paste(),
            mouse_protocol: match screen.mouse_protocol_mode() {
                MouseProtocolMode::None => MouseProtocol::None,
                MouseProtocolMode::Press => MouseProtocol::Press,
                MouseProtocolMode::PressRelease => MouseProtocol::PressRelease,
                MouseProtocolMode::ButtonMotion => MouseProtocol::ButtonMotion,
                MouseProtocolMode::AnyMotion => MouseProtocol::AnyMotion,
            },
            mouse_encoding: match screen.mouse_protocol_encoding() {
                MouseProtocolEncoding::Default => MouseEncoding::Default,
                MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
                MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
            },
        }
    }

    fn record_exit(&mut self, status: portable_pty::ExitStatus) {
        self.status = SessionStatus::Exited;
        self.exit = Some(status.into());
    }
}

impl Drop for TerminalProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct ReaderState {
    parser: Arc<Mutex<Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    terminal_palette: Arc<Mutex<Option<TerminalPalette>>>,
    generation: Arc<AtomicU64>,
    read_error: Arc<Mutex<Option<String>>>,
    cursor_style: Arc<Mutex<CursorStyle>>,
    keyboard: Arc<Mutex<KeyboardState>>,
    notify: Option<TerminalNotifier>,
}

fn spawn_reader(mut reader: Box<dyn Read + Send>, state: ReaderState) {
    thread::spawn(move || {
        let mut buffer = [0_u8; 16 * 1024];
        let mut color_queries = ColorQueryDetector::default();
        let mut controls = ControlDetector::default();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let output = &buffer[..count];
                    let reply = |bytes: &[u8]| {
                        let mut writer = state
                            .writer
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        let _ = writer.write_all(bytes);
                        let _ = writer.flush();
                    };
                    for response in color_query_responses(
                        &mut color_queries,
                        *state
                            .terminal_palette
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner()),
                        output,
                    ) {
                        reply(response.as_bytes());
                    }

                    let mut consumed = 0;
                    for (offset, recognized) in controls.process(output) {
                        let mut parser = state
                            .parser
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        parser.process(&output[consumed..offset]);
                        consumed = offset;
                        match recognized {
                            Recognized::CursorStyle(style) => {
                                *state
                                    .cursor_style
                                    .lock()
                                    .unwrap_or_else(|poison| poison.into_inner()) = style;
                            }
                            Recognized::Query(query) => {
                                let cursor = parser.screen().cursor_position();
                                drop(parser);
                                reply(query.response(cursor).as_bytes());
                            }
                            Recognized::Keyboard(change) => {
                                state
                                    .keyboard
                                    .lock()
                                    .unwrap_or_else(|poison| poison.into_inner())
                                    .apply(change);
                            }
                        }
                    }
                    state
                        .parser
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .process(&output[consumed..]);
                    state.generation.fetch_add(1, Ordering::Release);
                    if let Some(notify) = &state.notify {
                        notify();
                    }
                }
                Err(error) => {
                    *state
                        .read_error
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner()) = Some(error.to_string());
                    break;
                }
            }
        }
        if let Some(notify) = &state.notify {
            notify();
        }
    });
}

#[cfg(test)]
mod tests {
    use std::{
        sync::mpsc::sync_channel,
        time::{Duration, Instant},
    };

    use super::*;

    fn command(script: &str, cwd: &Path) -> CommandBuilder {
        let mut command = CommandBuilder::new("sh");
        command.args(["-c", script]);
        command.cwd(cwd);
        command
    }

    fn size() -> PtySize {
        PtySize {
            rows: 10,
            cols: 40,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    #[test]
    fn answers_cursor_position_queries_at_the_query_boundary() {
        let cwd = std::env::current_dir().unwrap();
        let process = TerminalProcess::spawn_command(
            command("printf 'first\\r\\nprompt> \\033[6n'; sleep 1", &cwd),
            &cwd,
            size(),
            None,
            None,
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while !process.with_screen(|screen| screen.contents().contains("[2;9R"))
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }

        let contents = process.with_screen(vt100::Screen::contents);
        assert!(contents.contains("[2;9R"), "screen was {contents:?}");
    }

    #[test]
    fn captures_output_and_exit_from_a_real_pty() {
        let cwd = std::env::current_dir().unwrap();
        let mut process =
            TerminalProcess::spawn_command(command("printf svarm", &cwd), &cwd, size(), None, None)
                .unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while !process.with_screen(|screen| screen.contents().contains("svarm"))
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(process.with_screen(|screen| screen.contents().contains("svarm")));
        while process.poll().unwrap() == SessionStatus::Running && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(process.poll().unwrap(), SessionStatus::Exited);
    }

    #[test]
    fn stopping_reaps_the_process() {
        let cwd = std::env::current_dir().unwrap();
        let mut process = TerminalProcess::spawn_command(
            command("exec sleep 60", &cwd),
            &cwd,
            size(),
            None,
            None,
        )
        .unwrap();

        process.stop().unwrap();
        assert_eq!(process.poll().unwrap(), SessionStatus::Exited);
    }

    #[test]
    fn eof_wakes_the_owner_without_output() {
        let cwd = std::env::current_dir().unwrap();
        let (tx, rx) = sync_channel(1);
        let notify = Arc::new(move || {
            let _ = tx.try_send(());
        });
        let _process = TerminalProcess::spawn_command(
            command("exit 0", &cwd),
            &cwd,
            size(),
            None,
            Some(notify),
        )
        .unwrap();

        rx.recv_timeout(Duration::from_secs(1))
            .expect("EOF did not wake the owner");
    }
}
