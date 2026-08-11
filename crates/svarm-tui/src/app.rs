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
    NewAgent(NewAgentPage),
    ConfirmClose,
    ConfirmQuit,
    Menu,
    Keybinds,
    Settings,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum NewAgentPage {
    #[default]
    Form,
    Workspaces,
    Agents,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum NewAgentField {
    #[default]
    Workspace,
    Agent,
    Start,
}

impl NewAgentField {
    fn cycle(self, delta: isize) -> Self {
        let current = match self {
            Self::Workspace => 0,
            Self::Agent => 1,
            Self::Start => 2,
        };
        match (current + delta).rem_euclid(3) {
            0 => Self::Workspace,
            1 => Self::Agent,
            _ => Self::Start,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceChoice {
    pub path: PathBuf,
    pub available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NewAgentDraft {
    pub workspace: Option<PathBuf>,
    pub agent: Option<AgentKind>,
    pub selected_field: NewAgentField,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NewAgentState {
    pub draft: NewAgentDraft,
    pub workspaces: Vec<WorkspaceChoice>,
    pub selected_workspace: usize,
    pub selected_agent: usize,
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
    launch_directory: PathBuf,
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
            launch_directory: snapshot.launch_directory.clone(),
            status: snapshot.status,
            output_generation: snapshot.output_generation,
            seen_generation: snapshot.output_generation,
        }
    }

    fn from_remote(snapshot: &AgentSnapshot) -> Self {
        Self {
            id: snapshot.id,
            kind: snapshot.kind,
            launch_directory: snapshot.launch_directory.clone(),
            status: snapshot.status,
            output_generation: snapshot.output_generation,
            seen_generation: snapshot.seen_generation,
        }
    }

    #[cfg(test)]
    fn update(&mut self, snapshot: &SessionSnapshot) -> bool {
        let changed = self.status != snapshot.status
            || self.output_generation != snapshot.output_generation
            || self.launch_directory != snapshot.launch_directory;
        self.launch_directory = snapshot.launch_directory.clone();
        self.status = snapshot.status;
        self.output_generation = snapshot.output_generation;
        changed
    }

    fn update_remote(&mut self, snapshot: &AgentSnapshot) -> bool {
        let changed = self.status != snapshot.status
            || self.output_generation != snapshot.output_generation
            || self.launch_directory != snapshot.launch_directory;
        self.launch_directory = snapshot.launch_directory.clone();
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

    pub fn workspace_name(&self) -> String {
        self.launch_directory
            .file_name()
            .unwrap_or(self.launch_directory.as_os_str())
            .to_string_lossy()
            .into_owned()
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
    session_id: Option<SessionId>,
    new_agent: Option<NewAgentState>,
}

impl App {
    #[cfg(test)]
    pub fn new(
        _workspace_name: String,
        theme: ThemeName,
        choose_agent: bool,
        notice: Option<String>,
    ) -> Self {
        Self {
            agents: Vec::new(),
            selected: 0,
            mode: Mode::Terminal,
            sidebar_visible: true,
            menu_selected: MenuItem::default(),
            theme,
            notice,
            exit_intent: ExitIntent::None,
            session_id: None,
            new_agent: None,
        }
        .with_test_new_agent(choose_agent)
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
            mode: Mode::Terminal,
            sidebar_visible: true,
            menu_selected: MenuItem::default(),
            theme,
            notice,
            exit_intent: ExitIntent::None,
            session_id: Some(snapshot.summary.id),
            new_agent: None,
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

    pub const fn session_id(&self) -> Option<SessionId> {
        self.session_id
    }

    pub fn open_new_agent(
        &mut self,
        workspace: Option<PathBuf>,
        agent: Option<AgentKind>,
        workspaces: Vec<WorkspaceChoice>,
    ) {
        let workspace = workspace.filter(|path| {
            workspaces
                .iter()
                .any(|choice| choice.available && choice.path == *path)
        });
        let selected_field = if workspace.is_none() {
            NewAgentField::Workspace
        } else if agent.is_none() {
            NewAgentField::Agent
        } else {
            NewAgentField::Start
        };
        let selected_workspace = workspace
            .as_ref()
            .and_then(|path| workspaces.iter().position(|choice| choice.path == *path))
            .unwrap_or(0);
        let selected_agent = agent
            .and_then(|kind| {
                AgentKind::ALL
                    .iter()
                    .position(|candidate| *candidate == kind)
            })
            .unwrap_or(0);
        self.new_agent = Some(NewAgentState {
            draft: NewAgentDraft {
                workspace,
                agent,
                selected_field,
            },
            workspaces,
            selected_workspace,
            selected_agent,
        });
        self.mode = Mode::NewAgent(NewAgentPage::Form);
    }

    pub fn new_agent(&self) -> Option<&NewAgentState> {
        self.new_agent.as_ref()
    }

    pub fn move_new_agent_selection(&mut self, delta: isize) {
        let Some(state) = &mut self.new_agent else {
            return;
        };
        match self.mode {
            Mode::NewAgent(NewAgentPage::Form) => {
                state.draft.selected_field = state.draft.selected_field.cycle(delta);
            }
            Mode::NewAgent(NewAgentPage::Workspaces) if !state.workspaces.is_empty() => {
                state.selected_workspace = (state.selected_workspace as isize + delta)
                    .rem_euclid(state.workspaces.len() as isize)
                    as usize;
            }
            Mode::NewAgent(NewAgentPage::Agents) => {
                state.selected_agent = (state.selected_agent as isize + delta)
                    .rem_euclid(AgentKind::ALL.len() as isize)
                    as usize;
            }
            _ => {}
        }
    }

    pub fn open_workspace_choices(&mut self) {
        if self.new_agent.is_some() {
            self.mode = Mode::NewAgent(NewAgentPage::Workspaces);
        }
    }

    pub fn open_agent_choices(&mut self) {
        if self.new_agent.is_some() {
            self.mode = Mode::NewAgent(NewAgentPage::Agents);
        }
    }

    pub fn confirm_workspace(&mut self) {
        let Some(state) = &mut self.new_agent else {
            return;
        };
        if let Some(choice) = state.workspaces.get(state.selected_workspace)
            && choice.available
        {
            state.draft.workspace = Some(choice.path.clone());
            self.mode = Mode::NewAgent(NewAgentPage::Form);
        }
    }

    pub fn confirm_agent(&mut self) {
        let Some(state) = &mut self.new_agent else {
            return;
        };
        state.draft.agent = AgentKind::ALL.get(state.selected_agent).copied();
        self.mode = Mode::NewAgent(NewAgentPage::Form);
    }

    pub fn set_agent_choice(&mut self, kind: AgentKind) {
        if let Some(state) = &mut self.new_agent {
            state.selected_agent = AgentKind::ALL
                .iter()
                .position(|candidate| *candidate == kind)
                .unwrap_or(0);
            state.draft.agent = Some(kind);
            self.mode = Mode::NewAgent(NewAgentPage::Form);
        }
    }

    pub fn back_to_new_agent_form(&mut self) {
        if self.new_agent.is_some() {
            self.mode = Mode::NewAgent(NewAgentPage::Form);
        }
    }

    pub fn cancel_new_agent(&mut self) {
        self.new_agent = None;
        self.mode = Mode::Terminal;
    }

    pub fn new_agent_submission(&self) -> Option<(AgentKind, PathBuf)> {
        let state = self.new_agent.as_ref()?;
        if state.draft.selected_field != NewAgentField::Start {
            return None;
        }
        Some((state.draft.agent?, state.draft.workspace.clone()?))
    }

    pub fn finish_new_agent(&mut self) {
        self.new_agent = None;
        self.mode = Mode::Terminal;
    }

    #[cfg(test)]
    fn with_test_new_agent(mut self, open: bool) -> Self {
        if open {
            self.open_new_agent(None, None, Vec::new());
        }
        self
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
            launch_directory: PathBuf::from("/tmp/workspace"),
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
    fn new_agent_form_focuses_the_first_missing_value_and_disables_incomplete_start() {
        let mut app = app();
        let workspaces = vec![WorkspaceChoice {
            path: PathBuf::from("/tmp/one"),
            available: true,
        }];

        app.open_new_agent(None, None, workspaces.clone());
        assert_eq!(
            app.new_agent().unwrap().draft.selected_field,
            NewAgentField::Workspace
        );
        assert_eq!(app.new_agent_submission(), None);

        app.open_new_agent(Some(PathBuf::from("/tmp/one")), None, workspaces.clone());
        assert_eq!(
            app.new_agent().unwrap().draft.selected_field,
            NewAgentField::Agent
        );

        app.open_new_agent(
            Some(PathBuf::from("/tmp/one")),
            Some(AgentKind::Claude),
            workspaces,
        );
        assert_eq!(
            app.new_agent().unwrap().draft.selected_field,
            NewAgentField::Start
        );
        assert_eq!(
            app.new_agent_submission(),
            Some((AgentKind::Claude, PathBuf::from("/tmp/one")))
        );
    }

    #[test]
    fn nested_new_agent_choices_preserve_and_change_only_the_confirmed_field() {
        let mut app = app();
        app.open_new_agent(
            Some(PathBuf::from("/tmp/one")),
            Some(AgentKind::Codex),
            vec![
                WorkspaceChoice {
                    path: PathBuf::from("/tmp/one"),
                    available: true,
                },
                WorkspaceChoice {
                    path: PathBuf::from("/tmp/two"),
                    available: true,
                },
            ],
        );

        app.open_workspace_choices();
        app.move_new_agent_selection(1);
        app.back_to_new_agent_form();
        assert_eq!(
            app.new_agent().unwrap().draft.workspace,
            Some(PathBuf::from("/tmp/one"))
        );
        assert_eq!(app.new_agent().unwrap().draft.agent, Some(AgentKind::Codex));

        app.open_workspace_choices();
        app.confirm_workspace();
        assert_eq!(
            app.new_agent().unwrap().draft.workspace,
            Some(PathBuf::from("/tmp/two"))
        );
        assert_eq!(app.new_agent().unwrap().draft.agent, Some(AgentKind::Codex));

        app.open_agent_choices();
        app.move_new_agent_selection(1);
        app.confirm_agent();
        assert_eq!(
            app.new_agent().unwrap().draft.agent,
            Some(AgentKind::Claude)
        );
    }

    #[test]
    fn missing_saved_workspace_stays_visible_but_cannot_become_the_draft() {
        let missing = PathBuf::from("/tmp/missing");
        let mut app = app();
        app.open_new_agent(
            Some(missing.clone()),
            Some(AgentKind::Codex),
            vec![WorkspaceChoice {
                path: missing,
                available: false,
            }],
        );

        assert_eq!(app.new_agent().unwrap().draft.workspace, None);
        app.open_workspace_choices();
        app.confirm_workspace();
        assert_eq!(app.mode(), Mode::NewAgent(NewAgentPage::Workspaces));
        assert_eq!(app.new_agent().unwrap().draft.workspace, None);
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
        assert!(app.agents()[1].has_unseen_output());
    }

    #[test]
    fn empty_session_hydrates_without_opening_the_agent_chooser() {
        let snapshot = SvarmSessionSnapshot {
            summary: summary(7, 20),
            selected_agent_id: None,
            rows: 24,
            cols: 80,
            agents: vec![],
        };

        let app = App::hydrate(snapshot, ThemeName::Dark, None);

        assert_eq!(app.mode(), Mode::Terminal);
        assert_eq!(app.selected_agent_id(), None);
        assert_eq!(app.exit_intent(), ExitIntent::None);
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
