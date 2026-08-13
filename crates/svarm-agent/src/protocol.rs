use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    AgentId, AgentKind, ProcessExit, SessionStatus, TerminalPalette,
    terminal_model::{TerminalSnapshot, TerminalSnapshotDiff},
};

pub use crate::terminal_model::{MouseEncoding, MouseProtocol, TerminalModes};

pub const PROTOCOL_VERSION: u16 = 11;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProtocolRange {
    pub min: u16,
    pub max: u16,
}

impl ProtocolRange {
    pub const CURRENT: Self = Self {
        min: PROTOCOL_VERSION,
        max: PROTOCOL_VERSION,
    };

    pub fn negotiate(self, peer: Self) -> Option<u16> {
        let min = self.min.max(peer.min);
        let max = self.max.min(peer.max);
        (min <= max).then_some(max)
    }
}

macro_rules! integer_id {
    ($name:ident, $value:ty) => {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub $value);
    };
}

integer_id!(RequestId, u64);
integer_id!(SessionId, u64);
integer_id!(ConnectionId, u64);
integer_id!(TerminalSequence, u64);
integer_id!(SessionRevision, u64);

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct LeaseToken(pub String);

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ServerInstanceId(pub String);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Envelope {
    pub protocol_version: u16,
    pub request_id: Option<RequestId>,
    pub message: Message,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum Message {
    Hello(Hello),
    Welcome(Welcome),
    Request(Request),
    Response(Response),
    Event(Event),
    Error(ProtocolError),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionRole {
    Interactive,
    Control,
    Probe,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostTerminalCapabilities {
    pub color_enabled: bool,
    pub true_color: bool,
    pub mouse: bool,
    pub bracketed_paste: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerCapabilities {
    pub takeover: bool,
    pub terminal_diffs: bool,
    pub available_harnesses: Vec<crate::AgentKind>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Hello {
    pub application_version: String,
    pub protocol: ProtocolRange,
    pub role: ConnectionRole,
    pub process_id: Option<u32>,
    pub terminal: HostTerminalCapabilities,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Welcome {
    pub application_version: String,
    pub protocol_version: u16,
    pub process_id: u32,
    pub instance_id: ServerInstanceId,
    pub capabilities: ServerCapabilities,
    pub connection_id: ConnectionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    AttachSession {
        session_id: SessionId,
        rows: u16,
        cols: u16,
        palette: Option<TerminalPalette>,
        takeover: bool,
    },
    CreateSession {
        rows: u16,
        cols: u16,
        palette: Option<TerminalPalette>,
    },
    DetachSession {
        lease_token: LeaseToken,
    },
    SpawnAgent {
        lease_token: LeaseToken,
        kind: AgentKind,
        launch_directory: PathBuf,
    },
    ArchiveAgent {
        lease_token: LeaseToken,
        agent_id: AgentId,
    },
    ResumeArchived {
        lease_token: LeaseToken,
        conversation_id: String,
    },
    CloseAgent {
        lease_token: LeaseToken,
        agent_id: AgentId,
    },
    StopAttachedSession {
        lease_token: LeaseToken,
    },
    Key {
        lease_token: LeaseToken,
        agent_id: AgentId,
        event: KeyInput,
    },
    InputBytes {
        lease_token: LeaseToken,
        agent_id: AgentId,
        bytes: Vec<u8>,
    },
    Paste {
        lease_token: LeaseToken,
        agent_id: AgentId,
        text: String,
    },
    Mouse {
        lease_token: LeaseToken,
        agent_id: AgentId,
        event: MouseInput,
    },
    TerminalViewport {
        lease_token: LeaseToken,
        agent_id: AgentId,
        scrollback: usize,
    },
    ResizeSession {
        lease_token: LeaseToken,
        rows: u16,
        cols: u16,
    },
    SelectAgent {
        lease_token: LeaseToken,
        agent_id: AgentId,
    },
    MarkSeen {
        lease_token: LeaseToken,
        agent_id: AgentId,
        generation: u64,
    },
    ResyncTerminal {
        lease_token: LeaseToken,
        agent_id: AgentId,
        last_sequence: Option<TerminalSequence>,
    },
    AcknowledgeFrame {
        lease_token: LeaseToken,
        agent_id: AgentId,
        sequence: TerminalSequence,
    },
    ServerStatus,
    ListSessions,
    GetSession {
        session_id: SessionId,
    },
    StopSession {
        session_id: SessionId,
        confirmed: bool,
    },
    StopServer {
        confirmed: bool,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Attached {
        session_id: SessionId,
        lease_token: LeaseToken,
    },
    Created {
        session_id: SessionId,
        lease_token: LeaseToken,
    },
    ServerStatus(ServerStatusSnapshot),
    Sessions {
        sessions: Vec<SessionSummary>,
    },
    Session {
        session: Option<SvarmSessionSnapshot>,
    },
    Stopped(StopSummary),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "event", content = "body", rename_all = "snake_case")]
pub enum Event {
    SvarmSessionSnapshot(SvarmSessionSnapshot),
    SvarmSessionChanged(SessionSummary),
    AgentAdded {
        revision: SessionRevision,
        agent: Box<AgentSnapshot>,
    },
    AgentChanged {
        revision: SessionRevision,
        agent: Box<AgentSnapshot>,
    },
    AgentRemoved {
        revision: SessionRevision,
        agent_id: AgentId,
    },
    AgentArchived {
        revision: SessionRevision,
        agent_id: AgentId,
        conversation: ArchivedConversation,
    },
    ArchivedResumed {
        revision: SessionRevision,
        conversation_id: String,
        agent: Box<AgentSnapshot>,
    },
    ConversationSwitched {
        revision: SessionRevision,
        agent: Box<AgentSnapshot>,
        archived: Option<ArchivedConversation>,
        reactivated_id: Option<String>,
    },
    TerminalFull(TerminalFull),
    TerminalDiff(TerminalDiff),
    TerminalViewport(TerminalViewport),
    SessionNotice(SessionNotice),
    LeaseRevoked {
        reason: String,
    },
    ServerStopping,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct InputModifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "key", content = "value", rename_all = "snake_case")]
pub enum KeyCode {
    Character(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Function(u8),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeyInput {
    pub code: KeyCode,
    pub modifiers: InputModifiers,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "button", rename_all = "snake_case")]
pub enum MouseKind {
    Down(MouseButton),
    Up(MouseButton),
    Drag(MouseButton),
    Moved,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MouseInput {
    pub kind: MouseKind,
    pub column: u16,
    pub row: u16,
    pub modifiers: InputModifiers,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AttachmentSummary {
    pub connection_id: ConnectionId,
    pub process_id: Option<u32>,
    pub attached_at_ms: u64,
    pub last_activity_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSummary {
    pub id: SessionId,
    pub running_agents: usize,
    pub total_agents: usize,
    pub attachment: Option<AttachmentSummary>,
    pub last_user_activity_ms: u64,
    pub revision: SessionRevision,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecognitionEvidence {
    pub provider: AgentKind,
    pub claim: AgentActivity,
    pub rule: String,
    pub evidence: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivity {
    #[default]
    Unknown,
    Idle,
    Working,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GitContext {
    pub branch: String,
    pub worktree: PathBuf,
    /// Whether `worktree` is a linked worktree rather than the repository's main checkout.
    pub linked: bool,
    pub additions: u64,
    pub deletions: u64,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentSnapshot {
    pub id: AgentId,
    pub kind: AgentKind,
    pub launch_directory: PathBuf,
    /// Where the agent is working now, when it can be observed and differs from where it started.
    pub working_directory: Option<PathBuf>,
    pub status: SessionStatus,
    pub exit: Option<ProcessExit>,
    pub output_generation: u64,
    pub seen_generation: u64,
    pub completed_generation: u64,
    pub terminal_sequence: TerminalSequence,
    pub read_error: Option<String>,
    pub conversation_title: Option<String>,
    pub conversation_id: Option<String>,
    pub activity: AgentActivity,
    pub recognition: Option<RecognitionEvidence>,
    pub git: Option<GitContext>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArchivedConversation {
    pub conversation_id: String,
    pub title: String,
    pub kind: AgentKind,
    pub launch_directory: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SvarmSessionSnapshot {
    pub summary: SessionSummary,
    pub selected_agent_id: Option<AgentId>,
    pub rows: u16,
    pub cols: u16,
    pub agents: Vec<AgentSnapshot>,
    pub archived: Vec<ArchivedConversation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalFull {
    pub agent_id: AgentId,
    pub output_generation: u64,
    pub sequence: TerminalSequence,
    pub snapshot: TerminalSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalDiff {
    pub agent_id: AgentId,
    pub output_generation: u64,
    pub base_sequence: TerminalSequence,
    pub sequence: TerminalSequence,
    pub diff: TerminalSnapshotDiff,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalViewport {
    pub agent_id: AgentId,
    pub requested_scrollback: usize,
    pub scrollback: usize,
    pub snapshot: TerminalSnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameDisposition {
    Apply,
    Duplicate,
    Gap,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TerminalFrameTracker {
    sequence: Option<TerminalSequence>,
}

impl TerminalFrameTracker {
    pub fn accept_full(&mut self, sequence: TerminalSequence) -> FrameDisposition {
        if self.sequence.is_some_and(|current| sequence <= current) {
            return FrameDisposition::Duplicate;
        }
        self.sequence = Some(sequence);
        FrameDisposition::Apply
    }

    pub fn accept_diff(
        &mut self,
        base: TerminalSequence,
        sequence: TerminalSequence,
    ) -> FrameDisposition {
        let Some(current) = self.sequence else {
            return FrameDisposition::Gap;
        };
        if sequence <= current {
            return FrameDisposition::Duplicate;
        }
        if base != current {
            return FrameDisposition::Gap;
        }
        self.sequence = Some(sequence);
        FrameDisposition::Apply
    }

    pub const fn sequence(self) -> Option<TerminalSequence> {
        self.sequence
    }

    pub fn reset(&mut self) {
        self.sequence = None;
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionNotice {
    pub level: NoticeLevel,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerStatusSnapshot {
    pub process_id: u32,
    pub application_version: String,
    pub protocol_version: u16,
    pub instance_id: ServerInstanceId,
    pub socket_path: PathBuf,
    pub uptime_ms: u64,
    pub session_count: usize,
    pub client_count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StopSummary {
    pub session_count: usize,
    pub agent_count: usize,
    pub cleanup_errors: usize,
    pub server_stopped: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    IncompatibleProtocol,
    SessionNotFound,
    SessionAlreadyAttached,
    SessionStopped,
    AgentNotFound,
    AgentExited,
    InvalidLease,
    InvalidDimensions,
    InvalidRequest,
    FrameTooLarge,
    PermissionDenied,
    ServerStopping,
    InternalError,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, String>,
}

impl ProtocolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            context: BTreeMap::new(),
        }
    }

    pub fn incompatible(client: ProtocolRange, server: ProtocolRange) -> Self {
        Self {
            code: ErrorCode::IncompatibleProtocol,
            message: "client and server protocol versions are incompatible".into(),
            context: BTreeMap::from([
                ("client_min".into(), client.min.to_string()),
                ("client_max".into(), client.max.to_string()),
                ("server_min".into(), server.min.to_string()),
                ("server_max".into(), server.max.to_string()),
            ]),
        }
    }

    pub fn actionable_message(&self) -> String {
        match self.code {
            ErrorCode::IncompatibleProtocol => format!(
                "{} (client {}–{}, server {}–{}). Upgrade or restart Svarm so the client and server versions match; the live server was not stopped.",
                self.message,
                self.context.get("client_min").map_or("?", String::as_str),
                self.context.get("client_max").map_or("?", String::as_str),
                self.context.get("server_min").map_or("?", String::as_str),
                self.context.get("server_max").map_or("?", String::as_str),
            ),
            ErrorCode::SessionAlreadyAttached => {
                let connection = self
                    .context
                    .get("connection_id")
                    .map_or("unknown", String::as_str);
                let process = self
                    .context
                    .get("process_id")
                    .map(|pid| format!(", process {pid}"))
                    .unwrap_or_default();
                let age = self
                    .context
                    .get("attachment_age_ms")
                    .and_then(|age| age.parse::<u64>().ok())
                    .map(|age| format!(", attached for {}s", age / 1_000))
                    .unwrap_or_default();
                format!(
                    "{} (connection {connection}{process}{age}). Retry with `--takeover` only if you intend to disconnect it.",
                    self.message
                )
            }
            _ => self.message.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::terminal_model::TerminalSize;

    use super::*;

    #[test]
    fn protocol_nine_spawn_request_has_a_stable_launch_directory() {
        let request = Request::SpawnAgent {
            lease_token: LeaseToken("lease".into()),
            kind: AgentKind::Codex,
            launch_directory: PathBuf::from("/tmp/workspace"),
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "request": "spawn_agent",
                "lease_token": "lease",
                "kind": "codex",
                "launch_directory": "/tmp/workspace"
            })
        );
    }

    #[test]
    fn protocol_nine_session_requests_are_workspace_neutral_and_id_targeted() {
        assert_eq!(
            serde_json::to_value(Request::CreateSession {
                rows: 24,
                cols: 80,
                palette: None,
            })
            .unwrap(),
            serde_json::json!({
                "request": "create_session",
                "rows": 24,
                "cols": 80,
                "palette": null
            })
        );
        assert_eq!(
            serde_json::to_value(Request::GetSession {
                session_id: SessionId(7),
            })
            .unwrap(),
            serde_json::json!({
                "request": "get_session",
                "session_id": 7
            })
        );
    }

    #[test]
    fn negotiation_selects_the_highest_shared_version() {
        assert_eq!(
            ProtocolRange { min: 1, max: 4 }.negotiate(ProtocolRange { min: 2, max: 3 }),
            Some(3)
        );
        assert_eq!(
            ProtocolRange { min: 1, max: 2 }.negotiate(ProtocolRange { min: 3, max: 4 }),
            None
        );
    }

    #[test]
    fn incompatible_protocol_errors_have_stable_context() {
        let error = ProtocolError::incompatible(
            ProtocolRange { min: 1, max: 2 },
            ProtocolRange { min: 4, max: 5 },
        );

        assert_eq!(error.code, ErrorCode::IncompatibleProtocol);
        assert_eq!(error.context["client_max"], "2");
        assert_eq!(error.context["server_min"], "4");
        let message = error.actionable_message();
        assert!(message.contains("client 1–2, server 4–5"));
        assert!(message.contains("live server was not stopped"));
    }

    #[test]
    fn terminal_sequences_reject_duplicates_and_detect_gaps() {
        let mut tracker = TerminalFrameTracker::default();
        assert_eq!(
            tracker.accept_diff(TerminalSequence(0), TerminalSequence(1)),
            FrameDisposition::Gap
        );
        assert_eq!(
            tracker.accept_full(TerminalSequence(2)),
            FrameDisposition::Apply
        );
        assert_eq!(
            tracker.accept_full(TerminalSequence(2)),
            FrameDisposition::Duplicate
        );
        assert_eq!(
            tracker.accept_diff(TerminalSequence(1), TerminalSequence(3)),
            FrameDisposition::Gap
        );
        assert_eq!(
            tracker.accept_diff(TerminalSequence(2), TerminalSequence(3)),
            FrameDisposition::Apply
        );
        assert_eq!(tracker.sequence(), Some(TerminalSequence(3)));
    }

    #[test]
    fn terminal_frames_serialize_semantics_without_backend_vt_streams() {
        let mut snapshot = TerminalSnapshot::blank(TerminalSize::new(1, 2));
        snapshot.cells[0].contents = "λ".into();
        let json = serde_json::to_string(&Event::TerminalFull(TerminalFull {
            agent_id: AgentId::new(7),
            output_generation: 3,
            sequence: TerminalSequence(9),
            snapshot,
        }))
        .unwrap();

        assert!(json.contains("\"snapshot\""));
        assert!(json.contains("\"t\":\"λ\""));
        assert!(!json.contains("formatted_screen"));
        assert!(!json.contains("formatted_changes"));
        assert!(!json.contains("\\u001b"));
    }
}
