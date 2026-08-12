use std::{
    collections::{HashMap, HashSet},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

use svarm_agent::vt100::{Parser, Screen};
use svarm_agent::{
    AgentId, AgentKind, CursorStyle, Result, TerminalPalette,
    framing::{read_frame, write_frame},
    protocol::{
        ConnectionRole, Envelope, Event, FrameDisposition, Hello, HostTerminalCapabilities,
        KeyInput, LeaseToken, Message, MouseInput, MouseProtocol, PROTOCOL_VERSION, ProtocolRange,
        Request, RequestId, Response, SessionId, StopSummary, SvarmSessionSnapshot,
        TerminalFrameTracker, TerminalModes, TerminalSequence, TerminalViewport,
    },
};

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(3);
/// Live frames carry only the visible grid. Historical viewports arrive separately on demand.
const SCROLLBACK_ROWS: usize = 0;

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
    TerminalChanged,
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
    EmbeddedToolChanged,
}

struct CachedTerminal {
    parser: Parser,
    tracker: TerminalFrameTracker,
    cursor_style: CursorStyle,
    modes: TerminalModes,
    scrollback: Option<ScrollbackView>,
    scrollback_request: Option<usize>,
}

struct ScrollbackView {
    parser: Parser,
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
    ) -> Result<(Self, SvarmSessionSnapshot)> {
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
        match read_frame::<_, Envelope>(&mut writer)? {
            Some(Envelope {
                message: Message::Welcome(_),
                ..
            }) => {}
            Some(Envelope {
                message: Message::Error(error),
                ..
            }) => return Err(error.actionable_message().into()),
            _ => return Err("Svarm server did not complete the protocol handshake".into()),
        }

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
                    updates.push(RemoteUpdate::TerminalChanged);
                }
                Message::Event(Event::TerminalDiff(frame)) => {
                    let id = frame.agent_id;
                    let sequence = frame.sequence;
                    let disposition = self.apply_diff(frame);
                    self.after_frame(id, sequence, disposition);
                    if disposition == FrameDisposition::Apply {
                        updates.push(RemoteUpdate::TerminalChanged);
                    }
                }
                Message::Event(Event::TerminalViewport(viewport)) => {
                    self.apply_viewport(viewport);
                    updates.push(RemoteUpdate::TerminalChanged);
                }
                Message::Event(event) => {
                    if let Event::AgentRemoved { agent_id, .. } = &event {
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
            .map(|terminal| terminal.cursor_style)
    }

    pub fn screen(&self, id: AgentId) -> Option<&Screen> {
        self.terminals.get(&id).map(|terminal| {
            terminal
                .scrollback
                .as_ref()
                .map_or_else(|| terminal.parser.screen(), |view| view.parser.screen())
        })
    }

    pub fn is_scrolled(&self, id: AgentId) -> bool {
        self.terminals
            .get(&id)
            .is_some_and(|terminal| terminal.scrollback.is_some())
    }

    pub fn wheel_routing(&self, id: AgentId) -> WheelRouting {
        let Some(terminal) = self.terminals.get(&id) else {
            return WheelRouting::ChildMouse;
        };
        if terminal.modes.mouse_protocol != MouseProtocol::None {
            WheelRouting::ChildMouse
        } else if terminal.parser.screen().alternate_screen()
            && terminal.modes.mouse_alternate_scroll
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
            .scrollback_request
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
        if let Some(terminal) = self.terminals.get_mut(&agent_id) {
            terminal.scrollback_request = Some(requested);
        }
        self.send(Request::TerminalViewport {
            lease_token: self.lease_token.clone(),
            agent_id,
            scrollback: requested,
        })
    }

    pub fn show_live(&mut self, agent_id: AgentId) -> bool {
        let Some(terminal) = self.terminals.get_mut(&agent_id) else {
            return false;
        };
        terminal.scrollback_request = None;
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
        let terminal = self
            .terminals
            .entry(frame.agent_id)
            .or_insert_with(|| CachedTerminal {
                parser: Parser::new(frame.rows, frame.cols, SCROLLBACK_ROWS),
                tracker: TerminalFrameTracker::default(),
                cursor_style: CursorStyle::default(),
                modes: TerminalModes::default(),
                scrollback: None,
                scrollback_request: None,
            });
        let disposition = terminal.tracker.accept_full(frame.sequence);
        if disposition == FrameDisposition::Apply {
            terminal.parser = Parser::new(frame.rows, frame.cols, SCROLLBACK_ROWS);
            terminal.parser.process(&frame.formatted_screen);
            terminal.cursor_style = frame.cursor_style;
            terminal.modes = frame.modes;
            terminal.scrollback = None;
            terminal.scrollback_request = None;
            self.pending_resync.remove(&frame.agent_id);
        }
        disposition
    }

    fn apply_diff(&mut self, frame: svarm_agent::protocol::TerminalDiff) -> FrameDisposition {
        let Some(terminal) = self.terminals.get_mut(&frame.agent_id) else {
            return FrameDisposition::Gap;
        };
        if terminal.parser.screen().size() != (frame.rows, frame.cols) {
            return FrameDisposition::Gap;
        }
        let disposition = terminal
            .tracker
            .accept_diff(frame.base_sequence, frame.sequence);
        if disposition == FrameDisposition::Apply {
            terminal.parser.process(&frame.formatted_changes);
            terminal.cursor_style = frame.cursor_style;
            terminal.modes = frame.modes;
        }
        disposition
    }

    fn apply_viewport(&mut self, viewport: TerminalViewport) {
        let Some(terminal) = self.terminals.get_mut(&viewport.agent_id) else {
            return;
        };
        if terminal.scrollback_request != Some(viewport.requested_scrollback) {
            return;
        }
        if viewport.scrollback == 0 {
            terminal.scrollback = None;
            terminal.scrollback_request = None;
            return;
        }
        let mut parser = Parser::new(viewport.rows, viewport.cols, 0);
        parser.process(&viewport.formatted_screen);
        terminal.scrollback = Some(ScrollbackView {
            parser,
            offset: viewport.scrollback,
        });
        terminal.scrollback_request = Some(viewport.scrollback);
    }

    fn after_frame(
        &mut self,
        agent_id: AgentId,
        sequence: TerminalSequence,
        disposition: FrameDisposition,
    ) {
        match disposition {
            FrameDisposition::Apply | FrameDisposition::Duplicate => {
                let _ = self.send(Request::AcknowledgeFrame {
                    lease_token: self.lease_token.clone(),
                    agent_id,
                    sequence,
                });
            }
            FrameDisposition::Gap => {
                if self.pending_resync.insert(agent_id) {
                    let last_sequence = self
                        .terminals
                        .get(&agent_id)
                        .and_then(|terminal| terminal.tracker.sequence());
                    let _ = self.send(Request::ResyncTerminal {
                        lease_token: self.lease_token.clone(),
                        agent_id,
                        last_sequence,
                    });
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
                    | ClientEvent::EmbeddedToolChanged,
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
    use super::*;

    #[test]
    fn historical_viewport_is_separate_from_the_live_parser() {
        let (writer, _peer) = UnixStream::pair().unwrap();
        let id = AgentId::new(1);
        let mut live = Parser::new(2, 12, 0);
        live.process(b"live");
        let mut agents = RemoteAgents {
            writer,
            next_request_id: 1,
            session_id: SessionId(1),
            lease_token: LeaseToken("test".into()),
            terminals: HashMap::from([(
                id,
                CachedTerminal {
                    parser: live,
                    tracker: TerminalFrameTracker::default(),
                    cursor_style: CursorStyle::default(),
                    modes: TerminalModes::default(),
                    scrollback: None,
                    scrollback_request: Some(4),
                },
            )]),
            pending_resync: HashSet::new(),
        };

        agents.apply_viewport(TerminalViewport {
            agent_id: id,
            rows: 2,
            cols: 12,
            requested_scrollback: 4,
            scrollback: 4,
            formatted_screen: b"older".to_vec(),
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
        let terminal = |parser, modes| CachedTerminal {
            parser,
            tracker: TerminalFrameTracker::default(),
            cursor_style: CursorStyle::default(),
            modes,
            scrollback: None,
            scrollback_request: None,
        };
        let mut agents = RemoteAgents {
            writer,
            next_request_id: 1,
            session_id: SessionId(1),
            lease_token: LeaseToken("test".into()),
            terminals: HashMap::new(),
            pending_resync: HashSet::new(),
        };

        agents.terminals.insert(
            id,
            terminal(Parser::new(2, 12, 0), TerminalModes::default()),
        );
        assert_eq!(agents.wheel_routing(id), WheelRouting::Scrollback);

        agents
            .terminals
            .get_mut(&id)
            .unwrap()
            .parser
            .process(b"\x1b[?1049h");
        assert_eq!(agents.wheel_routing(id), WheelRouting::Scrollback);

        agents
            .terminals
            .get_mut(&id)
            .unwrap()
            .modes
            .mouse_alternate_scroll = true;
        assert_eq!(agents.wheel_routing(id), WheelRouting::AlternateScreen);

        agents.terminals.get_mut(&id).unwrap().modes.mouse_protocol = MouseProtocol::PressRelease;
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
                    parser: Parser::new(2, 12, 0),
                    tracker: TerminalFrameTracker::default(),
                    cursor_style: CursorStyle::default(),
                    modes: TerminalModes::default(),
                    scrollback: None,
                    scrollback_request: None,
                },
            )]),
            pending_resync: HashSet::new(),
        };

        agents.scroll(id, 3).unwrap();
        agents.scroll(id, 3).unwrap();

        let offsets = [(); 2].map(|()| {
            let envelope: Envelope = read_frame(&mut peer).unwrap().unwrap();
            let Message::Request(Request::TerminalViewport { scrollback, .. }) = envelope.message
            else {
                panic!("expected viewport request");
            };
            scrollback
        });
        assert_eq!(offsets, [3, 6]);

        agents.apply_viewport(TerminalViewport {
            agent_id: id,
            rows: 2,
            cols: 12,
            requested_scrollback: 3,
            scrollback: 3,
            formatted_screen: b"stale".to_vec(),
        });
        assert!(agents.terminals[&id].scrollback.is_none());
        assert_eq!(agents.terminals[&id].scrollback_request, Some(6));

        agents.apply_viewport(TerminalViewport {
            agent_id: id,
            rows: 2,
            cols: 12,
            requested_scrollback: 6,
            scrollback: 5,
            formatted_screen: b"latest".to_vec(),
        });
        assert_eq!(agents.terminals[&id].scrollback.as_ref().unwrap().offset, 5);
        assert_eq!(agents.terminals[&id].scrollback_request, Some(5));
    }
}
