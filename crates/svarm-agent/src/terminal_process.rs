use std::{
    ffi::{OsStr, OsString},
    io::{Read, Write},
    path::Path,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
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

const NO_SCROLLBACK: usize = 0;

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
    pub output_closed: bool,
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
    alternate_scroll: Arc<AtomicBool>,
    output_closed: Arc<AtomicBool>,
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

    pub fn spawn_with_environment(
        program: &OsStr,
        args: &[OsString],
        cwd: &Path,
        size: PtySize,
        palette: Option<TerminalPalette>,
        environment: &[(OsString, Option<OsString>)],
        notify: Option<TerminalNotifier>,
    ) -> Result<Self> {
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.cwd(cwd);
        for (name, value) in environment {
            if let Some(value) = value {
                command.env(name, value);
            } else {
                command.env_remove(name);
            }
        }
        Self::spawn_command(command, cwd, size, palette, notify)
    }

    pub(crate) fn spawn_command(
        command: CommandBuilder,
        cwd: &Path,
        size: PtySize,
        palette: Option<TerminalPalette>,
        notify: Option<TerminalNotifier>,
    ) -> Result<Self> {
        Self::spawn_command_with_scrollback(command, cwd, size, palette, notify, NO_SCROLLBACK)
    }

    pub(crate) fn spawn_command_with_scrollback(
        command: CommandBuilder,
        cwd: &Path,
        size: PtySize,
        palette: Option<TerminalPalette>,
        notify: Option<TerminalNotifier>,
        scrollback_rows: usize,
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
            scrollback_rows,
        )));
        let generation = Arc::new(AtomicU64::new(0));
        let read_error = Arc::new(Mutex::new(None));
        let terminal_palette = Arc::new(Mutex::new(palette));
        let cursor_style = Arc::new(Mutex::new(CursorStyle::default()));
        let keyboard = Arc::new(Mutex::new(KeyboardState::default()));
        let alternate_scroll = Arc::new(AtomicBool::new(false));
        let output_closed = Arc::new(AtomicBool::new(false));
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
                alternate_scroll: alternate_scroll.clone(),
                output_closed: output_closed.clone(),
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
            alternate_scroll,
            output_closed,
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
            output_closed: self.output_closed.load(Ordering::Acquire),
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

    pub(crate) fn formatted_viewport(&self, requested: usize) -> (u16, u16, usize, Vec<u8>) {
        let mut parser = self.parser();
        let screen = parser.screen_mut();
        let original = screen.scrollback();
        screen.set_scrollback(requested);
        let scrollback = screen.scrollback();
        let (rows, cols) = screen.size();
        let formatted = screen.contents_formatted();
        screen.set_scrollback(original);
        (rows, cols, scrollback, formatted)
    }

    pub(crate) fn keyboard_disambiguates(&self) -> bool {
        self.keyboard
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .disambiguates()
    }

    pub(crate) fn terminal_modes(&self) -> TerminalModes {
        let parser = self.parser();
        self.modes(parser.screen())
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
            mouse_alternate_scroll: self.alternate_scroll.load(Ordering::Acquire),
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
    alternate_scroll: Arc<AtomicBool>,
    output_closed: Arc<AtomicBool>,
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
                            Recognized::KeyboardQuery => {
                                let flags = state
                                    .keyboard
                                    .lock()
                                    .unwrap_or_else(|poison| poison.into_inner())
                                    .flags();
                                drop(parser);
                                reply(format!("\x1b[?{flags}u").as_bytes());
                            }
                            Recognized::Keyboard(change) => {
                                state
                                    .keyboard
                                    .lock()
                                    .unwrap_or_else(|poison| poison.into_inner())
                                    .apply(change);
                            }
                            Recognized::AlternateScroll(enabled) => {
                                state.alternate_scroll.store(enabled, Ordering::Release);
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
        state.output_closed.store(true, Ordering::Release);
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
    fn answers_kitty_keyboard_queries_with_the_active_flags() {
        let cwd = std::env::current_dir().unwrap();
        let process = TerminalProcess::spawn_command(
            command(
                "stty raw -echo; printf '\\033[>7u\\033[?u'; dd bs=1 count=5 2>/dev/null | od -An -tx1; sleep 1",
                &cwd,
            ),
            &cwd,
            size(),
            None,
            None,
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while !process.with_screen(|screen| screen.contents().contains("1b 5b 3f 37 75"))
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }

        let screen = process.with_screen(vt100::Screen::contents);
        assert!(
            screen.contains("1b 5b 3f 37 75"),
            "keyboard query reply was not returned: {screen:?}"
        );
        assert!(process.terminal_modes().keyboard_disambiguate);
    }

    #[test]
    fn top_anchored_scroll_regions_feed_host_history() {
        let mut parser = Parser::new(5, 10, 100);
        parser.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");

        // Inline TUIs keep their composer below this region and move completed transcript rows
        // out through its top edge.
        parser.process(b"\x1b[1;3r\x1b[3;1H\x1b[S");
        parser.screen_mut().set_scrollback(usize::MAX);

        assert_eq!(parser.screen().scrollback(), 1);
        assert!(parser.screen().contents().starts_with("one"));
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
        let process = TerminalProcess::spawn_command(
            command("exit 0", &cwd),
            &cwd,
            size(),
            None,
            Some(notify),
        )
        .unwrap();

        rx.recv_timeout(Duration::from_secs(1))
            .expect("EOF did not wake the owner");
        assert!(process.snapshot().output_closed);
    }

    #[test]
    fn public_spawn_passes_literal_arguments_and_environment_without_a_shell_command() {
        let cwd = std::env::current_dir().unwrap();
        let mut process = TerminalProcess::spawn_with_environment(
            OsStr::new("sh"),
            &[
                OsString::from("-c"),
                OsString::from("printf '%s:%s' \"$1\" \"$SVARM_TEST_VALUE\""),
                OsString::from("sh"),
                OsString::from("path with spaces;$()"),
            ],
            &cwd,
            size(),
            None,
            &[(
                OsString::from("SVARM_TEST_VALUE"),
                Some(OsString::from("literal value")),
            )],
            None,
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while !process.with_screen(|screen| screen.contents().contains("literal value"))
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(process.with_screen(|screen| {
            screen
                .contents()
                .contains("path with spaces;$():literal value")
        }));
        while process.poll().unwrap() == SessionStatus::Running && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn reports_alternate_scroll_mode() {
        let cwd = std::env::current_dir().unwrap();
        let size = PtySize {
            rows: 4,
            cols: 40,
            pixel_width: 0,
            pixel_height: 0,
        };
        let process = TerminalProcess::spawn_command(
            command("printf '\\033[?1049h\\033[?1007h'; sleep 1", &cwd),
            &cwd,
            size,
            None,
            None,
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while process.with_screen(|screen| !screen.alternate_screen()) && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }

        assert!(process.snapshot().modes.mouse_alternate_scroll);
    }
}
