use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use svarm_agent::terminal_model::TerminalSnapshot;

use crate::{
    app::{AgentDisplayStatus, App, MenuItem, Mode, NewAgentField, NewAgentPage, SessionChooser},
    input::{MANAGEMENT_KEYBINDINGS, ManagementCommand},
    screen::TerminalScreen,
    theme::Theme,
};
use svarm_agent::{AgentKind, SessionStatus, TerminalProcessSnapshot};

pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;
const COMPACT_MODAL_WIDTH: u16 = 64;
const COMPACT_MODAL_HEIGHT: u16 = 12;
const STANDARD_MODAL_WIDTH: u16 = 72;
const STANDARD_MODAL_HEIGHT: u16 = 18;
pub const SIDEBAR_WIDTH: u16 = 28;
const AGENT_CARD_HEIGHT: u16 = 3;
const MENU_HEIGHT: u16 = MenuItem::ALL.len() as u16 + 2;
const MENU_WIDTH: u16 = 46;

#[derive(Clone, Copy)]
enum ModalSize {
    Compact,
    Standard,
    Large,
}

impl ModalSize {
    fn area(self, terminal: Rect) -> Rect {
        match self {
            Self::Compact => centered_rect(COMPACT_MODAL_WIDTH, COMPACT_MODAL_HEIGHT, terminal),
            Self::Standard => centered_rect(STANDARD_MODAL_WIDTH, STANDARD_MODAL_HEIGHT, terminal),
            Self::Large => centered_rect(
                terminal.width.saturating_sub(4).min(100),
                terminal.height.saturating_sub(2).min(30),
                terminal,
            ),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct UiModel<'a> {
    pub app: &'a App,
    pub screen: Option<&'a TerminalSnapshot>,
    pub scrolled: bool,
    pub embedded: Option<&'a TerminalProcessSnapshot>,
    pub theme: Theme,
    pub colors_enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClickAction {
    Management(ManagementCommand),
    ToggleMenu,
    SidebarItem(usize),
    MenuItem(MenuItem),
    Next,
    Previous,
    Confirm,
    Cancel,
    NewAgentField(NewAgentField),
    Workspace(usize),
    BrowseWorkspaces,
    AgentKind(usize),
    NativeBrowserItem(usize),
    NativeBrowserParent,
    ThemePrevious,
    ThemeNext,
    EmbeddedAccept,
    EmbeddedCancel,
    EmbeddedForceClose,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionChooserClick {
    Choose(usize),
    Next,
    Previous,
    Open,
    Cancel,
    New,
}

#[derive(Clone, Copy)]
struct ActionHint {
    text: &'static str,
    action: ClickAction,
}

#[derive(Clone, Copy)]
struct SessionActionHint {
    text: &'static str,
    action: SessionChooserClick,
    needs_new: bool,
}

const SESSION_HINTS: &[SessionActionHint] = &[
    SessionActionHint {
        text: "[Enter] open",
        action: SessionChooserClick::Open,
        needs_new: false,
    },
    SessionActionHint {
        text: "[j] next",
        action: SessionChooserClick::Next,
        needs_new: false,
    },
    SessionActionHint {
        text: "[k] previous",
        action: SessionChooserClick::Previous,
        needs_new: false,
    },
    SessionActionHint {
        text: "[Esc] cancel",
        action: SessionChooserClick::Cancel,
        needs_new: false,
    },
    SessionActionHint {
        text: "[n] new",
        action: SessionChooserClick::New,
        needs_new: true,
    },
];

const FORM_HINTS: &[ActionHint] = &[
    ActionHint {
        text: "[j] next",
        action: ClickAction::Next,
    },
    ActionHint {
        text: "[k] previous",
        action: ClickAction::Previous,
    },
    ActionHint {
        text: "[Enter] open",
        action: ClickAction::Confirm,
    },
    ActionHint {
        text: "[Esc] cancel",
        action: ClickAction::Cancel,
    },
];
const WORKSPACE_HINTS: &[ActionHint] = &[
    ActionHint {
        text: "[Enter] use",
        action: ClickAction::Confirm,
    },
    ActionHint {
        text: "[b] browse",
        action: ClickAction::BrowseWorkspaces,
    },
    ActionHint {
        text: "[j] next",
        action: ClickAction::Next,
    },
    ActionHint {
        text: "[k] previous",
        action: ClickAction::Previous,
    },
    ActionHint {
        text: "[Esc] back",
        action: ClickAction::Cancel,
    },
];
const AGENT_HINTS: &[ActionHint] = &[
    ActionHint {
        text: "[Enter] use",
        action: ClickAction::Confirm,
    },
    ActionHint {
        text: "[j] next",
        action: ClickAction::Next,
    },
    ActionHint {
        text: "[k] previous",
        action: ClickAction::Previous,
    },
    ActionHint {
        text: "[Esc] back",
        action: ClickAction::Cancel,
    },
];
const BROWSER_HINTS: &[ActionHint] = &[
    ActionHint {
        text: "[Enter/l] open/use",
        action: ClickAction::Confirm,
    },
    ActionHint {
        text: "[h] parent",
        action: ClickAction::NativeBrowserParent,
    },
    ActionHint {
        text: "[j] next",
        action: ClickAction::Next,
    },
    ActionHint {
        text: "[k] previous",
        action: ClickAction::Previous,
    },
    ActionHint {
        text: "[Esc] cancel",
        action: ClickAction::Cancel,
    },
];
const CONFIRM_HINTS: &[ActionHint] = &[
    ActionHint {
        text: "[y] Yes",
        action: ClickAction::Confirm,
    },
    ActionHint {
        text: "[Esc] Cancel",
        action: ClickAction::Cancel,
    },
];
const STOP_HINTS: &[ActionHint] = &[
    ActionHint {
        text: "[y] Stop session",
        action: ClickAction::Confirm,
    },
    ActionHint {
        text: "[Esc] Cancel",
        action: ClickAction::Cancel,
    },
];
const RESUME_HINTS: &[ActionHint] = &[
    ActionHint {
        text: "[j] next",
        action: ClickAction::Next,
    },
    ActionHint {
        text: "[k] previous",
        action: ClickAction::Previous,
    },
    ActionHint {
        text: "[y] Reactivate",
        action: ClickAction::Confirm,
    },
    ActionHint {
        text: "[Esc] Cancel",
        action: ClickAction::Cancel,
    },
];
const ARCHIVE_UNAVAILABLE_HINTS: &[ActionHint] = &[ActionHint {
    text: "[Enter/Esc] close",
    action: ClickAction::Cancel,
}];
const BACK_HINTS: &[ActionHint] = &[ActionHint {
    text: "[Esc] back",
    action: ClickAction::Cancel,
}];
const SETTINGS_HINTS: &[ActionHint] = &[
    ActionHint {
        text: "[←/h] previous",
        action: ClickAction::ThemePrevious,
    },
    ActionHint {
        text: "[→/l] next",
        action: ClickAction::ThemeNext,
    },
    ActionHint {
        text: "[Esc] back",
        action: ClickAction::Cancel,
    },
];
const EMBEDDED_HINTS: &[ActionHint] = &[
    ActionHint {
        text: "[q] use current directory",
        action: ClickAction::EmbeddedAccept,
    },
    ActionHint {
        text: "[Q] cancel",
        action: ClickAction::EmbeddedCancel,
    },
    ActionHint {
        text: "[Ctrl+B x] force close",
        action: ClickAction::EmbeddedForceClose,
    },
];

pub(crate) fn render(frame: &mut Frame<'_>, model: UiModel<'_>) {
    let app = model.app;
    let theme = model.theme;
    let area = frame.area();
    frame.render_widget(Block::new().style(theme.page()), area);

    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new(format!(
                "Svarm needs at least {MIN_WIDTH}x{MIN_HEIGHT}\ncurrent terminal: {}x{}",
                area.width, area.height
            ))
            .centered()
            .style(text(theme)),
            area,
        );
        return;
    }

    if app.sidebar_visible() {
        render_sidebar(frame, app, sidebar_area(area), theme, model.colors_enabled);
    }
    render_terminal(
        frame,
        model.screen,
        app.agents().is_empty(),
        model.scrolled,
        app.mode(),
        terminal_area(area, app.sidebar_visible()),
        theme,
    );
    if !app.sidebar_visible()
        && let Some(button) = menu_button_area(area, false)
    {
        frame.render_widget(
            Paragraph::new(" ≡ Menu  ^B m").style(theme.surface()),
            button,
        );
    }

    match app.mode() {
        Mode::NewAgent(NewAgentPage::Form) => render_new_agent_form(frame, app, theme),
        Mode::NewAgent(NewAgentPage::Workspaces) => render_workspace_choices(frame, app, theme),
        Mode::NewAgent(NewAgentPage::Agents) => render_agent_choices(frame, app, theme),
        Mode::NewAgent(NewAgentPage::NativeBrowser) => render_native_browser(frame, app, theme),
        Mode::NewAgent(NewAgentPage::EmbeddedBrowser) | Mode::ToolPrefix => {
            render_embedded_browser(frame, model.embedded, theme)
        }
        Mode::ConfirmClose => {
            render_confirmation(frame, theme, "Close agent?", "Close this agent?")
        }
        Mode::ConfirmArchive => render_confirmation(
            frame,
            theme,
            " Archive conversation? ",
            "Stop this active agent and archive its conversation?",
        ),
        Mode::ArchiveUnavailable => render_archive_unavailable(frame, theme),
        Mode::ConfirmResume => render_resume_confirmation(frame, app, theme),
        Mode::ConfirmQuit => render_stop_confirmation(frame, app, theme),
        Mode::Keybinds => render_keybinds(frame, theme),
        Mode::Settings => render_settings(frame, app, theme),
        _ => {}
    }
}

pub(crate) fn render_session_chooser(
    frame: &mut Frame<'_>,
    chooser: &SessionChooser,
    now_ms: u64,
    theme: Theme,
) {
    let area = frame.area();
    frame.render_widget(Block::new().style(theme.page()), area);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new(format!(
                "Svarm needs at least {MIN_WIDTH}x{MIN_HEIGHT}\ncurrent terminal: {}x{}",
                area.width, area.height
            ))
            .centered()
            .style(text(theme)),
            area,
        );
        return;
    }

    let block = Block::bordered()
        .title(Span::styled(
            " Open Svarm session ",
            accent(theme).add_modifier(Modifier::BOLD),
        ))
        .border_style(border(theme));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let footer_height = 2;
    let list_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(footer_height),
    );
    let visible_rows = usize::from(list_area.height);
    let start = chooser.viewport_start(visible_rows);
    let end = (start + visible_rows).min(chooser.row_count());
    let rows = (start..end).map(|index| {
        let selected = index == chooser.selected();
        let line = if let Some(session) = chooser.sessions().get(index) {
            session_row(session, now_ms, list_area.width.saturating_sub(3), theme)
        } else {
            Line::from(vec![
                Span::styled("+ ", success(theme)),
                Span::styled(
                    "Start new session",
                    text(theme).add_modifier(Modifier::BOLD),
                ),
            ])
        };
        let marker = Span::styled(if selected { "▌ " } else { "  " }, accent(theme));
        let mut spans = vec![marker];
        spans.extend(line.spans);
        ListItem::new(Line::from(spans)).style(if selected {
            theme.selected()
        } else {
            text(theme)
        })
    });
    frame.render_widget(List::new(rows), list_area);

    let footer = SESSION_HINTS
        .iter()
        .filter(|hint| !hint.needs_new || chooser.allow_new())
        .map(|hint| hint.text)
        .collect::<Vec<_>>()
        .join("  ");
    frame.render_widget(
        Paragraph::new(footer).style(theme.muted()),
        Rect::new(
            inner.x + 1,
            inner.bottom().saturating_sub(1),
            inner.width - 1,
            1,
        ),
    );
}

fn session_row(
    session: &svarm_agent::protocol::SessionSummary,
    now_ms: u64,
    _width: u16,
    theme: Theme,
) -> Line<'static> {
    let state = if session.attachment.is_some() {
        "attached"
    } else {
        "detached"
    };
    let age = format_age(now_ms.saturating_sub(session.last_user_activity_ms));
    Line::from(vec![
        Span::styled(format!("{}  ", session.id.0), accent(theme)),
        Span::styled(
            format!(
                "{}  {}/{} running  {}  ",
                state, session.running_agents, session.total_agents, age
            ),
            if session.attachment.is_some() {
                warning(theme)
            } else {
                theme.muted()
            },
        ),
    ])
}

fn format_age(age_ms: u64) -> String {
    let seconds = age_ms / 1_000;
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

pub fn terminal_area(area: Rect, sidebar_visible: bool) -> Rect {
    let sidebar_width = if sidebar_visible {
        SIDEBAR_WIDTH.min(area.width.saturating_sub(1))
    } else {
        0
    };
    Rect::new(
        area.x.saturating_add(sidebar_width),
        area.y,
        area.width.saturating_sub(sidebar_width),
        area.height,
    )
}

pub fn sidebar_area(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y,
        SIDEBAR_WIDTH.min(area.width.saturating_sub(1)),
        area.height,
    )
}

pub fn menu_button_area(area: Rect, sidebar_visible: bool) -> Option<Rect> {
    if area.height == 0 {
        return None;
    }
    if !sidebar_visible {
        return Some(Rect::new(
            area.x,
            area.bottom().saturating_sub(1),
            14.min(area.width),
            1,
        ));
    }
    let sidebar = sidebar_area(area);
    Some(Rect::new(
        sidebar.x,
        sidebar.bottom().saturating_sub(1),
        sidebar.width.saturating_sub(1),
        1,
    ))
}

pub fn new_agent_button_area(area: Rect, sidebar_visible: bool) -> Option<Rect> {
    if !sidebar_visible {
        return None;
    }
    let menu = menu_button_area(area, sidebar_visible)?;
    (menu.y > area.y).then_some(Rect::new(menu.x, menu.y - 1, menu.width, 1))
}

pub fn menu_item_at(area: Rect, column: u16, row: u16) -> Option<MenuItem> {
    let button = menu_button_area(area, true)?;
    let popup = menu_popup_area(button);
    if column <= popup.x || column >= popup.right().saturating_sub(1) {
        return None;
    }
    let index = usize::from(row.checked_sub(popup.y.saturating_add(1))?);
    MenuItem::ALL.get(index).copied()
}

pub fn agent_item_at(app: &App, area: Rect, column: u16, row: u16) -> Option<usize> {
    if !app.sidebar_visible() {
        return None;
    }
    let agents = agent_list_area(app, sidebar_area(area));
    if column < agents.x || column >= agents.right() || row < agents.y || row >= agents.bottom() {
        return None;
    }
    let content_row = sidebar_row_start(app, agents) + usize::from(row - agents.y);
    let active_height = app.agents().len() * usize::from(AGENT_CARD_HEIGHT);
    if content_row < active_height {
        return Some(content_row / usize::from(AGENT_CARD_HEIGHT));
    }
    let archived_row = content_row.checked_sub(active_height + 1)?;
    (archived_row < app.archived().len()).then_some(app.agents().len() + archived_row)
}

pub(crate) fn click_action(app: &App, area: Rect, column: u16, row: u16) -> Option<ClickAction> {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return None;
    }

    if matches!(
        app.mode(),
        Mode::Terminal | Mode::Menu | Mode::Keybinds | Mode::Settings
    ) {
        if app.mode() != Mode::Menu
            && new_agent_button_area(area, app.sidebar_visible())
                .is_some_and(|button| contains(button, column, row))
        {
            return Some(ClickAction::Management(ManagementCommand::ChooseAgent));
        }
        if menu_button_area(area, app.sidebar_visible())
            .is_some_and(|button| contains(button, column, row))
        {
            return Some(if app.sidebar_visible() {
                ClickAction::ToggleMenu
            } else {
                ClickAction::Management(ManagementCommand::OpenMenu)
            });
        }
    }

    match app.mode() {
        Mode::Terminal => {
            if let Some(index) = agent_item_at(app, area, column, row) {
                return Some(ClickAction::SidebarItem(index));
            }
            if app.agents().is_empty()
                && contains(terminal_area(area, app.sidebar_visible()), column, row)
            {
                return Some(ClickAction::Management(ManagementCommand::ChooseAgent));
            }
            None
        }
        Mode::Menu => menu_item_at(area, column, row).map(ClickAction::MenuItem),
        Mode::NewAgent(NewAgentPage::Form) => {
            let inner = dialog_inner(ModalSize::Compact, area);
            if !contains(inner, column, row) {
                return None;
            }
            match row.checked_sub(inner.y)? {
                1 => Some(ClickAction::NewAgentField(NewAgentField::Workspace)),
                2 => Some(ClickAction::NewAgentField(NewAgentField::Agent)),
                3 => Some(ClickAction::NewAgentField(NewAgentField::Start)),
                5 => hint_at(FORM_HINTS, inner.x, column),
                _ => None,
            }
        }
        Mode::NewAgent(NewAgentPage::Workspaces) => {
            let state = app.new_agent()?;
            let inner = dialog_inner(ModalSize::Compact, area);
            if !contains(inner, column, row) {
                return None;
            }
            let visible = 7;
            let start = state.selected_workspace.saturating_sub(visible - 1);
            let count = state.workspaces.len().saturating_sub(start).min(visible);
            let line = usize::from(row - inner.y);
            if state.workspaces.is_empty() && line == 1 {
                return Some(ClickAction::BrowseWorkspaces);
            }
            if (1..=count).contains(&line) {
                return Some(ClickAction::Workspace(start + line - 1));
            }
            (line == count + 2)
                .then(|| hint_at(WORKSPACE_HINTS, inner.x, column))
                .flatten()
        }
        Mode::NewAgent(NewAgentPage::Agents) => {
            let inner = dialog_inner(ModalSize::Compact, area);
            if !contains(inner, column, row) {
                return None;
            }
            let count = AgentKind::ALL.len();
            let line = usize::from(row - inner.y);
            if (1..=count).contains(&line) {
                return Some(ClickAction::AgentKind(line - 1));
            }
            (line == count + 2)
                .then(|| hint_at(AGENT_HINTS, inner.x, column))
                .flatten()
        }
        Mode::NewAgent(NewAgentPage::NativeBrowser) => {
            let browser = app.native_browser()?;
            let modal = ModalSize::Large.area(area);
            let inner = Block::bordered().inner(modal);
            if !contains(inner, column, row) {
                return None;
            }
            let visible = usize::from(modal.height.saturating_sub(6));
            let total = browser.entries.len() + 1;
            let start = browser.selected.saturating_sub(visible - 1);
            let count = total.saturating_sub(start).min(visible);
            let line = usize::from(row - inner.y);
            if (2..2 + count).contains(&line) {
                return Some(ClickAction::NativeBrowserItem(start + line - 2));
            }
            (line == count + 3)
                .then(|| hint_at(BROWSER_HINTS, inner.x, column))
                .flatten()
        }
        Mode::NewAgent(NewAgentPage::EmbeddedBrowser) => {
            let modal = embedded_modal_area(area);
            let footer_y = modal.bottom().saturating_sub(2);
            (row == footer_y)
                .then(|| hint_at(EMBEDDED_HINTS, modal.x + 1, column))
                .flatten()
        }
        Mode::ConfirmClose | Mode::ConfirmArchive => {
            let inner = dialog_inner(ModalSize::Standard, area);
            (row == inner.y + 3 && contains(inner, column, row))
                .then(|| hint_at(CONFIRM_HINTS, inner.x, column))
                .flatten()
        }
        Mode::ArchiveUnavailable => {
            let inner = dialog_inner(ModalSize::Compact, area);
            (row == inner.y + 3 && contains(inner, column, row))
                .then(|| hint_at(ARCHIVE_UNAVAILABLE_HINTS, inner.x, column))
                .flatten()
        }
        Mode::ConfirmResume => {
            let inner = dialog_inner(ModalSize::Standard, area);
            (row == inner.y + 3 && contains(inner, column, row))
                .then(|| hint_at(RESUME_HINTS, inner.x, column))
                .flatten()
        }
        Mode::ConfirmQuit => {
            let inner = dialog_inner(ModalSize::Standard, area);
            (row == inner.y + 4 && contains(inner, column, row))
                .then(|| hint_at(STOP_HINTS, inner.x, column))
                .flatten()
        }
        Mode::Keybinds => {
            let inner = dialog_inner(ModalSize::Standard, area);
            if !contains(inner, column, row) {
                return None;
            }
            let line = usize::from(row - inner.y);
            if let Some(binding) = line
                .checked_sub(1)
                .and_then(|index| MANAGEMENT_KEYBINDINGS.get(index))
            {
                return Some(ClickAction::Management(binding.command));
            }
            (line == MANAGEMENT_KEYBINDINGS.len() + 2)
                .then(|| hint_at(BACK_HINTS, inner.x, column))
                .flatten()
        }
        Mode::Settings => {
            let inner = dialog_inner(ModalSize::Standard, area);
            if !contains(inner, column, row) {
                return None;
            }
            let line = row - inner.y;
            if line == 1 {
                let previous = Rect::new(inner.x + 7, row, 17, 1);
                if contains(previous, column, row) {
                    return Some(ClickAction::ThemePrevious);
                }
                let next_x = inner.x + 24 + app.theme().label().chars().count() as u16;
                if contains(Rect::new(next_x, row, 3, 1), column, row) {
                    return Some(ClickAction::ThemeNext);
                }
            }
            (line == 3)
                .then(|| hint_at(SETTINGS_HINTS, inner.x, column))
                .flatten()
        }
        Mode::Prefix | Mode::ToolPrefix => None,
    }
}

pub(crate) fn session_chooser_click(
    chooser: &SessionChooser,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<SessionChooserClick> {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return None;
    }
    let inner = Block::bordered().inner(area);
    let list = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(2),
    );
    if contains(list, column, row) {
        let visible = usize::from(list.height);
        let index = chooser.viewport_start(visible) + usize::from(row - list.y);
        return (index < chooser.row_count()).then_some(SessionChooserClick::Choose(index));
    }
    if row != inner.bottom().saturating_sub(1) {
        return None;
    }
    let mut x = inner.x + 1;
    for hint in SESSION_HINTS {
        if hint.needs_new && !chooser.allow_new() {
            continue;
        }
        let width = hint.text.chars().count() as u16;
        if column >= x && column < x.saturating_add(width) {
            return Some(hint.action);
        }
        x = x.saturating_add(width + 2);
    }
    None
}

const fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

fn dialog_inner(size: ModalSize, terminal: Rect) -> Rect {
    Block::bordered().inner(size.area(terminal))
}

fn hint_at(hints: &[ActionHint], start_x: u16, column: u16) -> Option<ClickAction> {
    let mut x = start_x + 2;
    for hint in hints {
        let width = hint.text.chars().count() as u16;
        if column >= x && column < x.saturating_add(width) {
            return Some(hint.action);
        }
        x = x.saturating_add(width + 2);
    }
    None
}

fn menu_popup_area(button: Rect) -> Rect {
    Rect::new(
        button.x,
        button.y.saturating_sub(MENU_HEIGHT),
        MENU_WIDTH.max(button.width),
        MENU_HEIGHT,
    )
}

fn agent_list_area(app: &App, sidebar: Rect) -> Rect {
    let inner = Block::new()
        .borders(Borders::TOP | Borders::RIGHT)
        .inner(sidebar);
    let popup_height = if app.mode() == Mode::Menu {
        MENU_HEIGHT
    } else {
        0
    };
    let reserved = 2 + popup_height + u16::from(app.notice().is_some());
    Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(reserved),
    )
}

fn sidebar_row_start(app: &App, area: Rect) -> usize {
    let visible = usize::from(area.height.max(1));
    let max = app.sidebar_content_height().saturating_sub(visible);
    app.sidebar_scroll()
        .unwrap_or_else(|| {
            (app.selected_index() * usize::from(AGENT_CARD_HEIGHT))
                .saturating_sub(visible.saturating_sub(usize::from(AGENT_CARD_HEIGHT)))
        })
        .min(max)
}

pub fn agent_list_page_size(app: &App, area: Rect) -> usize {
    usize::from(agent_list_area(app, sidebar_area(area)).height).max(1)
}

fn render_sidebar(
    frame: &mut Frame<'_>,
    app: &App,
    area: Rect,
    theme: Theme,
    colors_enabled: bool,
) {
    let title = Span::styled(" svarm ", accent(theme).add_modifier(Modifier::BOLD));
    let block = Block::new()
        .title(title)
        .borders(Borders::TOP | Borders::RIGHT)
        .border_style(border(theme))
        .style(Style::default().bg(Color::Reset));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let new_button =
        new_agent_button_area(area, true).expect("visible sidebar has a new-agent button");
    let button = menu_button_area(area, true).expect("visible sidebar has a menu button");
    let agents_area = agent_list_area(app, area);

    let mut rows = Vec::new();
    for (index, agent) in app.agents().iter().enumerate() {
        let selected = index == app.selected_index();
        let marker = if selected { "▌" } else { " " };
        let status = agent.display_status();
        let (circle, status_style) = status_display(status, colors_enabled);
        let content_width = usize::from(agents_area.width.saturating_sub(2));
        let number = format!("{} · ", index + 1);
        let title = end_truncate(
            agent.conversation_title().unwrap_or("Unnamed conversation"),
            usize::from(agents_area.width).saturating_sub(3 + number.chars().count()),
        );
        let directory = agent
            .git()
            .map(|git| git.worktree.as_path())
            .unwrap_or_else(|| agent.launch_directory());
        let directory = directory
            .file_name()
            .unwrap_or(directory.as_os_str())
            .to_string_lossy();
        let selected_style = if selected {
            text(theme).add_modifier(Modifier::BOLD)
        } else {
            text(theme)
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled(marker, accent(theme)),
                Span::styled(format!("{circle} "), status_style),
                Span::styled(number, theme.muted()),
                Span::styled(title, selected_style.add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled(marker, accent(theme)),
                Span::raw(" "),
                Span::styled(agent.kind().label(), text(theme)),
                Span::styled(" · ", theme.muted()),
                Span::styled(
                    end_truncate(
                        &directory,
                        content_width.saturating_sub(agent.kind().label().chars().count() + 5),
                    ),
                    text(theme),
                ),
            ]),
        ];
        if let Some(git) = agent.git() {
            let tracking = git
                .ahead
                .zip(git.behind)
                .filter(|(ahead, behind)| *ahead != 0 || *behind != 0)
                .map_or_else(String::new, |(ahead, behind)| {
                    format!(" ↑{ahead} ↓{behind}")
                });
            let (additions, deletions) = if git.additions == 0 && git.deletions == 0 {
                (String::new(), String::new())
            } else {
                (
                    format!(" +{}", git.additions),
                    format!(" -{}", git.deletions),
                )
            };
            let branch_width = content_width
                .saturating_sub(2)
                .saturating_sub(additions.chars().count())
                .saturating_sub(deletions.chars().count())
                .saturating_sub(tracking.chars().count());
            lines.push(Line::from(vec![
                Span::styled(marker, accent(theme)),
                Span::raw(" "),
                Span::styled(end_truncate(&git.branch, branch_width), text(theme)),
                Span::styled(additions, success(theme)),
                Span::styled(deletions, Style::default().fg(theme.error)),
                Span::styled(tracking, theme.muted()),
            ]));
        } else {
            lines.push(Line::from(Span::styled(marker, accent(theme))));
        }
        rows.append(&mut lines);
    }
    if !app.archived().is_empty() {
        rows.push(Line::from(Span::styled(
            " Archived",
            theme.muted().add_modifier(Modifier::BOLD),
        )));
        rows.extend(
            app.archived()
                .iter()
                .enumerate()
                .map(|(index, conversation)| {
                    let number = format!("  {} · ", app.agents().len() + index + 1);
                    let title = end_truncate(
                        &conversation.title,
                        usize::from(agents_area.width).saturating_sub(number.chars().count()),
                    );
                    Line::from(vec![
                        Span::styled(number, theme.muted()),
                        Span::styled(title, text(theme)),
                    ])
                }),
        );
    }
    let scroll = u16::try_from(sidebar_row_start(app, agents_area)).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(rows).scroll((scroll, 0)), agents_area);

    if let Some(notice) = app.notice() {
        let y = agents_area.bottom();
        frame.render_widget(
            Paragraph::new(format!(" ! {notice}")).style(warning(theme)),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }

    let new_button_style = text(theme);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " + New agent",
                new_button_style.add_modifier(Modifier::BOLD),
            ),
            Span::styled("   ^B n", new_button_style),
        ]))
        .style(new_button_style),
        new_button,
    );

    if app.mode() == Mode::Menu {
        render_menu(frame, app, menu_popup_area(button), theme);
    }

    let button_style = if app.mode() == Mode::Menu {
        theme.selected()
    } else {
        text(theme)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ≡ Menu", button_style.add_modifier(Modifier::BOLD)),
            Span::styled("   ^B m", button_style),
        ]))
        .style(button_style),
        button,
    );
}

fn end_truncate(value: &str, width: usize) -> String {
    let mut characters = value.chars();
    let mut truncated = characters.by_ref().take(width).collect::<String>();
    if characters.next().is_some() && width > 0 {
        truncated.pop();
        truncated.push('…');
    }
    truncated
}

fn status_display(status: AgentDisplayStatus, colors_enabled: bool) -> (&'static str, Style) {
    match status {
        AgentDisplayStatus::Unknown | AgentDisplayStatus::Idle => {
            ("●", Style::default().add_modifier(Modifier::DIM))
        }
        AgentDisplayStatus::Working => ("●", status_color(Color::Yellow, colors_enabled)),
        AgentDisplayStatus::Done => ("●", status_color(Color::Green, colors_enabled)),
        AgentDisplayStatus::NeedsYou | AgentDisplayStatus::Failed => {
            ("●", status_color(Color::Red, colors_enabled))
        }
    }
}

fn status_color(color: Color, colors_enabled: bool) -> Style {
    Style::default().fg(if colors_enabled { color } else { Color::Reset })
}

fn render_menu(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .title(" Menu ")
        .border_style(accent(theme))
        .style(theme.surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = MenuItem::ALL.into_iter().enumerate().map(|(index, item)| {
        let marker = if item == app.menu_selected() {
            " ▌ "
        } else {
            "   "
        };
        let style = if item == app.menu_selected() {
            theme.selected()
        } else {
            text(theme)
        };
        ListItem::new(Line::from(vec![
            Span::styled(marker, accent(theme)),
            Span::styled(format!("[{}] ", index + 1), accent(theme)),
            Span::styled(item.label(), style),
        ]))
        .style(style)
    });
    frame.render_widget(List::new(rows), inner);
}

fn render_terminal(
    frame: &mut Frame<'_>,
    screen: Option<&TerminalSnapshot>,
    no_agents: bool,
    scrolled: bool,
    mode: Mode,
    area: Rect,
    theme: Theme,
) {
    let Some(screen) = screen else {
        frame.render_widget(
            Paragraph::new(if no_agents {
                "No agents open. Press Ctrl+B, then n to start one."
            } else {
                "Agent terminal unavailable."
            })
            .centered()
            .style(theme.muted()),
            area,
        );
        return;
    };
    // The cursor is the host terminal's own, placed below, so that it keeps the shape, color and
    // blink the user configured. Painting one into the buffer can only produce a static block.
    let pane = TerminalScreen::new(screen);
    if mode == Mode::Terminal
        && !scrolled
        && let Some(position) = pane.cursor_position(area)
    {
        frame.set_cursor_position(position);
    }
    frame.render_widget(pane, area);
}

fn render_new_agent_form(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(state) = app.new_agent() else {
        return;
    };
    let workspace = state.draft.workspace.as_ref().map_or_else(
        || "<choose workspace>".into(),
        |path| path.display().to_string(),
    );
    let agent = state.draft.agent.map_or("<choose agent>", AgentKind::label);
    let complete = state.draft.workspace.is_some() && state.draft.agent.is_some();
    let row = |field, label: &str, value: String| {
        let selected = state.draft.selected_field == field;
        Line::from(vec![
            Span::styled(if selected { " > " } else { "   " }, accent(theme)),
            Span::styled(format!("{label:<12}"), text(theme)),
            Span::styled(
                end_truncate(&value, 42),
                if selected {
                    theme.selected()
                } else {
                    theme.muted()
                },
            ),
        ])
    };
    render_dialog(
        frame,
        theme,
        " New agent ",
        ModalSize::Compact,
        vec![
            Line::from(""),
            row(NewAgentField::Workspace, "Workspace", workspace),
            row(NewAgentField::Agent, "Agent", agent.into()),
            Line::from(vec![
                Span::styled(
                    if state.draft.selected_field == NewAgentField::Start {
                        " > "
                    } else {
                        "   "
                    },
                    accent(theme),
                ),
                Span::styled(
                    if complete {
                        "Start agent"
                    } else {
                        "Start agent (disabled)"
                    },
                    if complete { text(theme) } else { theme.muted() },
                ),
            ]),
            Line::from(""),
            hint_line(FORM_HINTS, theme),
        ],
    );
}

fn render_workspace_choices(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(state) = app.new_agent() else {
        return;
    };
    let visible = 7;
    let start = state.selected_workspace.saturating_sub(visible - 1);
    let mut lines = vec![Line::from("")];
    if state.workspaces.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No saved workspaces. Press b to browse.",
            theme.muted(),
        )));
    } else {
        lines.extend(
            state
                .workspaces
                .iter()
                .enumerate()
                .skip(start)
                .take(visible)
                .map(|(index, choice)| {
                    let name = end_truncate(
                        &choice
                            .path
                            .file_name()
                            .unwrap_or(choice.path.as_os_str())
                            .to_string_lossy(),
                        14,
                    );
                    let missing = if choice.available { "" } else { "  missing" };
                    Line::from(vec![
                        Span::styled(
                            if index == state.selected_workspace {
                                " > "
                            } else {
                                "   "
                            },
                            accent(theme),
                        ),
                        Span::styled(format!("{name:<14}"), text(theme)),
                        Span::styled(
                            end_truncate(&choice.path.display().to_string(), 32),
                            theme.muted(),
                        ),
                        Span::styled(missing, warning(theme)),
                    ])
                }),
        );
    }
    lines.extend([Line::from(""), hint_line(WORKSPACE_HINTS, theme)]);
    render_dialog(
        frame,
        theme,
        " Choose workspace ",
        ModalSize::Compact,
        lines,
    );
}

fn render_agent_choices(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(state) = app.new_agent() else {
        return;
    };
    let mut lines = vec![Line::from("")];
    lines.extend(AgentKind::ALL.iter().enumerate().map(|(index, kind)| {
        Line::from(vec![
            Span::styled(
                if index == state.selected_agent {
                    " > "
                } else {
                    "   "
                },
                accent(theme),
            ),
            Span::styled(kind.label(), text(theme)),
        ])
    }));
    lines.extend([Line::from(""), hint_line(AGENT_HINTS, theme)]);
    render_dialog(frame, theme, " Choose agent ", ModalSize::Compact, lines);
}

fn render_native_browser(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(browser) = app.native_browser() else {
        return;
    };
    let area = ModalSize::Large.area(frame.area());
    let visible = usize::from(area.height.saturating_sub(6));
    let content_width = usize::from(area.width.saturating_sub(8));
    let start = browser.selected.saturating_sub(visible - 1);
    let rows = std::iter::once((0, "Use this directory".into()))
        .chain(
            browser
                .entries
                .iter()
                .enumerate()
                .map(|(index, choice)| (index + 1, format!("{}/", choice.label))),
        )
        .skip(start)
        .take(visible)
        .map(|(index, label)| {
            Line::from(vec![
                Span::styled(
                    if index == browser.selected {
                        " > "
                    } else {
                        "   "
                    },
                    accent(theme),
                ),
                Span::styled(end_truncate(&label, content_width), text(theme)),
            ])
        });
    let mut lines = vec![
        Line::from(Span::styled(
            format!(
                "  {}",
                end_truncate(&browser.current_path.display().to_string(), content_width,)
            ),
            theme.muted(),
        )),
        Line::from(""),
    ];
    lines.extend(rows);
    if browser.loading {
        lines.push(Line::from(Span::styled("  Loading…", theme.muted())));
    } else if let Some(error) = &browser.error {
        lines.push(Line::from(Span::styled(
            format!("  {}", end_truncate(error, content_width)),
            warning(theme),
        )));
    } else {
        lines.push(Line::from(""));
    }
    lines.push(hint_line(BROWSER_HINTS, theme));
    render_dialog(frame, theme, " Select workspace ", ModalSize::Large, lines);
}

fn render_embedded_browser(
    frame: &mut Frame<'_>,
    snapshot: Option<&TerminalProcessSnapshot>,
    theme: Theme,
) {
    let area = embedded_modal_area(frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::bordered()
            .title(" Select workspace · Yazi ")
            .border_style(accent(theme))
            .style(theme.surface()),
        area,
    );
    let content = embedded_terminal_area(frame.area());
    if let Some(snapshot) = snapshot {
        let terminal = TerminalScreen::new(&snapshot.terminal);
        if let Some(position) = terminal.cursor_position(content) {
            frame.set_cursor_position(position);
        }
        frame.render_widget(terminal, content);
    }
    frame.render_widget(
        Paragraph::new(hint_line(EMBEDDED_HINTS, theme)).style(theme.muted()),
        Rect::new(
            area.x.saturating_add(1),
            area.bottom().saturating_sub(2),
            area.width.saturating_sub(2),
            1,
        ),
    );
}

fn embedded_modal_area(area: Rect) -> Rect {
    ModalSize::Large.area(area)
}

pub(crate) fn embedded_terminal_area(area: Rect) -> Rect {
    let modal = embedded_modal_area(area);
    Rect::new(
        modal.x.saturating_add(1),
        modal.y.saturating_add(1),
        modal.width.saturating_sub(2),
        modal.height.saturating_sub(4),
    )
}

fn render_confirmation(frame: &mut Frame<'_>, theme: Theme, title: &str, prompt: &str) {
    render_dialog(
        frame,
        theme,
        title,
        ModalSize::Standard,
        vec![
            Line::from(""),
            Line::from(Span::styled(format!("  {prompt}"), warning(theme))),
            Line::from(""),
            hint_line(CONFIRM_HINTS, theme),
        ],
    );
}

fn render_archive_unavailable(frame: &mut Frame<'_>, theme: Theme) {
    render_dialog(
        frame,
        theme,
        " Archive unavailable ",
        ModalSize::Compact,
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "  Only named conversations can be archived.",
                warning(theme),
            )),
            Line::from(""),
            hint_line(ARCHIVE_UNAVAILABLE_HINTS, theme),
        ],
    );
}

fn render_stop_confirmation(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let session = app
        .session_id()
        .map_or_else(|| "local".into(), |id| id.0.to_string());
    let running = app
        .agents()
        .iter()
        .filter(|agent| agent.status() == SessionStatus::Running)
        .count();
    render_dialog(
        frame,
        theme,
        " Stop Svarm session? ",
        ModalSize::Standard,
        vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  Session {session} · {running} running agents"),
                warning(theme),
            )),
            Line::from("  This terminates every agent in the session."),
            Line::from(""),
            hint_line(STOP_HINTS, theme),
        ],
    );
}

fn render_resume_confirmation(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let title = app
        .pending_resume_title()
        .unwrap_or("Archived conversation");
    render_dialog(
        frame,
        theme,
        " Reactivate conversation? ",
        ModalSize::Standard,
        vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}", end_truncate(title, 62)),
                warning(theme),
            )),
            Line::from(""),
            hint_line(RESUME_HINTS, theme),
        ],
    );
}

fn render_keybinds(frame: &mut Frame<'_>, theme: Theme) {
    let mut lines = vec![Line::from("")];
    lines.extend(
        MANAGEMENT_KEYBINDINGS
            .iter()
            .map(|binding| Line::from(format!("  {:<27} {}", binding.keys, binding.action))),
    );
    lines.extend([Line::from(""), hint_line(BACK_HINTS, theme)]);
    render_dialog(frame, theme, " Keybinds ", ModalSize::Standard, lines);
}

fn render_settings(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    render_dialog(
        frame,
        theme,
        " Settings ",
        ModalSize::Standard,
        vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  Theme", text(theme).add_modifier(Modifier::BOLD)),
                Span::styled("              ‹  ", theme.muted()),
                Span::styled(
                    app.theme().label(),
                    accent(theme).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ›", theme.muted()),
            ]),
            Line::from(""),
            hint_line(SETTINGS_HINTS, theme),
        ],
    );
}

fn hint_line(hints: &[ActionHint], theme: Theme) -> Line<'static> {
    let mut spans = vec![Span::styled("  ", theme.muted())];
    for (index, hint) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", theme.muted()));
        }
        spans.push(Span::styled(hint.text, theme.muted()));
    }
    Line::from(spans)
}

fn render_dialog(
    frame: &mut Frame<'_>,
    theme: Theme,
    title: &str,
    size: ModalSize,
    lines: Vec<Line<'static>>,
) {
    let area = size.area(frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::bordered()
                    .title(Span::styled(
                        title.to_owned(),
                        accent(theme).add_modifier(Modifier::BOLD),
                    ))
                    .border_style(border(theme)),
            )
            .wrap(Wrap { trim: false })
            .style(theme.surface()),
        area,
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn text(theme: Theme) -> Style {
    Style::default().fg(theme.text)
}

fn accent(theme: Theme) -> Style {
    Style::default().fg(theme.accent)
}

fn success(theme: Theme) -> Style {
    Style::default().fg(theme.ok)
}

fn warning(theme: Theme) -> Style {
    Style::default().fg(theme.warn)
}

fn border(theme: Theme) -> Style {
    Style::default().fg(theme.border)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::{Terminal, backend::TestBackend, layout::Position};
    use svarm_agent::protocol::{
        AgentActivity, AgentSnapshot, ArchivedConversation, AttachmentSummary, ConnectionId,
        GitContext, SessionId, SessionRevision, SessionSummary, SvarmSessionSnapshot,
        TerminalSequence,
    };
    use svarm_agent::terminal_model::{TerminalPosition, TerminalSize};

    use super::*;

    fn terminal_snapshot(rows: u16, cols: u16, text: &str) -> TerminalSnapshot {
        let mut snapshot = TerminalSnapshot::blank(TerminalSize::new(rows, cols));
        for (column, character) in text.chars().enumerate() {
            snapshot.cell_mut(0, column as u16).unwrap().contents = character.to_string().into();
        }
        snapshot.state.cursor.position = TerminalPosition {
            row: 0,
            column: text.chars().count() as u16,
        };
        snapshot
    }

    #[test]
    fn terminal_area_only_reserves_the_sidebar() {
        assert_eq!(
            terminal_area(Rect::new(0, 0, 120, 40), true),
            Rect::new(SIDEBAR_WIDTH, 0, 120 - SIDEBAR_WIDTH, 40)
        );
        assert_eq!(
            terminal_area(Rect::new(0, 0, 120, 40), false),
            Rect::new(0, 0, 120, 40)
        );
    }

    #[test]
    fn menu_hit_areas_stay_at_the_bottom_of_the_sidebar() {
        let area = Rect::new(0, 0, 120, 40);
        assert_eq!(
            new_agent_button_area(area, true),
            Some(Rect::new(0, 38, 27, 1))
        );
        assert_eq!(menu_button_area(area, true), Some(Rect::new(0, 39, 27, 1)));
        assert_eq!(menu_item_at(area, 2, 36), Some(MenuItem::Keybinds));
        assert_eq!(menu_item_at(area, 2, 37), Some(MenuItem::Settings));
        assert_eq!(menu_item_at(area, 50, 36), None);
    }

    #[test]
    fn sidebar_renders_new_agent_button_above_menu() {
        let app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Monochrome,
            false,
            None,
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app: &app,
                        screen: None,
                        scrolled: false,
                        embedded: None,
                        theme: app.theme().theme(false),
                        colors_enabled: false,
                    },
                );
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("+ New agent"));
        assert!(rendered.contains("≡ Menu"));
    }

    #[test]
    fn centered_rect_clamps_to_the_terminal() {
        assert_eq!(
            centered_rect(100, 100, Rect::new(2, 3, 20, 10)),
            Rect::new(2, 3, 20, 10)
        );
    }

    #[test]
    fn modal_tiers_have_canonical_areas() {
        let terminal = Rect::new(0, 0, 120, 40);
        assert_eq!(ModalSize::Compact.area(terminal), Rect::new(28, 14, 64, 12));
        assert_eq!(
            ModalSize::Standard.area(terminal),
            Rect::new(24, 11, 72, 18)
        );
        assert_eq!(ModalSize::Large.area(terminal), Rect::new(10, 5, 100, 30));
        assert_eq!(
            embedded_modal_area(terminal),
            ModalSize::Large.area(terminal)
        );
    }

    #[test]
    fn prepared_ui_model_renders_at_supported_sizes() {
        let app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            true,
            None,
        );

        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render(
                        frame,
                        UiModel {
                            app: &app,
                            screen: None,
                            scrolled: false,
                            embedded: None,
                            theme: app.theme().theme(true),
                            colors_enabled: true,
                        },
                    );
                })
                .unwrap();
        }
    }

    #[test]
    fn native_browser_renders_loading_listing_and_error_states_at_supported_sizes() {
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            false,
            None,
        );
        app.open_new_agent(None, None, Vec::new());
        app.open_native_browser(PathBuf::from("/tmp/a very long workspace path"), 1);
        app.apply_directory_load(
            1,
            PathBuf::from("/tmp/a very long workspace path"),
            Ok(vec![crate::app::DirectoryChoice {
                path: PathBuf::from("/tmp/a very long workspace path/child"),
                label: "child".into(),
            }]),
        );

        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render(
                        frame,
                        UiModel {
                            app: &app,
                            screen: None,
                            scrolled: false,
                            embedded: None,
                            theme: app.theme().theme(true),
                            colors_enabled: true,
                        },
                    );
                })
                .unwrap();
        }
        assert!(render_app_text(&app).contains("Use this directory"));
    }

    #[test]
    fn session_chooser_preserves_id_state_and_footer_at_80x24() {
        let chooser = SessionChooser::new(
            vec![SessionSummary {
                id: SessionId(42),
                running_agents: 1,
                total_agents: 2,
                attachment: Some(AttachmentSummary {
                    connection_id: ConnectionId(1),
                    process_id: Some(7),
                    attached_at_ms: 1,
                    last_activity_ms: 1,
                }),
                last_user_activity_ms: 1,
                revision: SessionRevision(1),
            }],
            true,
        );
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_session_chooser(
                    frame,
                    &chooser,
                    2_001,
                    crate::theme::ThemeName::Monochrome.theme(false),
                )
            })
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Open Svarm session"));
        assert!(rendered.contains("42"));
        assert!(rendered.contains("attached"));
        assert!(rendered.contains("[Enter] open  [j] next  [k] previous  [Esc] cancel  [n] new"));
    }

    fn assert_hint_clicks(app: &App, area: Rect, x: u16, y: u16, hints: &[ActionHint]) {
        let mut column = x + 2;
        for hint in hints {
            assert_eq!(
                click_action(app, area, column, y),
                Some(hint.action),
                "{} should be clickable",
                hint.text
            );
            column += hint.text.chars().count() as u16 + 2;
        }
    }

    #[test]
    fn every_svarm_action_surface_has_a_click_target() {
        let area = Rect::new(0, 0, 80, 24);
        let compact = dialog_inner(ModalSize::Compact, area);
        let standard = dialog_inner(ModalSize::Standard, area);
        let large = dialog_inner(ModalSize::Large, area);
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            false,
            None,
        );

        let new_button = new_agent_button_area(area, true).unwrap();
        assert_eq!(
            click_action(&app, area, new_button.x, new_button.y),
            Some(ClickAction::Management(ManagementCommand::ChooseAgent))
        );
        let menu_button = menu_button_area(area, true).unwrap();
        assert_eq!(
            click_action(&app, area, menu_button.x, menu_button.y),
            Some(ClickAction::ToggleMenu)
        );
        app.toggle_sidebar();
        let collapsed_menu = menu_button_area(area, false).unwrap();
        assert_eq!(
            click_action(&app, area, collapsed_menu.x, collapsed_menu.y),
            Some(ClickAction::Management(ManagementCommand::OpenMenu))
        );
        app.toggle_sidebar();
        assert_eq!(
            click_action(&app, area, SIDEBAR_WIDTH + 1, 0),
            Some(ClickAction::Management(ManagementCommand::ChooseAgent))
        );

        app.set_mode(Mode::Menu);
        let popup = menu_popup_area(menu_button);
        for (index, item) in MenuItem::ALL.into_iter().enumerate() {
            assert_eq!(
                click_action(&app, area, popup.x + 1, popup.y + 1 + index as u16),
                Some(ClickAction::MenuItem(item))
            );
        }

        app.open_new_agent(None, None, Vec::new());
        app.open_workspace_choices();
        assert_eq!(
            click_action(&app, area, compact.x, compact.y + 1),
            Some(ClickAction::BrowseWorkspaces)
        );

        app.open_new_agent(
            None,
            None,
            vec![crate::app::WorkspaceChoice {
                path: PathBuf::from("/tmp/workspace"),
                available: true,
            }],
        );
        for (line, field) in [
            (1, NewAgentField::Workspace),
            (2, NewAgentField::Agent),
            (3, NewAgentField::Start),
        ] {
            assert_eq!(
                click_action(&app, area, compact.x, compact.y + line),
                Some(ClickAction::NewAgentField(field))
            );
        }
        assert_hint_clicks(&app, area, compact.x, compact.y + 5, FORM_HINTS);

        app.open_workspace_choices();
        assert_eq!(
            click_action(&app, area, compact.x, compact.y + 1),
            Some(ClickAction::Workspace(0))
        );
        assert_hint_clicks(&app, area, compact.x, compact.y + 3, WORKSPACE_HINTS);

        app.open_agent_choices();
        for index in 0..AgentKind::ALL.len() {
            assert_eq!(
                click_action(&app, area, compact.x, compact.y + 1 + index as u16),
                Some(ClickAction::AgentKind(index))
            );
        }
        assert_hint_clicks(
            &app,
            area,
            compact.x,
            compact.y + AgentKind::ALL.len() as u16 + 2,
            AGENT_HINTS,
        );

        app.open_native_browser(PathBuf::from("/tmp"), 1);
        app.apply_directory_load(
            1,
            PathBuf::from("/tmp"),
            Ok(vec![crate::app::DirectoryChoice {
                path: PathBuf::from("/tmp/child"),
                label: "child".into(),
            }]),
        );
        assert_eq!(
            click_action(&app, area, large.x, large.y + 2),
            Some(ClickAction::NativeBrowserItem(0))
        );
        assert_eq!(
            click_action(&app, area, large.x, large.y + 3),
            Some(ClickAction::NativeBrowserItem(1))
        );
        assert_hint_clicks(&app, area, large.x, large.y + 5, BROWSER_HINTS);

        app.open_embedded_browser();
        let embedded = embedded_modal_area(area);
        assert_hint_clicks(
            &app,
            area,
            embedded.x + 1,
            embedded.bottom() - 2,
            EMBEDDED_HINTS,
        );

        for mode in [Mode::ConfirmClose, Mode::ConfirmArchive] {
            app.set_mode(mode);
            assert_hint_clicks(&app, area, standard.x, standard.y + 3, CONFIRM_HINTS);
        }
        app.set_mode(Mode::ArchiveUnavailable);
        assert_hint_clicks(
            &app,
            area,
            compact.x,
            compact.y + 3,
            ARCHIVE_UNAVAILABLE_HINTS,
        );
        assert!(render_app_text(&app).contains("Only named conversations can be archived."));
        app.set_mode(Mode::ConfirmResume);
        assert_hint_clicks(&app, area, standard.x, standard.y + 3, RESUME_HINTS);
        app.set_mode(Mode::ConfirmQuit);
        assert_hint_clicks(&app, area, standard.x, standard.y + 4, STOP_HINTS);

        app.set_mode(Mode::Keybinds);
        for (index, binding) in MANAGEMENT_KEYBINDINGS.iter().enumerate() {
            assert_eq!(
                click_action(&app, area, standard.x, standard.y + 1 + index as u16),
                Some(ClickAction::Management(binding.command))
            );
        }
        assert_hint_clicks(
            &app,
            area,
            standard.x,
            standard.y + MANAGEMENT_KEYBINDINGS.len() as u16 + 2,
            BACK_HINTS,
        );

        app.set_mode(Mode::Settings);
        assert_hint_clicks(&app, area, standard.x, standard.y + 3, SETTINGS_HINTS);
        assert_eq!(
            click_action(&app, area, standard.x + 21, standard.y + 1),
            Some(ClickAction::ThemePrevious)
        );
        let next_theme = standard.x + 24 + app.theme().label().chars().count() as u16;
        assert_eq!(
            click_action(&app, area, next_theme, standard.y + 1),
            Some(ClickAction::ThemeNext)
        );

        app.set_mode(Mode::Terminal);
        app.toggle_sidebar();
        assert!(render_app_text(&app).contains("≡ Menu  ^B m"));
    }

    #[test]
    fn every_session_chooser_hint_and_row_has_a_click_target() {
        let chooser = SessionChooser::new(
            vec![SessionSummary {
                id: SessionId(42),
                running_agents: 0,
                total_agents: 0,
                attachment: None,
                last_user_activity_ms: 1,
                revision: SessionRevision(1),
            }],
            true,
        );
        let area = Rect::new(0, 0, 80, 24);
        let inner = Block::bordered().inner(area);
        assert_eq!(
            session_chooser_click(&chooser, area, inner.x, inner.y),
            Some(SessionChooserClick::Choose(0))
        );
        assert_eq!(
            session_chooser_click(&chooser, area, inner.x, inner.y + 1),
            Some(SessionChooserClick::Choose(1))
        );

        let mut column = inner.x + 1;
        for hint in SESSION_HINTS {
            assert_eq!(
                session_chooser_click(&chooser, area, column, inner.bottom().saturating_sub(1),),
                Some(hint.action),
                "{} should be clickable",
                hint.text
            );
            column += hint.text.chars().count() as u16 + 2;
        }
    }

    #[test]
    fn keybinds_and_menu_use_canonical_detach_and_stop_copy() {
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            false,
            None,
        );
        app.set_mode(Mode::Keybinds);
        let keybinds = render_app_text(&app);
        assert!(keybinds.contains("detach — agents keep running"));
        assert!(keybinds.contains("stop session — terminates all agents"));

        app.set_mode(Mode::Menu);
        let menu = render_app_text(&app);
        assert!(menu.contains("Detach — agents keep running"));
        assert!(menu.contains("Stop session — terminates all agents"));
        assert!(menu.contains("[1] Detach"));
        assert!(menu.contains("[4] Settings"));
    }

    #[test]
    fn stop_confirmation_names_target_and_running_agents_at_80x24() {
        let summary = SessionSummary {
            id: SessionId(7),
            running_agents: 1,
            total_agents: 1,
            attachment: None,
            last_user_activity_ms: 1,
            revision: SessionRevision(1),
        };
        let agent = AgentSnapshot {
            id: svarm_agent::AgentId::new(1),
            kind: svarm_agent::AgentKind::Codex,
            launch_directory: PathBuf::from("/tmp/project-seven"),
            status: SessionStatus::Running,
            exit: None,
            output_generation: 0,
            seen_generation: 0,
            completed_generation: 0,
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            conversation_title: None,
            conversation_id: None,
            activity: AgentActivity::Unknown,
            recognition: None,
            git: None,
        };
        let mut app = App::hydrate(
            SvarmSessionSnapshot {
                summary,
                selected_agent_id: Some(agent.id),
                rows: 24,
                cols: 80,
                agents: vec![agent],
                archived: Vec::new(),
            },
            crate::theme::ThemeName::Monochrome,
            None,
        );
        app.set_mode(Mode::ConfirmQuit);
        let rendered = render_app_text(&app);
        assert!(rendered.contains("Session 7 · 1 running agents"));
        assert!(!rendered.contains("/tmp/project-seven"));
        assert!(rendered.contains("terminates every agent"));
    }

    #[test]
    fn agent_cards_show_titles_status_markers_and_optional_git_context() {
        let summary = SessionSummary {
            id: SessionId(8),
            running_agents: 1,
            total_agents: 2,
            attachment: None,
            last_user_activity_ms: 1,
            revision: SessionRevision(1),
        };
        let agent = |id, status, output_generation, seen_generation| AgentSnapshot {
            id: svarm_agent::AgentId::new(id),
            kind: svarm_agent::AgentKind::Codex,
            launch_directory: PathBuf::from(if id == 1 {
                "/tmp/plain-directory"
            } else {
                "/tmp/project-eight"
            }),
            status,
            exit: (status == SessionStatus::Exited).then_some(svarm_agent::ProcessExit {
                code: 1,
                signal: None,
                success: false,
            }),
            output_generation,
            seen_generation,
            completed_generation: if id == 2 { output_generation } else { 0 },
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            conversation_title: (id != 1).then(|| format!("Conversation {id}")),
            conversation_id: None,
            activity: AgentActivity::Idle,
            recognition: None,
            git: (id == 2).then_some(GitContext {
                branch: "feature/sidebar".into(),
                worktree: "/tmp/project-eight".into(),
                additions: 557,
                deletions: 300,
                ahead: Some(2),
                behind: Some(4),
            }),
        };
        let exited = agent(1, SessionStatus::Exited, 1, 1);
        let unseen = agent(2, SessionStatus::Running, 2, 1);
        let app = App::hydrate(
            SvarmSessionSnapshot {
                summary: summary.clone(),
                selected_agent_id: Some(unseen.id),
                rows: 24,
                cols: 80,
                agents: vec![exited, unseen],
                archived: Vec::new(),
            },
            crate::theme::ThemeName::Monochrome,
            None,
        );
        let rendered = render_app_text(&app);
        assert!(rendered.contains("● 1 · Unnamed conversation"));
        assert!(rendered.contains("● 2 · Conversation 2"));
        assert!(!rendered.contains("failed"));
        assert!(!rendered.contains("done"));
        assert!(!rendered.contains("/tmp/project-eight"));
        assert!(rendered.contains("Codex · plain-directory"));
        assert!(rendered.contains("Codex · project-eight"));
        assert!(rendered.contains("featur… +557 -300 ↑2 ↓4"), "{rendered}");
        assert_eq!(
            rendered.matches('▌').count(),
            usize::from(AGENT_CARD_HEIGHT)
        );

        let plain = agent(1, SessionStatus::Exited, 1, 1);
        let plain_app = App::hydrate(
            SvarmSessionSnapshot {
                summary: summary.clone(),
                selected_agent_id: Some(plain.id),
                rows: 24,
                cols: 80,
                agents: vec![plain],
                archived: Vec::new(),
            },
            crate::theme::ThemeName::Monochrome,
            None,
        );
        assert_eq!(
            render_app_text(&plain_app).matches('▌').count(),
            usize::from(AGENT_CARD_HEIGHT)
        );

        let mut clean = agent(2, SessionStatus::Running, 2, 1);
        let git = clean.git.as_mut().unwrap();
        git.additions = 0;
        git.deletions = 0;
        git.ahead = Some(0);
        git.behind = Some(0);
        let clean_app = App::hydrate(
            SvarmSessionSnapshot {
                summary,
                selected_agent_id: Some(clean.id),
                rows: 24,
                cols: 80,
                agents: vec![clean],
                archived: Vec::new(),
            },
            crate::theme::ThemeName::Monochrome,
            None,
        );
        let clean_rendered = render_app_text(&clean_app);
        assert!(clean_rendered.contains("feature/sidebar"));
        assert!(!clean_rendered.contains("+0 -0"));
        assert!(!clean_rendered.contains("↑0 ↓0"));

        let theme = crate::theme::ThemeName::CatppuccinMocha.theme(true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app: &app,
                        screen: None,
                        scrolled: false,
                        embedded: None,
                        theme,
                        colors_enabled: true,
                    },
                )
            })
            .unwrap();
        let circle_colors = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.symbol() == "●")
            .map(|cell| cell.fg)
            .collect::<Vec<_>>();
        assert_eq!(circle_colors, vec![Color::Red, Color::Green]);
        for row in 0..24 {
            for column in 0..SIDEBAR_WIDTH {
                assert_eq!(
                    terminal.backend().buffer()[(column, row)].bg,
                    Color::Reset,
                    "sidebar background changed at ({column}, {row})"
                );
            }
        }
    }

    #[test]
    fn archived_conversations_render_as_title_only_below_active_cards() {
        let active = AgentSnapshot {
            id: svarm_agent::AgentId::new(1),
            kind: svarm_agent::AgentKind::Codex,
            launch_directory: "/tmp/active".into(),
            status: SessionStatus::Running,
            exit: None,
            output_generation: 0,
            seen_generation: 0,
            completed_generation: 0,
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            conversation_title: Some("Active title".into()),
            conversation_id: Some("active-id".into()),
            activity: AgentActivity::Idle,
            recognition: None,
            git: None,
        };
        let summary = SessionSummary {
            id: SessionId(10),
            running_agents: 1,
            total_agents: 1,
            attachment: None,
            last_user_activity_ms: 1,
            revision: SessionRevision(1),
        };
        let app = App::hydrate(
            SvarmSessionSnapshot {
                summary,
                selected_agent_id: Some(active.id),
                rows: 24,
                cols: 80,
                agents: vec![active.clone()],
                archived: vec![ArchivedConversation {
                    conversation_id: "archived-id".into(),
                    title: "Archived title".into(),
                    kind: svarm_agent::AgentKind::Claude,
                    launch_directory: "/tmp/hidden-archive-directory".into(),
                }],
            },
            crate::theme::ThemeName::Monochrome,
            None,
        );

        let rendered = render_app_text(&app);
        assert!(rendered.contains("Archived"));
        assert!(rendered.contains("2 · Archived title"));
        assert!(!rendered.contains("Claude Code"));
        assert!(!rendered.contains("hidden-archive-directory"));
        assert_eq!(agent_item_at(&app, Rect::new(0, 0, 80, 24), 2, 4), None);
        assert_eq!(agent_item_at(&app, Rect::new(0, 0, 80, 24), 2, 5), Some(1));
    }

    #[test]
    fn status_markers_share_one_glyph_and_keep_fixed_colors_across_themes() {
        assert_eq!(
            status_display(AgentDisplayStatus::Unknown, true),
            status_display(AgentDisplayStatus::Idle, true)
        );
        for status in [
            AgentDisplayStatus::Unknown,
            AgentDisplayStatus::Idle,
            AgentDisplayStatus::Working,
            AgentDisplayStatus::Done,
            AgentDisplayStatus::NeedsYou,
            AgentDisplayStatus::Failed,
        ] {
            assert_eq!(status_display(status, true).0, "●");
        }
        assert_eq!(
            status_display(AgentDisplayStatus::Working, true).1.fg,
            Some(Color::Yellow)
        );
        assert_eq!(
            status_display(AgentDisplayStatus::Done, true).1.fg,
            Some(Color::Green)
        );
        assert_eq!(
            status_display(AgentDisplayStatus::NeedsYou, true).1.fg,
            Some(Color::Red)
        );
        assert_eq!(
            status_display(AgentDisplayStatus::Working, false).1.fg,
            Some(Color::Reset)
        );
    }

    #[test]
    fn multiline_agent_list_scrolls_to_keep_the_selected_card_visible_at_80x24() {
        let agents = (1..=8)
            .map(|id| AgentSnapshot {
                id: svarm_agent::AgentId::new(id),
                kind: svarm_agent::AgentKind::Claude,
                launch_directory: PathBuf::from(format!("/tmp/project-{id}")),
                status: SessionStatus::Running,
                exit: None,
                output_generation: 1,
                seen_generation: 1,
                completed_generation: 0,
                terminal_sequence: TerminalSequence(0),
                read_error: None,
                conversation_title: Some(format!("Conversation {id}")),
                conversation_id: None,
                activity: AgentActivity::Idle,
                recognition: None,
                git: None,
            })
            .collect::<Vec<_>>();
        let mut app = App::hydrate(
            SvarmSessionSnapshot {
                summary: SessionSummary {
                    id: SessionId(9),
                    running_agents: agents.len(),
                    total_agents: agents.len(),
                    attachment: None,
                    last_user_activity_ms: 1,
                    revision: SessionRevision(1),
                },
                selected_agent_id: Some(svarm_agent::AgentId::new(8)),
                rows: 24,
                cols: 80,
                agents,
                archived: Vec::new(),
            },
            crate::theme::ThemeName::Monochrome,
            None,
        );

        let rendered = render_app_text(&app);
        assert!(rendered.contains("8 · Conversation 8"));
        assert!(rendered.contains("Claude Code"));
        assert!(!rendered.contains("8 · Claude Code"));
        assert!(!rendered.contains("idle"));

        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(agent_item_at(&app, area, 2, 1), Some(1));
        assert_eq!(agent_item_at(&app, area, 2, 19), Some(7));
        assert_eq!(agent_item_at(&app, area, 2, 22), None);
        assert_eq!(agent_item_at(&app, area, 40, 19), None);

        app.scroll_sidebar(-1, agent_list_page_size(&app, area));
        assert_eq!(agent_item_at(&app, area, 2, 1), Some(0));
        let rendered = render_app_text(&app);
        assert!(rendered.contains("1 · Conversation 1"));
        assert!(!rendered.contains("8 · Conversation 8"));
    }

    #[test]
    fn the_agent_cursor_is_the_host_terminal_cursor_not_a_painted_cell() {
        let screen = terminal_snapshot(24, 55, "prompt> ");
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            true,
            None,
        );
        app.set_mode(Mode::Terminal);

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app: &app,
                        screen: Some(&screen),
                        scrolled: false,
                        embedded: None,
                        theme: app.theme().theme(true),
                        colors_enabled: true,
                    },
                )
            })
            .unwrap();

        // Column 8 of the pane, which starts after the sidebar.
        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            Position::new(SIDEBAR_WIDTH + 8, 0)
        );
        let cursor_cell = &terminal.backend().buffer()[(SIDEBAR_WIDTH + 8, 0)];
        assert!(
            !cursor_cell.modifier.contains(Modifier::REVERSED) && cursor_cell.symbol() != "█",
            "the pane must not paint a cursor over the terminal's own"
        );
    }

    #[test]
    fn scrollback_does_not_overlay_a_position_label() {
        let screen = terminal_snapshot(24, 80, "terminal output");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        terminal
            .draw(|frame| {
                render_terminal(
                    frame,
                    Some(&screen),
                    false,
                    true,
                    Mode::Terminal,
                    frame.area(),
                    crate::theme::ThemeName::Dark.theme(true),
                );
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("terminal output"));
        assert!(!rendered.contains("scroll 12/40"));
    }

    #[test]
    fn a_hidden_agent_cursor_leaves_the_host_cursor_hidden() {
        let mut screen = terminal_snapshot(24, 55, "working");
        screen.state.cursor.visible = false;
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            true,
            None,
        );
        app.set_mode(Mode::Terminal);

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app: &app,
                        screen: Some(&screen),
                        scrolled: false,
                        embedded: None,
                        theme: app.theme().theme(true),
                        colors_enabled: true,
                    },
                )
            })
            .unwrap();

        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn embedded_terminal_uses_prepared_screen_area_and_cursor_at_80x24() {
        let mut terminal = terminal_snapshot(18, 74, "yazi> ");
        terminal.state.cursor.style = svarm_agent::CursorStyle::SteadyBar;
        let snapshot = TerminalProcessSnapshot {
            terminal,
            status: SessionStatus::Running,
            exit: None,
            read_error: None,
            generation: 1,
            modes: svarm_agent::protocol::TerminalModes::default(),
            output_closed: false,
        };
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            true,
            None,
        );
        app.open_embedded_browser();
        let area = Rect::new(0, 0, 80, 24);
        let content = embedded_terminal_area(area);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app: &app,
                        screen: None,
                        scrolled: false,
                        embedded: Some(&snapshot),
                        theme: app.theme().theme(true),
                        colors_enabled: true,
                    },
                )
            })
            .unwrap();

        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            Position::new(content.x + 6, content.y)
        );
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Select workspace · Yazi"));
        assert!(rendered.contains("force close"));
    }

    fn render_app_text(app: &App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app,
                        screen: None,
                        scrolled: false,
                        embedded: None,
                        theme: app.theme().theme(false),
                        colors_enabled: false,
                    },
                )
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }
}
