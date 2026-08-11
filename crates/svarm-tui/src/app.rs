use std::path::PathBuf;

#[cfg(test)]
use svarm_agent::SessionSnapshot;
use svarm_agent::{
    AgentId, AgentKind, SessionStatus,
    protocol::{AgentSnapshot, SessionId, SessionSummary, SvarmSessionSnapshot},
    server_session::sort_session_summaries,
};

use crate::theme::ThemeName;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Mode {
    #[default]
    Terminal,
    Prefix,
    ChooseAgent,
    ConfirmClose,
    ConfirmQuit,
    Menu,
    Keybinds,
    Settings,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ExitIntent {
    #[default]
    None,
    Detach,
    StopSession,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupChoice {
    Session(SessionId),
    NewSession,
    Cancel,
}

pub(crate) struct SessionChooser {
    sessions: Vec<SessionSummary>,
    allow_new: bool,
    selected: usize,
}

impl SessionChooser {
    pub fn new(mut sessions: Vec<SessionSummary>, allow_new: bool) -> Self {
        sort_session_summaries(&mut sessions);
        Self {
            sessions,
            allow_new,
            selected: 0,
        }
    }

    pub fn select_next(&mut self) {
        let count = self.row_count();
        if count > 0 {
            self.selected = (self.selected + 1) % count;
        }
    }

    pub fn select_previous(&mut self) {
        let count = self.row_count();
        if count > 0 {
            self.selected = self.selected.checked_sub(1).unwrap_or(count - 1);
        }
    }

    pub fn confirm(&self) -> Option<StartupChoice> {
        if let Some(session) = self.sessions.get(self.selected) {
            Some(StartupChoice::Session(session.id))
        } else if self.allow_new && self.selected == self.sessions.len() {
            Some(StartupChoice::NewSession)
        } else {
            None
        }
    }

    pub fn select_new(&mut self) -> Option<StartupChoice> {
        if !self.allow_new {
            return None;
        }
        self.selected = self.sessions.len();
        Some(StartupChoice::NewSession)
    }

    pub const fn cancel(&self) -> StartupChoice {
        StartupChoice::Cancel
    }

    pub fn viewport_start(&self, visible_rows: usize) -> usize {
        if visible_rows == 0 {
            return self.selected;
        }
        self.selected
            .saturating_sub(visible_rows - 1)
            .min(self.row_count().saturating_sub(visible_rows))
    }

    pub fn sessions(&self) -> &[SessionSummary] {
        &self.sessions
    }

    pub const fn allow_new(&self) -> bool {
        self.allow_new
    }

    pub const fn selected(&self) -> usize {
        self.selected
    }

    pub fn row_count(&self) -> usize {
        self.sessions.len() + usize::from(self.allow_new)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum MenuItem {
    #[default]
    Detach,
    StopSession,
    Keybinds,
    Settings,
}

impl MenuItem {
    pub const ALL: [Self; 4] = [
        Self::Detach,
        Self::StopSession,
        Self::Keybinds,
        Self::Settings,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Detach => "Detach — agents keep running",
            Self::StopSession => "Stop session — terminates all agents",
            Self::Keybinds => "Keybinds",
            Self::Settings => "Settings",
        }
    }

    fn cycle(self, delta: isize) -> Self {
        let current = match self {
            Self::Detach => 0,
            Self::StopSession => 1,
            Self::Keybinds => 2,
            Self::Settings => 3,
        };
        let next = (current + delta).rem_euclid(Self::ALL.len() as isize) as usize;
        Self::ALL[next]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentState {
    id: AgentId,
    kind: AgentKind,
    status: SessionStatus,
    output_generation: u64,
    seen_generation: u64,
}

impl AgentState {
    #[cfg(test)]
    fn new(snapshot: &SessionSnapshot) -> Self {
        Self {
            id: snapshot.id,
            kind: snapshot.kind,
            status: snapshot.status,
            output_generation: snapshot.output_generation,
            seen_generation: snapshot.output_generation,
        }
    }

    fn from_remote(snapshot: &AgentSnapshot) -> Self {
        Self {
            id: snapshot.id,
            kind: snapshot.kind,
            status: snapshot.status,
            output_generation: snapshot.output_generation,
            seen_generation: snapshot.seen_generation,
        }
    }

    #[cfg(test)]
    fn update(&mut self, snapshot: &SessionSnapshot) -> bool {
        let changed =
            self.status != snapshot.status || self.output_generation != snapshot.output_generation;
        self.status = snapshot.status;
        self.output_generation = snapshot.output_generation;
        changed
    }

    fn update_remote(&mut self, snapshot: &AgentSnapshot) -> bool {
        let changed =
            self.status != snapshot.status || self.output_generation != snapshot.output_generation;
        self.status = snapshot.status;
        self.output_generation = snapshot.output_generation;
        self.seen_generation = self.seen_generation.max(snapshot.seen_generation);
        changed
    }

    fn mark_seen(&mut self) -> Option<u64> {
        if self.seen_generation == self.output_generation {
            return None;
        }
        self.seen_generation = self.output_generation;
        Some(self.seen_generation)
    }

    pub const fn id(&self) -> AgentId {
        self.id
    }

    pub const fn kind(&self) -> AgentKind {
        self.kind
    }

    pub const fn status(&self) -> SessionStatus {
        self.status
    }

    pub const fn has_unseen_output(&self) -> bool {
        self.output_generation > self.seen_generation
    }
}

pub(crate) struct App {
    agents: Vec<AgentState>,
    selected: usize,
    mode: Mode,
    sidebar_visible: bool,
    menu_selected: MenuItem,
    theme: ThemeName,
    notice: Option<String>,
    exit_intent: ExitIntent,
    workspace_name: String,
    session_id: Option<SessionId>,
    workspace_path: Option<PathBuf>,
}

impl App {
    #[cfg(test)]
    pub fn new(
        workspace_name: String,
        theme: ThemeName,
        choose_agent: bool,
        notice: Option<String>,
    ) -> Self {
        Self {
            agents: Vec::new(),
            selected: 0,
            mode: if choose_agent {
                Mode::ChooseAgent
            } else {
                Mode::Terminal
            },
            sidebar_visible: true,
            menu_selected: MenuItem::default(),
            theme,
            notice,
            exit_intent: ExitIntent::None,
            workspace_name,
            session_id: None,
            workspace_path: None,
        }
    }

    pub fn hydrate(
        snapshot: SvarmSessionSnapshot,
        theme: ThemeName,
        notice: Option<String>,
    ) -> Self {
        let selected = snapshot
            .selected_agent_id
            .and_then(|id| snapshot.agents.iter().position(|agent| agent.id == id))
            .unwrap_or(0);
        Self {
            agents: snapshot
                .agents
                .iter()
                .map(AgentState::from_remote)
                .collect(),
            selected,
            mode: if snapshot.agents.is_empty() {
                Mode::ChooseAgent
            } else {
                Mode::Terminal
            },
            sidebar_visible: true,
            menu_selected: MenuItem::default(),
            theme,
            notice,
            exit_intent: ExitIntent::None,
            workspace_name: snapshot.summary.display_name,
            session_id: Some(snapshot.summary.id),
            workspace_path: Some(snapshot.summary.canonical_path),
        }
    }

    #[cfg(test)]
    pub fn add_agent(&mut self, snapshot: SessionSnapshot) {
        self.agents.push(AgentState::new(&snapshot));
        self.selected = self.agents.len() - 1;
        self.mode = Mode::Terminal;
    }

    pub fn add_remote_agent(&mut self, snapshot: AgentSnapshot) {
        self.agents.push(AgentState::from_remote(&snapshot));
        self.selected = self.agents.len() - 1;
        self.mode = Mode::Terminal;
    }

    pub fn update_remote_agent(&mut self, snapshot: AgentSnapshot) -> bool {
        let mut changed = false;
        if let Some(agent) = self.agents.iter_mut().find(|agent| agent.id == snapshot.id) {
            changed |= agent.update_remote(&snapshot);
        }
        if let Some(error) = snapshot.read_error
            && self.notice.as_deref() != Some(&error)
        {
            self.notice = Some(error);
            changed = true;
        }
        changed
    }

    #[cfg(test)]
    pub fn update_agent(&mut self, snapshot: SessionSnapshot) -> bool {
        let mut changed = false;
        if let Some(agent) = self.agents.iter_mut().find(|agent| agent.id == snapshot.id) {
            changed |= agent.update(&snapshot);
        }
        if let Some(error) = snapshot.read_error
            && self.notice.as_deref() != Some(&error)
        {
            self.notice = Some(error);
            changed = true;
        }
        changed
    }

    pub fn remove_agent(&mut self, id: AgentId) {
        let Some(index) = self.agents.iter().position(|agent| agent.id == id) else {
            return;
        };
        self.agents.remove(index);
        self.selected = self.selected.min(self.agents.len().saturating_sub(1));
        self.mode = Mode::Terminal;
    }

    pub fn select_next(&mut self) {
        if !self.agents.is_empty() {
            self.selected = (self.selected + 1) % self.agents.len();
        }
    }

    pub fn select_previous(&mut self) {
        if !self.agents.is_empty() {
            self.selected = self
                .selected
                .checked_sub(1)
                .unwrap_or(self.agents.len() - 1);
        }
    }

    pub fn select(&mut self, index: usize) {
        if index < self.agents.len() {
            self.selected = index;
        }
    }

    pub fn mark_selected_seen(&mut self) -> Option<(AgentId, u64)> {
        if let Some(agent) = self.agents.get_mut(self.selected) {
            let id = agent.id();
            return agent.mark_seen().map(|generation| (id, generation));
        }
        None
    }

    pub fn select_next_menu_item(&mut self) {
        self.menu_selected = self.menu_selected.cycle(1);
    }

    pub fn select_previous_menu_item(&mut self) {
        self.menu_selected = self.menu_selected.cycle(-1);
    }

    pub fn cycle_theme(&mut self, delta: isize) -> ThemeName {
        self.theme.cycle(delta);
        self.theme
    }

    pub fn set_notice(&mut self, notice: impl Into<String>) {
        self.notice = Some(notice.into());
    }

    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar_visible = !self.sidebar_visible;
    }

    pub fn show_sidebar(&mut self) {
        self.sidebar_visible = true;
    }

    pub fn request_detach(&mut self) {
        self.exit_intent = ExitIntent::Detach;
    }

    pub fn request_stop(&mut self) {
        self.exit_intent = ExitIntent::StopSession;
    }

    pub fn select_menu_item(&mut self, item: MenuItem) {
        self.menu_selected = item;
    }

    pub fn open_selected_menu_item(&mut self) {
        match self.menu_selected {
            MenuItem::Detach => self.request_detach(),
            MenuItem::StopSession => self.mode = Mode::ConfirmQuit,
            MenuItem::Keybinds => self.mode = Mode::Keybinds,
            MenuItem::Settings => self.mode = Mode::Settings,
        }
    }

    pub fn selected_agent_id(&self) -> Option<AgentId> {
        self.agents.get(self.selected).map(AgentState::id)
    }

    pub fn agents(&self) -> &[AgentState] {
        &self.agents
    }

    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    pub const fn mode(&self) -> Mode {
        self.mode
    }

    pub const fn sidebar_visible(&self) -> bool {
        self.sidebar_visible
    }

    pub const fn menu_selected(&self) -> MenuItem {
        self.menu_selected
    }

    pub const fn theme(&self) -> ThemeName {
        self.theme
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub const fn exit_intent(&self) -> ExitIntent {
        self.exit_intent
    }

    pub fn workspace_name(&self) -> &str {
        &self.workspace_name
    }

    pub const fn session_id(&self) -> Option<SessionId> {
        self.session_id
    }

    pub fn workspace_path(&self) -> Option<&PathBuf> {
        self.workspace_path.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use svarm_agent::protocol::{SessionRevision, TerminalSequence};

    fn snapshot(id: u64, generation: u64) -> SessionSnapshot {
        SessionSnapshot {
            id: AgentId::new(id),
            kind: AgentKind::Codex,
            status: SessionStatus::Running,
            output_generation: generation,
            read_error: None,
            exit: None,
        }
    }

    fn app() -> App {
        App::new("workspace".into(), ThemeName::Dark, false, None)
    }

    #[test]
    fn output_is_unseen_until_the_selected_agent_is_rendered() {
        let mut app = app();
        app.add_agent(snapshot(1, 0));

        assert!(app.update_agent(snapshot(1, 1)));
        assert!(app.agents()[0].has_unseen_output());
        let _ = app.mark_selected_seen();
        assert!(!app.agents()[0].has_unseen_output());
    }

    #[test]
    fn removing_the_last_agent_keeps_selection_valid() {
        let mut app = app();
        app.add_agent(snapshot(1, 0));
        app.add_agent(snapshot(2, 0));

        app.remove_agent(AgentId::new(2));
        assert_eq!(app.selected_agent_id(), Some(AgentId::new(1)));
        app.remove_agent(AgentId::new(1));
        assert_eq!(app.selected_agent_id(), None);
        assert_eq!(app.selected_index(), 0);
    }

    #[test]
    fn menu_navigation_uses_the_shared_item_order() {
        let mut app = app();

        app.select_previous_menu_item();
        assert_eq!(app.menu_selected(), MenuItem::Settings);
        app.select_next_menu_item();
        assert_eq!(app.menu_selected(), MenuItem::Detach);
    }

    #[test]
    fn detach_and_stop_are_distinct_pure_exit_transitions() {
        let mut detach = app();
        detach.request_detach();
        assert_eq!(detach.exit_intent(), ExitIntent::Detach);

        let mut stop = app();
        stop.select_menu_item(MenuItem::StopSession);
        stop.open_selected_menu_item();
        assert_eq!(stop.mode(), Mode::ConfirmQuit);
        assert_eq!(stop.exit_intent(), ExitIntent::None);
        stop.request_stop();
        assert_eq!(stop.exit_intent(), ExitIntent::StopSession);
    }

    #[test]
    fn runtime_observations_do_not_erase_unrelated_notices() {
        let mut app = App::new(
            "workspace".into(),
            ThemeName::Dark,
            false,
            Some("settings could not be loaded".into()),
        );

        app.add_agent(snapshot(1, 0));
        assert_eq!(app.notice(), Some("settings could not be loaded"));
    }

    #[test]
    fn hydration_restores_server_state_and_resets_transient_ui() {
        let first = remote_agent(1, 2, 1);
        let second = remote_agent(2, 4, 3);
        let snapshot = SvarmSessionSnapshot {
            summary: summary(7, 20),
            selected_agent_id: Some(second.id),
            rows: 24,
            cols: 80,
            agents: vec![first, second],
        };

        let app = App::hydrate(snapshot, ThemeName::Light, Some("notice".into()));

        assert_eq!(app.session_id(), Some(SessionId(7)));
        assert_eq!(app.selected_agent_id(), Some(AgentId::new(2)));
        assert_eq!(app.mode(), Mode::Terminal);
        assert!(app.sidebar_visible());
        assert_eq!(app.exit_intent(), ExitIntent::None);
        assert_eq!(app.workspace_path(), Some(&PathBuf::from("/tmp/project-7")));
        assert!(app.agents()[1].has_unseen_output());
    }

    #[test]
    fn chooser_orders_sessions_and_keeps_keyboard_selection_visible() {
        let mut chooser = SessionChooser::new(vec![summary(2, 10), summary(1, 20)], true);

        assert_eq!(chooser.sessions()[0].id, SessionId(1));
        assert_eq!(
            chooser.confirm(),
            Some(StartupChoice::Session(SessionId(1)))
        );
        chooser.select_next();
        chooser.select_next();
        assert_eq!(chooser.confirm(), Some(StartupChoice::NewSession));
        assert_eq!(chooser.viewport_start(2), 1);
        chooser.select_next();
        assert_eq!(chooser.selected(), 0);
        chooser.select_previous();
        assert_eq!(chooser.confirm(), Some(StartupChoice::NewSession));
        assert_eq!(chooser.cancel(), StartupChoice::Cancel);
    }

    #[test]
    fn attach_only_chooser_never_exposes_a_create_action() {
        let mut chooser = SessionChooser::new(vec![summary(1, 10)], false);

        assert_eq!(chooser.select_new(), None);
        assert_eq!(chooser.row_count(), 1);
        assert_eq!(
            chooser.confirm(),
            Some(StartupChoice::Session(SessionId(1)))
        );
    }

    fn summary(id: u64, activity: u64) -> SessionSummary {
        SessionSummary {
            id: SessionId(id),
            canonical_path: PathBuf::from(format!("/tmp/project-{id}")),
            display_name: format!("project-{id}"),
            running_agents: 1,
            total_agents: 1,
            attachment: None,
            last_user_activity_ms: activity,
            revision: SessionRevision(1),
        }
    }

    fn remote_agent(id: u64, output_generation: u64, seen_generation: u64) -> AgentSnapshot {
        AgentSnapshot {
            id: AgentId::new(id),
            kind: AgentKind::Codex,
            launch_directory: "/tmp".into(),
            status: SessionStatus::Running,
            exit: None,
            output_generation,
            seen_generation,
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            recognition: None,
        }
    }
}
