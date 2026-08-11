use std::{collections::BTreeMap, path::PathBuf};

use crate::{
    AgentId, TerminalPalette,
    protocol::{
        AttachmentSummary, ConnectionId, ErrorCode, LeaseToken, ProtocolError, SessionId,
        SessionRevision, SessionSummary, TerminalSequence,
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachmentLease {
    pub connection_id: ConnectionId,
    pub process_id: Option<u32>,
    pub token: LeaseToken,
    pub attached_at_ms: u64,
    pub last_activity_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachResult {
    pub revoked_connection: Option<ConnectionId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentState {
    seen_generation: u64,
    terminal_sequence: TerminalSequence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerSessionState {
    id: SessionId,
    canonical_path: PathBuf,
    display_name: String,
    selected_agent_id: Option<AgentId>,
    agents: BTreeMap<AgentId, AgentState>,
    rows: u16,
    cols: u16,
    terminal_palette: Option<TerminalPalette>,
    attachment: Option<AttachmentLease>,
    last_user_activity_ms: u64,
    revision: SessionRevision,
    stopped: bool,
}

impl ServerSessionState {
    pub fn new(
        id: SessionId,
        canonical_path: PathBuf,
        rows: u16,
        cols: u16,
        terminal_palette: Option<TerminalPalette>,
        now_ms: u64,
    ) -> Result<Self, ProtocolError> {
        validate_dimensions(rows, cols)?;
        let display_name = canonical_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| canonical_path.to_str().unwrap_or("workspace"))
            .to_owned();
        Ok(Self {
            id,
            canonical_path,
            display_name,
            selected_agent_id: None,
            agents: BTreeMap::new(),
            rows,
            cols,
            terminal_palette,
            attachment: None,
            last_user_activity_ms: now_ms,
            revision: SessionRevision(0),
            stopped: false,
        })
    }

    pub fn attach(
        &mut self,
        connection_id: ConnectionId,
        process_id: Option<u32>,
        token: LeaseToken,
        takeover: bool,
        now_ms: u64,
    ) -> Result<AttachResult, ProtocolError> {
        self.ensure_running()?;
        let revoked_connection = match &self.attachment {
            Some(lease) if !takeover => {
                let mut error = ProtocolError::new(
                    ErrorCode::SessionAlreadyAttached,
                    "Svarm session already has an interactive client",
                );
                error
                    .context
                    .insert("connection_id".into(), lease.connection_id.0.to_string());
                error.context.insert(
                    "attachment_age_ms".into(),
                    now_ms.saturating_sub(lease.attached_at_ms).to_string(),
                );
                return Err(error);
            }
            Some(lease) => Some(lease.connection_id),
            None => None,
        };
        self.attachment = Some(AttachmentLease {
            connection_id,
            process_id,
            token,
            attached_at_ms: now_ms,
            last_activity_ms: now_ms,
        });
        self.touch(now_ms);
        Ok(AttachResult { revoked_connection })
    }

    pub fn detach(&mut self, token: &LeaseToken, now_ms: u64) -> Result<(), ProtocolError> {
        self.validate_lease(token)?;
        self.attachment = None;
        self.touch(now_ms);
        Ok(())
    }

    pub fn disconnect(&mut self, connection_id: ConnectionId, now_ms: u64) -> bool {
        if self
            .attachment
            .as_ref()
            .is_none_or(|lease| lease.connection_id != connection_id)
        {
            return false;
        }
        self.attachment = None;
        self.touch(now_ms);
        true
    }

    pub fn validate_lease(&self, token: &LeaseToken) -> Result<(), ProtocolError> {
        self.ensure_running()?;
        match &self.attachment {
            Some(lease) if lease.token == *token => Ok(()),
            _ => Err(ProtocolError::new(
                ErrorCode::InvalidLease,
                "interactive session lease is missing or no longer valid",
            )),
        }
    }

    pub fn register_agent(&mut self, id: AgentId, generation: u64, now_ms: u64) {
        self.agents.insert(
            id,
            AgentState {
                seen_generation: generation,
                terminal_sequence: TerminalSequence(0),
            },
        );
        self.selected_agent_id = Some(id);
        self.touch(now_ms);
    }

    pub fn remove_agent(&mut self, id: AgentId, remaining_order: &[AgentId], now_ms: u64) {
        self.agents.remove(&id);
        if self.selected_agent_id == Some(id) {
            self.selected_agent_id = remaining_order.last().copied();
        }
        self.touch(now_ms);
    }

    pub fn select_agent(&mut self, id: AgentId, now_ms: u64) -> Result<(), ProtocolError> {
        if !self.agents.contains_key(&id) {
            return Err(ProtocolError::new(
                ErrorCode::AgentNotFound,
                "agent does not exist in this Svarm session",
            ));
        }
        self.selected_agent_id = Some(id);
        self.touch(now_ms);
        Ok(())
    }

    pub fn mark_seen(
        &mut self,
        id: AgentId,
        generation: u64,
        current_generation: u64,
        now_ms: u64,
    ) -> Result<(), ProtocolError> {
        let Some(agent) = self.agents.get_mut(&id) else {
            return Err(ProtocolError::new(
                ErrorCode::AgentNotFound,
                "agent does not exist in this Svarm session",
            ));
        };
        agent.seen_generation = agent
            .seen_generation
            .max(generation.min(current_generation));
        self.touch(now_ms);
        Ok(())
    }

    pub fn resize(&mut self, rows: u16, cols: u16, now_ms: u64) -> Result<(), ProtocolError> {
        validate_dimensions(rows, cols)?;
        self.rows = rows;
        self.cols = cols;
        self.touch(now_ms);
        Ok(())
    }

    pub fn set_terminal_palette(&mut self, terminal_palette: Option<TerminalPalette>, now_ms: u64) {
        self.terminal_palette = terminal_palette;
        self.touch(now_ms);
    }

    pub fn next_terminal_sequence(
        &mut self,
        id: AgentId,
    ) -> Result<TerminalSequence, ProtocolError> {
        let Some(agent) = self.agents.get_mut(&id) else {
            return Err(ProtocolError::new(
                ErrorCode::AgentNotFound,
                "agent does not exist in this Svarm session",
            ));
        };
        agent.terminal_sequence.0 = agent.terminal_sequence.0.checked_add(1).ok_or_else(|| {
            ProtocolError::new(ErrorCode::InternalError, "terminal sequence exhausted")
        })?;
        Ok(agent.terminal_sequence)
    }

    pub fn stop(&mut self) {
        self.stopped = true;
        self.attachment = None;
        self.bump_revision();
    }

    pub fn summary(&self, running_agents: usize, total_agents: usize) -> SessionSummary {
        SessionSummary {
            id: self.id,
            canonical_path: self.canonical_path.clone(),
            display_name: self.display_name.clone(),
            running_agents,
            total_agents,
            attachment: self.attachment.as_ref().map(|lease| AttachmentSummary {
                connection_id: lease.connection_id,
                process_id: lease.process_id,
                attached_at_ms: lease.attached_at_ms,
                last_activity_ms: lease.last_activity_ms,
            }),
            last_user_activity_ms: self.last_user_activity_ms,
            revision: self.revision,
        }
    }

    pub const fn id(&self) -> SessionId {
        self.id
    }

    pub fn canonical_path(&self) -> &PathBuf {
        &self.canonical_path
    }

    pub const fn selected_agent_id(&self) -> Option<AgentId> {
        self.selected_agent_id
    }

    pub const fn dimensions(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    pub const fn terminal_palette(&self) -> Option<TerminalPalette> {
        self.terminal_palette
    }

    pub fn attachment(&self) -> Option<&AttachmentLease> {
        self.attachment.as_ref()
    }

    pub fn seen_generation(&self, id: AgentId) -> Option<u64> {
        self.agents.get(&id).map(|agent| agent.seen_generation)
    }

    pub fn terminal_sequence(&self, id: AgentId) -> Option<TerminalSequence> {
        self.agents.get(&id).map(|agent| agent.terminal_sequence)
    }

    pub const fn revision(&self) -> SessionRevision {
        self.revision
    }

    fn ensure_running(&self) -> Result<(), ProtocolError> {
        if self.stopped {
            Err(ProtocolError::new(
                ErrorCode::SessionStopped,
                "Svarm session has stopped",
            ))
        } else {
            Ok(())
        }
    }

    fn touch(&mut self, now_ms: u64) {
        self.last_user_activity_ms = now_ms;
        if let Some(lease) = &mut self.attachment {
            lease.last_activity_ms = now_ms;
        }
        self.bump_revision();
    }

    fn bump_revision(&mut self) {
        self.revision.0 = self.revision.0.saturating_add(1);
    }
}

pub fn sort_session_summaries(summaries: &mut [SessionSummary]) {
    summaries.sort_by(|left, right| {
        right
            .last_user_activity_ms
            .cmp(&left.last_user_activity_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn validate_dimensions(rows: u16, cols: u16) -> Result<(), ProtocolError> {
    if rows == 0 || cols == 0 {
        Err(ProtocolError::new(
            ErrorCode::InvalidDimensions,
            "terminal rows and columns must both be nonzero",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: u64) -> ServerSessionState {
        ServerSessionState::new(
            SessionId(id),
            PathBuf::from(format!("/tmp/project-{id}")),
            24,
            80,
            None,
            10,
        )
        .unwrap()
    }

    #[test]
    fn lease_conflicts_require_explicit_takeover() {
        let mut session = session(1);
        session
            .attach(
                ConnectionId(1),
                Some(10),
                LeaseToken("one".into()),
                false,
                20,
            )
            .unwrap();

        let error = session
            .attach(
                ConnectionId(2),
                Some(20),
                LeaseToken("two".into()),
                false,
                30,
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::SessionAlreadyAttached);
        let result = session
            .attach(
                ConnectionId(2),
                Some(20),
                LeaseToken("two".into()),
                true,
                30,
            )
            .unwrap();
        assert_eq!(result.revoked_connection, Some(ConnectionId(1)));
        assert_eq!(session.attachment().unwrap().connection_id, ConnectionId(2));
    }

    #[test]
    fn disconnect_releases_only_its_own_lease() {
        let mut session = session(1);
        session
            .attach(ConnectionId(2), None, LeaseToken("lease".into()), false, 20)
            .unwrap();

        assert!(!session.disconnect(ConnectionId(1), 30));
        assert!(session.attachment().is_some());
        assert!(session.disconnect(ConnectionId(2), 30));
        assert!(session.attachment().is_none());
    }

    #[test]
    fn stale_seen_acknowledgements_never_mark_newer_output_seen() {
        let mut session = session(1);
        let id = AgentId::new(7);
        session.register_agent(id, 2, 20);

        session.mark_seen(id, 4, 9, 30).unwrap();
        assert_eq!(session.seen_generation(id), Some(4));
        session.mark_seen(id, 3, 12, 40).unwrap();
        assert_eq!(session.seen_generation(id), Some(4));
        session.mark_seen(id, 99, 12, 50).unwrap();
        assert_eq!(session.seen_generation(id), Some(12));
    }

    #[test]
    fn closing_selection_uses_stable_remaining_order() {
        let mut session = session(1);
        let first = AgentId::new(1);
        let second = AgentId::new(2);
        session.register_agent(first, 0, 20);
        session.register_agent(second, 0, 30);

        session.remove_agent(second, &[first], 40);
        assert_eq!(session.selected_agent_id(), Some(first));
        session.remove_agent(first, &[], 50);
        assert_eq!(session.selected_agent_id(), None);
    }

    #[test]
    fn summaries_sort_by_recent_activity_then_stable_id() {
        let first = session(1);
        let mut second = session(2);
        second.register_agent(AgentId::new(1), 0, 20);
        let mut summaries = vec![second.summary(1, 1), first.summary(0, 0)];
        summaries.push(session(3).summary(0, 0));

        sort_session_summaries(&mut summaries);
        assert_eq!(
            summaries
                .iter()
                .map(|summary| summary.id)
                .collect::<Vec<_>>(),
            [SessionId(2), SessionId(1), SessionId(3)]
        );
    }

    #[test]
    fn zero_dimensions_are_rejected_before_reaching_pty_code() {
        assert_eq!(
            ServerSessionState::new(SessionId(1), "/tmp".into(), 0, 80, None, 0)
                .unwrap_err()
                .code,
            ErrorCode::InvalidDimensions
        );
    }
}
