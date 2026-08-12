use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
};

use portable_pty::PtySize;

use crate::{
    AgentId, AgentKind, AgentSession, Result, SessionSnapshot, SessionStatus, TerminalPalette,
    session::{ConversationTracking, OutputNotifier},
    terminal_model::TerminalSnapshot,
};

pub struct AgentManager {
    sessions: HashMap<AgentId, AgentSession>,
    order: Vec<AgentId>,
    next_id: Option<u64>,
    pty_size: PtySize,
    terminal_palette: Option<TerminalPalette>,
    dirty_tx: SyncSender<AgentId>,
    dirty_rx: Receiver<AgentId>,
    wake: Option<OutputNotifier>,
}

impl AgentManager {
    pub fn new(
        pty_size: PtySize,
        terminal_palette: Option<TerminalPalette>,
        wake: Option<OutputNotifier>,
    ) -> Self {
        let (dirty_tx, dirty_rx) = sync_channel(1_024);
        Self {
            sessions: HashMap::new(),
            order: Vec::new(),
            next_id: Some(1),
            pty_size,
            terminal_palette,
            dirty_tx,
            dirty_rx,
            wake,
        }
    }

    /// Records the agent as dirty and wakes the owner so the new output is forwarded on this
    /// iteration of its loop rather than on its next timed tick.
    fn notifier(&self) -> OutputNotifier {
        let dirty_tx = self.dirty_tx.clone();
        let wake = self.wake.clone();
        Arc::new(move |id| {
            let _ = dirty_tx.try_send(id);
            if let Some(wake) = &wake {
                wake(id);
            }
        })
    }

    pub fn spawn(&mut self, kind: AgentKind, launch_directory: &Path) -> Result<SessionSnapshot> {
        let id = self.allocate_id()?;
        let conversation_id = kind
            .preassigns_conversation_id()
            .then(super::session::new_uuid)
            .transpose()?;
        let mut command =
            super::session::agent_command(kind, launch_directory, conversation_id.as_deref())?;
        let tracking = ConversationTracking::new(kind, conversation_id)?;
        tracking.configure(&mut command);
        let session = AgentSession::spawn_command_with_conversation(
            id,
            launch_directory,
            self.pty_size,
            command,
            self.terminal_palette,
            Some(self.notifier()),
            tracking,
        )?;
        let snapshot = session.snapshot();
        self.sessions.insert(id, session);
        self.order.push(id);
        Ok(snapshot)
    }

    pub fn resume(
        &mut self,
        kind: AgentKind,
        launch_directory: &Path,
        conversation_id: &str,
    ) -> Result<SessionSnapshot> {
        let id = self.allocate_id()?;
        let mut command =
            super::session::resume_agent_command(kind, launch_directory, conversation_id)?;
        let tracking = ConversationTracking::new(kind, Some(conversation_id.to_owned()))?;
        tracking.configure(&mut command);
        let session = AgentSession::spawn_command_with_conversation(
            id,
            launch_directory,
            self.pty_size,
            command,
            self.terminal_palette,
            Some(self.notifier()),
            tracking,
        )?;
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

    pub fn with_terminal<T>(
        &self,
        id: AgentId,
        read: impl FnOnce(&TerminalSnapshot) -> T,
    ) -> Option<T> {
        self.sessions
            .get(&id)
            .map(|session| session.with_terminal(read))
    }

    pub fn terminal_snapshot(&self, id: AgentId) -> Option<TerminalSnapshot> {
        self.sessions.get(&id).map(AgentSession::terminal_snapshot)
    }

    pub fn viewport(&self, id: AgentId, requested: usize) -> Option<TerminalSnapshot> {
        self.sessions
            .get(&id)
            .map(|session| session.viewport(requested))
    }

    pub(crate) fn working_directory(&self, id: AgentId) -> Option<PathBuf> {
        self.sessions
            .get(&id)
            .and_then(AgentSession::working_directory)
    }

    pub fn terminal_modes(&self, id: AgentId) -> Option<crate::protocol::TerminalModes> {
        self.sessions.get(&id).map(AgentSession::terminal_modes)
    }

    pub fn snapshot(&self, id: AgentId) -> Option<SessionSnapshot> {
        self.sessions.get(&id).map(AgentSession::snapshot)
    }

    pub fn snapshots(&self) -> Vec<SessionSnapshot> {
        self.order
            .iter()
            .filter_map(|id| self.snapshot(*id))
            .collect()
    }

    pub fn agent_ids(&self) -> &[AgentId] {
        &self.order
    }

    pub fn running_count(&self) -> usize {
        self.sessions
            .values()
            .filter(|session| session.snapshot().status == SessionStatus::Running)
            .count()
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn set_terminal_palette(&mut self, palette: Option<TerminalPalette>) {
        self.terminal_palette = palette;
        for session in self.sessions.values() {
            session.set_terminal_palette(palette);
        }
    }

    pub fn stop_all(&mut self) -> Vec<(AgentId, String)> {
        let mut errors = Vec::new();
        for id in &self.order {
            if let Some(session) = self.sessions.get_mut(id)
                && let Err(error) = session.stop()
            {
                errors.push((*id, error.to_string()));
            }
        }
        self.sessions.clear();
        self.order.clear();
        errors
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

    pub fn drain_dirty(&self) -> BTreeSet<AgentId> {
        self.dirty_rx.try_iter().collect()
    }

    #[cfg(test)]
    pub(crate) fn spawn_test_command(
        &mut self,
        kind: AgentKind,
        launch_directory: &Path,
        program: &str,
        args: &[String],
    ) -> Result<SessionSnapshot> {
        self.spawn_test_command_with_conversation(kind, launch_directory, program, args, None)
    }

    #[cfg(test)]
    pub(crate) fn spawn_test_command_with_conversation(
        &mut self,
        kind: AgentKind,
        launch_directory: &Path,
        program: &str,
        args: &[String],
        conversation_id: Option<String>,
    ) -> Result<SessionSnapshot> {
        use portable_pty::CommandBuilder;

        let id = self.allocate_id()?;
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.cwd(launch_directory);
        let session = AgentSession::spawn_command_with_conversation(
            id,
            launch_directory,
            self.pty_size,
            command,
            self.terminal_palette,
            Some(self.notifier()),
            ConversationTracking::without_signal(kind, conversation_id),
        )?;
        let snapshot = session.snapshot();
        self.sessions.insert(id, session);
        self.order.push(id);
        Ok(snapshot)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let size = pty_size(rows, cols);
        if size == self.pty_size {
            return Ok(());
        }
        let previous = self.pty_size;
        let mut resized = Vec::new();
        for id in &self.order {
            if let Some(session) = self.sessions.get(id) {
                if let Err(error) = session.resize(size.rows, size.cols) {
                    for resized_id in resized {
                        if let Some(session) = self.sessions.get(&resized_id) {
                            let _ = session.resize(previous.rows, previous.cols);
                        }
                    }
                    return Err(error);
                }
                resized.push(*id);
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
        let mut manager = AgentManager::new(pty_size(1, 1), None, None);
        manager.next_id = Some(u64::MAX);

        assert_eq!(manager.allocate_id().unwrap(), AgentId::new(u64::MAX));
        assert_eq!(
            manager.allocate_id().unwrap_err().to_string(),
            "agent identifier space exhausted"
        );
    }

    #[test]
    fn each_agent_keeps_its_own_launch_directory() {
        let first = std::env::current_dir().unwrap();
        let second = first.parent().unwrap().to_owned();
        let mut manager = AgentManager::new(pty_size(1, 1), None, None);
        let args = vec!["-c".into(), "exit 0".into()];

        let first_snapshot = manager
            .spawn_test_command(AgentKind::Codex, &first, "sh", &args)
            .unwrap();
        let second_snapshot = manager
            .spawn_test_command(AgentKind::Claude, &second, "sh", &args)
            .unwrap();

        assert_eq!(first_snapshot.launch_directory, first);
        assert_eq!(second_snapshot.launch_directory, second);
    }
}
