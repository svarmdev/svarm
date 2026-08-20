use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
#[cfg(test)]
use svarm_agent::SessionSnapshot;
use svarm_agent::{
    AgentId, AgentKind, ProcessExit, SessionStatus,
    harness_update::{HarnessUpdated, HarnessVersion},
    protocol::{
        AgentActivity, AgentSnapshot, ArchivedConversation, GitContext, RecognitionEvidence,
        SessionId, SessionSummary, SvarmSessionSnapshot, UsageOverview, UsageProviderReport,
    },
    server_session::sort_session_summaries,
};

use crate::theme::ThemeName;

pub const SIDEBAR_MIN_WIDTH: u16 = 3;
pub const SIDEBAR_DEFAULT_WIDTH: u16 = 28;
pub const SIDEBAR_MAX_WIDTH: u16 = 48;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Mode {
    #[default]
    Terminal,
    Prefix,
    ToolPrefix,
    Review,
    ReviewToolPrefix,
    NewAgent(NewAgentPage),
    ConfirmClose,
    ConfirmArchive,
    ArchiveUnavailable,
    ConfirmResume,
    ConfirmQuit,
    Menu,
    Keybinds,
    Settings,
    Usage,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum NewAgentPage {
    #[default]
    Form,
    Workspaces,
    Locations,
    Agents,
    CreatingWorktree,
    NativeBrowser,
    EmbeddedBrowser,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SettingsTab {
    #[default]
    Appearance,
    Harnesses,
}

impl SettingsTab {
    pub const ALL: [Self; 2] = [Self::Appearance, Self::Harnesses];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Harnesses => "Harnesses",
        }
    }

    fn cycle(self, delta: isize) -> Self {
        let current = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(Self::ALL.len() as isize) as usize;
        Self::ALL[next]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HarnessUpdateStatus {
    NotInstalled,
    NotChecked,
    Checking,
    UpToDate {
        current: String,
    },
    UpdateAvailable {
        current: String,
        latest: String,
    },
    Updating {
        current: String,
        latest: String,
    },
    CheckFailed {
        message: String,
    },
    UpdateFailed {
        current: String,
        latest: String,
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HarnessUpdateState {
    pub kind: AgentKind,
    pub status: HarnessUpdateStatus,
}

fn default_harness_updates() -> Vec<HarnessUpdateState> {
    AgentKind::ALL
        .into_iter()
        .map(|kind| HarnessUpdateState {
            kind,
            status: HarnessUpdateStatus::NotChecked,
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum NewAgentField {
    #[default]
    Workspace,
    Location,
    Agent,
    Start,
}

impl NewAgentField {
    pub const ALL: [Self; 4] = [Self::Workspace, Self::Location, Self::Agent, Self::Start];

    fn cycle(self, delta: isize) -> Self {
        let current = match self {
            Self::Workspace => 0,
            Self::Location => 1,
            Self::Agent => 2,
            Self::Start => 3,
        };
        let next = (current + delta).rem_euclid(Self::ALL.len() as isize) as usize;
        Self::ALL[next]
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Checkout {
    #[default]
    Local,
    NewWorktree,
}

impl Checkout {
    pub const ALL: [Self; 2] = [Self::Local, Self::NewWorktree];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Local => "Local checkout",
            Self::NewWorktree => "New worktree",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceChoice {
    pub path: PathBuf,
    pub available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryChoice {
    pub path: PathBuf,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeBrowserState {
    pub generation: u64,
    pub requested_path: PathBuf,
    pub current_path: PathBuf,
    pub entries: Vec<DirectoryChoice>,
    pub selected: usize,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BrowserAction {
    Select(PathBuf),
    Load(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NewAgentDraft {
    pub workspace: Option<PathBuf>,
    pub checkout: Checkout,
    pub agent: Option<AgentKind>,
    pub selected_field: NewAgentField,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NewAgentState {
    pub draft: NewAgentDraft,
    pub workspaces: Vec<WorkspaceChoice>,
    pub selected_workspace: usize,
    pub selected_location: usize,
    pub selected_agent: usize,
    pub repository_root: Option<PathBuf>,
    pub worktree_generation: u64,
    pub native_browser: Option<NativeBrowserState>,
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

    pub fn select(&mut self, index: usize) {
        if index < self.row_count() {
            self.selected = index;
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
    ReviewChanges,
    StopSession,
    Keybinds,
    Settings,
}

impl MenuItem {
    pub const ALL: [Self; 5] = [
        Self::Detach,
        Self::ReviewChanges,
        Self::StopSession,
        Self::Keybinds,
        Self::Settings,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Detach => "Detach",
            Self::ReviewChanges => "Review with Hunk",
            Self::StopSession => "Stop session",
            Self::Keybinds => "Keybinds",
            Self::Settings => "Settings",
        }
    }

    fn cycle(self, delta: isize) -> Self {
        let current = match self {
            Self::Detach => 0,
            Self::ReviewChanges => 1,
            Self::StopSession => 2,
            Self::Keybinds => 3,
            Self::Settings => 4,
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
    working_directory: Option<PathBuf>,
    status: SessionStatus,
    exit: Option<ProcessExit>,
    output_generation: u64,
    seen_generation: u64,
    completed_generation: u64,
    conversation_title: Option<String>,
    conversation_id: Option<String>,
    activity: AgentActivity,
    recognition: Option<RecognitionEvidence>,
    git: Option<GitContext>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentDisplayStatus {
    Unknown,
    Idle,
    Working,
    Done,
    NeedsYou,
    Failed,
}

impl AgentState {
    #[cfg(test)]
    fn new(snapshot: &SessionSnapshot) -> Self {
        Self {
            id: snapshot.id,
            kind: snapshot.kind,
            launch_directory: snapshot.launch_directory.clone(),
            working_directory: None,
            status: snapshot.status,
            exit: snapshot.exit.clone(),
            output_generation: snapshot.output_generation,
            seen_generation: snapshot.output_generation,
            completed_generation: 0,
            conversation_title: None,
            conversation_id: snapshot.conversation_id.clone(),
            activity: AgentActivity::Unknown,
            recognition: None,
            git: None,
        }
    }

    fn from_remote(snapshot: &AgentSnapshot) -> Self {
        Self {
            id: snapshot.id,
            kind: snapshot.kind,
            launch_directory: snapshot.launch_directory.clone(),
            working_directory: snapshot.working_directory.clone(),
            status: snapshot.status,
            exit: snapshot.exit.clone(),
            output_generation: snapshot.output_generation,
            seen_generation: snapshot.seen_generation,
            completed_generation: snapshot.completed_generation,
            conversation_title: snapshot.conversation_title.clone(),
            conversation_id: snapshot.conversation_id.clone(),
            activity: snapshot.activity,
            recognition: snapshot.recognition.clone(),
            git: snapshot.git.clone(),
        }
    }

    #[cfg(test)]
    fn update(&mut self, snapshot: &SessionSnapshot) -> bool {
        let changed = self.status != snapshot.status
            || self.exit != snapshot.exit
            || self.output_generation != snapshot.output_generation
            || self.launch_directory != snapshot.launch_directory;
        self.launch_directory = snapshot.launch_directory.clone();
        self.status = snapshot.status;
        self.exit = snapshot.exit.clone();
        self.output_generation = snapshot.output_generation;
        changed
    }

    fn update_remote(&mut self, snapshot: &AgentSnapshot) -> bool {
        // Output generations and diagnostic evidence are retained below, but they are not
        // independently visible. The selected agent's terminal frame requests its redraw, while
        // a background agent redraws only when its displayed metadata changes.
        let changed = self.status != snapshot.status
            || self.exit != snapshot.exit
            || self.completed_generation != snapshot.completed_generation
            || self.launch_directory != snapshot.launch_directory
            || self.working_directory != snapshot.working_directory
            || self.conversation_title != snapshot.conversation_title
            || self.conversation_id != snapshot.conversation_id
            || self.activity != snapshot.activity
            || self.git != snapshot.git;
        self.launch_directory = snapshot.launch_directory.clone();
        self.working_directory = snapshot.working_directory.clone();
        self.status = snapshot.status;
        self.exit = snapshot.exit.clone();
        self.output_generation = snapshot.output_generation;
        self.seen_generation = self.seen_generation.max(snapshot.seen_generation);
        self.completed_generation = snapshot.completed_generation;
        self.conversation_title = snapshot.conversation_title.clone();
        self.conversation_id = snapshot.conversation_id.clone();
        self.activity = snapshot.activity;
        self.recognition = snapshot.recognition.clone();
        self.git = snapshot.git.clone();
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

    pub fn launch_directory(&self) -> &Path {
        &self.launch_directory
    }

    pub fn working_directory(&self) -> Option<&Path> {
        self.working_directory.as_deref()
    }

    pub const fn status(&self) -> SessionStatus {
        self.status
    }

    pub fn conversation_title(&self) -> Option<&str> {
        self.conversation_title.as_deref()
    }

    pub fn conversation_id(&self) -> Option<&str> {
        self.conversation_id.as_deref()
    }

    pub const fn git(&self) -> Option<&GitContext> {
        self.git.as_ref()
    }

    pub fn display_status(&self) -> AgentDisplayStatus {
        if self.status == SessionStatus::Exited {
            return if self.exit.as_ref().is_some_and(|exit| exit.success) {
                AgentDisplayStatus::Done
            } else {
                AgentDisplayStatus::Failed
            };
        }
        match self.activity {
            AgentActivity::Unknown => AgentDisplayStatus::Unknown,
            AgentActivity::Idle if self.completed_generation > self.seen_generation => {
                AgentDisplayStatus::Done
            }
            AgentActivity::Idle => AgentDisplayStatus::Idle,
            AgentActivity::Working => AgentDisplayStatus::Working,
            AgentActivity::Blocked => AgentDisplayStatus::NeedsYou,
        }
    }

    #[cfg(test)]
    pub const fn has_unseen_output(&self) -> bool {
        self.output_generation > self.seen_generation
    }
}

pub(crate) struct App {
    agents: Vec<AgentState>,
    archived: Vec<ArchivedConversation>,
    available_harnesses: Vec<AgentKind>,
    selected: usize,
    sidebar_scroll: Option<usize>,
    mode: Mode,
    sidebar_visible: bool,
    sidebar_width: u16,
    menu_selected: MenuItem,
    theme: ThemeName,
    settings_tab: SettingsTab,
    harness_updates: Vec<HarnessUpdateState>,
    known_harness_update_releases: Vec<(AgentKind, String)>,
    harness_settings_selection: usize,
    harness_update_interval_minutes: u64,
    notice: Option<String>,
    notice_revision: u64,
    exit_intent: ExitIntent,
    session_id: Option<SessionId>,
    new_agent: Option<NewAgentState>,
    pending_resume: Option<String>,
    archive_target: Option<AgentId>,
    usage: UsageOverview,
    usage_tab: Option<AgentKind>,
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
            archived: Vec::new(),
            available_harnesses: AgentKind::ALL.to_vec(),
            selected: 0,
            sidebar_scroll: None,
            mode: Mode::Terminal,
            sidebar_visible: true,
            sidebar_width: SIDEBAR_DEFAULT_WIDTH,
            menu_selected: MenuItem::default(),
            theme,
            settings_tab: SettingsTab::default(),
            harness_updates: default_harness_updates(),
            known_harness_update_releases: Vec::new(),
            harness_settings_selection: 0,
            harness_update_interval_minutes: 60,
            notice,
            notice_revision: 0,
            exit_intent: ExitIntent::None,
            session_id: None,
            new_agent: None,
            pending_resume: None,
            archive_target: None,
            usage: UsageOverview::default(),
            usage_tab: None,
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
            archived: snapshot.archived,
            available_harnesses: AgentKind::ALL.to_vec(),
            selected,
            sidebar_scroll: None,
            mode: Mode::Terminal,
            sidebar_visible: true,
            sidebar_width: SIDEBAR_DEFAULT_WIDTH,
            menu_selected: MenuItem::default(),
            theme,
            settings_tab: SettingsTab::default(),
            harness_updates: default_harness_updates(),
            known_harness_update_releases: Vec::new(),
            harness_settings_selection: 0,
            harness_update_interval_minutes: 60,
            notice,
            notice_revision: 0,
            exit_intent: ExitIntent::None,
            session_id: Some(snapshot.summary.id),
            new_agent: None,
            pending_resume: None,
            archive_target: None,
            usage: UsageOverview::default(),
            usage_tab: None,
        }
    }

    pub fn set_available_harnesses(&mut self, available: Vec<AgentKind>) {
        self.available_harnesses = AgentKind::ALL
            .into_iter()
            .filter(|kind| available.contains(kind))
            .collect();
        for harness in &mut self.harness_updates {
            if !self.available_harnesses.contains(&harness.kind) {
                harness.status = HarnessUpdateStatus::NotInstalled;
            } else if harness.status == HarnessUpdateStatus::NotInstalled {
                harness.status = HarnessUpdateStatus::NotChecked;
            }
        }
    }

    pub fn available_harnesses(&self) -> &[AgentKind] {
        &self.available_harnesses
    }

    pub fn harness_installed(&self, kind: AgentKind) -> bool {
        self.available_harnesses.contains(&kind)
    }

    pub fn harness_update(&self, kind: AgentKind) -> Option<&HarnessUpdateState> {
        self.harness_updates
            .iter()
            .find(|harness| harness.kind == kind)
    }

    pub const fn harness_update_interval_minutes(&self) -> u64 {
        self.harness_update_interval_minutes
    }

    pub fn set_harness_update_interval_minutes(&mut self, minutes: u64) {
        self.harness_update_interval_minutes = minutes;
    }

    pub const fn harness_settings_selection(&self) -> usize {
        self.harness_settings_selection
    }

    pub fn move_harness_settings_selection(&mut self, delta: isize) {
        let count = AgentKind::ALL.len() + 1;
        self.harness_settings_selection =
            (self.harness_settings_selection as isize + delta).rem_euclid(count as isize) as usize;
    }

    pub fn select_harness_settings_row(&mut self, row: usize) {
        if row <= AgentKind::ALL.len() {
            self.harness_settings_selection = row;
        }
    }

    pub fn selected_harness_update_kind(&self) -> Option<AgentKind> {
        self.harness_settings_selection
            .checked_sub(1)
            .and_then(|index| AgentKind::ALL.get(index))
            .copied()
    }

    pub fn begin_harness_check(&mut self) {
        for harness in &mut self.harness_updates {
            if self.available_harnesses.contains(&harness.kind)
                && !matches!(harness.status, HarnessUpdateStatus::Updating { .. })
            {
                harness.status = HarnessUpdateStatus::Checking;
            }
        }
    }

    /// Applies a complete check and returns harnesses whose available release is newly observed.
    pub fn apply_harness_check(
        &mut self,
        results: Vec<(AgentKind, Result<HarnessVersion, String>)>,
    ) -> Vec<AgentKind> {
        let mut newly_available = Vec::new();
        for (kind, result) in results {
            let Some(harness) = self
                .harness_updates
                .iter_mut()
                .find(|harness| harness.kind == kind)
            else {
                continue;
            };
            if !self.available_harnesses.contains(&kind) {
                harness.status = HarnessUpdateStatus::NotInstalled;
                continue;
            }
            match result {
                Ok(version) if version.update_available => {
                    let already_known =
                        self.known_harness_update_releases
                            .iter()
                            .any(|(known_kind, latest)| {
                                *known_kind == kind && latest == &version.latest
                            });
                    if !already_known {
                        newly_available.push(kind);
                        self.known_harness_update_releases
                            .retain(|(known_kind, _)| *known_kind != kind);
                        self.known_harness_update_releases
                            .push((kind, version.latest.clone()));
                    }
                    harness.status = HarnessUpdateStatus::UpdateAvailable {
                        current: version.current,
                        latest: version.latest,
                    };
                }
                Ok(version) => {
                    harness.status = HarnessUpdateStatus::UpToDate {
                        current: version.current,
                    };
                }
                Err(message) => harness.status = HarnessUpdateStatus::CheckFailed { message },
            }
        }
        newly_available
    }

    pub fn begin_harness_update(&mut self, kind: AgentKind) -> bool {
        let Some(harness) = self
            .harness_updates
            .iter_mut()
            .find(|harness| harness.kind == kind)
        else {
            return false;
        };
        let (current, latest) = match &harness.status {
            HarnessUpdateStatus::UpdateAvailable { current, latest }
            | HarnessUpdateStatus::UpdateFailed {
                current, latest, ..
            } => (current.clone(), latest.clone()),
            _ => return false,
        };
        harness.status = HarnessUpdateStatus::Updating { current, latest };
        true
    }

    pub fn apply_harness_update(
        &mut self,
        kind: AgentKind,
        result: &Result<HarnessUpdated, String>,
    ) {
        let Some(harness) = self
            .harness_updates
            .iter_mut()
            .find(|harness| harness.kind == kind)
        else {
            return;
        };
        match result {
            Ok(updated) => {
                harness.status = HarnessUpdateStatus::UpToDate {
                    current: updated.current.clone(),
                };
            }
            Err(message) => {
                if let HarnessUpdateStatus::Updating { current, latest } = &harness.status {
                    harness.status = HarnessUpdateStatus::UpdateFailed {
                        current: current.clone(),
                        latest: latest.clone(),
                        message: message.clone(),
                    };
                } else {
                    harness.status = HarnessUpdateStatus::CheckFailed {
                        message: message.clone(),
                    };
                }
            }
        }
    }

    pub fn running_harness_count(&self, kind: AgentKind) -> usize {
        self.agents
            .iter()
            .filter(|agent| agent.kind() == kind && agent.status() == SessionStatus::Running)
            .count()
    }

    pub const fn settings_tab(&self) -> SettingsTab {
        self.settings_tab
    }

    pub fn move_settings_tab(&mut self, delta: isize) {
        self.settings_tab = self.settings_tab.cycle(delta);
    }

    pub fn select_settings_tab(&mut self, tab: SettingsTab) {
        self.settings_tab = tab;
    }

    pub fn open_harness_settings(&mut self) {
        self.mode = Mode::Settings;
        self.settings_tab = SettingsTab::Harnesses;
    }

    pub fn open_usage(&mut self) {
        self.mode = Mode::Usage;
    }

    pub const fn usage(&self) -> &UsageOverview {
        &self.usage
    }

    /// Apply a fresh overview. Returns whether anything the interface shows changed.
    ///
    /// The selected tab survives as long as its provider is still reported, so a refresh landing
    /// while the modal is open cannot move the user to another provider's numbers.
    pub fn set_usage(&mut self, usage: UsageOverview) -> bool {
        if self.usage == usage {
            return false;
        }
        self.usage = usage;
        self.clamp_usage_tab();
        true
    }

    fn clamp_usage_tab(&mut self) {
        let first = self.usage.providers.first().map(|provider| provider.kind);
        match self.usage_tab {
            Some(kind) if self.usage_provider(kind).is_some() => {}
            _ => self.usage_tab = first,
        }
    }

    pub fn usage_provider(&self, kind: AgentKind) -> Option<&UsageProviderReport> {
        self.usage
            .providers
            .iter()
            .find(|provider| provider.kind == kind)
    }

    /// The provider whose tab is showing, defaulting to the first when none was chosen.
    pub fn selected_usage(&self) -> Option<&UsageProviderReport> {
        match self.usage_tab {
            Some(kind) => self.usage_provider(kind),
            None => self.usage.providers.first(),
        }
    }

    pub fn move_usage_tab(&mut self, delta: isize) {
        let providers = &self.usage.providers;
        if providers.is_empty() {
            self.usage_tab = None;
            return;
        }
        let current = self
            .usage_tab
            .and_then(|kind| providers.iter().position(|provider| provider.kind == kind))
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(providers.len() as isize) as usize;
        self.usage_tab = Some(providers[next].kind);
    }

    /// Select a provider's tab. Selecting one that is not reported is ignored, so a stale click
    /// cannot leave the modal pointing at nothing.
    pub fn select_usage_tab(&mut self, kind: AgentKind) {
        if self.usage_provider(kind).is_some() {
            self.usage_tab = Some(kind);
        }
    }

    #[cfg(test)]
    pub fn add_agent(&mut self, snapshot: SessionSnapshot) {
        self.agents.push(AgentState::new(&snapshot));
        self.selected = self.agents.len() - 1;
        self.sidebar_scroll = None;
        self.mode = Mode::Terminal;
    }

    pub fn add_remote_agent(&mut self, snapshot: AgentSnapshot) {
        self.agents.push(AgentState::from_remote(&snapshot));
        self.selected = self.agents.len() - 1;
        self.sidebar_scroll = None;
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
            self.set_notice(error);
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
            self.set_notice(error);
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
        self.sidebar_scroll = None;
        self.mode = Mode::Terminal;
    }

    pub fn archive_remote_agent(&mut self, id: AgentId, conversation: ArchivedConversation) {
        self.remove_agent(id);
        self.archived
            .retain(|item| item.conversation_id != conversation.conversation_id);
        self.archived.insert(0, conversation);
    }

    pub fn resume_remote_agent(&mut self, conversation_id: &str, snapshot: AgentSnapshot) {
        self.archived
            .retain(|item| item.conversation_id != conversation_id);
        self.add_remote_agent(snapshot);
        self.pending_resume = None;
    }

    pub fn apply_conversation_switch(
        &mut self,
        snapshot: AgentSnapshot,
        archived: Option<ArchivedConversation>,
        reactivated_id: Option<&str>,
    ) {
        if let Some(id) = reactivated_id {
            self.archived.retain(|item| item.conversation_id != id);
        }
        if let Some(conversation) = archived {
            self.archived
                .retain(|item| item.conversation_id != conversation.conversation_id);
            self.archived.insert(0, conversation);
        }
        self.update_remote_agent(snapshot);
        self.sidebar_scroll = None;
    }

    pub fn select_next(&mut self) {
        if !self.agents.is_empty() {
            self.selected = (self.selected + 1) % self.agents.len();
            self.sidebar_scroll = None;
        }
    }

    pub fn select_previous(&mut self) {
        if !self.agents.is_empty() {
            self.selected = self
                .selected
                .checked_sub(1)
                .unwrap_or(self.agents.len() - 1);
            self.sidebar_scroll = None;
        }
    }

    pub fn select(&mut self, index: usize) {
        if index < self.agents.len() {
            self.selected = index;
            self.sidebar_scroll = None;
        }
    }

    pub fn scroll_sidebar(&mut self, rows: isize, visible: usize, card_height: usize) {
        let card_height = card_height.max(1);
        let max = self
            .sidebar_content_height(card_height)
            .saturating_sub(visible.max(1));
        let current = self.sidebar_scroll.unwrap_or_else(|| {
            (self.selected * card_height)
                .saturating_sub(visible.saturating_sub(card_height))
                .min(max)
        });
        let rows = rows.saturating_mul(card_height as isize);
        self.sidebar_scroll = Some(if rows >= 0 {
            current.saturating_add(rows as usize).min(max)
        } else {
            current.saturating_sub(rows.unsigned_abs())
        });
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
        self.notice_revision = self.notice_revision.wrapping_add(1);
    }

    pub fn clear_notice(&mut self) {
        self.notice = None;
        self.notice_revision = self.notice_revision.wrapping_add(1);
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

    pub fn set_sidebar_width(&mut self, width: u16) {
        self.sidebar_width = width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
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
            MenuItem::ReviewChanges => {}
            MenuItem::StopSession => self.mode = Mode::ConfirmQuit,
            MenuItem::Keybinds => self.mode = Mode::Keybinds,
            MenuItem::Settings => self.mode = Mode::Settings,
        }
    }

    pub fn selected_agent_id(&self) -> Option<AgentId> {
        self.agents.get(self.selected).map(AgentState::id)
    }

    pub fn selected_agent(&self) -> Option<&AgentState> {
        self.agents.get(self.selected)
    }

    pub fn open_review(&mut self) {
        self.mode = Mode::Review;
    }

    pub fn close_review(&mut self) {
        self.mode = Mode::Terminal;
    }

    pub fn agents(&self) -> &[AgentState] {
        &self.agents
    }

    pub fn archived(&self) -> &[ArchivedConversation] {
        &self.archived
    }

    pub fn sidebar_content_height(&self, card_height: usize) -> usize {
        let card_height = card_height.max(1);
        self.agents.len() * card_height
            + if self.archived.is_empty() {
                0
            } else if card_height == 1 {
                self.archived.len()
            } else {
                1 + self.archived.len()
            }
    }

    pub fn select_sidebar_index(&mut self, index: usize) -> bool {
        if index < self.agents.len() {
            self.select(index);
            return true;
        }
        self.request_resume_archived(index - self.agents.len());
        false
    }

    pub fn request_resume_archived(&mut self, index: usize) -> bool {
        let Some(conversation) = self.archived.get(index) else {
            return false;
        };
        self.pending_resume = Some(conversation.conversation_id.clone());
        self.mode = Mode::ConfirmResume;
        true
    }

    pub fn cycle_pending_archive(&mut self, delta: isize) {
        if self.archived.is_empty() {
            return;
        }
        let current = self
            .pending_resume
            .as_deref()
            .and_then(|id| {
                self.archived
                    .iter()
                    .position(|conversation| conversation.conversation_id == id)
            })
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(self.archived.len() as isize) as usize;
        self.pending_resume = Some(self.archived[next].conversation_id.clone());
    }

    pub fn request_archive(&mut self, index: usize) -> bool {
        let Some(agent) = self.agents.get(index) else {
            self.mode = Mode::Terminal;
            return false;
        };
        self.archive_target = Some(agent.id());
        if agent.conversation_title().is_none() || agent.conversation_id().is_none() {
            self.mode = Mode::ArchiveUnavailable;
            return false;
        }
        if agent.status() == SessionStatus::Running
            && !matches!(agent.activity, AgentActivity::Idle)
        {
            self.mode = Mode::ConfirmArchive;
            return false;
        }
        true
    }

    pub fn take_archive_target(&mut self) -> Option<AgentId> {
        self.archive_target.take()
    }

    pub fn pending_resume(&self) -> Option<&str> {
        self.pending_resume.as_deref()
    }

    pub fn pending_resume_title(&self) -> Option<&str> {
        let id = self.pending_resume()?;
        self.archived
            .iter()
            .find(|conversation| conversation.conversation_id == id)
            .map(|conversation| conversation.title.as_str())
    }

    pub fn cancel_confirmation(&mut self) {
        self.pending_resume = None;
        self.archive_target = None;
        self.mode = Mode::Terminal;
    }

    pub fn dismiss_archive_unavailable(&mut self) {
        self.archive_target = None;
        self.mode = Mode::Terminal;
    }

    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    pub const fn sidebar_scroll(&self) -> Option<usize> {
        self.sidebar_scroll
    }

    pub const fn mode(&self) -> Mode {
        self.mode
    }

    pub const fn sidebar_visible(&self) -> bool {
        self.sidebar_visible
    }

    pub const fn sidebar_width(&self) -> u16 {
        self.sidebar_width
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

    pub const fn notice_revision(&self) -> u64 {
        self.notice_revision
    }

    pub const fn exit_intent(&self) -> ExitIntent {
        self.exit_intent
    }

    pub const fn session_id(&self) -> Option<SessionId> {
        self.session_id
    }

    #[cfg(test)]
    pub fn open_new_agent(
        &mut self,
        workspace: Option<PathBuf>,
        agent: Option<AgentKind>,
        workspaces: Vec<WorkspaceChoice>,
    ) {
        self.open_new_agent_with_checkout(workspace, agent, Checkout::Local, workspaces);
    }

    pub fn open_new_agent_with_checkout(
        &mut self,
        workspace: Option<PathBuf>,
        agent: Option<AgentKind>,
        checkout: Checkout,
        workspaces: Vec<WorkspaceChoice>,
    ) {
        let agent = agent.filter(|kind| self.harness_installed(*kind));
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
                self.available_harnesses
                    .iter()
                    .position(|candidate| *candidate == kind)
            })
            .unwrap_or(0);
        let selected_location = Checkout::ALL
            .iter()
            .position(|candidate| *candidate == checkout)
            .unwrap_or(0);
        self.new_agent = Some(NewAgentState {
            draft: NewAgentDraft {
                workspace,
                checkout,
                agent,
                selected_field,
            },
            workspaces,
            selected_workspace,
            selected_location,
            selected_agent,
            repository_root: None,
            worktree_generation: 0,
            native_browser: None,
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
            Mode::NewAgent(NewAgentPage::Locations) => {
                state.selected_location = (state.selected_location as isize + delta)
                    .rem_euclid(Checkout::ALL.len() as isize)
                    as usize;
            }
            Mode::NewAgent(NewAgentPage::Agents) => {
                if !self.available_harnesses.is_empty() {
                    state.selected_agent = (state.selected_agent as isize + delta)
                        .rem_euclid(self.available_harnesses.len() as isize)
                        as usize;
                }
            }
            Mode::NewAgent(NewAgentPage::NativeBrowser) => {
                if let Some(browser) = &mut state.native_browser {
                    let rows = browser.entries.len() + 1;
                    browser.selected = (browser.selected as isize + delta)
                        .clamp(0, rows.saturating_sub(1) as isize)
                        as usize;
                }
            }
            _ => {}
        }
    }

    pub fn select_new_agent_field(&mut self, field: NewAgentField) {
        if let Some(state) = &mut self.new_agent {
            state.draft.selected_field = field;
        }
    }

    pub fn select_workspace(&mut self, index: usize) {
        if let Some(state) = &mut self.new_agent
            && index < state.workspaces.len()
        {
            state.selected_workspace = index;
        }
    }

    pub fn select_agent_kind(&mut self, index: usize) {
        if let Some(state) = &mut self.new_agent
            && index < self.available_harnesses.len()
        {
            state.selected_agent = index;
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

    pub fn open_location_choices(&mut self) {
        if self.new_agent.is_some() {
            self.mode = Mode::NewAgent(NewAgentPage::Locations);
        }
    }

    pub fn select_location(&mut self, index: usize) {
        if let Some(state) = &mut self.new_agent
            && index < Checkout::ALL.len()
        {
            state.selected_location = index;
        }
    }

    pub fn confirm_location(&mut self) {
        let Some(state) = &mut self.new_agent else {
            return;
        };
        let Some(choice) = Checkout::ALL.get(state.selected_location).copied() else {
            return;
        };
        if choice == Checkout::NewWorktree && state.repository_root.is_none() {
            return;
        }
        state.draft.checkout = choice;
        self.mode = Mode::NewAgent(NewAgentPage::Form);
    }

    pub fn set_checkout_choice(&mut self, checkout: Checkout) {
        let Some(state) = &mut self.new_agent else {
            return;
        };
        if checkout == Checkout::NewWorktree && state.repository_root.is_none() {
            return;
        }
        state.selected_location = Checkout::ALL
            .iter()
            .position(|candidate| *candidate == checkout)
            .unwrap_or(0);
        state.draft.checkout = checkout;
        self.mode = Mode::NewAgent(NewAgentPage::Form);
    }

    pub fn set_workspace_repository(&mut self, repository_root: Option<PathBuf>) {
        let Some(state) = &mut self.new_agent else {
            return;
        };
        state.repository_root = repository_root;
        if state.repository_root.is_none() {
            state.draft.checkout = Checkout::Local;
            state.selected_location = 0;
        }
    }

    pub fn begin_worktree(&mut self, generation: u64) {
        if let Some(state) = &mut self.new_agent {
            state.worktree_generation = generation;
            self.mode = Mode::NewAgent(NewAgentPage::CreatingWorktree);
        }
    }

    pub fn cancel_worktree(&mut self) {
        if self.mode != Mode::NewAgent(NewAgentPage::CreatingWorktree) {
            return;
        }
        if let Some(state) = &mut self.new_agent {
            state.worktree_generation = state.worktree_generation.saturating_add(1);
            self.mode = Mode::NewAgent(NewAgentPage::Form);
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
        state.draft.agent = self.available_harnesses.get(state.selected_agent).copied();
        self.mode = Mode::NewAgent(NewAgentPage::Form);
    }

    pub fn set_agent_choice(&mut self, kind: AgentKind) {
        let Some(index) = self
            .available_harnesses
            .iter()
            .position(|candidate| *candidate == kind)
        else {
            return;
        };
        if let Some(state) = &mut self.new_agent {
            state.selected_agent = index;
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

    pub fn new_agent_submission(&self) -> Option<(AgentKind, PathBuf, Checkout)> {
        let state = self.new_agent.as_ref()?;
        if state.draft.selected_field != NewAgentField::Start {
            return None;
        }
        Some((
            state.draft.agent?,
            state.draft.workspace.clone()?,
            state.draft.checkout,
        ))
    }

    pub fn finish_new_agent(&mut self) {
        self.new_agent = None;
        self.mode = Mode::Terminal;
    }

    pub fn open_native_browser(&mut self, path: PathBuf, generation: u64) {
        let Some(state) = &mut self.new_agent else {
            return;
        };
        state.native_browser = Some(NativeBrowserState {
            generation,
            requested_path: path.clone(),
            current_path: path,
            entries: Vec::new(),
            selected: 0,
            loading: true,
            error: None,
        });
        self.mode = Mode::NewAgent(NewAgentPage::NativeBrowser);
    }

    pub fn begin_directory_load(&mut self, path: PathBuf, generation: u64) {
        let Some(browser) = self
            .new_agent
            .as_mut()
            .and_then(|state| state.native_browser.as_mut())
        else {
            return;
        };
        browser.generation = generation;
        browser.requested_path = path;
        browser.loading = true;
        browser.error = None;
    }

    pub fn apply_directory_load(
        &mut self,
        generation: u64,
        path: PathBuf,
        result: Result<Vec<DirectoryChoice>, String>,
    ) -> bool {
        let Some(browser) = self
            .new_agent
            .as_mut()
            .and_then(|state| state.native_browser.as_mut())
        else {
            return false;
        };
        if browser.generation != generation || browser.requested_path != path {
            return false;
        }
        browser.loading = false;
        match result {
            Ok(entries) => {
                browser.current_path = path;
                browser.entries = entries;
                browser.selected = 0;
                browser.error = None;
            }
            Err(error) => browser.error = Some(error),
        }
        true
    }

    pub fn native_browser(&self) -> Option<&NativeBrowserState> {
        self.new_agent.as_ref()?.native_browser.as_ref()
    }

    pub fn set_native_browser_position(&mut self, position: usize) {
        if let Some(browser) = self
            .new_agent
            .as_mut()
            .and_then(|state| state.native_browser.as_mut())
        {
            browser.selected = position.min(browser.entries.len());
        }
    }

    pub fn native_browser_action(&self) -> Option<BrowserAction> {
        let browser = self.native_browser()?;
        if browser.selected == 0 {
            Some(BrowserAction::Select(browser.current_path.clone()))
        } else {
            browser
                .entries
                .get(browser.selected - 1)
                .map(|choice| BrowserAction::Load(choice.path.clone()))
        }
    }

    pub fn choose_browsed_workspace(&mut self, path: PathBuf) {
        let Some(state) = &mut self.new_agent else {
            return;
        };
        state.draft.workspace = Some(path.clone());
        if !state.workspaces.iter().any(|choice| choice.path == path) {
            state.workspaces.insert(
                0,
                WorkspaceChoice {
                    path,
                    available: true,
                },
            );
            state.selected_workspace = 0;
        }
        state.native_browser = None;
        self.mode = Mode::NewAgent(NewAgentPage::Form);
    }

    pub fn close_native_browser(&mut self) {
        if let Some(state) = &mut self.new_agent {
            state.native_browser = None;
            self.mode = Mode::NewAgent(NewAgentPage::Workspaces);
        }
    }

    pub fn open_embedded_browser(&mut self) {
        if self.new_agent.is_some() {
            self.mode = Mode::NewAgent(NewAgentPage::EmbeddedBrowser);
        }
    }

    pub fn close_embedded_browser(&mut self) {
        if self.new_agent.is_some() {
            self.mode = Mode::NewAgent(NewAgentPage::Workspaces);
        }
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
            conversation_id: None,
        }
    }

    fn app() -> App {
        App::new("workspace".into(), ThemeName::Dark, false, None)
    }

    fn available_update(kind: AgentKind, current: &str, latest: &str) -> HarnessVersion {
        HarnessVersion {
            kind,
            current: current.into(),
            latest: latest.into(),
            update_available: true,
        }
    }

    #[test]
    fn harness_checks_track_each_provider_and_only_announce_a_release_once() {
        let mut app = app();
        app.set_available_harnesses(vec![AgentKind::Codex, AgentKind::Claude]);
        app.begin_harness_check();

        let newly_available = app.apply_harness_check(vec![
            (
                AgentKind::Codex,
                Ok(available_update(AgentKind::Codex, "0.147.0", "0.148.0")),
            ),
            (AgentKind::Claude, Err("release service unavailable".into())),
        ]);

        assert_eq!(newly_available, vec![AgentKind::Codex]);
        assert!(matches!(
            &app.harness_update(AgentKind::Codex).unwrap().status,
            HarnessUpdateStatus::UpdateAvailable { current, latest }
                if current == "0.147.0" && latest == "0.148.0"
        ));
        assert!(matches!(
            &app.harness_update(AgentKind::Claude).unwrap().status,
            HarnessUpdateStatus::CheckFailed { message }
                if message == "release service unavailable"
        ));
        assert_eq!(
            app.harness_update(AgentKind::Grok).unwrap().status,
            HarnessUpdateStatus::NotInstalled
        );

        app.begin_harness_check();
        assert!(
            app.apply_harness_check(vec![(
                AgentKind::Codex,
                Ok(available_update(AgentKind::Codex, "0.147.0", "0.148.0")),
            )])
            .is_empty()
        );
    }

    #[test]
    fn failed_updates_remain_retryable_and_open_agents_are_counted() {
        let mut app = app();
        app.set_available_harnesses(vec![AgentKind::Codex]);
        app.apply_harness_check(vec![(
            AgentKind::Codex,
            Ok(available_update(AgentKind::Codex, "0.147.0", "0.148.0")),
        )]);
        app.add_agent(snapshot(1, 0));

        assert!(app.begin_harness_update(AgentKind::Codex));
        app.apply_harness_update(AgentKind::Codex, &Err("permission denied".into()));

        assert_eq!(app.running_harness_count(AgentKind::Codex), 1);
        assert!(matches!(
            &app.harness_update(AgentKind::Codex).unwrap().status,
            HarnessUpdateStatus::UpdateFailed { message, .. } if message == "permission denied"
        ));
        assert!(app.begin_harness_update(AgentKind::Codex));
    }

    #[test]
    fn harness_settings_navigation_includes_the_interval_and_every_harness() {
        let mut app = app();
        assert_eq!(app.harness_settings_selection(), 0);
        app.move_harness_settings_selection(-1);
        assert_eq!(
            app.selected_harness_update_kind(),
            Some(AgentKind::OpenCode)
        );
        app.move_harness_settings_selection(1);
        assert_eq!(app.selected_harness_update_kind(), None);

        app.open_harness_settings();
        assert_eq!(app.mode(), Mode::Settings);
        assert_eq!(app.settings_tab(), SettingsTab::Harnesses);
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
    fn remote_output_churn_is_recorded_without_a_metadata_redraw() {
        let mut app = app();
        let mut first = remote_agent(1, 0, 0);
        first.activity = AgentActivity::Working;
        first.recognition = Some(RecognitionEvidence {
            provider: AgentKind::Codex,
            claim: AgentActivity::Working,
            rule: "codex.active-turn".into(),
            evidence: "Thinking".into(),
        });
        app.add_remote_agent(first.clone());

        first.output_generation = 1;
        first.recognition.as_mut().unwrap().evidence = "Working".into();
        assert!(!app.update_remote_agent(first));
        assert!(app.agents()[0].has_unseen_output());
    }

    #[test]
    fn done_is_an_unviewed_idle_completion_and_blocked_remains_explicit() {
        let mut snapshot = remote_agent(1, 2, 1);
        snapshot.activity = AgentActivity::Idle;
        snapshot.completed_generation = 2;
        let mut app = app();
        app.add_remote_agent(snapshot.clone());

        assert_eq!(app.agents()[0].display_status(), AgentDisplayStatus::Done);
        assert_eq!(app.mark_selected_seen(), Some((AgentId::new(1), 2)));
        assert_eq!(app.agents()[0].display_status(), AgentDisplayStatus::Idle);

        snapshot.activity = AgentActivity::Blocked;
        assert!(app.update_remote_agent(snapshot));
        assert_eq!(
            app.agents()[0].display_status(),
            AgentDisplayStatus::NeedsYou
        );
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
    fn sidebar_scroll_is_bounded_and_selection_restores_follow_mode() {
        let mut app = app();
        for id in 1..=8 {
            app.add_agent(snapshot(id, 0));
        }

        app.scroll_sidebar(-1, 7, 3);
        assert_eq!(app.sidebar_scroll(), Some(14));
        app.scroll_sidebar(99, 7, 3);
        assert_eq!(app.sidebar_scroll(), Some(17));

        app.select_previous();
        assert_eq!(app.sidebar_scroll(), None);
    }

    #[test]
    fn sidebar_width_clamps_to_the_icon_rail_and_a_usable_maximum() {
        let mut app = app();
        assert_eq!(app.sidebar_width(), SIDEBAR_DEFAULT_WIDTH);
        app.set_sidebar_width(0);
        assert_eq!(app.sidebar_width(), SIDEBAR_MIN_WIDTH);
        app.set_sidebar_width(u16::MAX);
        assert_eq!(app.sidebar_width(), SIDEBAR_MAX_WIDTH);
        app.set_sidebar_width(16);
        assert_eq!(app.sidebar_width(), 16);
        assert_eq!(app.sidebar_content_height(1), 0);
        app.add_agent(snapshot(1, 0));
        app.add_agent(snapshot(2, 0));
        assert_eq!(app.sidebar_content_height(1), 2);
        assert_eq!(app.sidebar_content_height(3), 6);
    }

    #[test]
    fn menu_navigation_uses_the_shared_item_order() {
        let mut app = app();

        app.select_previous_menu_item();
        assert_eq!(app.menu_selected(), MenuItem::Settings);
        app.select_next_menu_item();
        assert_eq!(app.menu_selected(), MenuItem::Detach);
    }

    fn selected_tab(app: &App) -> Option<AgentKind> {
        app.selected_usage().map(|provider| provider.kind)
    }

    fn usage_report(kind: AgentKind) -> UsageProviderReport {
        UsageProviderReport {
            kind,
            report: svarm_agent::protocol::UsageReport::NotProbed,
            observed_at_ms: None,
            refreshing: false,
        }
    }

    fn overview(kinds: &[AgentKind]) -> UsageOverview {
        UsageOverview {
            providers: kinds.iter().copied().map(usage_report).collect(),
        }
    }

    #[test]
    fn usage_opens_and_defaults_to_the_first_reported_provider() {
        let mut app = app();
        app.set_usage(overview(&[AgentKind::Codex, AgentKind::Claude]));
        app.open_usage();

        assert_eq!(app.mode(), Mode::Usage);
        assert_eq!(selected_tab(&app), Some(AgentKind::Codex));
        assert_eq!(selected_tab(&app), Some(AgentKind::Codex));
    }

    #[test]
    fn usage_tabs_cycle_over_reported_providers_in_both_directions() {
        let mut app = app();
        app.set_usage(overview(&[AgentKind::Codex, AgentKind::Claude]));

        app.move_usage_tab(1);
        assert_eq!(selected_tab(&app), Some(AgentKind::Claude));
        app.move_usage_tab(1);
        assert_eq!(selected_tab(&app), Some(AgentKind::Codex), "cycles round");
        app.move_usage_tab(-1);
        assert_eq!(selected_tab(&app), Some(AgentKind::Claude));
    }

    #[test]
    fn selecting_a_provider_that_is_not_reported_is_ignored() {
        let mut app = app();
        app.set_usage(overview(&[AgentKind::Codex]));

        app.select_usage_tab(AgentKind::Grok);
        assert_eq!(selected_tab(&app), Some(AgentKind::Codex));
    }

    #[test]
    fn a_refreshed_overview_keeps_the_selected_tab_but_clamps_when_it_disappears() {
        let mut app = app();
        app.set_usage(overview(&[AgentKind::Codex, AgentKind::Claude]));
        app.select_usage_tab(AgentKind::Claude);

        // A provider still present keeps the selection put.
        assert!(app.set_usage(UsageOverview {
            providers: vec![
                usage_report(AgentKind::Codex),
                UsageProviderReport {
                    refreshing: true,
                    ..usage_report(AgentKind::Claude)
                },
            ],
        }));
        assert_eq!(selected_tab(&app), Some(AgentKind::Claude));

        // When it disappears the selection falls back rather than pointing at nothing.
        app.set_usage(overview(&[AgentKind::Codex]));
        assert_eq!(selected_tab(&app), Some(AgentKind::Codex));

        app.set_usage(overview(&[]));
        assert_eq!(selected_tab(&app), None);
        assert!(app.selected_usage().is_none());
    }

    #[test]
    fn an_unchanged_overview_reports_no_redraw() {
        let mut app = app();
        assert!(app.set_usage(overview(&[AgentKind::Codex])));
        assert!(!app.set_usage(overview(&[AgentKind::Codex])));
    }

    #[test]
    fn moving_tabs_without_any_provider_is_harmless() {
        let mut app = app();
        app.move_usage_tab(1);
        assert_eq!(selected_tab(&app), None);
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
            Some((
                AgentKind::Claude,
                PathBuf::from("/tmp/one"),
                Checkout::Local
            ))
        );
    }

    #[test]
    fn new_agent_form_restores_the_saved_checkout_choice() {
        let mut app = app();
        app.open_new_agent_with_checkout(
            Some(PathBuf::from("/tmp/one")),
            Some(AgentKind::Claude),
            Checkout::NewWorktree,
            vec![WorkspaceChoice {
                path: PathBuf::from("/tmp/one"),
                available: true,
            }],
        );

        assert_eq!(
            app.new_agent().unwrap().draft.checkout,
            Checkout::NewWorktree
        );
        assert_eq!(app.new_agent().unwrap().selected_location, 1);
    }

    #[test]
    fn new_agent_fields_cycle_through_location() {
        let mut app = app();
        app.open_new_agent(None, None, Vec::new());
        assert_eq!(
            app.new_agent().unwrap().draft.selected_field,
            NewAgentField::Workspace
        );
        app.move_new_agent_selection(1);
        assert_eq!(
            app.new_agent().unwrap().draft.selected_field,
            NewAgentField::Location
        );
        app.move_new_agent_selection(1);
        assert_eq!(
            app.new_agent().unwrap().draft.selected_field,
            NewAgentField::Agent
        );
        app.move_new_agent_selection(1);
        assert_eq!(
            app.new_agent().unwrap().draft.selected_field,
            NewAgentField::Start
        );
        app.move_new_agent_selection(1);
        assert_eq!(
            app.new_agent().unwrap().draft.selected_field,
            NewAgentField::Workspace
        );
        app.move_new_agent_selection(-1);
        assert_eq!(
            app.new_agent().unwrap().draft.selected_field,
            NewAgentField::Start
        );
    }

    #[test]
    fn new_agent_choices_only_include_available_harnesses() {
        let mut app = app();
        app.set_available_harnesses(vec![AgentKind::Claude]);
        app.open_new_agent(None, None, Vec::new());
        app.open_agent_choices();

        app.confirm_agent();

        assert_eq!(
            app.new_agent().unwrap().draft.agent,
            Some(AgentKind::Claude)
        );
        assert_eq!(app.available_harnesses(), &[AgentKind::Claude]);
    }

    #[test]
    fn a_non_repository_workspace_disables_worktrees_and_forces_local_checkout() {
        let workspaces = vec![
            WorkspaceChoice {
                path: PathBuf::from("/tmp/one"),
                available: true,
            },
            WorkspaceChoice {
                path: PathBuf::from("/tmp/two"),
                available: true,
            },
        ];
        let mut app = app();
        app.open_new_agent(
            Some(PathBuf::from("/tmp/one")),
            Some(AgentKind::Claude),
            workspaces,
        );
        app.set_workspace_repository(Some(PathBuf::from("/tmp/one")));
        app.set_checkout_choice(Checkout::NewWorktree);
        assert_eq!(
            app.new_agent().unwrap().draft.checkout,
            Checkout::NewWorktree
        );

        app.set_workspace_repository(None);
        assert_eq!(app.new_agent().unwrap().draft.checkout, Checkout::Local);
        app.set_checkout_choice(Checkout::NewWorktree);
        assert_eq!(app.new_agent().unwrap().draft.checkout, Checkout::Local);
        app.open_location_choices();
        app.select_location(1);
        app.confirm_location();
        assert_eq!(app.mode(), Mode::NewAgent(NewAgentPage::Locations));
        assert_eq!(app.new_agent().unwrap().draft.checkout, Checkout::Local);

        app.set_workspace_repository(Some(PathBuf::from("/tmp/two")));
        app.set_checkout_choice(Checkout::NewWorktree);
        app.select_new_agent_field(NewAgentField::Start);
        assert_eq!(
            app.new_agent_submission(),
            Some((
                AgentKind::Claude,
                PathBuf::from("/tmp/one"),
                Checkout::NewWorktree
            ))
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
    fn native_browser_discards_stale_results_and_keeps_the_last_good_listing_on_error() {
        let mut app = app();
        app.open_new_agent(None, Some(AgentKind::Codex), Vec::new());
        app.open_native_browser(PathBuf::from("/tmp"), 1);
        let entries = vec![DirectoryChoice {
            path: PathBuf::from("/tmp/one"),
            label: "one".into(),
        }];
        assert!(app.apply_directory_load(1, PathBuf::from("/tmp"), Ok(entries.clone())));

        app.begin_directory_load(PathBuf::from("/var"), 2);
        assert!(!app.apply_directory_load(1, PathBuf::from("/tmp"), Ok(Vec::new())));
        assert!(app.apply_directory_load(
            2,
            PathBuf::from("/var"),
            Err("permission denied".into())
        ));

        let browser = app.native_browser().unwrap();
        assert_eq!(browser.current_path, PathBuf::from("/tmp"));
        assert_eq!(browser.entries, entries);
        assert_eq!(browser.error.as_deref(), Some("permission denied"));
    }

    #[test]
    fn native_browser_rows_select_current_or_descend_and_stay_in_bounds() {
        let mut app = app();
        app.open_new_agent(None, Some(AgentKind::Codex), Vec::new());
        app.open_native_browser(PathBuf::from("/tmp"), 1);
        app.apply_directory_load(
            1,
            PathBuf::from("/tmp"),
            Ok(vec![DirectoryChoice {
                path: PathBuf::from("/tmp/child"),
                label: "child".into(),
            }]),
        );

        assert_eq!(
            app.native_browser_action(),
            Some(BrowserAction::Select(PathBuf::from("/tmp")))
        );
        app.move_new_agent_selection(99);
        assert_eq!(
            app.native_browser_action(),
            Some(BrowserAction::Load(PathBuf::from("/tmp/child")))
        );
        app.move_new_agent_selection(-99);
        assert_eq!(app.native_browser().unwrap().selected, 0);
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
            archived: Vec::new(),
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
            archived: Vec::new(),
        };

        let app = App::hydrate(snapshot, ThemeName::Dark, None);

        assert_eq!(app.mode(), Mode::Terminal);
        assert_eq!(app.selected_agent_id(), None);
        assert_eq!(app.exit_intent(), ExitIntent::None);
    }

    #[test]
    fn unnamed_conversations_open_an_archive_unavailable_modal() {
        let mut unnamed = app();
        unnamed.add_agent(snapshot(1, 0));
        assert!(!unnamed.request_archive(unnamed.selected_index()));
        assert_eq!(unnamed.mode(), Mode::ArchiveUnavailable);
        assert_eq!(unnamed.notice(), None);

        let mut active = remote_agent(1, 0, 0);
        active.conversation_title = Some("Archive work".into());
        active.conversation_id = Some("019ff1d3-375e-7a72-a176-c47497827e49".into());
        let mut app = App::hydrate(
            SvarmSessionSnapshot {
                summary: summary(7, 20),
                selected_agent_id: Some(active.id),
                rows: 24,
                cols: 80,
                agents: vec![active.clone()],
                archived: Vec::new(),
            },
            ThemeName::Dark,
            None,
        );
        assert!(!app.request_archive(app.selected_index()));
        assert_eq!(app.mode(), Mode::ConfirmArchive);

        active.activity = AgentActivity::Idle;
        app = App::hydrate(
            SvarmSessionSnapshot {
                summary: summary(7, 20),
                selected_agent_id: Some(active.id),
                rows: 24,
                cols: 80,
                agents: vec![active],
                archived: Vec::new(),
            },
            ThemeName::Dark,
            None,
        );
        assert!(app.request_archive(app.selected_index()));
    }

    #[test]
    fn archiving_a_non_selected_agent_preserves_the_current_selection() {
        let mut running = remote_agent(1, 0, 0);
        running.conversation_title = Some("Busy agent".into());
        running.conversation_id = Some("019ff1d3-375e-7a72-a176-c47497827e49".into());
        let mut idle = remote_agent(2, 0, 0);
        idle.conversation_title = Some("Idle agent".into());
        idle.conversation_id = Some("019ff1d3-375e-7a72-a176-c47497827e4a".into());
        idle.activity = AgentActivity::Idle;
        let mut busy = remote_agent(3, 0, 0);
        busy.conversation_title = Some("Another busy agent".into());
        busy.conversation_id = Some("019ff1d3-375e-7a72-a176-c47497827e4b".into());
        let mut app = App::hydrate(
            SvarmSessionSnapshot {
                summary: summary(7, 20),
                selected_agent_id: Some(running.id),
                rows: 24,
                cols: 80,
                agents: vec![running, idle, busy],
                archived: Vec::new(),
            },
            ThemeName::Dark,
            None,
        );
        assert_eq!(app.selected_index(), 0);

        // Archiving the second (unselected, idle) agent should not disturb selection.
        assert!(app.request_archive(1));
        assert_eq!(app.selected_index(), 0);
        assert_eq!(app.take_archive_target(), Some(AgentId::new(2)));

        // Same for the confirmation-required path: targeting a different, busy agent
        // opens the modal without moving the selection off the currently selected one.
        assert!(!app.request_archive(2));
        assert_eq!(app.mode(), Mode::ConfirmArchive);
        assert_eq!(app.selected_index(), 0);

        // Cancelling clears the pending target instead of leaving it stale.
        app.cancel_confirmation();
        assert_eq!(app.take_archive_target(), None);
    }

    #[test]
    fn archived_numbers_open_reactivation_confirmation_without_selecting_an_agent() {
        let active = remote_agent(1, 0, 0);
        let mut app = App::hydrate(
            SvarmSessionSnapshot {
                summary: summary(7, 20),
                selected_agent_id: Some(active.id),
                rows: 24,
                cols: 80,
                agents: vec![active],
                archived: vec![archived("first", "First archived")],
            },
            ThemeName::Dark,
            None,
        );

        assert!(!app.select_sidebar_index(1));
        assert_eq!(app.mode(), Mode::ConfirmResume);
        assert_eq!(app.pending_resume(), Some("first"));
        assert_eq!(app.selected_agent_id(), Some(AgentId::new(1)));
    }

    #[test]
    fn archived_conversations_can_be_reached_and_cycled_from_the_keyboard() {
        let mut app = App::hydrate(
            SvarmSessionSnapshot {
                summary: summary(7, 20),
                selected_agent_id: None,
                rows: 24,
                cols: 80,
                agents: Vec::new(),
                archived: vec![
                    archived("first", "First archived"),
                    archived("second", "Second archived"),
                ],
            },
            ThemeName::Dark,
            None,
        );

        assert!(app.request_resume_archived(0));
        assert_eq!(app.pending_resume(), Some("first"));
        assert_eq!(app.pending_resume_title(), Some("First archived"));
        app.cycle_pending_archive(1);
        assert_eq!(app.pending_resume(), Some("second"));
        app.cycle_pending_archive(1);
        assert_eq!(app.pending_resume(), Some("first"));
        app.cycle_pending_archive(-1);
        assert_eq!(app.pending_resume(), Some("second"));
    }

    #[test]
    fn conversation_switch_archives_the_old_id_and_removes_a_reactivated_id() {
        let mut active = remote_agent(1, 0, 0);
        active.conversation_id = Some("new".into());
        let mut app = App::hydrate(
            SvarmSessionSnapshot {
                summary: summary(7, 20),
                selected_agent_id: Some(active.id),
                rows: 24,
                cols: 80,
                agents: vec![active.clone()],
                archived: vec![archived("new", "Previously archived")],
            },
            ThemeName::Dark,
            None,
        );

        app.apply_conversation_switch(
            active,
            Some(archived("old", "Old conversation")),
            Some("new"),
        );

        assert_eq!(app.archived().len(), 1);
        assert_eq!(app.archived()[0].conversation_id, "old");
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
            working_directory: None,
            status: SessionStatus::Running,
            exit: None,
            output_generation,
            seen_generation,
            completed_generation: 0,
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            conversation_title: None,
            conversation_id: None,
            activity: AgentActivity::Unknown,
            recognition: None,
            git: None,
        }
    }

    fn archived(id: &str, title: &str) -> ArchivedConversation {
        ArchivedConversation {
            conversation_id: id.into(),
            title: title.into(),
            kind: AgentKind::Codex,
            launch_directory: "/tmp".into(),
        }
    }
}
