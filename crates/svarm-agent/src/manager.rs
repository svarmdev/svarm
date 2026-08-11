use std::{collections::HashMap, path::PathBuf};

use portable_pty::PtySize;

use crate::{
    AgentId, AgentKind, AgentSession, Result, SessionSnapshot, TerminalPalette, TerminalSnapshot,
};

pub struct AgentManager {
    sessions: HashMap<AgentId, AgentSession>,
    order: Vec<AgentId>,
    cwd: PathBuf,
    next_id: Option<u64>,
    pty_size: PtySize,
    terminal_palette: Option<TerminalPalette>,
}

impl AgentManager {
    pub fn new(cwd: PathBuf, pty_size: PtySize, terminal_palette: Option<TerminalPalette>) -> Self {
        Self {
            sessions: HashMap::new(),
            order: Vec::new(),
            cwd,
            next_id: Some(1),
            pty_size,
            terminal_palette,
        }
    }

    pub fn spawn(&mut self, kind: AgentKind) -> Result<SessionSnapshot> {
        let id = self.allocate_id()?;
        let session =
            AgentSession::spawn(id, kind, &self.cwd, self.pty_size, self.terminal_palette)?;
        let snapshot = session.snapshot();
        self.sessions.insert(id, session);
        self.order.push(id);
        Ok(snapshot)
    }

    pub fn close(&mut self, id: AgentId) -> Result<()> {
        let Some(session) = self.sessions.get_mut(&id) else {
            return Ok(());
        };
        session.stop()?;
        self.sessions.remove(&id);
        self.order.retain(|candidate| *candidate != id);
        Ok(())
    }

    pub fn send(&self, id: AgentId, bytes: &[u8]) -> Result<()> {
        if let Some(session) = self.sessions.get(&id) {
            session.send(bytes)?;
        }
        Ok(())
    }

    pub fn terminal_snapshot(&self, id: AgentId) -> Option<TerminalSnapshot> {
        self.sessions.get(&id).map(AgentSession::terminal_snapshot)
    }

    pub fn poll(&mut self) -> Vec<Result<SessionSnapshot>> {
        let mut snapshots = Vec::with_capacity(self.order.len());
        for id in &self.order {
            if let Some(session) = self.sessions.get_mut(id) {
                snapshots.push(session.poll());
            }
        }
        snapshots
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let size = pty_size(rows, cols);
        if size == self.pty_size {
            return Ok(());
        }
        for id in &self.order {
            if let Some(session) = self.sessions.get(id) {
                session.resize(size.rows, size.cols)?;
            }
        }
        self.pty_size = size;
        Ok(())
    }

    fn allocate_id(&mut self) -> Result<AgentId> {
        let value = self
            .next_id
            .take()
            .ok_or("agent identifier space exhausted")?;
        self.next_id = value.checked_add(1);
        Ok(AgentId::new(value))
    }
}

pub const fn pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: if rows == 0 { 1 } else { rows },
        cols: if cols == 0 { 1 } else { cols },
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

    #[test]
    fn agent_ids_are_monotonic_and_exhaust_cleanly() {
        let mut manager = AgentManager::new(PathBuf::new(), pty_size(1, 1), None);
        manager.next_id = Some(u64::MAX);

        assert_eq!(manager.allocate_id().unwrap(), AgentId::new(u64::MAX));
        assert_eq!(
            manager.allocate_id().unwrap_err().to_string(),
            "agent identifier space exhausted"
        );
    }
}
