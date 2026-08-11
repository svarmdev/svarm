use std::{path::Path, sync::Arc};

use portable_pty::{CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};

use crate::{
    AgentId, AgentKind, CursorStyle, ProcessExit, Result, SessionStatus, TerminalPalette,
    terminal_process::TerminalProcess,
};

/// Called when an agent's terminal changes so its owner can wake immediately.
pub type OutputNotifier = Arc<dyn Fn(AgentId) + Send + Sync>;

pub struct AgentSession {
    id: AgentId,
    kind: AgentKind,
    terminal: TerminalProcess,
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
        let notify = notify.map(|notify| Arc::new(move || notify(id)) as _);
        Ok(Self {
            id,
            kind,
            terminal: TerminalProcess::spawn_command(command, cwd, size, palette, notify)?,
        })
    }

    pub fn keyboard_disambiguates(&self) -> bool {
        self.terminal.keyboard_disambiguates()
    }

    pub fn cursor_style(&self) -> CursorStyle {
        self.terminal.cursor_style()
    }

    pub fn with_screen<T>(&self, read: impl FnOnce(&vt100::Screen) -> T) -> T {
        self.terminal.with_screen(read)
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
        SessionSnapshot {
            id: self.id,
            kind: self.kind,
            status: self.terminal.status(),
            output_generation: self.terminal.generation(),
            read_error: self.terminal.read_error(),
            exit: self.terminal.exit(),
        }
    }

    pub fn poll(&mut self) -> Result<SessionSnapshot> {
        self.terminal.poll()?;
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

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn native_agent_owns_its_terminal_colors() {
        let cwd = std::env::current_dir().unwrap();
        let command = agent_command(AgentKind::Codex, &cwd);

        assert_eq!(command.get_env("TERM"), Some(OsStr::new("xterm-256color")));
        assert_eq!(command.get_env("COLORTERM"), Some(OsStr::new("truecolor")));
        assert_eq!(command.get_env("NO_COLOR"), None);
    }
}
