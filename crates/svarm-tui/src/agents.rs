use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

use svarm_agent::{
    AgentId, AgentKind, CursorStyle, Result, TerminalPalette,
    framing::{read_frame, write_frame},
    protocol::{
        ConnectionRole, Envelope, Event, FrameDisposition, Hello, HostTerminalCapabilities,
        KeyInput, LeaseToken, Message, MouseInput, MouseProtocol, PROTOCOL_VERSION, ProtocolRange,
        Request, RequestId, Response, SessionId, StopSummary, SvarmSessionSnapshot,
        TerminalFrameTracker, TerminalSequence, TerminalViewport,
    },
    terminal_model::TerminalSnapshot,
};

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(3);
#[derive(Clone, Debug)]
pub enum InitialSession {
    Create,
    Attach {
        session_id: SessionId,
        takeover: bool,
    },
}

#[derive(Clone, Debug, Default)]
pub struct InitialAgentRequest {
    pub kind: Option<AgentKind>,
    pub workspace: Option<PathBuf>,
}

pub(crate) enum RemoteUpdate {
    Event(Box<Event>),
    TerminalChanged(AgentId),
    TerminalViewportChanged(AgentId),
    Error(String),
    Disconnected(String),
}

pub(crate) enum Incoming {
    Envelope(Box<Envelope>),
    Disconnected(String),
}

/// The single stream the interface blocks on. Host input and server frames arrive through one
/// channel so the loop can sleep until either happens, rather than polling each in turn.
pub(crate) enum ClientEvent {
    Host(crossterm::event::Event),
    Remote(Incoming),
    DirectoryLoaded(crate::workspace::DirectoryLoadResult),
    WorktreeCreated(crate::workspace::WorktreeCreateResult),
    EmbeddedToolChanged,
    HarnessUpdateReady,
}

struct CachedTerminal {
    snapshot: TerminalSnapshot,
    tracker: TerminalFrameTracker,
    scrollback: Option<ScrollbackView>,
    /// The request currently awaiting a viewport response.
    scrollback_request: Option<usize>,
    /// The latest offset requested by input while a response is in flight.
    scrollback_target: Option<usize>,
}

struct ScrollbackView {
    snapshot: TerminalSnapshot,
    offset: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WheelRouting {
    ChildMouse,
    AlternateScreen,
    Scrollback,
}

pub(crate) struct RemoteAgents {
    writer: UnixStream,
    next_request_id: u64,
    session_id: SessionId,
    lease_token: LeaseToken,
    terminals: HashMap<AgentId, CachedTerminal>,
    pending_resync: HashSet<AgentId>,
}

impl RemoteAgents {
    pub fn connect(
        socket_path: &Path,
        target: InitialSession,
        rows: u16,
        cols: u16,
        palette: Option<TerminalPalette>,
        events: SyncSender<ClientEvent>,
    ) -> Result<(Self, SvarmSessionSnapshot, Vec<AgentKind>)> {
        let mut writer = UnixStream::connect(socket_path).map_err(|error| {
            format!(
                "could not connect to Svarm server at {}: {error}",
                socket_path.display()
            )
        })?;
        writer.set_read_timeout(Some(CONNECTION_TIMEOUT))?;
        writer.set_write_timeout(Some(CONNECTION_TIMEOUT))?;
        write_frame(
            &mut writer,
            &Envelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: Some(RequestId(1)),
                message: Message::Hello(Hello {
                    application_version: env!("CARGO_PKG_VERSION").into(),
                    protocol: ProtocolRange::CURRENT,
                    role: ConnectionRole::Interactive,
                    process_id: Some(std::process::id()),
                    terminal: HostTerminalCapabilities {
                        color_enabled: palette.is_some(),
                        true_color: true,
                        mouse: true,
                        bracketed_paste: true,
                    },
                }),
            },
        )?;
        let available_harnesses = match read_frame::<_, Envelope>(&mut writer)? {
            Some(Envelope {
                message: Message::Welcome(welcome),
                ..
            }) => welcome.capabilities.available_harnesses,
            Some(Envelope {
                message: Message::Error(error),
                ..
            }) => return Err(error.actionable_message().into()),
            _ => return Err("Svarm server did not complete the protocol handshake".into()),
        };

        let request = match target {
            InitialSession::Create => Request::CreateSession {
                rows,
                cols,
                palette,
            },
            InitialSession::Attach {
                session_id,
                takeover,
            } => Request::AttachSession {
                session_id,
                rows,
                cols,
                palette,
                takeover,
            },
        };
        write_frame(
            &mut writer,
            &Envelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: Some(RequestId(2)),
                message: Message::Request(request),
            },
        )?;
        let (session_id, lease_token) = loop {
            let Some(envelope) = read_frame::<_, Envelope>(&mut writer)? else {
                return Err("Svarm server disconnected during session attachment".into());
            };
            if envelope.request_id != Some(RequestId(2)) {
                continue;
            }
            match envelope.message {
                Message::Response(Response::Created {
                    session_id,
                    lease_token,
                })
                | Message::Response(Response::Attached {
                    session_id,
                    lease_token,
                }) => break (session_id, lease_token),
                Message::Error(error) => return Err(error.actionable_message().into()),
                _ => return Err("Svarm server returned an invalid attach response".into()),
            }
        };
        let snapshot = loop {
            let Some(envelope) = read_frame::<_, Envelope>(&mut writer)? else {
                return Err("Svarm server disconnected before sending session state".into());
            };
            if let Message::Event(Event::SvarmSessionSnapshot(snapshot)) = envelope.message {
                break snapshot;
            }
        };

        writer.set_read_timeout(None)?;
        writer.set_write_timeout(Some(CONNECTION_TIMEOUT))?;
        let mut reader = writer.try_clone()?;
        thread::spawn(move || {
            loop {
                let incoming = match read_frame(&mut reader) {
                    Ok(Some(envelope)) => Incoming::Envelope(Box::new(envelope)),
                    Ok(None) => {
                        let _ = events.send(ClientEvent::Remote(Incoming::Disconnected(
                            "Svarm server closed the connection".into(),
                        )));
                        break;
                    }
                    Err(error) => {
                        let _ = events.send(ClientEvent::Remote(Incoming::Disconnected(
                            error.to_string(),
                        )));
                        break;
                    }
                };
                if events.send(ClientEvent::Remote(incoming)).is_err() {
                    break;
                }
            }
        });
        Ok((
            Self {
                writer,
                next_request_id: 3,
                session_id,
                lease_token,
                terminals: HashMap::new(),
                pending_resync: HashSet::new(),
            },
            snapshot,
            available_harnesses,
        ))
    }

    pub fn apply(&mut self, incoming: Incoming) -> Vec<RemoteUpdate> {
        let mut updates = Vec::new();
        match incoming {
            Incoming::Envelope(envelope) => match envelope.message {
                Message::Event(Event::TerminalFull(frame)) => {
                    let id = frame.agent_id;
                    let sequence = frame.sequence;
                    let disposition = self.apply_full(frame);
                    self.after_frame(id, sequence, disposition);
                    if disposition == FrameDisposition::Apply {
                        updates.push(RemoteUpdate::TerminalChanged(id));
                    }
                }
                Message::Event(Event::TerminalDiff(frame)) => {
                    let id = frame.agent_id;
                    let sequence = frame.sequence;
                    let disposition = self.apply_diff(frame);
                    self.after_frame(id, sequence, disposition);
                    if disposition == FrameDisposition::Apply {
                        updates.push(RemoteUpdate::TerminalChanged(id));
                    }
                }
                Message::Event(Event::TerminalViewport(viewport)) => {
                    let id = viewport.agent_id;
                    if self.apply_viewport(viewport) {
                        updates.push(RemoteUpdate::TerminalViewportChanged(id));
                    }
                }
                Message::Event(event) => {
                    if let Event::AgentRemoved { agent_id, .. }
                    | Event::AgentArchived { agent_id, .. } = &event
                    {
                        self.terminals.remove(agent_id);
                        self.pending_resync.remove(agent_id);
                    }
                    updates.push(RemoteUpdate::Event(Box::new(event)));
                }
                Message::Error(error) => {
                    updates.push(RemoteUpdate::Error(error.actionable_message()))
                }
                Message::Response(_) => {}
                _ => updates.push(RemoteUpdate::Error(
                    "Svarm server sent an unexpected message".into(),
                )),
            },
            Incoming::Disconnected(error) => {
                updates.push(RemoteUpdate::Disconnected(error));
            }
        }
        updates
    }

    pub fn cursor_style(&self, id: AgentId) -> Option<CursorStyle> {
        self.terminals
            .get(&id)
            .map(|terminal| terminal.snapshot.state.cursor.style)
    }

    pub fn screen(&self, id: AgentId) -> Option<&TerminalSnapshot> {
        self.terminals.get(&id).map(|terminal| {
            terminal
                .scrollback
                .as_ref()
                .map_or(&terminal.snapshot, |view| &view.snapshot)
        })
    }

    pub fn is_scrolled(&self, id: AgentId) -> bool {
        self.terminals
            .get(&id)
            .is_some_and(|terminal| terminal.scrollback.is_some())
    }

    pub fn scrollback_request_pending(&self, id: AgentId) -> bool {
        self.terminals
            .get(&id)
            .is_some_and(|terminal| terminal.scrollback_request.is_some())
    }

    pub fn wheel_routing(&self, id: AgentId) -> WheelRouting {
        let Some(terminal) = self.terminals.get(&id) else {
            return WheelRouting::ChildMouse;
        };
        if terminal.snapshot.state.modes.mouse_protocol != MouseProtocol::None {
            WheelRouting::ChildMouse
        } else if terminal.snapshot.state.alternate_screen
            && terminal.snapshot.state.modes.mouse_alternate_scroll
        {
            WheelRouting::AlternateScreen
        } else {
            WheelRouting::Scrollback
        }
    }

    pub fn scroll(&mut self, agent_id: AgentId, rows: isize) -> Result<()> {
        let Some(terminal) = self.terminals.get(&agent_id) else {
            return Ok(());
        };
        let current = terminal
            .scrollback_target
            .or_else(|| terminal.scrollback.as_ref().map(|view| view.offset))
            .unwrap_or(0);
        let requested = if rows >= 0 {
            current.saturating_add(rows as usize)
        } else {
            current.saturating_sub(rows.unsigned_abs())
        };
        if requested == 0 {
            self.show_live(agent_id);
            return Ok(());
        }
        let send = if let Some(terminal) = self.terminals.get_mut(&agent_id) {
            terminal.scrollback_target = Some(requested);
            if terminal.scrollback_request.is_none() {
                terminal.scrollback_request = Some(requested);
                true
            } else {
                false
            }
        } else {
            false
        };
        if !send {
            return Ok(());
        }
        self.send(Request::TerminalViewport {
            lease_token: self.lease_token.clone(),
            agent_id,
            scrollback: requested,
        })
    }

    /// Ask for the usage overview. The answer arrives as a `UsageChanged` event, and a refreshed
    /// probe lands as a second one once it finishes.
    pub fn request_usage(&mut self, refresh: bool) -> Result<()> {
        self.send(Request::ReadUsage { refresh })
    }

    pub fn show_live(&mut self, agent_id: AgentId) -> bool {
        let Some(terminal) = self.terminals.get_mut(&agent_id) else {
            return false;
        };
        terminal.scrollback_target = None;
        terminal.scrollback.take().is_some()
    }

    pub fn spawn(
        &mut self,
        kind: AgentKind,
        launch_directory: PathBuf,
        events: &Receiver<ClientEvent>,
    ) -> Result<()> {
        match self.send_and_wait(
            Request::SpawnAgent {
                lease_token: self.lease_token.clone(),
                kind,
                launch_directory,
            },
            events,
        )? {
            Response::Ok => Ok(()),
            _ => Err("Svarm server returned an invalid spawn response".into()),
        }
    }

    pub fn close(&mut self, agent_id: AgentId) -> Result<()> {
        self.send(Request::CloseAgent {
            lease_token: self.lease_token.clone(),
            agent_id,
        })
    }

    pub fn archive(&mut self, agent_id: AgentId) -> Result<()> {
        self.send(Request::ArchiveAgent {
            lease_token: self.lease_token.clone(),
            agent_id,
        })
    }

    pub fn resume_archived(&mut self, conversation_id: String) -> Result<()> {
        self.send(Request::ResumeArchived {
            lease_token: self.lease_token.clone(),
            conversation_id,
        })
    }

    pub fn key(&mut self, agent_id: AgentId, event: KeyInput) -> Result<()> {
        self.send(Request::Key {
            lease_token: self.lease_token.clone(),
            agent_id,
            event,
        })
    }

    pub fn literal(&mut self, agent_id: AgentId, bytes: Vec<u8>) -> Result<()> {
        self.send(Request::InputBytes {
            lease_token: self.lease_token.clone(),
            agent_id,
            bytes,
        })
    }

    pub fn paste(&mut self, agent_id: AgentId, text: String) -> Result<()> {
        self.send(Request::Paste {
            lease_token: self.lease_token.clone(),
            agent_id,
            text,
        })
    }

    pub fn mouse(&mut self, agent_id: AgentId, event: MouseInput) -> Result<()> {
        self.send(Request::Mouse {
            lease_token: self.lease_token.clone(),
            agent_id,
            event,
        })
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        for terminal in self.terminals.values_mut() {
            terminal.scrollback = None;
            terminal.scrollback_request = None;
            terminal.scrollback_target = None;
        }
        self.send(Request::ResizeSession {
            lease_token: self.lease_token.clone(),
            rows,
            cols,
        })
    }

    pub fn select(&mut self, agent_id: AgentId) -> Result<()> {
        self.send(Request::SelectAgent {
            lease_token: self.lease_token.clone(),
            agent_id,
        })
    }

    pub fn mark_seen(&mut self, agent_id: AgentId, generation: u64) -> Result<()> {
        self.send(Request::MarkSeen {
            lease_token: self.lease_token.clone(),
            agent_id,
            generation,
        })
    }

    pub fn detach(&mut self, events: &Receiver<ClientEvent>) -> Result<()> {
        match self.send_and_wait(
            Request::DetachSession {
                lease_token: self.lease_token.clone(),
            },
            events,
        )? {
            Response::Ok => Ok(()),
            _ => Err("Svarm server returned an invalid detach response".into()),
        }
    }

    pub fn stop(&mut self, events: &Receiver<ClientEvent>) -> Result<StopSummary> {
        match self.send_and_wait(
            Request::StopAttachedSession {
                lease_token: self.lease_token.clone(),
            },
            events,
        )? {
            Response::Stopped(summary) => Ok(summary),
            _ => Err("Svarm server returned an invalid stop response".into()),
        }
    }

    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    fn apply_full(&mut self, frame: svarm_agent::protocol::TerminalFull) -> FrameDisposition {
        if frame.snapshot.validate().is_err() {
            return FrameDisposition::Gap;
        }
        let disposition = match self.terminals.entry(frame.agent_id) {
            Entry::Vacant(entry) => {
                let mut tracker = TerminalFrameTracker::default();
                let disposition = tracker.accept_full(frame.sequence);
                entry.insert(CachedTerminal {
                    snapshot: frame.snapshot,
                    tracker,
                    scrollback: None,
                    scrollback_request: None,
                    scrollback_target: None,
                });
                disposition
            }
            Entry::Occupied(mut entry) => {
                let terminal = entry.get_mut();
                let disposition = terminal.tracker.accept_full(frame.sequence);
                if disposition == FrameDisposition::Apply {
                    terminal.snapshot = frame.snapshot;
                    terminal.scrollback = None;
                    terminal.scrollback_request = None;
                    terminal.scrollback_target = None;
                }
                disposition
            }
        };
        if disposition == FrameDisposition::Apply {
            self.pending_resync.remove(&frame.agent_id);
        }
        disposition
    }

    fn apply_diff(&mut self, frame: svarm_agent::protocol::TerminalDiff) -> FrameDisposition {
        let Some(terminal) = self.terminals.get_mut(&frame.agent_id) else {
            return FrameDisposition::Gap;
        };
        let disposition = terminal
            .tracker
            .accept_diff(frame.base_sequence, frame.sequence);
        if disposition == FrameDisposition::Apply && terminal.snapshot.apply(&frame.diff).is_err() {
            terminal.tracker.reset();
            return FrameDisposition::Gap;
        }
        disposition
    }

    fn apply_viewport(&mut self, viewport: TerminalViewport) -> bool {
        let Some(terminal) = self.terminals.get_mut(&viewport.agent_id) else {
            return false;
        };
        if terminal.scrollback_request != Some(viewport.requested_scrollback) {
            return false;
        }
        terminal.scrollback_request = None;
        if terminal.scrollback_target != Some(viewport.requested_scrollback) {
            let Some(requested) = terminal.scrollback_target else {
                return false;
            };
            terminal.scrollback_request = Some(requested);
            let request = Request::TerminalViewport {
                lease_token: self.lease_token.clone(),
                agent_id: viewport.agent_id,
                scrollback: requested,
            };
            let _ = self.send(request);
            return false;
        }
        if viewport.scrollback == 0 {
            terminal.scrollback = None;
            terminal.scrollback_target = None;
            return true;
        }
        if viewport.snapshot.validate().is_err() {
            return false;
        }
        terminal.scrollback = Some(ScrollbackView {
            snapshot: viewport.snapshot,
            offset: viewport.scrollback,
        });
        terminal.scrollback_target = Some(viewport.scrollback);
        true
    }

    fn after_frame(
        &mut self,
        agent_id: AgentId,
        sequence: TerminalSequence,
        disposition: FrameDisposition,
    ) {
        if let Some(request) = self.frame_follow_up(agent_id, sequence, disposition) {
            let _ = self.send(request);
        }
    }

    fn frame_follow_up(
        &mut self,
        agent_id: AgentId,
        sequence: TerminalSequence,
        disposition: FrameDisposition,
    ) -> Option<Request> {
        match disposition {
            FrameDisposition::Apply | FrameDisposition::Duplicate => {
                Some(Request::AcknowledgeFrame {
                    lease_token: self.lease_token.clone(),
                    agent_id,
                    sequence,
                })
            }
            FrameDisposition::Gap => {
                if self.pending_resync.insert(agent_id) {
                    let last_sequence = self
                        .terminals
                        .get(&agent_id)
                        .and_then(|terminal| terminal.tracker.sequence());
                    Some(Request::ResyncTerminal {
                        lease_token: self.lease_token.clone(),
                        agent_id,
                        last_sequence,
                    })
                } else {
                    None
                }
            }
        }
    }

    fn send(&mut self, request: Request) -> Result<()> {
        let request_id = RequestId(self.next_request_id);
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or("client request identifier space exhausted")?;
        write_frame(
            &mut self.writer,
            &Envelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: Some(request_id),
                message: Message::Request(request),
            },
        )?;
        Ok(())
    }

    fn send_and_wait(
        &mut self,
        request: Request,
        events: &Receiver<ClientEvent>,
    ) -> Result<Response> {
        let request_id = RequestId(self.next_request_id);
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or("client request identifier space exhausted")?;
        write_frame(
            &mut self.writer,
            &Envelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: Some(request_id),
                message: Message::Request(request),
            },
        )?;
        let deadline = Instant::now() + CONNECTION_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match events.recv_timeout(remaining) {
                Ok(ClientEvent::Remote(Incoming::Envelope(envelope)))
                    if envelope.request_id == Some(request_id) =>
                {
                    return match envelope.message {
                        Message::Response(response) => Ok(response),
                        Message::Error(error) => Err(error.actionable_message().into()),
                        _ => Err("Svarm server returned an invalid response".into()),
                    };
                }
                // Host keystrokes and unrelated frames keep arriving while the session is being
                // torn down; they no longer matter, so drop them and keep waiting.
                Ok(
                    ClientEvent::Remote(Incoming::Envelope(_))
                    | ClientEvent::Host(_)
                    | ClientEvent::DirectoryLoaded(_)
                    | ClientEvent::WorktreeCreated(_)
                    | ClientEvent::EmbeddedToolChanged
                    | ClientEvent::HarnessUpdateReady,
                ) => {}
                Ok(ClientEvent::Remote(Incoming::Disconnected(error))) => return Err(error.into()),
                Err(error) => return Err(format!("Svarm server did not respond: {error}").into()),
            }
        }
    }
}

impl Drop for RemoteAgents {
    fn drop(&mut self) {
        let _ = self.writer.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use svarm_agent::{
        protocol::{TerminalDiff, TerminalFull},
        terminal_model::{TerminalCell, TerminalCellPatch, TerminalModes, TerminalSize},
    };

    use super::*;

    fn snapshot(text: &str) -> TerminalSnapshot {
        let mut snapshot = TerminalSnapshot::blank(TerminalSize::new(2, 12));
        for (column, character) in text.chars().enumerate() {
            snapshot.cell_mut(0, column as u16).unwrap().contents = character.to_string().into();
        }
        snapshot
    }

    fn remote(writer: UnixStream) -> RemoteAgents {
        RemoteAgents {
            writer,
            next_request_id: 1,
            session_id: SessionId(1),
            lease_token: LeaseToken("test".into()),
            terminals: HashMap::new(),
            pending_resync: HashSet::new(),
        }
    }

    #[test]
    fn semantic_frames_apply_in_order_and_gaps_request_a_full_resync() {
        let (writer, _peer) = UnixStream::pair().unwrap();
        let id = AgentId::new(1);
        let mut agents = remote(writer);

        assert_eq!(
            agents.apply_full(TerminalFull {
                agent_id: id,
                output_generation: 1,
                sequence: TerminalSequence(1),
                snapshot: snapshot("one"),
            }),
            FrameDisposition::Apply
        );
        assert_eq!(agents.screen(id).unwrap().contents().trim(), "one");

        assert_eq!(
            agents.apply_full(TerminalFull {
                agent_id: id,
                output_generation: 1,
                sequence: TerminalSequence(1),
                snapshot: snapshot("duplicate"),
            }),
            FrameDisposition::Duplicate
        );
        assert_eq!(agents.screen(id).unwrap().contents().trim(), "one");

        let before = snapshot("one");
        let after = snapshot("two");
        assert_eq!(
            agents.apply_diff(TerminalDiff {
                agent_id: id,
                output_generation: 2,
                base_sequence: TerminalSequence(1),
                sequence: TerminalSequence(2),
                diff: before.diff(&after).unwrap(),
            }),
            FrameDisposition::Apply
        );
        assert_eq!(agents.screen(id).unwrap().contents().trim(), "two");

        let disposition = agents.apply_diff(TerminalDiff {
            agent_id: id,
            output_generation: 3,
            base_sequence: TerminalSequence(99),
            sequence: TerminalSequence(3),
            diff: after.diff(&snapshot("gap")).unwrap(),
        });
        assert_eq!(disposition, FrameDisposition::Gap);
        let request = agents
            .frame_follow_up(id, TerminalSequence(3), disposition)
            .unwrap();
        assert!(matches!(
            request,
            Request::ResyncTerminal {
                agent_id,
                last_sequence: Some(TerminalSequence(2)),
                ..
            } if agent_id == id
        ));

        let mut invalid = after.diff(&snapshot("bad")).unwrap();
        invalid.cells.push(TerminalCellPatch {
            index: 99,
            cell: TerminalCell::default(),
        });
        assert_eq!(
            agents.apply_diff(TerminalDiff {
                agent_id: id,
                output_generation: 3,
                base_sequence: TerminalSequence(2),
                sequence: TerminalSequence(3),
                diff: invalid,
            }),
            FrameDisposition::Gap
        );

        assert_eq!(
            agents.apply_full(TerminalFull {
                agent_id: id,
                output_generation: 4,
                sequence: TerminalSequence(4),
                snapshot: TerminalSnapshot::blank(TerminalSize::new(3, 20)),
            }),
            FrameDisposition::Apply
        );
        assert_eq!(agents.screen(id).unwrap().size(), TerminalSize::new(3, 20));
        assert!(!agents.pending_resync.contains(&id));
    }

    #[test]
    fn historical_viewport_is_separate_from_the_live_snapshot() {
        let (writer, _peer) = UnixStream::pair().unwrap();
        let id = AgentId::new(1);
        let mut agents = RemoteAgents {
            writer,
            next_request_id: 1,
            session_id: SessionId(1),
            lease_token: LeaseToken("test".into()),
            terminals: HashMap::from([(
                id,
                CachedTerminal {
                    snapshot: snapshot("live"),
                    tracker: TerminalFrameTracker::default(),
                    scrollback: None,
                    scrollback_request: Some(4),
                    scrollback_target: Some(4),
                },
            )]),
            pending_resync: HashSet::new(),
        };

        agents.apply_viewport(TerminalViewport {
            agent_id: id,
            requested_scrollback: 4,
            scrollback: 4,
            snapshot: snapshot("older"),
        });

        assert!(agents.screen(id).unwrap().contents().contains("older"));
        assert!(agents.is_scrolled(id));
        assert_eq!(agents.terminals[&id].scrollback.as_ref().unwrap().offset, 4);
        assert!(agents.show_live(id));
        assert!(agents.screen(id).unwrap().contents().contains("live"));
    }

    #[test]
    fn wheel_routing_follows_the_live_child_terminal_modes() {
        let (writer, _peer) = UnixStream::pair().unwrap();
        let id = AgentId::new(1);
        let terminal = |mut snapshot: TerminalSnapshot, modes| {
            snapshot.state.modes = modes;
            CachedTerminal {
                snapshot,
                tracker: TerminalFrameTracker::default(),
                scrollback: None,
                scrollback_request: None,
                scrollback_target: None,
            }
        };
        let mut agents = RemoteAgents {
            writer,
            next_request_id: 1,
            session_id: SessionId(1),
            lease_token: LeaseToken("test".into()),
            terminals: HashMap::new(),
            pending_resync: HashSet::new(),
        };

        agents
            .terminals
            .insert(id, terminal(snapshot(""), TerminalModes::default()));
        assert_eq!(agents.wheel_routing(id), WheelRouting::Scrollback);

        agents
            .terminals
            .get_mut(&id)
            .unwrap()
            .snapshot
            .state
            .alternate_screen = true;
        assert_eq!(agents.wheel_routing(id), WheelRouting::Scrollback);

        agents
            .terminals
            .get_mut(&id)
            .unwrap()
            .snapshot
            .state
            .modes
            .mouse_alternate_scroll = true;
        assert_eq!(agents.wheel_routing(id), WheelRouting::AlternateScreen);

        agents
            .terminals
            .get_mut(&id)
            .unwrap()
            .snapshot
            .state
            .modes
            .mouse_protocol = MouseProtocol::PressRelease;
        assert_eq!(agents.wheel_routing(id), WheelRouting::ChildMouse);
    }

    #[test]
    fn rapid_scroll_requests_accumulate_from_the_pending_offset() {
        let (writer, mut peer) = UnixStream::pair().unwrap();
        let id = AgentId::new(1);
        let mut agents = RemoteAgents {
            writer,
            next_request_id: 1,
            session_id: SessionId(1),
            lease_token: LeaseToken("test".into()),
            terminals: HashMap::from([(
                id,
                CachedTerminal {
                    snapshot: snapshot(""),
                    tracker: TerminalFrameTracker::default(),
                    scrollback: None,
                    scrollback_request: None,
                    scrollback_target: None,
                },
            )]),
            pending_resync: HashSet::new(),
        };

        agents.scroll(id, 3).unwrap();
        agents.scroll(id, 3).unwrap();

        let envelope: Envelope = read_frame(&mut peer).unwrap().unwrap();
        let Message::Request(Request::TerminalViewport { scrollback, .. }) = envelope.message
        else {
            panic!("expected viewport request");
        };
        assert_eq!(scrollback, 3);

        assert!(!agents.apply_viewport(TerminalViewport {
            agent_id: id,
            requested_scrollback: 3,
            scrollback: 3,
            snapshot: snapshot("stale"),
        }));
        assert!(agents.terminals[&id].scrollback.is_none());
        assert_eq!(agents.terminals[&id].scrollback_request, Some(6));
        assert_eq!(agents.terminals[&id].scrollback_target, Some(6));

        let envelope: Envelope = read_frame(&mut peer).unwrap().unwrap();
        let Message::Request(Request::TerminalViewport { scrollback, .. }) = envelope.message
        else {
            panic!("expected coalesced viewport request");
        };
        assert_eq!(scrollback, 6);

        assert!(agents.apply_viewport(TerminalViewport {
            agent_id: id,
            requested_scrollback: 6,
            scrollback: 5,
            snapshot: snapshot("latest"),
        }));
        assert_eq!(agents.terminals[&id].scrollback.as_ref().unwrap().offset, 5);
        assert_eq!(agents.terminals[&id].scrollback_request, None);
        assert_eq!(agents.terminals[&id].scrollback_target, Some(5));
    }
}
