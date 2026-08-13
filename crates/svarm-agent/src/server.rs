use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
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

use crate::{
    AgentId, AgentKind, AgentManager, Result as AgentResult, SessionSnapshot, SessionStatus,
    framing::{read_frame, write_frame},
    git,
    history::ConversationHistory,
    input::{encode_key, encode_mouse, encode_paste},
    ipc::unix::UnixListenerGuard,
    naming::TitleNamer,
    protocol::{
        AgentActivity, AgentSnapshot, ArchivedConversation, ConnectionId, ConnectionRole, Envelope,
        ErrorCode, Event, GitContext, KeyCode, KeyInput, LeaseToken, Message, ProtocolError,
        ProtocolRange, RecognitionEvidence, Request, RequestId, Response, ServerCapabilities,
        ServerInstanceId, ServerStatusSnapshot, SessionId, SessionSummary, StopSummary,
        SvarmSessionSnapshot, TerminalDiff, TerminalFull, TerminalModes, TerminalSequence,
        TerminalViewport, Welcome,
    },
    pty_size,
    recognition::{self, ScreenRecognition},
    server_session::{ServerSessionState, sort_session_summaries},
    terminal_model::{TerminalSnapshot, TerminalSnapshotDiff},
};

const EVENT_TICK: Duration = Duration::from_millis(16);
const SELECTED_GIT_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const BACKGROUND_GIT_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
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
            let incoming_terminal = terminal_frame(envelope.as_ref());
            let Some((agent_id, full)) = incoming_terminal.or_else(|| {
                state
                    .frames
                    .iter()
                    .find_map(|queued| terminal_frame(queued.as_ref()))
            }) else {
                return QueueResult::Full;
            };
            state.frames.retain(|queued| {
                terminal_frame(queued.as_ref()).is_none_or(|(queued_id, _)| queued_id != agent_id)
            });
            if state.frames.len() == CONNECTION_QUEUE {
                return QueueResult::Full;
            }
            if incoming_terminal.is_none() {
                state.frames.push_back(envelope);
                self.ready.notify_one();
                return QueueResult::NeedsFull(agent_id);
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
    previous: HashMap<AgentId, ObservedAgent>,
    input_drafts: HashMap<AgentId, InputDraft>,
    git_checked: HashMap<AgentId, Instant>,
    git_cache: HashMap<PathBuf, CachedGitContext>,
    git_worker: git::ContextWorker,
    git_in_flight: Option<GitProbe>,
    frame_bases: HashMap<AgentId, FrameBasis>,
    archived: Vec<ArchivedConversation>,
    namer: TitleNamer,
    history: ConversationHistory,
}

struct GitProbe {
    agent_id: AgentId,
    working_directory: Option<PathBuf>,
    directory: PathBuf,
}

#[derive(Clone)]
struct CachedGitContext {
    checked_at: Instant,
    context: Option<GitContext>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedAgent {
    session: SessionSnapshot,
    conversation_title: Option<String>,
    activity: AgentActivity,
    last_affirmative_activity: Option<AgentActivity>,
    completed_generation: u64,
    recognition: Option<RecognitionEvidence>,
    working_directory: Option<PathBuf>,
    git: Option<GitContext>,
}

#[derive(Default)]
struct InputDraft {
    chars: Vec<char>,
    cursor: usize,
    title: Option<String>,
    /// Every message submitted while the conversation is still waiting for a generated name.
    prompts: Vec<String>,
    /// The name a generator produced. Preferred over the first message once it arrives.
    generated: Option<String>,
    /// A generator is running for this conversation, so no second one is started.
    naming: bool,
}

impl InputDraft {
    /// A conversation that already carries its final name, such as a resumed archived one.
    fn named(title: String) -> Self {
        Self {
            generated: Some(title),
            ..Self::default()
        }
    }

    fn from_prompt(prompt: String) -> Self {
        Self {
            title: Some(prompt.clone()),
            prompts: vec![prompt],
            ..Self::default()
        }
    }

    /// Records a key and reports whether it submitted a message.
    fn apply_key(&mut self, input: &KeyInput) -> bool {
        if self.settled() {
            return false;
        }
        let control = input.modifiers.control;
        let alt = input.modifiers.alt;
        match &input.code {
            KeyCode::Character('c') if control => self.clear(),
            KeyCode::Character('u') if control => self.clear(),
            KeyCode::Character('a') if control => self.cursor = 0,
            KeyCode::Character('e') if control => self.cursor = self.chars.len(),
            KeyCode::Character(character) if !control && !alt => self.insert(*character),
            KeyCode::Enter if input.modifiers.shift => self.insert('\n'),
            KeyCode::Enter => return self.submit(),
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.chars.remove(self.cursor);
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.chars.len() {
                    self.chars.remove(self.cursor);
                }
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.chars.len(),
            KeyCode::Up | KeyCode::Down | KeyCode::Escape => self.clear(),
            _ => {}
        }
        false
    }

    /// Records pasted text and reports whether it submitted a message.
    fn apply_paste(&mut self, text: &str) -> bool {
        if self.settled() {
            return false;
        }
        for character in text.chars() {
            self.insert(character);
        }
        if text.ends_with('\n') || text.ends_with('\r') {
            return self.submit();
        }
        false
    }

    fn insert(&mut self, character: char) {
        self.chars.insert(self.cursor, character);
        self.cursor += 1;
    }

    fn submit(&mut self) -> bool {
        let message = self.chars.iter().collect::<String>();
        let message = message.split_whitespace().collect::<Vec<_>>().join(" ");
        self.chars.clear();
        self.cursor = 0;
        if message.is_empty() || message.starts_with('/') {
            return false;
        }
        if self.title.is_none() {
            self.title = Some(message.clone());
        }
        self.prompts.push(message);
        true
    }

    fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    /// Once a name has been generated the conversation is named for good, and there is nothing left
    /// for the draft to observe.
    const fn settled(&self) -> bool {
        self.generated.is_some()
    }

    fn title(&self) -> Option<&str> {
        self.generated.as_deref().or(self.title.as_deref())
    }
}

struct FrameBasis {
    sequence: TerminalSequence,
    acknowledged: Option<TerminalSequence>,
    snapshot: TerminalSnapshot,
}

enum FramePayload {
    Full(TerminalSnapshot),
    Diff {
        base_sequence: TerminalSequence,
        diff: TerminalSnapshotDiff,
    },
}

impl SessionRuntime {
    fn new(state: ServerSessionState, wake: Option<crate::session::OutputNotifier>) -> Self {
        // Tests must never invoke a real coding agent to name a conversation; they opt into a
        // stubbed generator explicitly with `with_namer`.
        #[cfg(test)]
        let namer = TitleNamer::disabled();
        #[cfg(not(test))]
        let namer = TitleNamer::from_environment();
        Self::with_namer_and_history(state, wake, namer, ConversationHistory::from_environment())
    }

    #[cfg(test)]
    fn with_namer(
        state: ServerSessionState,
        wake: Option<crate::session::OutputNotifier>,
        namer: TitleNamer,
    ) -> Self {
        Self::with_namer_and_history(state, wake, namer, ConversationHistory::default())
    }

    fn with_namer_and_history(
        state: ServerSessionState,
        wake: Option<crate::session::OutputNotifier>,
        namer: TitleNamer,
        history: ConversationHistory,
    ) -> Self {
        let (rows, cols) = state.dimensions();
        let agents = AgentManager::new(pty_size(rows, cols), state.terminal_palette(), wake);
        Self {
            state,
            agents,
            previous: HashMap::new(),
            input_drafts: HashMap::new(),
            git_checked: HashMap::new(),
            git_cache: HashMap::new(),
            git_worker: git::ContextWorker::new(),
            git_in_flight: None,
            frame_bases: HashMap::new(),
            archived: Vec::new(),
            namer,
            history,
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
        self.input_drafts.insert(snapshot.id, InputDraft::default());
        let observed = self.observe(snapshot.clone());
        self.previous.insert(snapshot.id, observed);
        Ok(snapshot)
    }

    fn close(&mut self, id: AgentId, now_ms: u64) -> AgentResult<()> {
        self.agents.close(id)?;
        self.previous.remove(&id);
        self.input_drafts.remove(&id);
        self.git_checked.remove(&id);
        self.frame_bases.remove(&id);
        self.state.remove_agent(id, self.agents.agent_ids(), now_ms);
        Ok(())
    }

    fn archive(&mut self, id: AgentId, now_ms: u64) -> Result<ArchivedConversation, ProtocolError> {
        let observed = self.previous.get(&id).ok_or_else(agent_not_found)?;
        let conversation_id = observed.session.conversation_id.clone().ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::InvalidRequest,
                "conversation is not resumable yet",
            )
        })?;
        let title = observed.conversation_title.clone().ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::InvalidRequest,
                "unnamed conversations cannot be archived",
            )
        })?;
        let conversation = ArchivedConversation {
            conversation_id,
            title,
            kind: observed.session.kind,
            launch_directory: observed.session.launch_directory.clone(),
        };
        self.close(id, now_ms).map_err(internal_error)?;
        self.archived
            .retain(|item| item.conversation_id != conversation.conversation_id);
        self.archived.insert(0, conversation.clone());
        self.state.metadata_changed();
        Ok(conversation)
    }

    fn resume_archived(
        &mut self,
        conversation_id: &str,
        now_ms: u64,
        _config: &ServerConfig,
    ) -> Result<SessionSnapshot, ProtocolError> {
        let index = self
            .archived
            .iter()
            .position(|item| item.conversation_id == conversation_id)
            .ok_or_else(|| {
                ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    "archived conversation was not found",
                )
            })?;
        let conversation = self.archived[index].clone();
        #[cfg(test)]
        let snapshot = if let Some((program, args)) = &_config.test_agent_command {
            self.agents
                .spawn_test_command_with_conversation(
                    conversation.kind,
                    &conversation.launch_directory,
                    program,
                    args,
                    Some(conversation.conversation_id.clone()),
                )
                .map_err(internal_error)?
        } else {
            self.agents
                .resume(
                    conversation.kind,
                    &conversation.launch_directory,
                    &conversation.conversation_id,
                )
                .map_err(internal_error)?
        };
        #[cfg(not(test))]
        let snapshot = self
            .agents
            .resume(
                conversation.kind,
                &conversation.launch_directory,
                &conversation.conversation_id,
            )
            .map_err(internal_error)?;
        self.state
            .register_agent(snapshot.id, snapshot.output_generation, now_ms);
        self.input_drafts
            .insert(snapshot.id, InputDraft::named(conversation.title));
        let observed = self.observe(snapshot.clone());
        self.previous.insert(snapshot.id, observed);
        self.archived.remove(index);
        self.state.metadata_changed();
        Ok(snapshot)
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
            archived: self.archived.clone(),
        }
    }

    fn agent_snapshot(&self, snapshot: SessionSnapshot) -> AgentSnapshot {
        let observed = self.previous.get(&snapshot.id);
        AgentSnapshot {
            id: snapshot.id,
            kind: snapshot.kind,
            launch_directory: snapshot.launch_directory,
            working_directory: observed.and_then(|agent| agent.working_directory.clone()),
            status: snapshot.status,
            exit: snapshot.exit,
            output_generation: snapshot.output_generation,
            seen_generation: self.state.seen_generation(snapshot.id).unwrap_or(0),
            completed_generation: observed.map_or(0, |agent| agent.completed_generation),
            terminal_sequence: self
                .state
                .terminal_sequence(snapshot.id)
                .unwrap_or(crate::protocol::TerminalSequence(0)),
            read_error: snapshot.read_error,
            conversation_title: observed.and_then(|agent| agent.conversation_title.clone()),
            conversation_id: snapshot.conversation_id,
            activity: observed.map_or(AgentActivity::Unknown, |agent| agent.activity),
            recognition: observed.and_then(|agent| agent.recognition.clone()),
            git: observed.and_then(|agent| agent.git.clone()),
        }
    }

    fn observe(&mut self, session: SessionSnapshot) -> ObservedAgent {
        let screen_changed = self
            .previous
            .get(&session.id)
            .is_none_or(|previous| previous.session.output_generation != session.output_generation);
        let terminal = screen_changed
            .then(|| self.agents.terminal_snapshot(session.id))
            .flatten();
        self.observe_with_terminal(session, terminal.as_ref())
    }

    fn observe_with_terminal(
        &mut self,
        session: SessionSnapshot,
        screen: Option<&TerminalSnapshot>,
    ) -> ObservedAgent {
        let previous = self.previous.get(&session.id).cloned();
        let conversation_title = self
            .input_drafts
            .get(&session.id)
            .and_then(InputDraft::title)
            .map(str::to_owned);
        let recognition = if let Some(screen) = screen {
            let screen_recognition = recognition::recognize(session.kind, screen);
            let title_recognition =
                recognition::recognize_title(session.kind, screen.state.title.trim());
            // Provider titles still contribute status evidence, but thread names come from the
            // first user message above.
            let _ = title_recognition
                .as_ref()
                .and_then(|recognized| recognized.conversation_title.as_ref());
            let from_screen = match screen_recognition {
                ScreenRecognition::Recognized(evidence) => Some(evidence),
                ScreenRecognition::Preserve => previous
                    .as_ref()
                    .and_then(|agent| agent.recognition.clone()),
                ScreenRecognition::Unknown => None,
            };
            let from_title = title_recognition.map(|recognized| recognized.evidence);
            // A provider that explicitly reports action required outranks any
            // other screen claim: the title is the provider's own statement that
            // the turn is waiting on the user.
            match (from_screen, from_title) {
                (Some(screen), Some(title))
                    if title.claim == AgentActivity::Blocked
                        && screen.claim != AgentActivity::Blocked =>
                {
                    Some(title)
                }
                (Some(screen), _) => Some(screen),
                (None, title) => title,
            }
        } else {
            previous
                .as_ref()
                .and_then(|agent| agent.recognition.clone())
        };
        let activity = recognition
            .as_ref()
            .map_or(AgentActivity::Unknown, |evidence| evidence.claim);
        let previous_affirmative = previous
            .as_ref()
            .and_then(|agent| agent.last_affirmative_activity);
        let completed_generation = if activity == AgentActivity::Idle
            && previous_affirmative == Some(AgentActivity::Working)
        {
            session.output_generation
        } else {
            previous
                .as_ref()
                .map_or(0, |agent| agent.completed_generation)
        };
        let last_affirmative_activity = if activity == AgentActivity::Unknown {
            previous_affirmative
        } else {
            Some(activity)
        };

        let (working_directory, git) = match previous.as_ref() {
            Some(agent) => (agent.working_directory.clone(), agent.git.clone()),
            None => (None, None),
        };

        ObservedAgent {
            session,
            conversation_title,
            activity,
            last_affirmative_activity,
            completed_generation,
            recognition,
            working_directory,
            git,
        }
    }

    fn apply_git_result(&mut self, now: Instant) -> BTreeSet<AgentId> {
        let mut changed = BTreeSet::new();
        let Some(result) = self.git_worker.try_result() else {
            return changed;
        };
        let Some(probe) = self.git_in_flight.take() else {
            return changed;
        };
        debug_assert_eq!(result.directory, probe.directory);

        // Keyed by the directory that was probed, never by the worktree it resolved to. A linked
        // worktree can live inside its own repository, so "inside the worktree root" does not
        // mean "on that worktree's branch" and reusing a context by path prefix would report the
        // parent checkout for every agent that moved into a nested worktree.
        let cached = CachedGitContext {
            checked_at: now,
            context: result.context,
        };
        self.git_cache
            .insert(probe.directory.clone(), cached.clone());

        // Do not apply a result to an agent that moved while Git was running. Clearing its last
        // check makes the new directory the next probe rather than showing stale worktree data.
        let current_working_directory = self.agents.working_directory(probe.agent_id);
        if current_working_directory == probe.working_directory {
            if self.apply_git_observation(probe.agent_id, probe.working_directory, &cached) {
                changed.insert(probe.agent_id);
            }
        } else {
            if let Some(observed) = self.previous.get_mut(&probe.agent_id) {
                let observation_changed = observed.working_directory != current_working_directory
                    || observed.git.is_some();
                observed.working_directory = current_working_directory;
                observed.git = None;
                if observation_changed {
                    changed.insert(probe.agent_id);
                }
            }
            self.git_checked.remove(&probe.agent_id);
        }

        // One probe answers for every agent standing in the same directory, which is the ordinary
        // case of several agents launched in one checkout.
        let shared = self
            .previous
            .iter()
            .filter_map(|(id, observed)| {
                let directory = observed
                    .working_directory
                    .as_deref()
                    .unwrap_or(&observed.session.launch_directory);
                (*id != probe.agent_id && directory == probe.directory).then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in shared {
            let working_directory = self.previous[&id].working_directory.clone();
            if self.apply_git_observation(id, working_directory, &cached) {
                changed.insert(id);
            }
        }

        self.forget_unvisited_git_cache();

        changed
    }

    /// Keying by probed directory means an agent that wanders leaves an entry behind. Keep only
    /// the directories agents are standing in now.
    fn forget_unvisited_git_cache(&mut self) {
        let visited = self
            .previous
            .values()
            .map(|observed| {
                observed
                    .working_directory
                    .clone()
                    .unwrap_or_else(|| observed.session.launch_directory.clone())
            })
            .collect::<HashSet<_>>();
        self.git_cache
            .retain(|directory, _| visited.contains(directory));
    }

    fn schedule_git_refresh(&mut self, now: Instant) -> BTreeSet<AgentId> {
        let mut changed = BTreeSet::new();
        if self.git_in_flight.is_some() {
            return changed;
        }

        let selected = self.state.selected_agent_id();
        let mut candidates = self.agents.agent_ids().to_vec();
        if let Some(selected) = selected
            && let Some(index) = candidates.iter().position(|id| *id == selected)
        {
            candidates.swap(0, index);
        }
        for id in candidates {
            let interval = git_refresh_interval(selected == Some(id));
            let Some(observed) = self.previous.get(&id) else {
                continue;
            };
            // Read the live directory before the interval gate: the `readlink` behind it costs
            // far less than the Git probe it decides on, and an agent that just moved into
            // another checkout should not keep showing the old branch until the interval lapses.
            let working_directory = self.agents.working_directory(id);
            let directory = working_directory
                .clone()
                .unwrap_or_else(|| observed.session.launch_directory.clone());
            let moved = working_directory != observed.working_directory;
            if !moved
                && self
                    .git_checked
                    .get(&id)
                    .is_some_and(|checked| now.duration_since(*checked) < interval)
            {
                continue;
            }

            if let Some(cached) = self.cached_git_context(&directory, interval, now) {
                if self.apply_git_observation(id, working_directory, &cached) {
                    changed.insert(id);
                }
                continue;
            }
            if self.git_worker.request(directory.clone()) {
                self.git_in_flight = Some(GitProbe {
                    agent_id: id,
                    working_directory,
                    directory,
                });
            }
            break;
        }
        changed
    }

    fn cached_git_context(
        &self,
        directory: &Path,
        interval: Duration,
        now: Instant,
    ) -> Option<CachedGitContext> {
        let cached = self.git_cache.get(directory)?;
        (now.duration_since(cached.checked_at) < interval).then(|| cached.clone())
    }

    fn apply_git_observation(
        &mut self,
        id: AgentId,
        working_directory: Option<PathBuf>,
        cached: &CachedGitContext,
    ) -> bool {
        let Some(observed) = self.previous.get_mut(&id) else {
            return false;
        };
        let changed =
            observed.working_directory != working_directory || observed.git != cached.context;
        observed.working_directory = working_directory;
        observed.git = cached.context.clone();
        self.git_checked.insert(id, cached.checked_at);
        changed
    }

    fn force_git_refresh(&mut self, id: AgentId) {
        self.git_checked.remove(&id);
    }

    /// The modes the agent's input has to be encoded against. Read in place: this runs on every
    /// keystroke, and copying the screen for it would put the copy in the input path.
    fn input_modes(&self, id: AgentId) -> Option<TerminalModes> {
        self.agents.terminal_modes(id)
    }

    fn record_key(&mut self, id: AgentId, input: &KeyInput) {
        if self
            .input_drafts
            .get_mut(&id)
            .is_some_and(|draft| draft.apply_key(input))
        {
            self.request_name(id);
        }
    }

    fn record_paste(&mut self, id: AgentId, text: &str) {
        if self
            .input_drafts
            .get_mut(&id)
            .is_some_and(|draft| draft.apply_paste(text))
        {
            self.request_name(id);
        }
    }

    /// Ask a generator to name this conversation. One request per conversation: the first submitted
    /// message says what the work is for, and a name that keeps changing under the user is worse
    /// than a slightly stale one.
    fn request_name(&mut self, id: AgentId) {
        let conversation_id = self
            .previous
            .get(&id)
            .and_then(|observed| observed.session.conversation_id.clone());
        self.request_name_for(id, conversation_id);
    }

    fn request_name_for(&mut self, id: AgentId, conversation_id: Option<String>) {
        let Some(draft) = self.input_drafts.get(&id) else {
            return;
        };
        if draft.naming || draft.settled() {
            return;
        }
        let kind = match self.agents.snapshot(id) {
            Some(snapshot) => snapshot.kind,
            None => return,
        };
        let started = self
            .namer
            .request(id, conversation_id, kind, &draft.prompts);
        if let Some(draft) = self.input_drafts.get_mut(&id) {
            draft.naming = started;
        }
    }

    /// Apply names that finished generating. A name is dropped when its agent is gone or has since
    /// moved to another conversation, so a `/clear` mid-generation cannot mislabel the new thread.
    fn apply_generated_names(&mut self) {
        for result in self.namer.drain() {
            let current = self
                .previous
                .get(&result.agent)
                .and_then(|observed| observed.session.conversation_id.clone());
            let Some(draft) = self.input_drafts.get_mut(&result.agent) else {
                continue;
            };
            if current != result.conversation_id || draft.settled() {
                continue;
            }
            draft.generated = Some(result.title);
            draft.naming = false;
            draft.clear();
        }
    }

    fn terminal_event(&mut self, id: AgentId, force_full: bool) -> Option<Event> {
        self.terminal_event_with_snapshot(id, force_full, None)
    }

    fn terminal_event_with_snapshot(
        &mut self,
        id: AgentId,
        force_full: bool,
        terminal: Option<TerminalSnapshot>,
    ) -> Option<Event> {
        let agent = self.agents.snapshot(id)?;
        let terminal = terminal.or_else(|| self.agents.terminal_snapshot(id))?;
        let old_basis = self.frame_bases.remove(&id);
        let previous = old_basis.as_ref().filter(|_| !force_full);
        let payload = previous
            .and_then(|basis| {
                basis
                    .snapshot
                    .diff(&terminal)
                    .map(|diff| FramePayload::Diff {
                        base_sequence: basis.sequence,
                        diff,
                    })
            })
            .unwrap_or_else(|| FramePayload::Full(terminal.clone()));

        let acknowledged = old_basis.as_ref().and_then(|basis| basis.acknowledged);
        let sequence = self.state.next_terminal_sequence(id).ok()?;
        let event = match payload {
            FramePayload::Full(snapshot) => Event::TerminalFull(TerminalFull {
                agent_id: id,
                output_generation: agent.output_generation,
                sequence,
                snapshot,
            }),
            FramePayload::Diff {
                base_sequence,
                diff,
            } => Event::TerminalDiff(TerminalDiff {
                agent_id: id,
                output_generation: agent.output_generation,
                base_sequence,
                sequence,
                diff,
            }),
        };
        self.frame_bases.insert(
            id,
            FrameBasis {
                sequence,
                acknowledged,
                snapshot: terminal,
            },
        );
        Some(event)
    }

    fn terminal_viewport(&self, id: AgentId, requested: usize) -> Option<Event> {
        let snapshot = self.agents.viewport(id, requested)?;
        let scrollback = snapshot.state.scrollback.position;
        Some(Event::TerminalViewport(TerminalViewport {
            agent_id: id,
            requested_scrollback: requested,
            scrollback,
            snapshot,
        }))
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
        self.apply_generated_names();
        let dirty = self.agents.drain_dirty();
        let attached = self.state.attachment().is_some();
        let now = Instant::now();
        let mut changed = self.apply_git_result(now);
        let mut switched = Vec::new();
        let mut switched_ids = BTreeSet::new();
        let mut terminals = Vec::new();
        for result in self.agents.poll() {
            let Ok(snapshot) = result else {
                continue;
            };
            let previous = self.previous.get(&snapshot.id).cloned();
            let id_changed = previous.as_ref().is_some_and(|old| {
                old.session.conversation_id.is_some()
                    && snapshot.conversation_id.is_some()
                    && old.session.conversation_id != snapshot.conversation_id
            });
            let mut archived = None;
            let mut reactivated_id = None;
            if id_changed {
                let new_id = snapshot.conversation_id.as_deref().unwrap();
                if let Some(index) = self
                    .archived
                    .iter()
                    .position(|item| item.conversation_id == new_id)
                {
                    let reactivated = self.archived.remove(index);
                    reactivated_id = Some(reactivated.conversation_id);
                    self.input_drafts
                        .insert(snapshot.id, InputDraft::named(reactivated.title));
                } else {
                    let draft = self
                        .history
                        .first_user_message(snapshot.kind, new_id, &snapshot.launch_directory)
                        .map(InputDraft::from_prompt)
                        .unwrap_or_default();
                    self.input_drafts.insert(snapshot.id, draft);
                    self.request_name_for(snapshot.id, Some(new_id.to_owned()));
                }
                if let Some(old) = previous.as_ref()
                    && let (Some(conversation_id), Some(title)) = (
                        old.session.conversation_id.clone(),
                        old.conversation_title.clone(),
                    )
                {
                    let conversation = ArchivedConversation {
                        conversation_id,
                        title,
                        kind: old.session.kind,
                        launch_directory: old.session.launch_directory.clone(),
                    };
                    self.archived
                        .retain(|item| item.conversation_id != conversation.conversation_id);
                    self.archived.insert(0, conversation.clone());
                    archived = Some(conversation);
                }
            }
            let screen_changed = previous.as_ref().is_none_or(|previous| {
                previous.session.output_generation != snapshot.output_generation
            });
            let terminal = screen_changed
                .then(|| self.agents.terminal_snapshot(snapshot.id))
                .flatten();
            let observed = self.observe_with_terminal(snapshot.clone(), terminal.as_ref());
            if id_changed {
                changed.remove(&snapshot.id);
                switched_ids.insert(snapshot.id);
                switched.push((snapshot.clone(), archived, reactivated_id));
            } else if previous.as_ref() != Some(&observed) {
                changed.insert(snapshot.id);
            }
            self.previous.insert(snapshot.id, observed);
            if attached
                && (dirty.contains(&snapshot.id)
                    || previous.as_ref().is_some_and(|old| {
                        old.session.output_generation != snapshot.output_generation
                    }))
            {
                terminals.push((snapshot.id, terminal));
            }
        }
        changed.extend(self.schedule_git_refresh(now));
        for id in switched_ids {
            changed.remove(&id);
        }
        if !changed.is_empty() || !switched.is_empty() {
            self.state.metadata_changed();
        }
        let revision = self.state.revision();
        let mut events = changed
            .into_iter()
            .filter_map(|id| self.agents.snapshot(id))
            .map(|snapshot| Event::AgentChanged {
                revision,
                agent: Box::new(self.agent_snapshot(snapshot)),
            })
            .collect::<Vec<_>>();
        events.extend(
            switched
                .into_iter()
                .map(
                    |(snapshot, archived, reactivated_id)| Event::ConversationSwitched {
                        revision,
                        agent: Box::new(self.agent_snapshot(snapshot)),
                        archived,
                        reactivated_id,
                    },
                ),
        );
        for (id, terminal) in terminals {
            if let Some(event) = self.terminal_event_with_snapshot(id, false, terminal) {
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
        let mut next_session_poll = Instant::now();
        while !self.stopping {
            if self.config.handle_signals && SHUTDOWN_REQUESTED.swap(false, Ordering::SeqCst) {
                self.begin_shutdown();
                continue;
            }
            self.accept_connections()?;
            // Input still wakes the coordinator immediately, but terminal observation is paced
            // below. A noisy child therefore cannot turn each PTY read into its own frame.
            let timeout = next_session_poll.saturating_duration_since(Instant::now());
            match self.input_rx.recv_timeout(timeout) {
                Ok(input) => self.handle_input(input),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(input) = self.input_rx.try_recv() {
                self.handle_input(input);
            }
            let now = Instant::now();
            if session_poll_due(&mut next_session_poll, now) {
                // Keep output notifications coalesced until the frame boundary. Clearing before
                // polling lets output that arrives during the poll schedule the following frame.
                self.output_wake.clear();
                self.poll_sessions();
            }
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
            // Nothing to do: the wake only records that output is pending for the next frame
            // boundary. `OutputWake` keeps later reads coalesced until that boundary.
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
                    runtime.agents.is_empty() && runtime.archived.is_empty()
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
                        agent: Box::new(runtime.agent_snapshot(snapshot.clone())),
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
            Request::ArchiveAgent {
                lease_token,
                agent_id,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let now = self.now_ms();
                let runtime = self.sessions.get_mut(&session_id).unwrap();
                let conversation = runtime.archive(agent_id, now)?;
                let mut outcome = Outcome::new(Response::Ok);
                outcome.events.push((
                    id,
                    Event::AgentArchived {
                        revision: runtime.state.revision(),
                        agent_id,
                        conversation,
                    },
                ));
                Ok(outcome)
            }
            Request::ResumeArchived {
                lease_token,
                conversation_id,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let now = self.now_ms();
                let runtime = self.sessions.get_mut(&session_id).unwrap();
                let snapshot = runtime.resume_archived(&conversation_id, now, &self.config)?;
                let mut outcome = Outcome::new(Response::Ok);
                outcome.events.push((
                    id,
                    Event::ArchivedResumed {
                        revision: runtime.state.revision(),
                        conversation_id,
                        agent: Box::new(runtime.agent_snapshot(snapshot.clone())),
                    },
                ));
                if let Some(event) = runtime.full_terminal(snapshot.id) {
                    outcome.events.push((id, event));
                }
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
                let runtime = self.sessions.get_mut(&session_id).unwrap();
                runtime.state.select_agent(agent_id, now)?;
                runtime.force_git_refresh(agent_id);
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
                runtime.record_key(agent_id, &event);
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
                runtime.record_paste(agent_id, &text);
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
            Request::TerminalViewport {
                lease_token,
                agent_id,
                scrollback,
            } => {
                let session_id = self.attached_session(id, &lease_token)?;
                let event = self.sessions[&session_id]
                    .terminal_viewport(agent_id, scrollback)
                    .ok_or_else(agent_not_found)?;
                let mut outcome = Outcome::new(Response::Ok);
                outcome.events.push((id, event));
                Ok(outcome)
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
            if runtime.agents.is_empty() && runtime.archived.is_empty() {
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

fn session_poll_due(next: &mut Instant, now: Instant) -> bool {
    if now < *next {
        return false;
    }
    *next = now + EVENT_TICK;
    true
}

const fn git_refresh_interval(selected: bool) -> Duration {
    if selected {
        SELECTED_GIT_REFRESH_INTERVAL
    } else {
        BACKGROUND_GIT_REFRESH_INTERVAL
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

fn terminal_frame(envelope: &Envelope) -> Option<(AgentId, bool)> {
    match &envelope.message {
        Message::Event(Event::TerminalFull(frame)) => Some((frame.agent_id, true)),
        Message::Event(Event::TerminalDiff(frame)) => Some((frame.agent_id, false)),
        Message::Event(Event::TerminalViewport(frame)) => Some((frame.agent_id, true)),
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
    use std::{fs, io::Write, process::Command, time::SystemTime};

    use super::*;
    use crate::{
        CursorStyle,
        naming::TitleResult,
        protocol::{Hello, HostTerminalCapabilities, PROTOCOL_VERSION, TerminalDiff, TerminalFull},
        terminal_backend::Vt100Backend,
        terminal_model::{TerminalBackend, TerminalSize},
    };

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

    fn run_git(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?} failed");
    }

    #[test]
    fn relative_agent_directories_are_canonicalized_at_the_server_edge() {
        assert_eq!(
            canonicalize_agent_directory(Path::new(".")).unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }

    #[test]
    fn session_polling_advances_at_most_once_per_frame_interval() {
        let start = Instant::now();
        let mut next = start;

        assert!(session_poll_due(&mut next, start));
        assert_eq!(next, start + EVENT_TICK);
        assert!(!session_poll_due(
            &mut next,
            start + EVENT_TICK - Duration::from_millis(1)
        ));
        assert!(session_poll_due(&mut next, start + EVENT_TICK));
        assert_eq!(next, start + EVENT_TICK + EVENT_TICK);
    }

    #[test]
    fn selected_git_refreshes_more_often_than_background_git() {
        assert_eq!(git_refresh_interval(true), Duration::from_secs(1));
        assert_eq!(git_refresh_interval(false), Duration::from_secs(5));
    }

    #[test]
    fn fresh_git_context_is_shared_only_with_the_same_directory() {
        let (runtime, now, context) = runtime_with_cached_repository_context();

        assert_eq!(
            runtime
                .cached_git_context(
                    Path::new("/repo"),
                    SELECTED_GIT_REFRESH_INTERVAL,
                    now + Duration::from_millis(999),
                )
                .and_then(|cached| cached.context),
            Some(context)
        );
        assert!(
            runtime
                .cached_git_context(
                    Path::new("/repo"),
                    SELECTED_GIT_REFRESH_INTERVAL,
                    now + SELECTED_GIT_REFRESH_INTERVAL,
                )
                .is_none()
        );
        // A subdirectory is probed on its own rather than borrowing the checkout root's answer,
        // because a directory inside a repository can belong to a different worktree.
        assert!(
            runtime
                .cached_git_context(
                    Path::new("/repo/crates/svarm"),
                    SELECTED_GIT_REFRESH_INTERVAL,
                    now,
                )
                .is_none()
        );
    }

    #[test]
    fn a_nested_worktree_does_not_inherit_the_parent_repository_context() {
        let (runtime, now, _) = runtime_with_cached_repository_context();

        assert!(
            runtime
                .cached_git_context(
                    Path::new("/repo/.claude/worktrees/feature"),
                    SELECTED_GIT_REFRESH_INTERVAL,
                    now,
                )
                .is_none()
        );
    }

    fn runtime_with_cached_repository_context() -> (SessionRuntime, Instant, GitContext) {
        let state = ServerSessionState::new(SessionId(1), 24, 80, None, 0).unwrap();
        let mut runtime = SessionRuntime::new(state, None);
        let now = Instant::now();
        let context = GitContext {
            branch: "main".into(),
            worktree: PathBuf::from("/repo"),
            linked: false,
            additions: 0,
            deletions: 0,
            ahead: None,
            behind: None,
        };
        runtime.git_cache.insert(
            PathBuf::from("/repo"),
            CachedGitContext {
                checked_at: now,
                context: Some(context.clone()),
            },
        );
        (runtime, now, context)
    }

    #[test]
    fn first_submitted_message_becomes_the_thread_title() {
        let mut draft = InputDraft::default();
        for character in "  refactor   the sidebar  ".chars() {
            draft.apply_key(&KeyInput {
                code: KeyCode::Character(character),
                modifiers: Default::default(),
            });
        }
        draft.apply_key(&KeyInput {
            code: KeyCode::Enter,
            modifiers: Default::default(),
        });

        assert_eq!(draft.title(), Some("refactor the sidebar"));

        draft.apply_paste("a later message");
        assert_eq!(draft.title(), Some("refactor the sidebar"));
    }

    #[test]
    fn empty_submissions_keep_the_thread_unnamed() {
        let mut draft = InputDraft::default();
        draft.apply_key(&KeyInput {
            code: KeyCode::Enter,
            modifiers: Default::default(),
        });
        assert_eq!(draft.title(), None);
    }

    #[test]
    fn slash_commands_keep_the_thread_unnamed_for_the_next_real_prompt() {
        let mut draft = InputDraft::default();
        for character in "/new".chars() {
            draft.apply_key(&KeyInput {
                code: KeyCode::Character(character),
                modifiers: Default::default(),
            });
        }
        draft.apply_key(&KeyInput {
            code: KeyCode::Enter,
            modifiers: Default::default(),
        });
        assert_eq!(draft.title(), None);

        draft.apply_paste("Actual task\n");
        assert_eq!(draft.title(), Some("Actual task"));
    }

    #[test]
    fn submissions_are_collected_until_a_name_is_generated() {
        let mut draft = InputDraft::default();
        assert!(draft.apply_paste("first task\n"));
        assert!(draft.apply_paste("second task\n"));
        assert!(!draft.apply_paste("/status\n"));
        assert_eq!(draft.prompts, vec!["first task", "second task"]);
        assert_eq!(draft.title(), Some("first task"));

        draft.generated = Some("Sidebar work".into());
        assert_eq!(draft.title(), Some("Sidebar work"));
        // A named conversation stops observing input entirely.
        assert!(!draft.apply_paste("third task\n"));
        assert_eq!(draft.prompts, vec!["first task", "second task"]);
    }

    #[test]
    fn a_resumed_conversation_keeps_its_name_and_asks_for_no_other() {
        let mut draft = InputDraft::named("Archived work".into());

        assert!(!draft.apply_paste("a new message\n"));
        assert_eq!(draft.title(), Some("Archived work"));
    }

    #[test]
    fn a_generated_name_replaces_the_first_message_in_agent_updates() {
        let cwd = std::env::current_dir().unwrap();
        let state = ServerSessionState::new(SessionId(1), 24, 80, None, 0).unwrap();
        let mut runtime = SessionRuntime::with_namer(
            state,
            None,
            TitleNamer::fixed("printf", &["Generated sidebar name\\n"]),
        );
        let config = ServerConfig::new(cwd.join("unused.sock"), "test")
            .with_test_agent_command("sh", &["-c", "sleep 1"]);
        let snapshot = runtime.spawn(AgentKind::Codex, &cwd, 0, &config).unwrap();

        runtime.record_paste(snapshot.id, "fix teh sidbar truncation pls\n");
        runtime.poll_events();
        assert_eq!(
            runtime.snapshot().agents[0].conversation_title.as_deref(),
            Some("fix teh sidbar truncation pls"),
            "the first message names the conversation until a generated name arrives"
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        let renamed = loop {
            let events = runtime.poll_events();
            if let Some(event) = events.into_iter().find(|event| {
                matches!(event, Event::AgentChanged { agent, .. }
                    if agent.conversation_title.as_deref() == Some("Generated sidebar name"))
            }) {
                break event;
            }
            assert!(Instant::now() < deadline, "no generated name arrived");
            thread::sleep(Duration::from_millis(5));
        };

        assert!(matches!(renamed, Event::AgentChanged { .. }));
        assert_eq!(
            runtime.snapshot().agents[0].conversation_title.as_deref(),
            Some("Generated sidebar name")
        );
        runtime.agents.stop_all();
    }

    #[test]
    fn a_name_for_an_abandoned_conversation_is_discarded() {
        let cwd = std::env::current_dir().unwrap();
        let state = ServerSessionState::new(SessionId(1), 24, 80, None, 0).unwrap();
        let mut runtime = SessionRuntime::new(state, None);
        let config = ServerConfig::new(cwd.join("unused.sock"), "test")
            .with_test_agent_command("sh", &["-c", "sleep 1"]);
        let snapshot = runtime.spawn(AgentKind::Codex, &cwd, 0, &config).unwrap();
        runtime.record_paste(snapshot.id, "first task\n");
        runtime.poll_events();
        runtime
            .previous
            .get_mut(&snapshot.id)
            .unwrap()
            .session
            .conversation_id = Some("129ff1d3-375e-7a72-a176-c47497827e49".into());

        // The generator answers for the conversation the agent has since left.
        runtime.namer.deliver(TitleResult {
            agent: snapshot.id,
            conversation_id: Some("019ff1d3-375e-7a72-a176-c47497827e49".into()),
            title: "Stale name".into(),
        });
        runtime.apply_generated_names();

        assert_eq!(
            runtime.snapshot().agents[0].conversation_title.as_deref(),
            Some("first task")
        );
        runtime.agents.stop_all();
    }

    #[test]
    fn runtime_publishes_the_first_message_title_with_agent_updates() {
        let cwd = std::env::current_dir().unwrap();
        let state = ServerSessionState::new(SessionId(1), 24, 80, None, 0).unwrap();
        let mut runtime = SessionRuntime::new(state, None);
        let config = ServerConfig::new(cwd.join("unused.sock"), "test")
            .with_test_agent_command("sh", &["-c", "sleep 1"]);
        let snapshot = runtime.spawn(AgentKind::Codex, &cwd, 0, &config).unwrap();

        runtime.record_paste(snapshot.id, "name this thread");
        runtime.record_key(
            snapshot.id,
            &KeyInput {
                code: KeyCode::Enter,
                modifiers: Default::default(),
            },
        );
        let events = runtime.poll_events();

        assert_eq!(
            runtime.snapshot().agents[0].conversation_title.as_deref(),
            Some("name this thread")
        );
        assert!(events.iter().any(|event| matches!(
            event,
            Event::AgentChanged { agent, .. }
                if agent.conversation_title.as_deref() == Some("name this thread")
        )));
        runtime.agents.stop_all();
    }

    #[test]
    fn action_required_titles_outrank_a_non_blocked_screen_claim() {
        let cwd = std::env::current_dir().unwrap();
        let state = ServerSessionState::new(SessionId(1), 24, 80, None, 0).unwrap();
        let mut runtime = SessionRuntime::new(state, None);
        let session = SessionSnapshot {
            id: AgentId(1),
            kind: AgentKind::Codex,
            launch_directory: cwd,
            status: SessionStatus::Running,
            output_generation: 1,
            read_error: None,
            exit: None,
            conversation_id: None,
        };

        let mut backend = Vt100Backend::new(TerminalSize::new(24, 80), 0);
        backend.process(
            "\x1b]2;[ ! ] Action Required | Task\x07\x1b[20;1H❯\r\n? for shortcuts".as_bytes(),
        );
        let screen = backend.snapshot(CursorStyle::default(), backend.modes(false, false));
        assert!(matches!(
            recognition::recognize(AgentKind::Codex, &screen),
            ScreenRecognition::Recognized(evidence) if evidence.claim == AgentActivity::Idle
        ));

        let observed = runtime.observe_with_terminal(session, Some(&screen));
        assert_eq!(observed.activity, AgentActivity::Blocked);
    }

    #[test]
    fn runtime_archives_and_resumes_only_compact_conversation_metadata() {
        let cwd = std::env::current_dir().unwrap();
        let state = ServerSessionState::new(SessionId(1), 24, 80, None, 0).unwrap();
        let mut runtime = SessionRuntime::new(state, None);
        let config = ServerConfig::new(cwd.join("unused.sock"), "test")
            .with_test_agent_command("sh", &["-c", "sleep 1"]);
        let snapshot = runtime.spawn(AgentKind::Codex, &cwd, 0, &config).unwrap();
        assert_eq!(
            runtime.archive(snapshot.id, 1).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
        let id = "019ff1d3-375e-7a72-a176-c47497827e49";
        let observed = runtime.previous.get_mut(&snapshot.id).unwrap();
        observed.session.conversation_id = Some(id.into());
        observed.conversation_title = Some("Keep this title".into());

        let archived = runtime.archive(snapshot.id, 1).unwrap();
        assert!(runtime.agents.is_empty());
        assert_eq!(archived.conversation_id, id);
        assert_eq!(runtime.snapshot().archived, vec![archived.clone()]);

        let resumed = runtime.resume_archived(id, 2, &config).unwrap();
        let snapshot = runtime.snapshot();
        assert!(snapshot.archived.is_empty());
        assert_eq!(snapshot.agents[0].id, resumed.id);
        assert_eq!(snapshot.agents[0].conversation_id.as_deref(), Some(id));
        assert_eq!(
            snapshot.agents[0].conversation_title.as_deref(),
            Some("Keep this title")
        );
        runtime.agents.stop_all();
    }

    #[test]
    fn provider_conversation_switch_archives_the_previous_named_id() {
        let directory = temp_dir();
        let marker = directory.join("switch-now");
        let old_id = "019ff1d3-375e-7a72-a176-c47497827e49";
        let new_id = "129ff1d3-375e-7a72-a176-c47497827e49";
        let script = format!(
            "printf '\\033]2;Ready | {old_id}\\a'; while [ ! -f '{}' ]; do sleep 0.01; done; printf '\\033]2;Ready | {new_id}\\a'; sleep 1",
            marker.display()
        );
        let state = ServerSessionState::new(SessionId(1), 24, 80, None, 0).unwrap();
        let mut runtime = SessionRuntime::new(state, None);
        let config = ServerConfig::new(directory.join("unused.sock"), "test")
            .with_test_agent_command("sh", &["-c", &script]);
        let agent = runtime
            .spawn(AgentKind::Codex, &directory, 0, &config)
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.snapshot().agents[0].conversation_id.as_deref() != Some(old_id)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
            runtime.poll_events();
        }
        assert_eq!(
            runtime.snapshot().agents[0].conversation_id.as_deref(),
            Some(old_id)
        );
        runtime.record_paste(agent.id, "First task");
        runtime.record_key(
            agent.id,
            &KeyInput {
                code: KeyCode::Enter,
                modifiers: Default::default(),
            },
        );
        runtime.poll_events();

        fs::write(&marker, "go").unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let switched = loop {
            let events = runtime.poll_events();
            if let Some(event) = events
                .into_iter()
                .find(|event| matches!(event, Event::ConversationSwitched { .. }))
            {
                break event;
            }
            assert!(Instant::now() < deadline, "conversation did not switch");
            thread::sleep(Duration::from_millis(5));
        };

        assert!(matches!(
            switched,
            Event::ConversationSwitched {
                archived: Some(ArchivedConversation {
                    conversation_id,
                    title,
                    ..
                }),
                ..
            } if conversation_id == old_id && title == "First task"
        ));
        assert_eq!(
            runtime.snapshot().agents[0].conversation_id.as_deref(),
            Some(new_id)
        );
        runtime.agents.stop_all();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn external_conversation_switch_uses_its_first_prompt_for_naming() {
        let directory = temp_dir();
        let history_home = temp_dir();
        let marker = directory.join("switch-now");
        let old_id = "019ff1d3-375e-7a72-a176-c47497827e49";
        let new_id = "129ff1d3-375e-7a72-a176-c47497827e49";
        let history_path =
            history_home.join(format!(".codex/sessions/2026/08/13/rollout-{new_id}.jsonl"));
        fs::create_dir_all(history_path.parent().unwrap()).unwrap();
        fs::write(
            history_path,
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"fix the resumed workspace\"}}\n",
        )
        .unwrap();
        let script = format!(
            "printf '\\033]2;Ready | {old_id}\\a'; while [ ! -f '{}' ]; do sleep 0.01; done; printf '\\033]2;Ready | {new_id}\\a'; sleep 1",
            marker.display()
        );
        let state = ServerSessionState::new(SessionId(1), 24, 80, None, 0).unwrap();
        let mut runtime = SessionRuntime::with_namer_and_history(
            state,
            None,
            TitleNamer::fixed("printf", &["Generated resumed name\\n"]),
            ConversationHistory::from_home(&history_home),
        );
        let config = ServerConfig::new(directory.join("unused.sock"), "test")
            .with_test_agent_command("sh", &["-c", &script]);
        runtime
            .spawn(AgentKind::Codex, &directory, 0, &config)
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.snapshot().agents[0].conversation_id.as_deref() != Some(old_id)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
            runtime.poll_events();
        }
        assert_eq!(
            runtime.snapshot().agents[0].conversation_id.as_deref(),
            Some(old_id)
        );

        fs::write(&marker, "go").unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let switched = loop {
            let events = runtime.poll_events();
            if let Some(event) = events
                .into_iter()
                .find(|event| matches!(event, Event::ConversationSwitched { .. }))
            {
                break event;
            }
            assert!(Instant::now() < deadline, "conversation did not switch");
            thread::sleep(Duration::from_millis(5));
        };
        assert!(matches!(
            switched,
            Event::ConversationSwitched { agent, .. }
                if agent.conversation_title.as_deref() == Some("fix the resumed workspace")
        ));

        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.snapshot().agents[0].conversation_title.as_deref()
            != Some("Generated resumed name")
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
            runtime.poll_events();
        }
        assert_eq!(
            runtime.snapshot().agents[0].conversation_title.as_deref(),
            Some("Generated resumed name")
        );
        runtime.agents.stop_all();
        fs::remove_dir_all(directory).unwrap();
        fs::remove_dir_all(history_home).unwrap();
    }

    #[test]
    fn scrollback_is_retained_without_entering_the_continuous_frame_basis() {
        let cwd = std::env::current_dir().unwrap();
        let state = ServerSessionState::new(SessionId(1), 5, 30, None, 0).unwrap();
        let mut runtime = SessionRuntime::new(state, None);
        let config = ServerConfig::new(cwd.join("unused.sock"), "test").with_test_agent_command(
            "sh",
            &[
                "-c",
                "i=1; while [ $i -le 20 ]; do printf 'line-%02d\\r\\n' \"$i\"; i=$((i+1)); done; sleep 1",
            ],
        );
        let snapshot = runtime.spawn(AgentKind::Codex, &cwd, 0, &config).unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime
            .agents
            .with_terminal(snapshot.id, |screen| screen.state.scrollback.retained_rows)
            .unwrap_or(0)
            == 0
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }

        runtime.terminal_event(snapshot.id, true).unwrap();
        assert_eq!(
            runtime.frame_bases[&snapshot.id].snapshot.cells.len(),
            5 * 30
        );
        let Event::TerminalViewport(viewport) =
            runtime.terminal_viewport(snapshot.id, usize::MAX).unwrap()
        else {
            panic!("expected terminal viewport");
        };
        assert!(viewport.scrollback > 0);
        assert!(viewport.snapshot.contents().contains("line-01"));

        runtime.agents.stop_all();
    }

    #[test]
    fn runtime_snapshots_carry_activity_and_git_context() {
        let cwd = std::env::current_dir().unwrap();
        let expected_git = git::context(&cwd).unwrap();
        let state = ServerSessionState::new(SessionId(1), 24, 80, None, 0).unwrap();
        let mut runtime = SessionRuntime::new(state, None);
        let config = ServerConfig::new(cwd.join("unused.sock"), "test").with_test_agent_command(
            "sh",
            &[
                "-c",
                r"printf '\033]2;[ ! ] Action Required | Initial conversation\a\033[20;1HWorking  esc to interrupt'; sleep 0.3; printf '\033]2;⠙ Working | Refactor sidebar\a'; sleep 0.2; printf '\033]2;Ready | Refactor sidebar\a\033[20;1H\033[2KReady\033[21;1H\033[2K? for shortcuts'; sleep 2",
            ],
        );
        runtime.spawn(AgentKind::Codex, &cwd, 0, &config).unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            runtime.poll_events();
            let snapshot = runtime.snapshot();
            let agent = &snapshot.agents[0];
            if agent.activity == AgentActivity::Working {
                assert_eq!(agent.conversation_title, None);
                assert_eq!(
                    agent
                        .recognition
                        .as_ref()
                        .map(|evidence| evidence.rule.as_str()),
                    Some("codex.active-turn")
                );
                let git = agent.git.as_ref().unwrap();
                assert!(!git.branch.is_empty());
                assert_eq!(git.worktree, expected_git.worktree);
                break;
            }
            assert!(Instant::now() < deadline, "agent metadata was not observed");
            thread::sleep(Duration::from_millis(5));
        }

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            runtime.poll_events();
            let snapshot = runtime.snapshot();
            let agent = &snapshot.agents[0];
            if agent.activity == AgentActivity::Idle {
                assert!(agent.completed_generation > 0);
                assert_eq!(agent.completed_generation, agent.output_generation);
                break;
            }
            assert!(Instant::now() < deadline, "completion was not observed");
            thread::sleep(Duration::from_millis(5));
        }

        runtime.agents.stop_all();
    }

    #[test]
    fn runtime_emits_agent_change_when_the_git_branch_changes() {
        let directory = temp_dir();
        run_git(&directory, &["init", "-q", "-b", "main"]);
        run_git(
            &directory,
            &[
                "-c",
                "user.name=Svarm Test",
                "-c",
                "user.email=svarm@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );

        let state = ServerSessionState::new(SessionId(1), 24, 80, None, 0).unwrap();
        let mut runtime = SessionRuntime::new(state, None);
        let config = ServerConfig::new(directory.join("unused.sock"), "test")
            .with_test_agent_command("sh", &["-c", "exec sleep 60"]);
        let spawned = runtime
            .spawn(AgentKind::Codex, &directory, 0, &config)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            runtime.poll_events();
            if runtime.snapshot().agents[0]
                .git
                .as_ref()
                .is_some_and(|git| git.branch == "main")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "initial Git context was not observed"
            );
            thread::sleep(Duration::from_millis(5));
        }

        run_git(&directory, &["switch", "-q", "-c", "feature/sidebar"]);
        runtime.force_git_refresh(spawned.id);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let events = runtime.poll_events();
            if events.iter().any(|event| {
                matches!(
                    event,
                    Event::AgentChanged { agent, .. }
                        if agent.git.as_ref().is_some_and(|git| git.branch == "feature/sidebar")
                )
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "updated Git branch was not observed"
            );
            thread::sleep(Duration::from_millis(5));
        }

        runtime.agents.stop_all();
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_reports_the_worktree_the_agent_moved_into() {
        let directory = temp_dir();
        let linked = directory.join("linked");
        run_git(&directory, &["init", "-q", "-b", "main"]);
        run_git(
            &directory,
            &[
                "-c",
                "user.name=Svarm Test",
                "-c",
                "user.email=svarm@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );
        run_git(
            &directory,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked-branch",
                linked.to_str().unwrap(),
            ],
        );

        let state = ServerSessionState::new(SessionId(1), 24, 80, None, 0).unwrap();
        let mut runtime = SessionRuntime::new(state, None);
        // The agent launches in the main checkout and then enters the linked worktree itself,
        // exactly as a coding agent that switches worktrees does.
        let enter = format!("cd '{}' && exec sleep 60", linked.display());
        let config = ServerConfig::new(directory.join("unused.sock"), "test")
            .with_test_agent_command("sh", &["-c", &enter]);
        let spawned = runtime
            .spawn(AgentKind::Codex, &directory, 0, &config)
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        runtime.force_git_refresh(spawned.id);
        let observed = loop {
            let _ = runtime.poll_events();
            let agent = runtime.snapshot().agents.remove(0);
            if agent.git.as_ref().is_some_and(|git| git.linked) || Instant::now() >= deadline {
                break agent;
            }
            thread::sleep(Duration::from_millis(10));
        };

        let git = observed.git.expect("git context");
        assert_eq!(git.branch, "linked-branch");
        assert_eq!(git.worktree, linked.canonicalize().unwrap());
        assert!(git.linked);
        assert_eq!(
            observed.working_directory,
            Some(linked.canonicalize().unwrap())
        );

        runtime.agents.stop_all();
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_worktree_nested_in_the_repository_is_not_reported_as_the_main_checkout() {
        let directory = temp_dir();
        // Coding agents create their worktrees inside the repository they came from, so the
        // nested path sits under the main checkout's root and must still be probed on its own.
        let nested = directory.join(".claude").join("worktrees").join("nested");
        run_git(&directory, &["init", "-q", "-b", "main"]);
        run_git(
            &directory,
            &[
                "-c",
                "user.name=Svarm Test",
                "-c",
                "user.email=svarm@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );
        run_git(
            &directory,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "nested-branch",
                nested.to_str().unwrap(),
            ],
        );

        let state = ServerSessionState::new(SessionId(1), 24, 80, None, 0).unwrap();
        let mut runtime = SessionRuntime::new(state, None);
        let enter = format!("cd '{}' && exec sleep 60", nested.display());
        let mover = ServerConfig::new(directory.join("unused.sock"), "test")
            .with_test_agent_command("sh", &["-c", &enter]);
        let spawned = runtime
            .spawn(AgentKind::Codex, &directory, 0, &mover)
            .unwrap();

        // Spawned second so it is the selected agent and is therefore probed first: the main
        // checkout's context reaches the cache before the agent that left it is ever looked at.
        let resident = ServerConfig::new(directory.join("unused.sock"), "test")
            .with_test_agent_command("sh", &["-c", "exec sleep 60"]);
        runtime
            .spawn(AgentKind::Codex, &directory, 0, &resident)
            .unwrap();

        // Shorter than the background refresh interval on purpose. Borrowing the main checkout's
        // context corrected itself once that entry went stale, which hid the defect; the nested
        // worktree has to be reported without waiting for an expiry.
        let deadline = Instant::now() + BACKGROUND_GIT_REFRESH_INTERVAL / 2;
        let observed = loop {
            let _ = runtime.poll_events();
            let agent = runtime
                .snapshot()
                .agents
                .into_iter()
                .find(|agent| agent.id == spawned.id)
                .expect("moved agent");
            if agent.git.as_ref().is_some_and(|git| git.linked) || Instant::now() >= deadline {
                break agent;
            }
            thread::sleep(Duration::from_millis(10));
        };

        let git = observed.git.expect("git context");
        assert_eq!(git.branch, "nested-branch");
        assert_eq!(git.worktree, nested.canonicalize().unwrap());
        assert!(git.linked);

        runtime.agents.stop_all();
        fs::remove_dir_all(directory).unwrap();
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

        let _ = client.request(Request::DetachSession { lease_token });
        drop(client);
        let mut reattached = Client::connect(&socket, ConnectionRole::Interactive);
        let (_, attached) = reattached.request(Request::AttachSession {
            session_id,
            rows: 10,
            cols: 40,
            palette: None,
            takeover: false,
        });
        let reattached_lease = match attached {
            Response::Attached { lease_token, .. } => lease_token,
            other => panic!("unexpected attach response: {other:?}"),
        };
        let restored = reattached.event_until(|event| {
            matches!(event, Event::SvarmSessionSnapshot(snapshot) if snapshot.agents.len() == 2)
        });
        let Event::SvarmSessionSnapshot(restored) = restored else {
            unreachable!()
        };
        assert_eq!(restored.agents[0].launch_directory, first);
        assert_eq!(restored.agents[1].launch_directory, second);
        let _ = reattached.request(Request::StopAttachedSession {
            lease_token: reattached_lease,
        });
        drop(reattached);
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
        let first = client.event_until(|event| terminal_event_contains(event, "before"));
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
        let _ = reattached.event_until(|event| terminal_event_contains(event, "after"));
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
        assert!(
            matches!(full, Event::TerminalFull(frame) if frame.snapshot.size() == TerminalSize::new(10, 40))
        );
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
        let _ = reattached.event_until(|event| terminal_event_contains(event, "finished"));
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
        let mut backend = Vt100Backend::new(TerminalSize::new(5, 30), 100);
        backend
            .process(b"\x1b[?2004h\x1b[?1000h\x1b[31mprimary\x1b[?1049h\x1b[2J\x1b[32malternate");
        let full = backend.snapshot(CursorStyle::default(), backend.modes(false, false));
        let mut client = full.clone();
        assert_eq!(client, full);

        backend.process(b"\x1b[?1049l\x1b[2;3H\x1b[34msecond\x1b[?1000l\x1b[?1003h");
        let next = backend.snapshot(CursorStyle::default(), backend.modes(false, false));
        let diff = full.diff(&next).unwrap();
        client.apply(&diff).unwrap();
        assert_eq!(client, next);
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

    #[test]
    fn terminal_queue_pressure_preserves_control_responses() {
        let queue = OutgoingQueue::new();
        for sequence in 1..=CONNECTION_QUEUE as u64 {
            assert!(matches!(
                queue.push(Box::new(terminal_envelope(false, sequence))),
                QueueResult::Queued
            ));
        }
        let response = Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(RequestId(99)),
            message: Message::Response(Response::Ok),
        };

        assert!(matches!(
            queue.push(Box::new(response)),
            QueueResult::NeedsFull(agent_id) if agent_id == AgentId::new(1)
        ));
        assert_eq!(queue.len(), 1);
        assert!(matches!(
            queue.pop().unwrap().message,
            Message::Response(Response::Ok)
        ));
    }

    #[test]
    fn viewport_replaces_obsolete_terminal_frames_under_pressure() {
        let queue = OutgoingQueue::new();
        for sequence in 1..=CONNECTION_QUEUE as u64 {
            assert!(matches!(
                queue.push(Box::new(terminal_envelope(false, sequence))),
                QueueResult::Queued
            ));
        }
        let viewport = Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: None,
            message: Message::Event(Event::TerminalViewport(TerminalViewport {
                agent_id: AgentId::new(1),
                requested_scrollback: 20,
                scrollback: 20,
                snapshot: TerminalSnapshot::blank(TerminalSize::new(1, 1)),
            })),
        };

        assert!(matches!(
            queue.push(Box::new(viewport)),
            QueueResult::Queued
        ));
        assert_eq!(queue.len(), 1);
        assert!(matches!(
            queue.pop().unwrap().message,
            Message::Event(Event::TerminalViewport(_))
        ));
    }

    fn terminal_event_contains(event: &Event, needle: &str) -> bool {
        match event {
            Event::TerminalFull(frame) => frame.snapshot.contents().contains(needle),
            Event::TerminalDiff(frame) => frame
                .diff
                .cells
                .iter()
                .fold(String::new(), |mut text, patch| {
                    patch
                        .cell
                        .contents
                        .with_str(|contents| text.push_str(contents));
                    text
                })
                .contains(needle),
            _ => false,
        }
    }

    fn terminal_envelope(full: bool, sequence: u64) -> Envelope {
        let sequence = TerminalSequence(sequence);
        let mut snapshot = TerminalSnapshot::blank(TerminalSize::new(1, 1));
        snapshot.cells[0].contents = "x".into();
        let message = if full {
            Message::Event(Event::TerminalFull(TerminalFull {
                agent_id: AgentId::new(1),
                output_generation: sequence.0,
                sequence,
                snapshot,
            }))
        } else {
            let blank = TerminalSnapshot::blank(TerminalSize::new(1, 1));
            let diff = blank.diff(&snapshot).unwrap();
            Message::Event(Event::TerminalDiff(TerminalDiff {
                agent_id: AgentId::new(1),
                output_generation: sequence.0,
                base_sequence: TerminalSequence(sequence.0.saturating_sub(1)),
                sequence,
                diff,
            }))
        };
        Envelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: None,
            message,
        }
    }
}
