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

pub struct AgentSession {
    pub id: u64,
    pub kind: AgentKind,
    pub cwd: PathBuf,
    parser: Arc<Mutex<Parser>>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    status: SessionStatus,
    generation: Arc<AtomicU64>,
    read_error: Arc<Mutex<Option<String>>>,
}

impl AgentSession {
    pub fn spawn(id: u64, kind: AgentKind, cwd: &Path, size: PtySize) -> Result<Self> {
        let mut command = CommandBuilder::new(kind.command());
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        command.env("SVARM", "1");
        if kind == AgentKind::Claude {
            command.env_remove("CLAUDECODE");
        }
        Self::spawn_command(id, kind, cwd, size, command)
    }

    fn spawn_command(
        id: u64,
        kind: AgentKind,
        cwd: &Path,
        size: PtySize,
        command: CommandBuilder,
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
        let writer = pair.master.take_writer()?;
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
            generation.clone(),
            read_error.clone(),
        );

        Ok(Self {
            id,
            kind,
            cwd: cwd.to_path_buf(),
            parser,
            writer: Mutex::new(writer),
            master: pair.master,
            child,
            status: SessionStatus::Running,
            generation,
            read_error,
        })
    }

    pub fn parser(&self) -> MutexGuard<'_, Parser> {
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

    pub fn stop(&mut self) -> Result<()> {
        if self.poll_status()? == SessionStatus::Running {
            self.child.kill()?;
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

impl Drop for AgentSession {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    parser: Arc<Mutex<Parser>>,
    generation: Arc<AtomicU64>,
    read_error: Arc<Mutex<Option<String>>>,
) {
    thread::spawn(move || {
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

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
}
