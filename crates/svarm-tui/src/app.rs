use svarm_agent::{AgentId, AgentKind, SessionSnapshot, SessionStatus};

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
pub(crate) enum MenuItem {
    #[default]
    Keybinds,
    Settings,
}

impl MenuItem {
    pub const ALL: [Self; 2] = [Self::Keybinds, Self::Settings];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Keybinds => "Keybinds",
            Self::Settings => "Settings",
        }
    }

    pub const fn mode(self) -> Mode {
        match self {
            Self::Keybinds => Mode::Keybinds,
            Self::Settings => Mode::Settings,
        }
    }

    fn cycle(self, delta: isize) -> Self {
        let current = match self {
            Self::Keybinds => 0,
            Self::Settings => 1,
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
    fn new(snapshot: &SessionSnapshot) -> Self {
        Self {
            id: snapshot.id,
            kind: snapshot.kind,
            status: snapshot.status,
            output_generation: snapshot.output_generation,
            seen_generation: snapshot.output_generation,
        }
    }

    fn update(&mut self, snapshot: &SessionSnapshot) -> bool {
        let changed =
            self.status != snapshot.status || self.output_generation != snapshot.output_generation;
        self.status = snapshot.status;
        self.output_generation = snapshot.output_generation;
        changed
    }

    fn mark_seen(&mut self) {
        self.seen_generation = self.output_generation;
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
    quit_requested: bool,
    workspace_name: String,
}

impl App {
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
            quit_requested: false,
            workspace_name,
        }
    }

    pub fn add_agent(&mut self, snapshot: SessionSnapshot) {
        self.agents.push(AgentState::new(&snapshot));
        self.selected = self.agents.len() - 1;
        self.mode = Mode::Terminal;
    }

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
        self.mark_selected_seen();
    }

    pub fn select_next(&mut self) {
        if !self.agents.is_empty() {
            self.selected = (self.selected + 1) % self.agents.len();
            self.mark_selected_seen();
        }
    }

    pub fn select_previous(&mut self) {
        if !self.agents.is_empty() {
            self.selected = self
                .selected
                .checked_sub(1)
                .unwrap_or(self.agents.len() - 1);
            self.mark_selected_seen();
        }
    }

    pub fn select(&mut self, index: usize) {
        if index < self.agents.len() {
            self.selected = index;
            self.mark_selected_seen();
        }
    }

    pub fn mark_selected_seen(&mut self) {
        if let Some(agent) = self.agents.get_mut(self.selected) {
            agent.mark_seen();
        }
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

    pub fn request_quit(&mut self) {
        self.quit_requested = true;
    }

    pub fn select_menu_item(&mut self, item: MenuItem) {
        self.menu_selected = item;
    }

    pub fn open_selected_menu_item(&mut self) {
        self.mode = self.menu_selected.mode();
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

    pub const fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    pub fn workspace_name(&self) -> &str {
        &self.workspace_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        app.mark_selected_seen();
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
        assert_eq!(app.menu_selected(), MenuItem::Keybinds);
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
}
