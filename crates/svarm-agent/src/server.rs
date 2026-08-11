use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io,
    net::Shutdown,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use vt100::{MouseProtocolEncoding, MouseProtocolMode, Screen};

use crate::{
    AgentId, AgentKind, AgentManager, Result as AgentResult, SessionSnapshot, SessionStatus,
    framing::{read_frame, write_frame},
    input::{encode_key, encode_mouse, encode_paste},
    ipc::unix::UnixListenerGuard,
    protocol::{
        AgentSnapshot, ConnectionId, ConnectionRole, Envelope, ErrorCode, Event, LeaseToken,
        Message, MouseEncoding, MouseProtocol, ProtocolError, ProtocolRange, Request, RequestId,
        Response, ServerCapabilities, ServerInstanceId, ServerStatusSnapshot, SessionId,
        SessionSummary, StopSummary, SvarmSessionSnapshot, TerminalDiff, TerminalFull,
        TerminalModes, TerminalSequence, Welcome,
    },
    pty_size,
    server_session::{ServerSessionState, sort_session_summaries},
};

const EVENT_TICK: Duration = Duration::from_millis(16);
const EMPTY_SERVER_GRACE: Duration = Duration::from_secs(2);
const CONNECTION_QUEUE: usize = 64;
const INPUT_QUEUE: usize = 1_024;
const CONNECTION_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

pub struct ServerConfig {
    pub socket_path: PathBuf,
    pub application_version: String,
    handle_signals: bool,
    #[cfg(test)]
    test_agent_command: Option<(String, Vec<String>)>,
}

impl ServerConfig {
    pub fn new(socket_path: PathBuf, application_version: impl Into<String>) -> Self {
        Self {
            socket_path,
            application_version: application_version.into(),
            handle_signals: false,
            #[cfg(test)]
            test_agent_command: None,
        }
    }

    pub const fn with_signal_handling(mut self) -> Self {
        self.handle_signals = true;
        self
    }

    #[cfg(test)]
    fn with_test_agent_command(mut self, program: &str, args: &[&str]) -> Self {
        self.test_agent_command = Some((
            program.into(),
            args.iter().map(|argument| (*argument).to_owned()).collect(),
        ));
        self
    }
}

pub fn run_foreground(config: ServerConfig) -> AgentResult<()> {
    run_foreground_ready(config, || Ok(()))
}

pub fn run_foreground_ready(
    config: ServerConfig,
    ready: impl FnOnce() -> AgentResult<()>,
) -> AgentResult<()> {
    let _signals = config
        .handle_signals
        .then(SignalGuard::install)
        .transpose()?;
    let server = Server::new(config)?;
    ready()?;
    server.run()
}

struct SignalGuard {
    interrupt: libc::sighandler_t,
    terminate: libc::sighandler_t,
    hangup: libc::sighandler_t,
}

impl SignalGuard {
    fn install() -> io::Result<Self> {
        SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
        // SAFETY: signal handlers only update an atomic flag or use SIG_IGN.
        let interrupt = unsafe {
            libc::signal(
                libc::SIGINT,
                request_shutdown as *const () as libc::sighandler_t,
            )
        };
        if interrupt == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: see above.
        let terminate = unsafe {
            libc::signal(
                libc::SIGTERM,
                request_shutdown as *const () as libc::sighandler_t,
            )
        };
        if terminate == libc::SIG_ERR {
            // SAFETY: restoring the handler returned by signal is valid.
            unsafe { libc::signal(libc::SIGINT, interrupt) };
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ignoring SIGHUP prevents a launching terminal from owning server lifetime.
        let hangup = unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
        if hangup == libc::SIG_ERR {
            // SAFETY: restoring handlers returned by signal is valid.
            unsafe {
                libc::signal(libc::SIGINT, interrupt);
                libc::signal(libc::SIGTERM, terminate);
            }
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            interrupt,
            terminate,
            hangup,
        })
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        // SAFETY: restoring handlers returned by signal is valid.
        unsafe {
            libc::signal(libc::SIGINT, self.interrupt);
            libc::signal(libc::SIGTERM, self.terminate);
            libc::signal(libc::SIGHUP, self.hangup);
        }
    }
}

extern "C" fn request_shutdown(_signal: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

struct Connection {
    outgoing: Arc<OutgoingQueue>,
    stream: UnixStream,
    role: Option<ConnectionRole>,
    protocol_version: Option<u16>,
    process_id: Option<u32>,
    attached_session: Option<SessionId>,
    writer: Option<thread::JoinHandle<()>>,
}

struct OutgoingQueue {
    state: Mutex<OutgoingState>,
    ready: Condvar,
}

struct OutgoingState {
    frames: VecDeque<Box<Envelope>>,
    closed: bool,
}

enum QueueResult {
    Queued,
    NeedsFull(AgentId),
    Full,
}

impl OutgoingQueue {
    fn new() -> Self {
        Self {
            state: Mutex::new(OutgoingState {
                frames: VecDeque::with_capacity(CONNECTION_QUEUE),
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }

    fn push(&self, envelope: Box<Envelope>) -> QueueResult {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.closed {
            return QueueResult::Full;
        }
        if state.frames.len() == CONNECTION_QUEUE {
            let Some((agent_id, full)) = terminal_frame(envelope.as_ref()) else {
                return QueueResult::Full;
            };
            state.frames.retain(|queued| {
                terminal_frame(queued.as_ref()).is_none_or(|(queued_id, _)| queued_id != agent_id)
            });
            if state.frames.len() == CONNECTION_QUEUE {
                return QueueResult::Full;
            }
            if !full {
                return QueueResult::NeedsFull(agent_id);
            }
        }
        state.frames.push_back(envelope);
        self.ready.notify_one();
        QueueResult::Queued
    }

    fn pop(&self) -> Option<Box<Envelope>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        loop {
            if let Some(envelope) = state.frames.pop_front() {
                return Some(envelope);
            }
            if state.closed {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }

    fn close(&self) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .closed = true;
        self.ready.notify_all();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .frames
            .len()
    }
}

enum ServerInput {
    Frame(ConnectionId, Box<Envelope>),
    Disconnected(ConnectionId),
    /// An agent produced output. Carries no payload: the loop polls every session anyway, and a
    /// single pending wake stands in for any number of reads (see [`OutputWake`]).
    AgentOutput,
}

/// Collapses the reader threads' per-read notifications into at most one queued wake-up, so a
/// noisy agent cannot flood the input queue and delay real client requests.
struct OutputWake {
    input: SyncSender<ServerInput>,
    pending: AtomicBool,
}

impl OutputWake {
    fn notifier(self: &Arc<Self>) -> crate::session::OutputNotifier {
        let wake = Arc::clone(self);
        Arc::new(move |_| wake.raise())
    }

    fn raise(&self) {
        if !self.pending.swap(true, Ordering::AcqRel)
            && self.input.try_send(ServerInput::AgentOutput).is_err()
        {
            self.pending.store(false, Ordering::Release);
        }
    }

    fn clear(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

struct SessionRuntime {
    state: ServerSessionState,
    agents: AgentManager,
    previous: HashMap<AgentId, SessionSnapshot>,
    frame_bases: HashMap<AgentId, FrameBasis>,
}

struct FrameBasis {
    sequence: TerminalSequence,
    acknowledged: Option<TerminalSequence>,
    screen: Screen,
}

enum FramePayload {
    Full(Vec<u8>),
    Diff {
        base_sequence: TerminalSequence,
        bytes: Vec<u8>,
    },
}

impl SessionRuntime {
    fn new(state: ServerSessionState, wake: Option<crate::session::OutputNotifier>) -> Self {
        let (rows, cols) = state.dimensions();
        let agents = AgentManager::new(pty_size(rows, cols), state.terminal_palette(), wake);
        Self {
            state,
            agents,
            previous: HashMap::new(),
            frame_bases: HashMap::new(),
        }
    }

    fn spawn(
        &mut self,
        kind: AgentKind,
        launch_directory: &Path,
        now_ms: u64,
        _config: &ServerConfig,
    ) -> AgentResult<SessionSnapshot> {
        #[cfg(test)]
        let snapshot = if let Some((program, args)) = &_config.test_agent_command {
            self.agents
                .spawn_test_command(kind, launch_directory, program, args)?
        } else {
            self.agents.spawn(kind, launch_directory)?
        };
        #[cfg(not(test))]
        let snapshot = self.agents.spawn(kind, launch_directory)?;

        self.state
            .register_agent(snapshot.id, snapshot.output_generation, now_ms);
        self.previous.insert(snapshot.id, snapshot.clone());
        Ok(snapshot)
    }

    fn close(&mut self, id: AgentId, now_ms: u64) -> AgentResult<()> {
        self.agents.close(id)?;
        self.previous.remove(&id);
        self.frame_bases.remove(&id);
        self.state.remove_agent(id, self.agents.agent_ids(), now_ms);
        Ok(())
    }

    fn send_input(&mut self, id: AgentId, bytes: &[u8], now_ms: u64) -> Result<(), ProtocolError> {
        let snapshot = self.agents.snapshot(id).ok_or_else(agent_not_found)?;
        if snapshot.status == SessionStatus::Exited {
            return Err(ProtocolError::new(
                ErrorCode::AgentExited,
                "agent has exited",
            ));
        }
        if !bytes.is_empty() {
            self.agents.send(id, bytes).map_err(internal_error)?;
        }
        self.state.record_activity(now_ms);
        Ok(())
    }

    fn summary(&self) -> SessionSummary {
        self.state
            .summary(self.agents.running_count(), self.agents.len())
    }

    fn snapshot(&self) -> SvarmSessionSnapshot {
        let agents = self
            .agents
            .snapshots()
            .into_iter()
            .map(|snapshot| self.agent_snapshot(snapshot))
            .collect();
        let (rows, cols) = self.state.dimensions();
        SvarmSessionSnapshot {
            summary: self.summary(),
            selected_agent_id: self.state.selected_agent_id(),
            rows,
            cols,
            agents,
        }
    }

    fn agent_snapshot(&self, snapshot: SessionSnapshot) -> AgentSnapshot {
        AgentSnapshot {
            id: snapshot.id,
            kind: snapshot.kind,
            launch_directory: snapshot.launch_directory,
            status: snapshot.status,
            exit: snapshot.exit,
            output_generation: snapshot.output_generation,
            seen_generation: self.state.seen_generation(snapshot.id).unwrap_or(0),
            terminal_sequence: self
                .state
                .terminal_sequence(snapshot.id)
                .unwrap_or(crate::protocol::TerminalSequence(0)),
            read_error: snapshot.read_error,
            recognition: None,
        }
    }

    /// The modes the agent's input has to be encoded against. Read in place: this runs on every
    /// keystroke, and copying the screen for it would put the copy in the input path.
    fn input_modes(&self, id: AgentId) -> Option<TerminalModes> {
        let disambiguate = self.agents.keyboard_disambiguates(id);
        self.agents.with_screen(id, |screen| TerminalModes {
            keyboard_disambiguate: disambiguate,
            ..terminal_modes(screen)
        })
    }

    fn terminal_event(&mut self, id: AgentId, force_full: bool) -> Option<Event> {
        let snapshot = self.agents.snapshot(id)?;
        let previous = self.frame_bases.get(&id).filter(|_| !force_full);
        // Serialize against the basis and take the next basis in a single visit to the live
        // screen, so emitting a frame costs one copy rather than a snapshot plus a basis copy.
        let (payload, screen) = self.agents.with_screen(id, |screen| {
            let previous = previous.filter(|basis| basis.screen.size() == screen.size());
            let payload = match previous {
                Some(basis) => FramePayload::Diff {
                    base_sequence: basis.sequence,
                    bytes: screen.state_diff(&basis.screen),
                },
                None => FramePayload::Full(screen.state_formatted()),
            };
            (payload, screen.clone())
        })?;

        let (rows, cols) = screen.size();
        let modes = terminal_modes(&screen);
        let cursor_style = self.agents.cursor_style(id).unwrap_or_default();
        let sequence = self.state.next_terminal_sequence(id).ok()?;
        let event = match payload {
            FramePayload::Full(formatted_screen) => Event::TerminalFull(TerminalFull {
                agent_id: id,
                rows,
                cols,
                output_generation: snapshot.output_generation,
                sequence,
                formatted_screen,
                modes,
                cursor_style,
            }),
            FramePayload::Diff {
                base_sequence,
                bytes,
            } => Event::TerminalDiff(TerminalDiff {
                agent_id: id,
                rows,
                cols,
                output_generation: snapshot.output_generation,
                base_sequence,
                sequence,
                formatted_changes: bytes,
                modes,
                cursor_style,
            }),
        };
        let acknowledged = self
            .frame_bases
            .get(&id)
            .and_then(|basis| basis.acknowledged);
        self.frame_bases.insert(
            id,
            FrameBasis {
                sequence,
                acknowledged,
                screen,
            },
        );
        Some(event)
    }

    fn full_terminal(&mut self, id: AgentId) -> Option<Event> {
        self.terminal_event(id, true)
    }

    fn acknowledge(
        &mut self,
        id: AgentId,
        sequence: TerminalSequence,
    ) -> Result<(), ProtocolError> {
        let Some(basis) = self.frame_bases.get_mut(&id) else {
            return Err(agent_not_found());
        };
        if sequence > basis.sequence {
            return Err(ProtocolError::new(
                ErrorCode::InvalidRequest,
                "terminal acknowledgement is newer than the last server frame",
            ));
        }
        if basis
            .acknowledged
            .is_none_or(|acknowledged| sequence > acknowledged)
        {
            basis.acknowledged = Some(sequence);
        }
        Ok(())
    }

    fn synchronization_events(&mut self) -> Vec<Event> {
        self.frame_bases.clear();
        let ids = self.agents.agent_ids().to_vec();
        let mut terminals = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(event) = self.terminal_event(id, false) {
                terminals.push(event);
            }
        }
        let mut events = vec![Event::SvarmSessionSnapshot(self.snapshot())];
        events.extend(terminals);
        events
    }

    fn poll_events(&mut self) -> Vec<Event> {
        let dirty = self.agents.drain_dirty();
        let attached = self.state.attachment().is_some();
        let mut changed = Vec::new();
        let mut terminal_ids = Vec::new();
        for result in self.agents.poll() {
            let Ok(snapshot) = result else {
                continue;
            };
            let previous = self.previous.insert(snapshot.id, snapshot.clone());
            if previous.as_ref() != Some(&snapshot) {
                changed.push(snapshot.clone());
            }
            if attached
                && (dirty.contains(&snapshot.id)
                    || previous
                        .as_ref()
                        .is_some_and(|old| old.output_generation != snapshot.output_generation))
            {
                terminal_ids.push(snapshot.id);
            }
        }
        if !changed.is_empty() {
            self.state.metadata_changed();
        }
        let revision = self.state.revision();
        let mut events = changed
            .into_iter()
            .map(|snapshot| Event::AgentChanged {
                revision,
                agent: self.agent_snapshot(snapshot),
            })
            .collect::<Vec<_>>();
        for id in terminal_ids {
            if let Some(event) = self.terminal_event(id, false) {
                events.push(event);
            }
        }
        events
    }
}

struct Outcome {
    response: Response,
    events: Vec<(ConnectionId, Event)>,
    disconnect: Vec<ConnectionId>,
}

impl Outcome {
    fn new(response: Response) -> Self {
        Self {
            response,
            events: Vec::new(),
            disconnect: Vec::new(),
        }
    }
}

struct Server {
    config: ServerConfig,
    listener: UnixListenerGuard,
    started: Instant,
    started_ms: u64,
    instance_id: ServerInstanceId,
    connections: BTreeMap<ConnectionId, Connection>,
    sessions: BTreeMap<SessionId, SessionRuntime>,
    next_connection_id: u64,
    next_session_id: u64,
    next_token: u64,
    input_tx: SyncSender<ServerInput>,
    input_rx: Receiver<ServerInput>,
    output_wake: Arc<OutputWake>,
    stopping: bool,
    empty_since: Option<Instant>,
}

impl Server {
    fn new(config: ServerConfig) -> AgentResult<Self> {
        let listener = UnixListenerGuard::bind(&config.socket_path)?;
        let started_ms = unix_time_ms();
        let (input_tx, input_rx) = mpsc::sync_channel(INPUT_QUEUE);
        let output_wake = Arc::new(OutputWake {
            input: input_tx.clone(),
            pending: AtomicBool::new(false),
        });
        Ok(Self {
            config,
            listener,
            started: Instant::now(),
            started_ms,
            instance_id: ServerInstanceId(format!("{:x}-{:x}", std::process::id(), started_ms)),
            connections: BTreeMap::new(),
            sessions: BTreeMap::new(),
            next_connection_id: 1,
            next_session_id: 1,
            next_token: 1,
            input_tx,
            input_rx,
            output_wake,
            stopping: false,
            empty_since: Some(Instant::now()),
        })
    }

    fn run(mut self) -> AgentResult<()> {
        while !self.stopping {
            if self.config.handle_signals && SHUTDOWN_REQUESTED.swap(false, Ordering::SeqCst) {
                self.begin_shutdown();
                continue;
            }
            self.accept_connections()?;
            // Agent output raises a wake, so this blocks only while nothing is happening; the
            // tick just paces connection accounting and the idle-shutdown check.
            match self.input_rx.recv_timeout(EVENT_TICK) {
                Ok(input) => self.handle_input(input),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(input) = self.input_rx.try_recv() {
                self.handle_input(input);
            }
            self.output_wake.clear();
            self.poll_sessions();
            self.update_lifetime();
        }
        self.finish_shutdown();
        Ok(())
    }

    fn begin_shutdown(&mut self) {
        let connections = self.connections.keys().copied().collect::<Vec<_>>();
        for connection_id in connections {
            self.send_event(connection_id, Event::ServerStopping);
        }
        self.stopping = true;
    }

    fn accept_connections(&mut self) -> AgentResult<()> {
        while let Some(stream) = self.listener.accept()? {
            let id = ConnectionId(self.next_connection_id);
            self.next_connection_id = self
                .next_connection_id
                .checked_add(1)
                .ok_or("connection identifier space exhausted")?;
            let connection = start_connection(id, stream, self.input_tx.clone())?;
            self.connections.insert(id, connection);
            self.empty_since = None;
        }
        Ok(())
    }

    fn handle_input(&mut self, input: ServerInput) {
        match input {
            ServerInput::Frame(id, envelope) => self.handle_envelope(id, *envelope),
            ServerInput::Disconnected(id) => self.disconnect(id),
            // Nothing to do: the wake exists only to break out of `recv_timeout` so that the
            // `poll_sessions` call below runs now instead of up to a tick later.
            ServerInput::AgentOutput => {}
        }
    }

    fn handle_envelope(&mut self, id: ConnectionId, envelope: Envelope) {
        let Some(connection) = self.connections.get(&id) else {
            return;
        };
        if connection.role.is_none() {
            self.handle_hello(id, envelope);
            return;
        }
        let protocol_version = connection.protocol_version.unwrap_or(0);
        if envelope.protocol_version != protocol_version {
            self.send_error(
                id,
                envelope.request_id,
                ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    "message protocol version differs from the negotiated version",
                ),
            );
            return;
        }
        let Some(request_id) = envelope.request_id else {
            self.send_error(
                id,
                None,
                ProtocolError::new(ErrorCode::InvalidRequest, "requests require a request ID"),
            );
            return;
        };
        let Message::Request(request) = envelope.message else {
            self.send_error(
                id,
                Some(request_id),
                ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    "only request messages are accepted after the handshake",
                ),
            );
            return;
        };
        match self.apply_request(id, request) {
            Ok(outcome) => {
                self.send_response(id, request_id, outcome.response);
                for (target, event) in outcome.events {
                    self.send_event(target, event);
                }
                for target in outcome.disconnect {
                    self.disconnect_after_flush(target);
                }
            }
            Err(error) => self.send_error(id, Some(request_id), error),
        }
    }

    fn handle_hello(&mut self, id: ConnectionId, envelope: Envelope) {
        let request_id = envelope.request_id;
        let Message::Hello(hello) = envelope.message else {
            self.send_error(
                id,
                request_id,
                ProtocolError::new(ErrorCode::InvalidRequest, "Hello must be the first message"),
            );
            self.disconnect_after_flush(id);
            return;
        };
        let Some(version) = hello.protocol.negotiate(ProtocolRange::CURRENT) else {
            self.send_error(
                id,
                request_id,
                ProtocolError::incompatible(hello.protocol, ProtocolRange::CURRENT),
            );
            self.disconnect_after_flush(id);
            return;
        };
        if let Some(connection) = self.connections.get_mut(&id) {
            connection.role = Some(hello.role);
            connection.protocol_version = Some(version);
            connection.process_id = hello.process_id;
        }
        self.send(
            id,
            Envelope {
                protocol_version: version,
                request_id,
                message: Message::Welcome(Welcome {
                    application_version: self.config.application_version.clone(),
                    protocol_version: version,
                    process_id: std::process::id(),
                    instance_id: self.instance_id.clone(),
                    capabilities: ServerCapabilities {
                        takeover: true,
                        terminal_diffs: true,
                    },
                    connection_id: id,
                }),
            },
        );
    }

    fn apply_request(
        &mut self,
        id: ConnectionId,
        request: Request,
    ) -> Result<Outcome, ProtocolError> {
        self.check_role(id, &request)?;
        match request {
            Request::CreateSession {
                rows,
                cols,
                palette,
            } => self.create_session(id, rows, cols, palette),
            Request::AttachSession {
                session_id,
                rows,
                cols,
                palette,
                takeover,
            } => self.attach_session(id, session_id, rows, cols, palette, takeover),
            Request::DetachSession { lease_token } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let now = self.now_ms();
                let remove = {
                    let runtime = self
                        .sessions
                        .get_mut(&session_id)
                        .expect("attached session exists");
                    runtime.state.detach(&lease_token, now)?;
                    runtime.frame_bases.clear();
                    runtime.agents.is_empty()
                };
                if remove {
                    self.sessions.remove(&session_id);
                }
                self.connections.get_mut(&id).unwrap().attached_session = None;
                Ok(Outcome::new(Response::Ok))
            }
            Request::SpawnAgent {
                lease_token,
                kind,
                launch_directory,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let launch_directory = canonicalize_agent_directory(&launch_directory)?;
                let now = self.now_ms();
                let runtime = self.sessions.get_mut(&session_id).unwrap();
                let snapshot = runtime
                    .spawn(kind, &launch_directory, now, &self.config)
                    .map_err(internal_error)?;
                let mut outcome = Outcome::new(Response::Ok);
                outcome.events.push((
                    id,
                    Event::AgentAdded {
                        revision: runtime.state.revision(),
                        agent: runtime.agent_snapshot(snapshot.clone()),
                    },
                ));
                if let Some(event) = runtime.full_terminal(snapshot.id) {
                    outcome.events.push((id, event));
                }
                Ok(outcome)
            }
            Request::CloseAgent {
                lease_token,
                agent_id,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let now = self.now_ms();
                let runtime = self.sessions.get_mut(&session_id).unwrap();
                if runtime.agents.snapshot(agent_id).is_none() {
                    return Err(agent_not_found());
                }
                runtime.close(agent_id, now).map_err(internal_error)?;
                let mut outcome = Outcome::new(Response::Ok);
                outcome.events.push((
                    id,
                    Event::AgentRemoved {
                        revision: runtime.state.revision(),
                        agent_id,
                    },
                ));
                Ok(outcome)
            }
            Request::StopAttachedSession { lease_token } => {
                let session_id = self.attached_session(id, &lease_token)?;
                self.stop_session_runtime(session_id, Some(id))
            }
            Request::InputBytes {
                lease_token,
                agent_id,
                bytes,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let now = self.now_ms();
                let runtime = self.sessions.get_mut(&session_id).unwrap();
                runtime.send_input(agent_id, &bytes, now)?;
                Ok(Outcome::new(Response::Ok))
            }
            Request::ResizeSession {
                lease_token,
                rows,
                cols,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let now = self.now_ms();
                let runtime = self.sessions.get_mut(&session_id).unwrap();
                runtime.state.resize(rows, cols, now)?;
                runtime.agents.resize(rows, cols).map_err(internal_error)?;
                runtime.frame_bases.clear();
                let events = runtime
                    .agents
                    .agent_ids()
                    .to_vec()
                    .into_iter()
                    .filter_map(|agent_id| runtime.full_terminal(agent_id))
                    .map(|event| (id, event))
                    .collect();
                Ok(Outcome {
                    response: Response::Ok,
                    events,
                    disconnect: Vec::new(),
                })
            }
            Request::SelectAgent {
                lease_token,
                agent_id,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let now = self.now_ms();
                self.sessions
                    .get_mut(&session_id)
                    .unwrap()
                    .state
                    .select_agent(agent_id, now)?;
                Ok(Outcome::new(Response::Ok))
            }
            Request::MarkSeen {
                lease_token,
                agent_id,
                generation,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let now = self.now_ms();
                let runtime = self.sessions.get_mut(&session_id).unwrap();
                let current = runtime
                    .agents
                    .snapshot(agent_id)
                    .ok_or_else(agent_not_found)?
                    .output_generation;
                runtime
                    .state
                    .mark_seen(agent_id, generation, current, now)?;
                Ok(Outcome::new(Response::Ok))
            }
            Request::ResyncTerminal {
                lease_token,
                agent_id,
                ..
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let event = self
                    .sessions
                    .get_mut(&session_id)
                    .unwrap()
                    .full_terminal(agent_id)
                    .ok_or_else(agent_not_found)?;
                let mut outcome = Outcome::new(Response::Ok);
                outcome.events.push((id, event));
                Ok(outcome)
            }
            Request::AcknowledgeFrame {
                lease_token,
                agent_id,
                sequence,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                self.sessions
                    .get_mut(&session_id)
                    .unwrap()
                    .acknowledge(agent_id, sequence)?;
                Ok(Outcome::new(Response::Ok))
            }
            Request::ServerStatus => Ok(Outcome::new(Response::ServerStatus(self.status()))),
            Request::ListSessions => {
                let mut sessions = self
                    .sessions
                    .values()
                    .map(SessionRuntime::summary)
                    .collect::<Vec<_>>();
                sort_session_summaries(&mut sessions);
                Ok(Outcome::new(Response::Sessions { sessions }))
            }
            Request::GetSession { session_id } => {
                let runtime = self.sessions.get(&session_id).ok_or_else(|| {
                    ProtocolError::new(ErrorCode::SessionNotFound, "Svarm session was not found")
                })?;
                Ok(Outcome::new(Response::Session {
                    session: Some(runtime.snapshot()),
                }))
            }
            Request::StopSession {
                session_id,
                confirmed,
            } => {
                require_confirmation(confirmed)?;
                self.stop_session_runtime(session_id, Some(id))
            }
            Request::StopServer { confirmed } => {
                require_confirmation(confirmed)?;
                let session_count = self.sessions.len();
                let agent_count = self
                    .sessions
                    .values()
                    .map(|runtime| runtime.agents.len())
                    .sum();
                let cleanup_errors = self
                    .sessions
                    .values_mut()
                    .map(|runtime| {
                        runtime.state.stop();
                        runtime.agents.stop_all().len()
                    })
                    .sum();
                self.sessions.clear();
                let mut outcome = Outcome::new(Response::Stopped(StopSummary {
                    session_count,
                    agent_count,
                    cleanup_errors,
                    server_stopped: true,
                }));
                for connection_id in self.connections.keys().copied() {
                    if connection_id != id {
                        outcome.events.push((connection_id, Event::ServerStopping));
                    }
                }
                self.stopping = true;
                Ok(outcome)
            }
            Request::Key {
                lease_token,
                agent_id,
                event,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let now = self.now_ms();
                let runtime = self.sessions.get_mut(&session_id).unwrap();
                let modes = runtime.input_modes(agent_id).ok_or_else(agent_not_found)?;
                let bytes = encode_key(&event, modes).unwrap_or_default();
                runtime.send_input(agent_id, &bytes, now)?;
                Ok(Outcome::new(Response::Ok))
            }
            Request::Paste {
                lease_token,
                agent_id,
                text,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let now = self.now_ms();
                let runtime = self.sessions.get_mut(&session_id).unwrap();
                let modes = runtime.input_modes(agent_id).ok_or_else(agent_not_found)?;
                let bytes = encode_paste(&text, modes);
                runtime.send_input(agent_id, &bytes, now)?;
                Ok(Outcome::new(Response::Ok))
            }
            Request::Mouse {
                lease_token,
                agent_id,
                event,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let now = self.now_ms();
                let runtime = self.sessions.get_mut(&session_id).unwrap();
                let modes = runtime.input_modes(agent_id).ok_or_else(agent_not_found)?;
                let bytes = encode_mouse(&event, modes).unwrap_or_default();
                runtime.send_input(agent_id, &bytes, now)?;
                Ok(Outcome::new(Response::Ok))
            }
        }
    }

    fn create_session(
        &mut self,
        id: ConnectionId,
        rows: u16,
        cols: u16,
        palette: Option<crate::TerminalPalette>,
    ) -> Result<Outcome, ProtocolError> {
        if self.connections[&id].attached_session.is_some() {
            return Err(ProtocolError::new(
                ErrorCode::InvalidRequest,
                "connection is already attached to a Svarm session",
            ));
        }
        let session_id = SessionId(self.next_session_id);
        self.next_session_id = self
            .next_session_id
            .checked_add(1)
            .ok_or_else(|| ProtocolError::new(ErrorCode::InternalError, "session IDs exhausted"))?;
        let now = self.now_ms();
        let mut runtime = SessionRuntime::new(
            ServerSessionState::new(session_id, rows, cols, palette, now)?,
            Some(self.output_wake.notifier()),
        );
        let token = self.lease_token();
        runtime.state.attach(
            id,
            self.connections[&id].process_id,
            token.clone(),
            false,
            now,
        )?;
        let events = runtime
            .synchronization_events()
            .into_iter()
            .map(|event| (id, event))
            .collect();
        self.sessions.insert(session_id, runtime);
        self.connections.get_mut(&id).unwrap().attached_session = Some(session_id);
        Ok(Outcome {
            response: Response::Created {
                session_id,
                lease_token: token,
            },
            events,
            disconnect: Vec::new(),
        })
    }

    fn attach_session(
        &mut self,
        id: ConnectionId,
        session_id: SessionId,
        rows: u16,
        cols: u16,
        palette: Option<crate::TerminalPalette>,
        takeover: bool,
    ) -> Result<Outcome, ProtocolError> {
        if self.connections[&id].attached_session.is_some() {
            return Err(ProtocolError::new(
                ErrorCode::InvalidRequest,
                "connection is already attached to a Svarm session",
            ));
        }
        let now = self.now_ms();
        let process_id = self.connections[&id].process_id;
        {
            let runtime = self.sessions.get_mut(&session_id).ok_or_else(|| {
                ProtocolError::new(ErrorCode::SessionNotFound, "Svarm session was not found")
            })?;
            runtime.state.validate_attach(takeover, now)?;
            ServerSessionState::validate_resize(rows, cols)?;
            runtime.agents.resize(rows, cols).map_err(internal_error)?;
            runtime.agents.set_terminal_palette(palette);
        }
        let token = self.lease_token();
        let runtime = self
            .sessions
            .get_mut(&session_id)
            .expect("session validated before attachment commit");
        let attach = runtime
            .state
            .attach(id, process_id, token.clone(), takeover, now)?;
        runtime
            .state
            .resize(rows, cols, now)
            .expect("dimensions validated before attachment commit");
        runtime.state.set_terminal_palette(palette, now);
        let events = runtime
            .synchronization_events()
            .into_iter()
            .map(|event| (id, event))
            .collect::<Vec<_>>();
        self.connections.get_mut(&id).unwrap().attached_session = Some(session_id);
        let mut outcome = Outcome {
            response: Response::Attached {
                session_id,
                lease_token: token,
            },
            events,
            disconnect: Vec::new(),
        };
        if let Some(old) = attach.revoked_connection {
            if let Some(connection) = self.connections.get_mut(&old) {
                connection.attached_session = None;
            }
            outcome.events.push((
                old,
                Event::LeaseRevoked {
                    reason: "another client explicitly took over this Svarm session".into(),
                },
            ));
            outcome.disconnect.push(old);
        }
        Ok(outcome)
    }

    fn stop_session_runtime(
        &mut self,
        session_id: SessionId,
        requester: Option<ConnectionId>,
    ) -> Result<Outcome, ProtocolError> {
        let mut runtime = self.sessions.remove(&session_id).ok_or_else(|| {
            ProtocolError::new(ErrorCode::SessionNotFound, "Svarm session was not found")
        })?;
        let agent_count = runtime.agents.len();
        let attached = runtime.state.attachment().map(|lease| lease.connection_id);
        runtime.state.stop();
        let cleanup_errors = runtime.agents.stop_all().len();
        if let Some(connection_id) = attached
            && let Some(connection) = self.connections.get_mut(&connection_id)
        {
            connection.attached_session = None;
        }
        let mut outcome = Outcome::new(Response::Stopped(StopSummary {
            session_count: 1,
            agent_count,
            cleanup_errors,
            server_stopped: false,
        }));
        if let Some(connection_id) = attached
            && Some(connection_id) != requester
        {
            outcome.events.push((
                connection_id,
                Event::LeaseRevoked {
                    reason: "Svarm session was stopped by a control client".into(),
                },
            ));
            outcome.disconnect.push(connection_id);
        }
        Ok(outcome)
    }

    fn check_role(&self, id: ConnectionId, request: &Request) -> Result<(), ProtocolError> {
        let role = self.connections[&id].role.expect("handshake completed");
        let allowed = match role {
            ConnectionRole::Interactive => !matches!(
                request,
                Request::ServerStatus
                    | Request::ListSessions
                    | Request::GetSession { .. }
                    | Request::StopSession { .. }
                    | Request::StopServer { .. }
            ),
            ConnectionRole::Control => matches!(
                request,
                Request::ServerStatus
                    | Request::ListSessions
                    | Request::GetSession { .. }
                    | Request::StopSession { .. }
                    | Request::StopServer { .. }
            ),
            ConnectionRole::Probe => matches!(request, Request::ServerStatus),
        };
        if allowed {
            Ok(())
        } else {
            Err(ProtocolError::new(
                ErrorCode::InvalidRequest,
                "request is not permitted for this connection role",
            ))
        }
    }

    fn attached_session(
        &self,
        id: ConnectionId,
        token: &LeaseToken,
    ) -> Result<SessionId, ProtocolError> {
        let session_id = self.connections[&id].attached_session.ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::InvalidLease,
                "connection has no interactive lease",
            )
        })?;
        self.sessions[&session_id].state.validate_lease(token)?;
        Ok(session_id)
    }

    fn poll_sessions(&mut self) {
        let session_ids = self.sessions.keys().copied().collect::<Vec<_>>();
        for session_id in session_ids {
            let Some(runtime) = self.sessions.get_mut(&session_id) else {
                continue;
            };
            let connection_id = runtime.state.attachment().map(|lease| lease.connection_id);
            let events = runtime.poll_events();
            if let Some(connection_id) = connection_id {
                for event in events {
                    self.send_event(connection_id, event);
                }
            }
        }
    }

    fn disconnect(&mut self, id: ConnectionId) {
        self.remove_connection(id, true);
    }

    fn disconnect_after_flush(&mut self, id: ConnectionId) {
        self.remove_connection(id, false);
    }

    fn remove_connection(&mut self, id: ConnectionId, shutdown_now: bool) {
        let now = self.now_ms();
        let Some(mut connection) = self.connections.remove(&id) else {
            return;
        };
        let mut remove_session = None;
        if let Some(session_id) = connection.attached_session
            && let Some(runtime) = self.sessions.get_mut(&session_id)
            && runtime.state.disconnect(id, now)
        {
            runtime.frame_bases.clear();
            if runtime.agents.is_empty() {
                remove_session = Some(session_id);
            }
        }
        if let Some(session_id) = remove_session {
            self.sessions.remove(&session_id);
        }
        connection.outgoing.close();
        if shutdown_now {
            let _ = connection.stream.shutdown(Shutdown::Both);
        }
        if let Some(writer) = connection.writer.take() {
            let _ = writer.join();
        }
    }

    fn send_response(&mut self, id: ConnectionId, request_id: RequestId, response: Response) {
        self.send_message(id, Some(request_id), Message::Response(response));
    }

    fn send_error(
        &mut self,
        id: ConnectionId,
        request_id: Option<RequestId>,
        error: ProtocolError,
    ) {
        self.send_message(id, request_id, Message::Error(error));
    }

    fn send_event(&mut self, id: ConnectionId, event: Event) {
        self.send_message(id, None, Message::Event(event));
    }

    fn send_message(&mut self, id: ConnectionId, request_id: Option<RequestId>, message: Message) {
        let protocol_version = self
            .connections
            .get(&id)
            .and_then(|connection| connection.protocol_version)
            .unwrap_or(crate::protocol::PROTOCOL_VERSION);
        self.send(
            id,
            Envelope {
                protocol_version,
                request_id,
                message,
            },
        );
    }

    fn send(&mut self, id: ConnectionId, envelope: Envelope) {
        let result = self
            .connections
            .get(&id)
            .map(|connection| connection.outgoing.push(Box::new(envelope)));
        match result {
            Some(QueueResult::NeedsFull(agent_id)) => {
                let full = self.connections.get(&id).and_then(|connection| {
                    connection.attached_session.and_then(|session_id| {
                        self.sessions
                            .get_mut(&session_id)
                            .and_then(|runtime| runtime.full_terminal(agent_id))
                    })
                });
                let Some(event) = full else {
                    self.disconnect(id);
                    return;
                };
                let version = self.connections[&id]
                    .protocol_version
                    .unwrap_or(crate::protocol::PROTOCOL_VERSION);
                let envelope = Envelope {
                    protocol_version: version,
                    request_id: None,
                    message: Message::Event(event),
                };
                if !matches!(
                    self.connections[&id].outgoing.push(Box::new(envelope)),
                    QueueResult::Queued
                ) {
                    self.disconnect(id);
                }
            }
            Some(QueueResult::Full) => self.disconnect(id),
            Some(QueueResult::Queued) | None => {}
        }
    }

    fn status(&self) -> ServerStatusSnapshot {
        ServerStatusSnapshot {
            process_id: std::process::id(),
            application_version: self.config.application_version.clone(),
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            instance_id: self.instance_id.clone(),
            socket_path: self.listener.path().to_owned(),
            uptime_ms: self.started.elapsed().as_millis() as u64,
            session_count: self.sessions.len(),
            client_count: self.connections.len(),
        }
    }

    fn update_lifetime(&mut self) {
        if !self.sessions.is_empty() || !self.connections.is_empty() {
            self.empty_since = None;
            return;
        }
        let empty_since = self.empty_since.get_or_insert_with(Instant::now);
        if empty_since.elapsed() >= EMPTY_SERVER_GRACE {
            self.stopping = true;
        }
    }

    fn lease_token(&mut self) -> LeaseToken {
        let token = LeaseToken(format!(
            "{}-{:x}-{:x}",
            self.instance_id.0, self.next_token, self.started_ms
        ));
        self.next_token = self.next_token.saturating_add(1);
        token
    }

    fn now_ms(&self) -> u64 {
        self.started_ms
            .saturating_add(self.started.elapsed().as_millis() as u64)
    }

    fn finish_shutdown(&mut self) {
        for runtime in self.sessions.values_mut() {
            runtime.state.stop();
            let _ = runtime.agents.stop_all();
        }
        self.sessions.clear();
        for connection in self.connections.values() {
            connection.outgoing.close();
        }
        for (_, mut connection) in std::mem::take(&mut self.connections) {
            if let Some(writer) = connection.writer.take() {
                let _ = writer.join();
            }
        }
    }
}

fn start_connection(
    id: ConnectionId,
    stream: UnixStream,
    input: SyncSender<ServerInput>,
) -> io::Result<Connection> {
    stream.set_write_timeout(Some(CONNECTION_WRITE_TIMEOUT))?;
    let reader = stream.try_clone()?;
    let writer = stream.try_clone()?;
    let outgoing = Arc::new(OutgoingQueue::new());
    let reader_input = input.clone();
    thread::spawn(move || {
        let mut reader = reader;
        while let Ok(Some(envelope)) = read_frame(&mut reader) {
            if reader_input
                .send(ServerInput::Frame(id, Box::new(envelope)))
                .is_err()
            {
                break;
            }
        }
        let _ = reader_input.send(ServerInput::Disconnected(id));
    });
    let writer_outgoing = outgoing.clone();
    let writer = thread::spawn(move || {
        let mut writer = writer;
        while let Some(envelope) = writer_outgoing.pop() {
            if write_frame(&mut writer, envelope.as_ref()).is_err() {
                break;
            }
        }
        let _ = writer.shutdown(Shutdown::Both);
        let _ = input.try_send(ServerInput::Disconnected(id));
    });
    Ok(Connection {
        outgoing,
        stream,
        role: None,
        protocol_version: None,
        process_id: None,
        attached_session: None,
        writer: Some(writer),
    })
}

fn terminal_modes(screen: &Screen) -> TerminalModes {
    TerminalModes {
        keyboard_disambiguate: false,
        application_cursor: screen.application_cursor(),
        application_keypad: screen.application_keypad(),
        bracketed_paste: screen.bracketed_paste(),
        mouse_protocol: match screen.mouse_protocol_mode() {
            MouseProtocolMode::None => MouseProtocol::None,
            MouseProtocolMode::Press => MouseProtocol::Press,
            MouseProtocolMode::PressRelease => MouseProtocol::PressRelease,
            MouseProtocolMode::ButtonMotion => MouseProtocol::ButtonMotion,
            MouseProtocolMode::AnyMotion => MouseProtocol::AnyMotion,
        },
        mouse_encoding: match screen.mouse_protocol_encoding() {
            MouseProtocolEncoding::Default => MouseEncoding::Default,
            MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
            MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
        },
    }
}

fn terminal_frame(envelope: &Envelope) -> Option<(AgentId, bool)> {
    match &envelope.message {
        Message::Event(Event::TerminalFull(frame)) => Some((frame.agent_id, true)),
        Message::Event(Event::TerminalDiff(frame)) => Some((frame.agent_id, false)),
        _ => None,
    }
}

fn require_confirmation(confirmed: bool) -> Result<(), ProtocolError> {
    if confirmed {
        Ok(())
    } else {
        Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            "destructive operation requires explicit confirmation",
        ))
    }
}

fn canonicalize_agent_directory(path: &Path) -> Result<PathBuf, ProtocolError> {
    let canonical = path.canonicalize().map_err(|error| {
        ProtocolError::new(
            ErrorCode::InvalidRequest,
            format!(
                "could not resolve agent workspace {}: {error}",
                path.display()
            ),
        )
    })?;
    if !canonical.is_dir() {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            format!("agent workspace is not a directory: {}", path.display()),
        ));
    }
    Ok(canonical)
}

fn agent_not_found() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::AgentNotFound,
        "agent does not exist in this Svarm session",
    )
}

fn internal_error(_error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(ErrorCode::InternalError, "server operation failed")
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write, time::SystemTime};

    use super::*;
    use crate::{
        CursorStyle,
        protocol::{Hello, HostTerminalCapabilities, PROTOCOL_VERSION, TerminalDiff, TerminalFull},
    };
    use vt100::Parser;

    struct Client {
        stream: UnixStream,
        next_request: u64,
    }

    impl Client {
        fn connect(path: &Path, role: ConnectionRole) -> Self {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut stream = loop {
                match UnixStream::connect(path) {
                    Ok(stream) => break stream,
                    Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                    Err(error) => panic!("server did not accept connections: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            write_frame(
                &mut stream,
                &Envelope {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: Some(RequestId(1)),
                    message: Message::Hello(Hello {
                        application_version: "test".into(),
                        protocol: ProtocolRange::CURRENT,
                        role,
                        process_id: Some(std::process::id()),
                        terminal: HostTerminalCapabilities::default(),
                    }),
                },
            )
            .unwrap();
            let welcome: Envelope = read_frame(&mut stream).unwrap().unwrap();
            assert!(matches!(welcome.message, Message::Welcome(_)));
            Self {
                stream,
                next_request: 2,
            }
        }

        fn request(&mut self, request: Request) -> (RequestId, Response) {
            let request_id = RequestId(self.next_request);
            self.next_request += 1;
            write_frame(
                &mut self.stream,
                &Envelope {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: Some(request_id),
                    message: Message::Request(request),
                },
            )
            .unwrap();
            loop {
                let envelope: Envelope = read_frame(&mut self.stream).unwrap().unwrap();
                if envelope.request_id == Some(request_id) {
                    return match envelope.message {
                        Message::Response(response) => (request_id, response),
                        Message::Error(error) => panic!("request failed: {error:?}"),
                        other => panic!("unexpected request response: {other:?}"),
                    };
                }
            }
        }

        fn request_error(&mut self, request: Request) -> ProtocolError {
            let request_id = RequestId(self.next_request);
            self.next_request += 1;
            write_frame(
                &mut self.stream,
                &Envelope {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: Some(request_id),
                    message: Message::Request(request),
                },
            )
            .unwrap();
            loop {
                let envelope: Envelope = read_frame(&mut self.stream).unwrap().unwrap();
                if envelope.request_id == Some(request_id) {
                    return match envelope.message {
                        Message::Error(error) => error,
                        other => panic!("expected request error, received: {other:?}"),
                    };
                }
            }
        }

        fn event_until(&mut self, predicate: impl Fn(&Event) -> bool) -> Event {
            let mut last_event = None;
            loop {
                let envelope: Envelope = read_frame(&mut self.stream)
                    .unwrap_or_else(|error| {
                        panic!("timed out waiting for server event after {last_event:?}: {error}")
                    })
                    .unwrap_or_else(|| panic!("server disconnected after {last_event:?}"));
                if let Message::Event(event) = envelope.message {
                    if predicate(&event) {
                        return event;
                    }
                    last_event = Some(event);
                }
            }
        }
    }

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "svarm-server-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn one_session_launches_agents_in_distinct_validated_directories() {
        let directory = temp_dir();
        let first = directory.join("first");
        let second = directory.join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let regular_file = directory.join("not-a-directory");
        fs::write(&regular_file, b"file").unwrap();
        let socket = directory.join("server.sock");
        let server_socket = socket.clone();
        let server = thread::spawn(move || {
            run_foreground(
                ServerConfig::new(server_socket, "test")
                    .with_test_agent_command("sh", &["-c", "exec sleep 60"]),
            )
            .unwrap()
        });

        let mut client = Client::connect(&socket, ConnectionRole::Interactive);
        let (_, created) = client.request(Request::CreateSession {
            rows: 10,
            cols: 40,
            palette: None,
        });
        let (session_id, lease_token) = match created {
            Response::Created {
                session_id,
                lease_token,
            } => (session_id, lease_token),
            other => panic!("unexpected create response: {other:?}"),
        };
        let _ = client.event_until(|event| matches!(event, Event::SvarmSessionSnapshot(_)));

        for expected in [&first, &second] {
            let _ = client.request(Request::SpawnAgent {
                lease_token: lease_token.clone(),
                kind: AgentKind::Codex,
                launch_directory: expected.clone(),
            });
            let added = client.event_until(|event| matches!(event, Event::AgentAdded { .. }));
            assert!(
                matches!(added, Event::AgentAdded { agent, .. } if agent.launch_directory == *expected)
            );
            let _ = client.event_until(|event| matches!(event, Event::TerminalFull(_)));
        }

        for invalid in [directory.join("missing"), regular_file] {
            let error = client.request_error(Request::SpawnAgent {
                lease_token: lease_token.clone(),
                kind: AgentKind::Claude,
                launch_directory: invalid.clone(),
            });
            assert_eq!(error.code, ErrorCode::InvalidRequest);
            assert!(error.message.contains(&invalid.display().to_string()));
        }

        let mut control = Client::connect(&socket, ConnectionRole::Control);
        let (_, response) = control.request(Request::GetSession { session_id });
        let Response::Session {
            session: Some(snapshot),
        } = response
        else {
            panic!("unexpected session response: {response:?}");
        };
        assert_eq!(snapshot.agents.len(), 2);
        assert_eq!(snapshot.selected_agent_id, Some(snapshot.agents[1].id));

        let _ = client.request(Request::StopAttachedSession { lease_token });
        drop(client);
        let _ = control.request(Request::StopServer { confirmed: true });
        drop(control);
        server.join().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn agent_keeps_running_and_producing_output_while_detached() {
        let directory = temp_dir();
        let socket = directory.join("server.sock");
        let config = ServerConfig::new(socket.clone(), "test").with_test_agent_command(
            "sh",
            &[
                "-c",
                "printf before; sleep 0.1; printf after; exec sleep 60",
            ],
        );
        let server = thread::spawn(move || run_foreground(config).unwrap());

        let mut client = Client::connect(&socket, ConnectionRole::Interactive);
        let (_, created) = client.request(Request::CreateSession {
            rows: 10,
            cols: 40,
            palette: None,
        });
        let (session_id, lease_token) = match created {
            Response::Created {
                session_id,
                lease_token,
            } => (session_id, lease_token),
            other => panic!("unexpected create response: {other:?}"),
        };
        let _ = client.event_until(|event| matches!(event, Event::SvarmSessionSnapshot(_)));
        let _ = client.request(Request::SpawnAgent {
            lease_token: lease_token.clone(),
            kind: AgentKind::Codex,
            launch_directory: directory.clone(),
        });
        let first = client.event_until(|event| {
            matches!(event, Event::TerminalFull(frame) if contains(&frame.formatted_screen, b"before"))
                || matches!(event, Event::TerminalDiff(frame) if contains(&frame.formatted_changes, b"before"))
        });
        assert!(matches!(
            first,
            Event::TerminalFull(_) | Event::TerminalDiff(_)
        ));
        let _ = client.request(Request::DetachSession { lease_token });
        drop(client);
        thread::sleep(Duration::from_millis(200));

        let mut reattached = Client::connect(&socket, ConnectionRole::Interactive);
        let (_, attached) = reattached.request(Request::AttachSession {
            session_id,
            rows: 10,
            cols: 40,
            palette: None,
            takeover: false,
        });
        let lease_token = match attached {
            Response::Attached { lease_token, .. } => lease_token,
            other => panic!("unexpected attach response: {other:?}"),
        };
        let _ = reattached.event_until(|event| {
            matches!(event, Event::TerminalFull(frame) if contains(&frame.formatted_screen, b"after"))
                || matches!(event, Event::TerminalDiff(frame) if contains(&frame.formatted_changes, b"after"))
        });
        let _ = reattached.request(Request::StopAttachedSession { lease_token });
        drop(reattached);

        let mut control = Client::connect(&socket, ConnectionRole::Control);
        let (_, stopped) = control.request(Request::StopServer { confirmed: true });
        assert!(matches!(
            stopped,
            Response::Stopped(StopSummary {
                server_stopped: true,
                ..
            })
        ));
        drop(control);
        server.join().unwrap();
        assert!(!socket.exists());
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn malformed_connection_does_not_stop_the_server() {
        let directory = temp_dir();
        let socket = directory.join("server.sock");
        let server_socket = socket.clone();
        let server = thread::spawn(move || {
            run_foreground(ServerConfig::new(server_socket, "test")).unwrap()
        });

        let mut malformed = UnixStream::connect(&socket).unwrap_or_else(|_| {
            let _ = Client::connect(&socket, ConnectionRole::Probe);
            UnixStream::connect(&socket).unwrap()
        });
        malformed.write_all(&[0, 0, 0, 1, b'{']).unwrap();
        drop(malformed);

        let mut oversized = UnixStream::connect(&socket).unwrap();
        oversized
            .write_all(&((crate::framing::MAX_FRAME_LEN as u32) + 1).to_be_bytes())
            .unwrap();
        drop(oversized);

        let mut control = Client::connect(&socket, ConnectionRole::Control);
        let (_, status) = control.request(Request::ServerStatus);
        assert!(matches!(status, Response::ServerStatus(_)));
        let _ = control.request(Request::StopServer { confirmed: true });
        drop(control);
        server.join().unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn incompatible_handshake_leaves_server_reachable() {
        let directory = temp_dir();
        let socket = directory.join("server.sock");
        let server_socket = socket.clone();
        let server = thread::spawn(move || {
            run_foreground(ServerConfig::new(server_socket, "test")).unwrap()
        });
        let probe = Client::connect(&socket, ConnectionRole::Probe);
        drop(probe);

        let mut incompatible = UnixStream::connect(&socket).unwrap();
        incompatible
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write_frame(
            &mut incompatible,
            &Envelope {
                protocol_version: PROTOCOL_VERSION + 1,
                request_id: Some(RequestId(1)),
                message: Message::Hello(Hello {
                    application_version: "future".into(),
                    protocol: ProtocolRange {
                        min: PROTOCOL_VERSION + 1,
                        max: PROTOCOL_VERSION + 1,
                    },
                    role: ConnectionRole::Probe,
                    process_id: None,
                    terminal: HostTerminalCapabilities::default(),
                }),
            },
        )
        .unwrap();
        let response: Envelope = read_frame(&mut incompatible).unwrap().unwrap();
        assert!(matches!(
            response.message,
            Message::Error(ProtocolError {
                code: ErrorCode::IncompatibleProtocol,
                ..
            })
        ));
        drop(incompatible);

        let mut control = Client::connect(&socket, ConnectionRole::Control);
        let (_, status) = control.request(Request::ServerStatus);
        assert!(matches!(status, Response::ServerStatus(_)));
        let _ = control.request(Request::StopServer { confirmed: true });
        drop(control);
        server.join().unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn takeover_flushes_lease_revocation_before_disconnect() {
        let directory = temp_dir();
        let socket = directory.join("server.sock");
        let server_socket = socket.clone();
        let server = thread::spawn(move || {
            run_foreground(
                ServerConfig::new(server_socket, "test")
                    .with_test_agent_command("sh", &["-c", "exec sleep 60"]),
            )
            .unwrap()
        });

        let mut old = Client::connect(&socket, ConnectionRole::Interactive);
        let (_, created) = old.request(Request::CreateSession {
            rows: 10,
            cols: 40,
            palette: None,
        });
        let (session_id, old_lease) = match created {
            Response::Created {
                session_id,
                lease_token,
            } => (session_id, lease_token),
            other => panic!("unexpected create response: {other:?}"),
        };
        let _ = old.event_until(|event| matches!(event, Event::SvarmSessionSnapshot(_)));
        let _ = old.request(Request::SpawnAgent {
            lease_token: old_lease.clone(),
            kind: AgentKind::Codex,
            launch_directory: directory.clone(),
        });
        let added = old.event_until(|event| matches!(event, Event::AgentAdded { .. }));
        let agent_id = match added {
            Event::AgentAdded { agent, .. } => agent.id,
            _ => unreachable!(),
        };
        let _ = old.event_until(|event| matches!(event, Event::TerminalFull(_)));

        let mut conflict = Client::connect(&socket, ConnectionRole::Interactive);
        let error = conflict.request_error(Request::AttachSession {
            session_id,
            rows: 20,
            cols: 60,
            palette: None,
            takeover: false,
        });
        assert_eq!(error.code, ErrorCode::SessionAlreadyAttached);
        assert!(error.context.contains_key("connection_id"));
        assert!(error.context.contains_key("attachment_age_ms"));
        let mut inspector = Client::connect(&socket, ConnectionRole::Control);
        let (_, session) = inspector.request(Request::GetSession { session_id });
        let snapshot = match session {
            Response::Session {
                session: Some(snapshot),
            } => snapshot,
            other => panic!("unexpected session response: {other:?}"),
        };
        assert_eq!((snapshot.rows, snapshot.cols), (10, 40));
        assert_eq!(
            snapshot
                .summary
                .attachment
                .unwrap()
                .connection_id
                .0
                .to_string(),
            error.context["connection_id"]
        );
        let _ = old.request(Request::ResyncTerminal {
            lease_token: old_lease,
            agent_id,
            last_sequence: None,
        });
        let full = old.event_until(
            |event| matches!(event, Event::TerminalFull(frame) if frame.agent_id == agent_id),
        );
        assert!(matches!(full, Event::TerminalFull(frame) if (frame.rows, frame.cols) == (10, 40)));
        drop(inspector);
        drop(conflict);

        let mut replacement = Client::connect(&socket, ConnectionRole::Interactive);
        let (_, attached) = replacement.request(Request::AttachSession {
            session_id,
            rows: 10,
            cols: 40,
            palette: None,
            takeover: true,
        });
        let lease_token = match attached {
            Response::Attached { lease_token, .. } => lease_token,
            other => panic!("unexpected attach response: {other:?}"),
        };
        let revoked = old.event_until(|event| matches!(event, Event::LeaseRevoked { .. }));
        assert!(matches!(revoked, Event::LeaseRevoked { reason } if reason.contains("took over")));
        let _ = replacement.request(Request::StopAttachedSession { lease_token });
        drop(old);
        drop(replacement);

        let mut control = Client::connect(&socket, ConnectionRole::Control);
        let _ = control.request(Request::StopServer { confirmed: true });
        drop(control);
        server.join().unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn child_exit_while_detached_is_reaped_and_restored() {
        let directory = temp_dir();
        let socket = directory.join("server.sock");
        let config = ServerConfig::new(socket.clone(), "test")
            .with_test_agent_command("sh", &["-c", "sleep 0.1; printf finished"]);
        let server = thread::spawn(move || run_foreground(config).unwrap());

        let mut client = Client::connect(&socket, ConnectionRole::Interactive);
        let (_, created) = client.request(Request::CreateSession {
            rows: 10,
            cols: 40,
            palette: None,
        });
        let (session_id, lease_token) = match created {
            Response::Created {
                session_id,
                lease_token,
            } => (session_id, lease_token),
            other => panic!("unexpected create response: {other:?}"),
        };
        let _ = client.event_until(|event| matches!(event, Event::SvarmSessionSnapshot(_)));
        let _ = client.request(Request::SpawnAgent {
            lease_token: lease_token.clone(),
            kind: AgentKind::Codex,
            launch_directory: directory.clone(),
        });
        let _ = client.request(Request::DetachSession { lease_token });
        drop(client);
        thread::sleep(Duration::from_millis(250));

        let mut reattached = Client::connect(&socket, ConnectionRole::Interactive);
        let (_, attached) = reattached.request(Request::AttachSession {
            session_id,
            rows: 10,
            cols: 40,
            palette: None,
            takeover: false,
        });
        let lease_token = match attached {
            Response::Attached { lease_token, .. } => lease_token,
            other => panic!("unexpected attach response: {other:?}"),
        };
        let snapshot = reattached.event_until(|event| {
            matches!(event, Event::SvarmSessionSnapshot(snapshot) if snapshot.agents.iter().any(|agent| agent.status == SessionStatus::Exited))
        });
        assert!(
            matches!(snapshot, Event::SvarmSessionSnapshot(snapshot) if snapshot.agents[0].exit.is_some())
        );
        let _ = reattached.event_until(|event| {
            matches!(event, Event::TerminalFull(frame) if contains(&frame.formatted_screen, b"finished"))
        });
        let _ = reattached.request(Request::StopAttachedSession { lease_token });
        drop(reattached);

        let mut control = Client::connect(&socket, ConnectionRole::Control);
        let _ = control.request(Request::StopServer { confirmed: true });
        drop(control);
        server.join().unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn full_and_diff_frames_reconstruct_screen_cursor_and_input_modes() {
        let mut authoritative = Parser::new(5, 30, 100);
        authoritative
            .process(b"\x1b[?2004h\x1b[?1000h\x1b[31mprimary\x1b[?1049h\x1b[2J\x1b[32malternate");
        let full = authoritative.screen().state_formatted();
        let mut client = Parser::new(5, 30, 100);
        for byte in full {
            client.process(&[byte]);
        }
        assert_screens_match(authoritative.screen(), client.screen());

        let previous = authoritative.screen().clone();
        authoritative.process(b"\x1b[?1049l\x1b[2;3H\x1b[34msecond\x1b[?1000l\x1b[?1003h");
        let diff = authoritative.screen().state_diff(&previous);
        for byte in diff {
            client.process(&[byte]);
        }
        assert_screens_match(authoritative.screen(), client.screen());
    }

    #[test]
    fn terminal_queue_pressure_discards_obsolete_diffs_and_requires_a_full_frame() {
        let queue = OutgoingQueue::new();
        for sequence in 1..=CONNECTION_QUEUE as u64 {
            assert!(matches!(
                queue.push(Box::new(terminal_envelope(false, sequence))),
                QueueResult::Queued
            ));
        }
        match queue.push(Box::new(terminal_envelope(
            false,
            CONNECTION_QUEUE as u64 + 1,
        ))) {
            QueueResult::NeedsFull(agent_id) => assert_eq!(agent_id, AgentId::new(1)),
            _ => panic!("queue pressure should require a full terminal frame"),
        }
        assert_eq!(queue.len(), 0);
        assert!(matches!(
            queue.push(Box::new(terminal_envelope(
                true,
                CONNECTION_QUEUE as u64 + 2
            ))),
            QueueResult::Queued
        ));
        assert_eq!(queue.len(), 1);
    }

    fn assert_screens_match(expected: &Screen, actual: &Screen) {
        assert_eq!(actual.size(), expected.size());
        assert_eq!(actual.contents(), expected.contents());
        assert_eq!(actual.state_formatted(), expected.state_formatted());
        assert_eq!(actual.cursor_position(), expected.cursor_position());
        assert_eq!(actual.application_cursor(), expected.application_cursor());
        assert_eq!(actual.application_keypad(), expected.application_keypad());
        assert_eq!(actual.bracketed_paste(), expected.bracketed_paste());
        assert_eq!(actual.mouse_protocol_mode(), expected.mouse_protocol_mode());
        assert_eq!(
            actual.mouse_protocol_encoding(),
            expected.mouse_protocol_encoding()
        );
    }

    fn terminal_envelope(full: bool, sequence: u64) -> Envelope {
        let sequence = TerminalSequence(sequence);
        let message = if full {
            Message::Event(Event::TerminalFull(TerminalFull {
                agent_id: AgentId::new(1),
                rows: 1,
                cols: 1,
                output_generation: sequence.0,
                sequence,
                formatted_screen: vec![b'x'],
                modes: TerminalModes::default(),
                cursor_style: CursorStyle::default(),
            }))
        } else {
            Message::Event(Event::TerminalDiff(TerminalDiff {
                agent_id: AgentId::new(1),
                rows: 1,
                cols: 1,
                output_generation: sequence.0,
                base_sequence: TerminalSequence(sequence.0.saturating_sub(1)),
                sequence,
                formatted_changes: vec![b'x'],
                modes: TerminalModes::default(),
                cursor_style: CursorStyle::default(),
            }))
        };
        Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: None,
            message,
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }
}
