use std::{
    collections::{HashMap, HashSet},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use svarm_agent::{
    AgentId, AgentKind, Result, TerminalPalette,
    framing::{read_frame, write_frame},
    protocol::{
        ConnectionRole, Envelope, Event, FrameDisposition, Hello, HostTerminalCapabilities,
        KeyInput, LeaseToken, Message, MouseInput, PROTOCOL_VERSION, ProtocolRange, Request,
        RequestId, Response, SessionId, SvarmSessionSnapshot, TerminalFrameTracker,
        TerminalSequence,
    },
};
use tui_term::vt100::{Parser, Screen};

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(3);
const INCOMING_QUEUE: usize = 1_024;
const SCROLLBACK_ROWS: usize = 10_000;

#[derive(Clone, Debug)]
pub enum InitialSession {
    Create(PathBuf),
    Attach {
        session_id: SessionId,
        takeover: bool,
    },
}

pub(crate) enum RemoteUpdate {
    Event(Event),
    TerminalChanged,
    Error(String),
    Disconnected(String),
}

enum Incoming {
    Envelope(Envelope),
    Disconnected(String),
}

struct CachedTerminal {
    parser: Parser,
    tracker: TerminalFrameTracker,
}

pub(crate) struct RemoteAgents {
    writer: UnixStream,
    incoming: Receiver<Incoming>,
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
            InitialSession::Create(canonical_path) => Request::CreateSession {
                canonical_path,
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
        let (incoming_tx, incoming) = mpsc::sync_channel(INCOMING_QUEUE);
        thread::spawn(move || {
            loop {
                match read_frame(&mut reader) {
                    Ok(Some(envelope)) => {
                        if incoming_tx.send(Incoming::Envelope(envelope)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = incoming_tx.send(Incoming::Disconnected(
                            "Svarm server closed the connection".into(),
                        ));
                        break;
                    }
                    Err(error) => {
                        let _ = incoming_tx.send(Incoming::Disconnected(error.to_string()));
                        break;
                    }
                }
            }
        });
        Ok((
            Self {
                writer,
                incoming,
                next_request_id: 3,
                session_id,
                lease_token,
                terminals: HashMap::new(),
                pending_resync: HashSet::new(),
            },
            snapshot,
        ))
    }

    pub fn drain(&mut self) -> Vec<RemoteUpdate> {
        let incoming = self.incoming.try_iter().collect::<Vec<_>>();
        let mut updates = Vec::new();
        for incoming in incoming {
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
                    Message::Event(event) => {
                        if let Event::AgentRemoved { agent_id, .. } = &event {
                            self.terminals.remove(agent_id);
                            self.pending_resync.remove(agent_id);
                        }
                        updates.push(RemoteUpdate::Event(event));
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
        }
        updates
    }

    pub fn screen(&self, id: AgentId) -> Option<&Screen> {
        self.terminals
            .get(&id)
            .map(|terminal| terminal.parser.screen())
    }

    pub fn spawn(&mut self, kind: AgentKind) -> Result<()> {
        self.send(Request::SpawnAgent {
            lease_token: self.lease_token.clone(),
            kind,
        })
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

    pub fn detach(&mut self) -> Result<()> {
        self.send_and_wait(Request::DetachSession {
            lease_token: self.lease_token.clone(),
        })
    }

    pub fn stop(&mut self) -> Result<()> {
        self.send_and_wait(Request::StopAttachedSession {
            lease_token: self.lease_token.clone(),
        })
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
            });
        let disposition = terminal.tracker.accept_full(frame.sequence);
        if disposition == FrameDisposition::Apply {
            terminal.parser = Parser::new(frame.rows, frame.cols, SCROLLBACK_ROWS);
            terminal.parser.process(&frame.formatted_screen);
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
        }
        disposition
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

    fn send_and_wait(&mut self, request: Request) -> Result<()> {
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
        loop {
            match self.incoming.recv_timeout(CONNECTION_TIMEOUT) {
                Ok(Incoming::Envelope(envelope)) if envelope.request_id == Some(request_id) => {
                    return match envelope.message {
                        Message::Response(_) => Ok(()),
                        Message::Error(error) => Err(error.actionable_message().into()),
                        _ => Err("Svarm server returned an invalid response".into()),
                    };
                }
                Ok(Incoming::Envelope(_)) => {}
                Ok(Incoming::Disconnected(error)) => return Err(error.into()),
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
