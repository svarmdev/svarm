use std::{collections::HashMap, path::PathBuf, sync::MutexGuard};

use svarm_agent::{
    AgentId, AgentKind, AgentSession, PtySize, Result, SessionSnapshot, TerminalPalette,
};
use tui_term::vt100::Parser;

pub(crate) struct AgentRuntime {
    sessions: HashMap<AgentId, AgentSession>,
    cwd: PathBuf,
    next_id: u64,
    pty_size: PtySize,
    terminal_palette: Option<TerminalPalette>,
}

impl AgentRuntime {
    pub fn new(cwd: PathBuf, pty_size: PtySize, terminal_palette: Option<TerminalPalette>) -> Self {
        Self {
            sessions: HashMap::new(),
            cwd,
            next_id: 1,
            pty_size,
            terminal_palette,
        }
    }

    pub fn spawn(&mut self, kind: AgentKind) -> Result<SessionSnapshot> {
        let id = AgentId::new(self.next_id);
        let session =
            AgentSession::spawn(id, kind, &self.cwd, self.pty_size, self.terminal_palette)?;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("agent identifier space exhausted")?;
        let snapshot = session.snapshot();
        self.sessions.insert(id, session);
        Ok(snapshot)
    }

    pub fn close(&mut self, id: AgentId) -> Result<()> {
        let Some(session) = self.sessions.get_mut(&id) else {
            return Ok(());
        };
        session.stop()?;
        self.sessions.remove(&id);
        Ok(())
    }

    pub fn send(&self, id: AgentId, bytes: &[u8]) -> Result<()> {
        if let Some(session) = self.sessions.get(&id) {
            session.send(bytes)?;
        }
        Ok(())
    }

    pub fn parser(&self, id: AgentId) -> Option<MutexGuard<'_, Parser>> {
        self.sessions.get(&id).map(AgentSession::parser)
    }

    pub fn poll(&mut self) -> Vec<Result<SessionSnapshot>> {
        self.sessions.values_mut().map(AgentSession::poll).collect()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let size = pty_size(rows, cols);
        if size == self.pty_size {
            return Ok(());
        }
        for session in self.sessions.values() {
            session.resize(size.rows, size.cols)?;
        }
        self.pty_size = size;
        Ok(())
    }
}

pub(crate) fn pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_never_has_a_zero_dimension() {
        assert_eq!(pty_size(0, 0).rows, 1);
        assert_eq!(pty_size(0, 0).cols, 1);
    }
}
