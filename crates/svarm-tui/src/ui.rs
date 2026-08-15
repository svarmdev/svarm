use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use svarm_agent::terminal_model::TerminalSnapshot;

use crate::{
    app::{
        AgentDisplayStatus, AgentState, App, Checkout, MenuItem, Mode, NewAgentField, NewAgentPage,
        SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH, SessionChooser, SettingsTab,
    },
    input::{MANAGEMENT_KEYBINDINGS, ManagementCommand},
    screen::TerminalScreen,
    selection::VisibleSelection,
    theme::Theme,
};
use svarm_agent::{
    AgentKind, SessionStatus, TerminalProcessSnapshot,
    protocol::{UsageProviderReport, UsageReport, UsageWindow},
};

pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;
const COMPACT_MODAL_WIDTH: u16 = 64;
const COMPACT_MODAL_HEIGHT: u16 = 12;
const STANDARD_MODAL_WIDTH: u16 = 72;
const STANDARD_MODAL_HEIGHT: u16 = 18;
pub const SIDEBAR_WIDTH: u16 = SIDEBAR_DEFAULT_WIDTH;
const AGENT_CARD_HEIGHT: u16 = 3;
const COLLAPSED_CARD_HEIGHT: u16 = 1;
/// Marks a directory that is a linked git worktree rather than the repository's main checkout.
const LINKED_WORKTREE: &str = "⑂";
/// Nerd Font git-branch glyph (Powerline branch symbol), used in place of `LINKED_WORKTREE`
/// when `nerd_fonts` is enabled. Falls back to the plain glyph otherwise.
const LINKED_WORKTREE_NERD_FONT: &str = "\u{e0a0}";
/// Hit-box width for the per-card archive button on a card's title line.
const ARCHIVE_BUTTON_WIDTH: u16 = 3;
/// Archive-button glyph and its Nerd Font counterpart (nf-fa-archive), selected by
/// `nerd_fonts`; the plain glyph is the default, no-dependency fallback.
const ARCHIVE_BUTTON_TEXT: &str = "⨯";
const ARCHIVE_BUTTON_TEXT_NERD_FONT: &str = "\u{f187}";
/// The codepoint svarm asks fontconfig about to decide whether Nerd Font glyphs will
/// render. It is the rarest glyph svarm draws, so a font covering it covers the rest.
pub(crate) const NERD_FONT_PROBE_CODEPOINT: &str = "f187";

fn worktree_icon(nerd_fonts: bool) -> &'static str {
    if nerd_fonts {
        LINKED_WORKTREE_NERD_FONT
    } else {
        LINKED_WORKTREE
    }
}

fn archive_icon(nerd_fonts: bool) -> &'static str {
    if nerd_fonts {
        ARCHIVE_BUTTON_TEXT_NERD_FONT
    } else {
        ARCHIVE_BUTTON_TEXT
    }
}
const MENU_HEIGHT: u16 = MenuItem::ALL.len() as u16 + 2;

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
    pub selection: Option<VisibleSelection>,
    pub toast: Option<&'a str>,
    pub embedded: Option<&'a TerminalProcessSnapshot>,
    pub theme: Theme,
    pub colors_enabled: bool,
    pub nerd_fonts: bool,
    pub pointer: Option<(u16, u16)>,
    /// Read by the runtime, not by rendering, so countdowns stay a pure function of the frame.
    pub now_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClickAction {
    Management(ManagementCommand),
    ToggleMenu,
    ResizeSidebar,
    SidebarItem(usize),
    ArchiveCard(usize),
    MenuItem(MenuItem),
    Next,
    Previous,
    Confirm,
    Cancel,
    NewAgentField(NewAgentField),
    Workspace(usize),
    Location(usize),
    BrowseWorkspaces,
    AgentKind(usize),
    NativeBrowserItem(usize),
    NativeBrowserParent,
    ThemePrevious,
    ThemeNext,
    SettingsPrevious,
    SettingsNext,
    SettingsTab(SettingsTab),
    UsageNext,
    UsageRefresh,
    UsageTab(AgentKind),
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
const LOCATION_HINTS: &[ActionHint] = &[
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
const CREATING_HINTS: &[ActionHint] = &[ActionHint {
    text: "[Esc] cancel",
    action: ClickAction::Cancel,
}];
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
        text: "[Tab] next tab",
        action: ClickAction::SettingsNext,
    },
    ActionHint {
        text: "[S-Tab] previous tab",
        action: ClickAction::SettingsPrevious,
    },
    ActionHint {
        text: "[Esc] back",
        action: ClickAction::Cancel,
    },
];
const HARNESS_SETTINGS_HINTS: &[ActionHint] = &[
    ActionHint {
        text: "[Tab] next tab",
        action: ClickAction::SettingsNext,
    },
    ActionHint {
        text: "[S-Tab] previous tab",
        action: ClickAction::SettingsPrevious,
    },
    ActionHint {
        text: "[Esc] back",
        action: ClickAction::Cancel,
    },
];
const USAGE_HINTS: &[ActionHint] = &[
    ActionHint {
        text: "[Tab] next tab",
        action: ClickAction::UsageNext,
    },
    ActionHint {
        text: "[r] refresh",
        action: ClickAction::UsageRefresh,
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

    let hovered = model
        .pointer
        .and_then(|(column, row)| hover_action(app, area, column, row));

    if app.sidebar_visible() {
        render_sidebar(
            frame,
            app,
            area,
            theme,
            model.colors_enabled,
            model.nerd_fonts,
            hovered,
        );
    }
    render_terminal(
        frame,
        model,
        terminal_area(area, layout_sidebar_width(app, area)),
    );
    if !app.sidebar_visible()
        && let Some(button) = menu_button_area(area, 0)
    {
        let style = chrome_style(
            theme,
            hovered == Some(ClickAction::Management(ManagementCommand::OpenMenu)),
            false,
            theme.surface(),
        );
        frame.render_widget(Paragraph::new(" ≡ Menu  ^B m").style(style), button);
    }

    match app.mode() {
        Mode::NewAgent(NewAgentPage::Form) => render_new_agent_form(frame, app, theme, hovered),
        Mode::NewAgent(NewAgentPage::Workspaces) => {
            render_workspace_choices(frame, app, theme, hovered)
        }
        Mode::NewAgent(NewAgentPage::Locations) => {
            render_location_choices(frame, app, theme, hovered)
        }
        Mode::NewAgent(NewAgentPage::CreatingWorktree) => {
            render_creating_worktree(frame, app, theme, hovered)
        }
        Mode::NewAgent(NewAgentPage::Agents) => render_agent_choices(frame, app, theme, hovered),
        Mode::NewAgent(NewAgentPage::NativeBrowser) => {
            render_native_browser(frame, app, theme, hovered)
        }
        Mode::NewAgent(NewAgentPage::EmbeddedBrowser) | Mode::ToolPrefix => {
            render_embedded_browser(frame, model.embedded, theme, hovered)
        }
        Mode::ConfirmClose => {
            render_confirmation(frame, theme, "Close agent?", "Close this agent?", hovered)
        }
        Mode::ConfirmArchive => render_confirmation(
            frame,
            theme,
            " Archive conversation? ",
            "Stop this active agent and archive its conversation?",
            hovered,
        ),
        Mode::ArchiveUnavailable => render_archive_unavailable(frame, theme, hovered),
        Mode::ConfirmResume => render_resume_confirmation(frame, app, theme, hovered),
        Mode::ConfirmQuit => render_stop_confirmation(frame, app, theme, hovered),
        Mode::Keybinds => render_keybinds(frame, theme, hovered),
        Mode::Settings => render_settings(frame, app, theme, hovered),
        Mode::Usage => render_usage(frame, app, theme, hovered, model.now_ms),
        _ => {}
    }
    if let Some(toast) = model.toast {
        render_toast(frame, toast, theme);
    }
}

fn render_toast(frame: &mut Frame<'_>, message: &str, theme: Theme) {
    let area = frame.area();
    let content = format!(" {message} ");
    let width = u16::try_from(content.chars().count())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(area.width);
    let toast = Rect::new(
        area.right().saturating_sub(width),
        area.y,
        width,
        3.min(area.height),
    );
    frame.render_widget(Clear, toast);
    frame.render_widget(
        Paragraph::new(content)
            .block(Block::bordered())
            .style(theme.selected()),
        toast,
    );
}

pub(crate) fn render_session_chooser(
    frame: &mut Frame<'_>,
    chooser: &SessionChooser,
    now_ms: u64,
    theme: Theme,
    pointer: Option<(u16, u16)>,
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
    let hovered =
        pointer.and_then(|(column, row)| session_chooser_click(chooser, area, column, row));
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
        ListItem::new(Line::from(spans)).style(chrome_style(
            theme,
            hovered == Some(SessionChooserClick::Choose(index)),
            selected,
            text(theme),
        ))
    });
    frame.render_widget(List::new(rows), list_area);

    let mut spans = Vec::new();
    for hint in SESSION_HINTS
        .iter()
        .filter(|hint| !hint.needs_new || chooser.allow_new())
    {
        if !spans.is_empty() {
            spans.push(Span::styled("  ", theme.muted()));
        }
        let style = if hovered == Some(hint.action) {
            theme.selected()
        } else {
            theme.muted()
        };
        spans.push(Span::styled(hint.text, style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
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

pub fn clamp_sidebar_width(width: u16, terminal_width: u16) -> u16 {
    let max = SIDEBAR_MAX_WIDTH
        .min(terminal_width.saturating_sub(1) / 2)
        .max(SIDEBAR_MIN_WIDTH);
    width.clamp(SIDEBAR_MIN_WIDTH, max)
}

pub fn layout_sidebar_width(app: &App, area: Rect) -> u16 {
    if app.sidebar_visible() {
        clamp_sidebar_width(app.sidebar_width(), area.width)
    } else {
        0
    }
}

pub fn sidebar_collapsed(width: u16) -> bool {
    width > 0 && width <= SIDEBAR_MIN_WIDTH
}

pub fn sidebar_card_height(width: u16) -> u16 {
    if sidebar_collapsed(width) {
        COLLAPSED_CARD_HEIGHT
    } else {
        AGENT_CARD_HEIGHT
    }
}

pub fn terminal_area(area: Rect, sidebar_width: u16) -> Rect {
    Rect::new(
        area.x.saturating_add(sidebar_width),
        area.y,
        area.width.saturating_sub(sidebar_width),
        area.height,
    )
}

pub fn sidebar_area(area: Rect, sidebar_width: u16) -> Rect {
    Rect::new(area.x, area.y, sidebar_width, area.height)
}

pub fn resize_handle_area(app: &App, area: Rect) -> Option<Rect> {
    let width = layout_sidebar_width(app, area);
    if width == 0 {
        return None;
    }
    let sidebar = sidebar_area(area, width);
    Some(Rect::new(
        sidebar.right().saturating_sub(1),
        sidebar.y,
        1,
        sidebar.height,
    ))
}

pub fn menu_button_area(area: Rect, sidebar_width: u16) -> Option<Rect> {
    if area.height == 0 {
        return None;
    }
    if sidebar_width == 0 {
        return Some(Rect::new(
            area.x,
            area.bottom().saturating_sub(1),
            14.min(area.width),
            1,
        ));
    }
    let sidebar = sidebar_area(area, sidebar_width);
    Some(Rect::new(
        sidebar.x,
        sidebar.bottom().saturating_sub(1),
        sidebar.width.saturating_sub(1),
        1,
    ))
}

pub fn new_agent_button_area(area: Rect, sidebar_width: u16) -> Option<Rect> {
    if sidebar_width == 0 {
        return None;
    }
    let menu = menu_button_area(area, sidebar_width)?;
    (menu.y > area.y).then_some(Rect::new(menu.x, menu.y - 1, menu.width, 1))
}

pub fn usage_button_area(area: Rect, sidebar_width: u16) -> Option<Rect> {
    let new_agent = new_agent_button_area(area, sidebar_width)?;
    (new_agent.y > area.y).then_some(Rect::new(new_agent.x, new_agent.y - 1, new_agent.width, 1))
}

pub fn menu_item_at(app: &App, area: Rect, column: u16, row: u16) -> Option<MenuItem> {
    let button = menu_button_area(area, layout_sidebar_width(app, area))?;
    let popup = menu_popup_area(button);
    if column <= popup.x || column >= popup.right().saturating_sub(1) {
        return None;
    }
    let index = usize::from(row.checked_sub(popup.y.saturating_add(1))?);
    MenuItem::ALL.get(index).copied()
}

pub fn agent_item_at(app: &App, area: Rect, column: u16, row: u16) -> Option<usize> {
    let width = layout_sidebar_width(app, area);
    if width == 0 {
        return None;
    }
    let agents = agent_list_area(app, sidebar_area(area, width), width);
    if column < agents.x || column >= agents.right() || row < agents.y || row >= agents.bottom() {
        return None;
    }
    let card_height = usize::from(sidebar_card_height(width));
    let content_row = sidebar_row_start(app, agents, card_height) + usize::from(row - agents.y);
    let active_height = app.agents().len() * card_height;
    if content_row < active_height {
        return Some(content_row / card_height);
    }
    let header = usize::from(!sidebar_collapsed(width));
    let archived_row = content_row.checked_sub(active_height + header)?;
    (archived_row < app.archived().len()).then_some(app.agents().len() + archived_row)
}

/// The archive button only appears on an active card's title line (the card's first
/// row), reserved as a fixed-width hit box at the right edge of the sidebar.
fn archive_button_at(app: &App, area: Rect, column: u16, row: u16) -> Option<usize> {
    let width = layout_sidebar_width(app, area);
    if width == 0 || sidebar_collapsed(width) {
        return None;
    }
    let agents = agent_list_area(app, sidebar_area(area, width), width);
    if column < agents.x || column >= agents.right() || row < agents.y || row >= agents.bottom() {
        return None;
    }
    if column < agents.right().saturating_sub(ARCHIVE_BUTTON_WIDTH) {
        return None;
    }
    let card_height = usize::from(AGENT_CARD_HEIGHT);
    let content_row = sidebar_row_start(app, agents, card_height) + usize::from(row - agents.y);
    let active_height = app.agents().len() * card_height;
    if content_row >= active_height || !content_row.is_multiple_of(card_height) {
        return None;
    }
    Some(content_row / card_height)
}

pub(crate) fn click_action(app: &App, area: Rect, column: u16, row: u16) -> Option<ClickAction> {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return None;
    }

    let sidebar_width = layout_sidebar_width(app, area);
    if matches!(
        app.mode(),
        Mode::Terminal | Mode::Menu | Mode::Keybinds | Mode::Settings | Mode::Usage
    ) {
        if resize_handle_area(app, area).is_some_and(|handle| contains(handle, column, row)) {
            return Some(ClickAction::ResizeSidebar);
        }
        if app.mode() != Mode::Menu
            && usage_button_area(area, sidebar_width)
                .is_some_and(|button| contains(button, column, row))
        {
            return Some(ClickAction::Management(ManagementCommand::OpenUsage));
        }
        if app.mode() != Mode::Menu
            && new_agent_button_area(area, sidebar_width)
                .is_some_and(|button| contains(button, column, row))
        {
            return Some(ClickAction::Management(ManagementCommand::ChooseAgent));
        }
        if menu_button_area(area, sidebar_width).is_some_and(|button| contains(button, column, row))
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
            if let Some(index) = archive_button_at(app, area, column, row) {
                return Some(ClickAction::ArchiveCard(index));
            }
            if let Some(index) = agent_item_at(app, area, column, row) {
                return Some(ClickAction::SidebarItem(index));
            }
            if app.agents().is_empty() && contains(terminal_area(area, sidebar_width), column, row)
            {
                return Some(ClickAction::Management(ManagementCommand::ChooseAgent));
            }
            None
        }
        Mode::Menu => {
            if let Some(item) = menu_item_at(app, area, column, row) {
                return Some(ClickAction::MenuItem(item));
            }
            // A click anywhere outside the popover dismisses the menu; one inside it
            // that missed an entry (a border or gap) is ignored.
            let inside_popover = menu_button_area(area, sidebar_width)
                .is_some_and(|button| contains(menu_popup_area(button), column, row));
            (!inside_popover).then_some(ClickAction::Cancel)
        }
        Mode::NewAgent(NewAgentPage::Form) => {
            let inner = dialog_inner(ModalSize::Compact, area);
            if !contains(inner, column, row) {
                return None;
            }
            match row.checked_sub(inner.y)? {
                1 => Some(ClickAction::NewAgentField(NewAgentField::Workspace)),
                2 => Some(ClickAction::NewAgentField(NewAgentField::Location)),
                3 => Some(ClickAction::NewAgentField(NewAgentField::Agent)),
                4 => Some(ClickAction::NewAgentField(NewAgentField::Start)),
                6 => hint_at(FORM_HINTS, inner.x, column),
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
        Mode::NewAgent(NewAgentPage::Locations) => {
            let inner = dialog_inner(ModalSize::Compact, area);
            if !contains(inner, column, row) {
                return None;
            }
            let count = Checkout::ALL.len();
            let line = usize::from(row - inner.y);
            if (1..=count).contains(&line) {
                return Some(ClickAction::Location(line - 1));
            }
            (line == count + 2)
                .then(|| hint_at(LOCATION_HINTS, inner.x, column))
                .flatten()
        }
        Mode::NewAgent(NewAgentPage::CreatingWorktree) => {
            let inner = dialog_inner(ModalSize::Compact, area);
            if !contains(inner, column, row) {
                return None;
            }
            let line = usize::from(row - inner.y);
            (line == 4)
                .then(|| hint_at(CREATING_HINTS, inner.x, column))
                .flatten()
        }
        Mode::NewAgent(NewAgentPage::Agents) => {
            let inner = dialog_inner(ModalSize::Compact, area);
            if !contains(inner, column, row) {
                return None;
            }
            let count = app.available_harnesses().len();
            let line = usize::from(row - inner.y);
            if (1..=count).contains(&line) {
                return Some(ClickAction::AgentKind(line - 1));
            }
            (line == if count == 0 { 3 } else { count + 2 })
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
            if let Some(binding) = MANAGEMENT_KEYBINDINGS.get(line) {
                return Some(ClickAction::Management(binding.command));
            }
            (line == MANAGEMENT_KEYBINDINGS.len())
                .then(|| hint_at(BACK_HINTS, inner.x, column))
                .flatten()
        }
        Mode::Usage => {
            let inner = dialog_inner(ModalSize::Standard, area);
            if !contains(inner, column, row) {
                return None;
            }
            let line = row - inner.y;
            if line == 1 {
                for provider in &app.usage().providers {
                    if contains(usage_tab_area(provider.kind, app, inner, row), column, row) {
                        return Some(ClickAction::UsageTab(provider.kind));
                    }
                }
            }
            (line == USAGE_HINT_LINE)
                .then(|| hint_at(USAGE_HINTS, inner.x, column))
                .flatten()
        }
        Mode::Settings => {
            let inner = dialog_inner(ModalSize::Standard, area);
            if !contains(inner, column, row) {
                return None;
            }
            let line = row - inner.y;
            if line == 1 {
                for tab in SettingsTab::ALL {
                    if contains(settings_tab_area(tab, inner, row), column, row) {
                        return Some(ClickAction::SettingsTab(tab));
                    }
                }
            }
            if app.settings_tab() == SettingsTab::Appearance && line == 3 {
                let previous = Rect::new(inner.x + 7, row, 17, 1);
                if contains(previous, column, row) {
                    return Some(ClickAction::ThemePrevious);
                }
                let next_x = inner.x + 24 + app.theme().label().chars().count() as u16;
                if contains(Rect::new(next_x, row, 3, 1), column, row) {
                    return Some(ClickAction::ThemeNext);
                }
            }
            (line == settings_hint_line(app.settings_tab()))
                .then(|| hint_at(settings_hints(app.settings_tab()), inner.x, column))
                .flatten()
        }
        Mode::Prefix | Mode::ToolPrefix => None,
    }
}

pub(crate) fn hover_action(app: &App, area: Rect, column: u16, row: u16) -> Option<ClickAction> {
    match click_action(app, area, column, row)? {
        ClickAction::Cancel if app.mode() == Mode::Menu => None,
        ClickAction::Management(ManagementCommand::ChooseAgent) => {
            new_agent_button_area(area, layout_sidebar_width(app, area))
                .filter(|button| contains(*button, column, row))
                .map(|_| ClickAction::Management(ManagementCommand::ChooseAgent))
        }
        action => Some(action),
    }
}

fn chrome_style(theme: Theme, hovered: bool, selected: bool, base: Style) -> Style {
    if selected || hovered {
        theme.selected()
    } else {
        base
    }
}

fn hover_style(theme: Theme, hovered: bool, base: Style) -> Style {
    if hovered { theme.selected() } else { base }
}

fn fill_card_line(line: Line<'static>, width: u16, style: Style) -> Line<'static> {
    let used = u16::try_from(line.width()).unwrap_or(u16::MAX);
    let pad = width.saturating_sub(used);
    let mut spans = line.spans;
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(usize::from(pad))));
    }
    Line::from(spans).style(style)
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
        button.width.max(SIDEBAR_WIDTH.saturating_sub(1)),
        MENU_HEIGHT,
    )
}

fn trailing_shortcut(label: Span<'static>, shortcut: Span<'static>, width: u16) -> Line<'static> {
    let used = label.content.chars().count() + shortcut.content.chars().count();
    let gap = usize::from(width).saturating_sub(used);
    Line::from(vec![label, Span::raw(" ".repeat(gap)), shortcut])
}

fn sidebar_inner(sidebar: Rect) -> Rect {
    Block::new().borders(Borders::RIGHT).inner(sidebar)
}

fn agent_list_area(app: &App, sidebar: Rect, sidebar_width: u16) -> Rect {
    let inner = sidebar_inner(sidebar);
    let popup_height = if app.mode() == Mode::Menu && !sidebar_collapsed(sidebar_width) {
        MENU_HEIGHT
    } else {
        0
    };
    // One row each for the usage, new-agent, and menu buttons stacked at the bottom.
    let reserved = 3 + popup_height + u16::from(app.notice().is_some());
    Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(reserved),
    )
}

fn sidebar_row_start(app: &App, area: Rect, card_height: usize) -> usize {
    let visible = usize::from(area.height.max(1));
    let max = app
        .sidebar_content_height(card_height)
        .saturating_sub(visible);
    app.sidebar_scroll()
        .unwrap_or_else(|| {
            (app.selected_index() * card_height)
                .saturating_sub(visible.saturating_sub(card_height.max(1)))
        })
        .min(max)
}

pub fn agent_list_page_size(app: &App, area: Rect) -> usize {
    let width = layout_sidebar_width(app, area);
    usize::from(agent_list_area(app, sidebar_area(area, width), width).height).max(1)
}

pub fn sidebar_list_card_height(app: &App, area: Rect) -> usize {
    usize::from(sidebar_card_height(layout_sidebar_width(app, area)))
}

fn render_sidebar(
    frame: &mut Frame<'_>,
    app: &App,
    area: Rect,
    theme: Theme,
    colors_enabled: bool,
    nerd_fonts: bool,
    hovered: Option<ClickAction>,
) {
    let width = layout_sidebar_width(app, area);
    let sidebar = sidebar_area(area, width);
    let collapsed = sidebar_collapsed(width);
    let card_height = usize::from(sidebar_card_height(width));
    let resizing = hovered == Some(ClickAction::ResizeSidebar);
    let block = Block::new()
        .borders(Borders::RIGHT)
        .border_style(if resizing {
            accent(theme)
        } else {
            border(theme)
        })
        .style(Style::default().bg(Color::Reset));
    let inner = block.inner(sidebar);
    frame.render_widget(block, sidebar);

    let new_button =
        new_agent_button_area(area, width).expect("visible sidebar has a new-agent button");
    let button = menu_button_area(area, width).expect("visible sidebar has a menu button");
    let agents_area = agent_list_area(app, sidebar, width);

    let mut rows = Vec::new();
    for (index, agent) in app.agents().iter().enumerate() {
        let selected = index == app.selected_index();
        let status = agent.display_status();
        let (circle, status_style) = status_display(status, colors_enabled);
        let mut lines = if collapsed {
            vec![collapsed_status_line(circle, status_style, selected, theme)]
        } else {
            expanded_agent_card(
                agent,
                index,
                selected,
                circle,
                status_style,
                agents_area.width,
                nerd_fonts,
                hovered,
                theme,
            )
        };
        if hovered == Some(ClickAction::SidebarItem(index)) {
            lines = lines
                .into_iter()
                .map(|line| fill_card_line(line, agents_area.width, theme.hover_fill()))
                .collect();
        }
        rows.append(&mut lines);
    }
    if !app.archived().is_empty() {
        if !collapsed {
            rows.push(Line::from(Span::styled(
                " Archived",
                theme.muted().add_modifier(Modifier::BOLD),
            )));
        }
        rows.extend(
            app.archived()
                .iter()
                .enumerate()
                .map(|(index, conversation)| {
                    let line = if collapsed {
                        collapsed_status_line("○", theme.muted(), false, theme)
                    } else {
                        let number = format!("  {} · ", app.agents().len() + index + 1);
                        let title = end_truncate(
                            &conversation.title,
                            usize::from(agents_area.width).saturating_sub(number.chars().count()),
                        );
                        Line::from(vec![
                            Span::styled(number, theme.muted()),
                            Span::styled(title, text(theme)),
                        ])
                    };
                    if hovered == Some(ClickAction::SidebarItem(app.agents().len() + index)) {
                        fill_card_line(line, agents_area.width, theme.hover_fill())
                    } else {
                        line
                    }
                }),
        );
    }
    let scroll =
        u16::try_from(sidebar_row_start(app, agents_area, card_height)).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(rows).scroll((scroll, 0)), agents_area);

    if let Some(notice) = app.notice() {
        let y = agents_area.bottom();
        let message = if collapsed {
            " !".to_string()
        } else {
            format!(" ! {notice}")
        };
        frame.render_widget(
            Paragraph::new(message).style(warning(theme)),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }

    if let Some(usage_button) = usage_button_area(area, width) {
        let usage_style = chrome_style(
            theme,
            hovered == Some(ClickAction::Management(ManagementCommand::OpenUsage)),
            false,
            text(theme),
        );
        frame.render_widget(
            Paragraph::new(trailing_shortcut(
                Span::styled(" % Usage", usage_style.add_modifier(Modifier::BOLD)),
                Span::styled("^B u", usage_style),
                usage_button.width,
            ))
            .style(usage_style),
            usage_button,
        );
    }

    let new_button_style = chrome_style(
        theme,
        hovered == Some(ClickAction::Management(ManagementCommand::ChooseAgent)),
        false,
        text(theme),
    );
    frame.render_widget(
        Paragraph::new(sidebar_button_label(
            " +",
            " + New agent",
            "^B n",
            new_button_style.add_modifier(Modifier::BOLD),
            new_button_style,
            new_button.width,
            collapsed,
        ))
        .style(new_button_style),
        new_button,
    );

    if app.mode() == Mode::Menu {
        render_menu(frame, menu_popup_area(button), theme, hovered);
    }

    let button_style = hover_style(theme, hovered == Some(ClickAction::ToggleMenu), text(theme));
    frame.render_widget(
        Paragraph::new(sidebar_button_label(
            " ≡",
            " ≡ Menu",
            "^B m",
            button_style.add_modifier(Modifier::BOLD),
            button_style,
            button.width,
            collapsed,
        ))
        .style(button_style),
        button,
    );
}

fn collapsed_status_line(
    circle: &'static str,
    status_style: Style,
    selected: bool,
    theme: Theme,
) -> Line<'static> {
    let marker = if selected { "▌" } else { " " };
    Line::from(vec![
        Span::styled(marker, accent(theme)),
        Span::styled(circle, status_style),
    ])
}

#[allow(clippy::too_many_arguments)]
fn expanded_agent_card(
    agent: &AgentState,
    index: usize,
    selected: bool,
    circle: &'static str,
    status_style: Style,
    width: u16,
    nerd_fonts: bool,
    hovered: Option<ClickAction>,
    theme: Theme,
) -> Vec<Line<'static>> {
    let marker = if selected { "▌" } else { " " };
    let content_width = usize::from(width.saturating_sub(2));
    let number = format!("{} · ", index + 1);
    let title = end_truncate(
        agent.conversation_title().unwrap_or("Unnamed conversation"),
        usize::from(width)
            .saturating_sub(3 + number.chars().count())
            .saturating_sub(usize::from(ARCHIVE_BUTTON_WIDTH)),
    );
    let selected_style = if selected {
        text(theme).add_modifier(Modifier::BOLD)
    } else {
        text(theme)
    };
    // Where the agent is now, preferring the checkout root over a directory inside it, and
    // falling back to where it was launched only while the live directory is unknown.
    let directory = agent
        .git()
        .map(|git| git.worktree.as_path())
        .or_else(|| agent.working_directory())
        .unwrap_or_else(|| agent.launch_directory());
    let directory = directory
        .file_name()
        .unwrap_or(directory.as_os_str())
        .to_string_lossy();
    let worktree_marker = if agent.git().is_some_and(|git| git.linked) {
        worktree_icon(nerd_fonts)
    } else {
        ""
    };
    let archive_button_style = if hovered == Some(ClickAction::ArchiveCard(index)) {
        theme.selected()
    } else {
        theme.muted()
    };
    let mut title_spans = vec![
        Span::styled(marker, accent(theme)),
        Span::styled(format!("{circle} "), status_style),
        Span::styled(number, theme.muted()),
        Span::styled(title, selected_style.add_modifier(Modifier::BOLD)),
    ];
    let used = u16::try_from(Line::from(title_spans.clone()).width()).unwrap_or(u16::MAX);
    let pad = width
        .saturating_sub(used)
        .saturating_sub(ARCHIVE_BUTTON_WIDTH);
    if pad > 0 {
        title_spans.push(Span::raw(" ".repeat(usize::from(pad))));
    }
    title_spans.push(Span::styled(
        format!(" {} ", archive_icon(nerd_fonts)),
        archive_button_style,
    ));
    let mut lines = vec![
        Line::from(title_spans),
        Line::from(vec![
            Span::styled(marker, accent(theme)),
            Span::raw(" "),
            Span::styled(agent.kind().label(), text(theme)),
            Span::styled(" · ", theme.muted()),
            Span::styled(worktree_marker, theme.muted()),
            Span::styled(
                end_truncate(
                    &directory,
                    content_width
                        .saturating_sub(agent.kind().label().chars().count() + 5)
                        .saturating_sub(worktree_marker.chars().count()),
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
    lines
}

fn sidebar_button_label(
    compact: &'static str,
    label: &'static str,
    shortcut: &'static str,
    label_style: Style,
    shortcut_style: Style,
    width: u16,
    collapsed: bool,
) -> Line<'static> {
    if collapsed {
        Line::from(Span::styled(compact, label_style))
    } else {
        trailing_shortcut(
            Span::styled(label, label_style),
            Span::styled(shortcut, shortcut_style),
            width,
        )
    }
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

fn render_menu(frame: &mut Frame<'_>, area: Rect, theme: Theme, hovered: Option<ClickAction>) {
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .title(" Menu ")
        .border_style(accent(theme))
        .style(theme.surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = MenuItem::ALL.into_iter().enumerate().map(|(index, item)| {
        let style = hover_style(
            theme,
            hovered == Some(ClickAction::MenuItem(item)),
            text(theme),
        );
        ListItem::new(Line::from(vec![
            Span::styled(format!(" [{}] ", index + 1), accent(theme)),
            Span::styled(item.label(), style),
        ]))
        .style(style)
    });
    frame.render_widget(List::new(rows), inner);
}

fn render_terminal(frame: &mut Frame<'_>, model: UiModel<'_>, area: Rect) {
    let Some(screen) = model.screen else {
        frame.render_widget(
            Paragraph::new(if model.app.agents().is_empty() {
                "No agents open. Press Ctrl+B, then n to start one."
            } else {
                "Agent terminal unavailable."
            })
            .centered()
            .style(model.theme.muted()),
            area,
        );
        return;
    };
    // The cursor is the host terminal's own, placed below, so that it keeps the shape, color and
    // blink the user configured. Painting one into the buffer can only produce a static block.
    let pane = TerminalScreen::new(screen).with_selection(model.selection, model.theme.selected());
    if model.app.mode() == Mode::Terminal
        && !model.scrolled
        && model.selection.is_none()
        && let Some(position) = pane.cursor_position(area)
    {
        frame.set_cursor_position(position);
    }
    frame.render_widget(pane, area);
}

fn render_new_agent_form(
    frame: &mut Frame<'_>,
    app: &App,
    theme: Theme,
    hovered: Option<ClickAction>,
) {
    let Some(state) = app.new_agent() else {
        return;
    };
    let workspace = state.draft.workspace.as_ref().map_or_else(
        || "<choose workspace>".into(),
        |path| path.display().to_string(),
    );
    let location = state.draft.checkout.label().to_string();
    let agent = state.draft.agent.map_or("<choose agent>", AgentKind::label);
    let complete = state.draft.workspace.is_some() && state.draft.agent.is_some();
    let row = |field, label: &str, value: String| {
        let selected = state.draft.selected_field == field;
        Line::from(vec![
            Span::styled(if selected { " > " } else { "   " }, accent(theme)),
            Span::styled(format!("{label:<12}"), text(theme)),
            Span::styled(
                end_truncate(&value, 42),
                chrome_style(
                    theme,
                    hovered == Some(ClickAction::NewAgentField(field)),
                    selected,
                    theme.muted(),
                ),
            ),
        ])
    };
    let start_selected = state.draft.selected_field == NewAgentField::Start;
    render_dialog(
        frame,
        theme,
        " New agent ",
        ModalSize::Compact,
        vec![
            Line::from(""),
            row(NewAgentField::Workspace, "Workspace", workspace),
            row(NewAgentField::Location, "Location", location),
            row(NewAgentField::Agent, "Agent", agent.into()),
            Line::from(vec![
                Span::styled(if start_selected { " > " } else { "   " }, accent(theme)),
                Span::styled(
                    if complete {
                        "Start agent"
                    } else {
                        "Start agent (disabled)"
                    },
                    hover_style(
                        theme,
                        !start_selected
                            && hovered == Some(ClickAction::NewAgentField(NewAgentField::Start)),
                        if complete { text(theme) } else { theme.muted() },
                    ),
                ),
            ]),
            Line::from(""),
            hint_line(FORM_HINTS, theme, hovered),
        ],
    );
}

fn render_workspace_choices(
    frame: &mut Frame<'_>,
    app: &App,
    theme: Theme,
    hovered: Option<ClickAction>,
) {
    let Some(state) = app.new_agent() else {
        return;
    };
    let visible = 7;
    let start = state.selected_workspace.saturating_sub(visible - 1);
    let mut lines = vec![Line::from("")];
    if state.workspaces.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No saved workspaces. Press b to browse.",
            chrome_style(
                theme,
                hovered == Some(ClickAction::BrowseWorkspaces),
                false,
                theme.muted(),
            ),
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
                    let selected = index == state.selected_workspace;
                    let style = hover_style(
                        theme,
                        !selected && hovered == Some(ClickAction::Workspace(index)),
                        text(theme),
                    );
                    Line::from(vec![
                        Span::styled(if selected { " > " } else { "   " }, accent(theme)),
                        Span::styled(format!("{name:<14}"), style),
                        Span::styled(
                            end_truncate(&choice.path.display().to_string(), 32),
                            theme.muted(),
                        ),
                        Span::styled(missing, warning(theme)),
                    ])
                }),
        );
    }
    lines.extend([Line::from(""), hint_line(WORKSPACE_HINTS, theme, hovered)]);
    render_dialog(
        frame,
        theme,
        " Choose workspace ",
        ModalSize::Compact,
        lines,
    );
}

fn render_location_choices(
    frame: &mut Frame<'_>,
    app: &App,
    theme: Theme,
    hovered: Option<ClickAction>,
) {
    let Some(state) = app.new_agent() else {
        return;
    };
    let mut lines = vec![Line::from("")];
    lines.extend(Checkout::ALL.iter().enumerate().map(|(index, choice)| {
        let selected = index == state.selected_location;
        let disabled = *choice == Checkout::NewWorktree && state.repository_root.is_none();
        let reason = if disabled {
            "  not a git repository"
        } else {
            ""
        };
        let style = if disabled && !selected {
            theme.muted()
        } else {
            hover_style(
                theme,
                !selected && hovered == Some(ClickAction::Location(index)),
                text(theme),
            )
        };
        Line::from(vec![
            Span::styled(if selected { " > " } else { "   " }, accent(theme)),
            Span::styled(choice.label(), style),
            Span::styled(reason, warning(theme)),
        ])
    }));
    lines.extend([Line::from(""), hint_line(LOCATION_HINTS, theme, hovered)]);
    render_dialog(frame, theme, " Choose location ", ModalSize::Compact, lines);
}

fn render_creating_worktree(
    frame: &mut Frame<'_>,
    app: &App,
    theme: Theme,
    hovered: Option<ClickAction>,
) {
    let checkout = app
        .new_agent()
        .and_then(|state| state.draft.workspace.as_ref())
        .map_or_else(|| "…".into(), |path| path.display().to_string());
    render_dialog(
        frame,
        theme,
        " Creating worktree ",
        ModalSize::Compact,
        vec![
            Line::from(""),
            Line::from(Span::styled("  Creating worktree", text(theme))),
            Line::from(Span::styled(
                format!("  {}", end_truncate(&checkout, 56)),
                theme.muted(),
            )),
            Line::from(""),
            hint_line(CREATING_HINTS, theme, hovered),
        ],
    );
}

fn render_agent_choices(
    frame: &mut Frame<'_>,
    app: &App,
    theme: Theme,
    hovered: Option<ClickAction>,
) {
    let Some(state) = app.new_agent() else {
        return;
    };
    let mut lines = vec![Line::from("")];
    if app.available_harnesses().is_empty() {
        lines.push(Line::from(Span::styled(
            "  No installed harnesses found.",
            theme.muted(),
        )));
    } else {
        lines.extend(
            app.available_harnesses()
                .iter()
                .enumerate()
                .map(|(index, kind)| {
                    let selected = index == state.selected_agent;
                    Line::from(vec![
                        Span::styled(if selected { " > " } else { "   " }, accent(theme)),
                        Span::styled(
                            kind.label(),
                            hover_style(
                                theme,
                                !selected && hovered == Some(ClickAction::AgentKind(index)),
                                text(theme),
                            ),
                        ),
                    ])
                }),
        );
    }
    lines.extend([Line::from(""), hint_line(AGENT_HINTS, theme, hovered)]);
    render_dialog(frame, theme, " Choose agent ", ModalSize::Compact, lines);
}

fn render_native_browser(
    frame: &mut Frame<'_>,
    app: &App,
    theme: Theme,
    hovered: Option<ClickAction>,
) {
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
            let selected = index == browser.selected;
            Line::from(vec![
                Span::styled(if selected { " > " } else { "   " }, accent(theme)),
                Span::styled(
                    end_truncate(&label, content_width),
                    hover_style(
                        theme,
                        !selected && hovered == Some(ClickAction::NativeBrowserItem(index)),
                        text(theme),
                    ),
                ),
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
    lines.push(hint_line(BROWSER_HINTS, theme, hovered));
    render_dialog(frame, theme, " Select workspace ", ModalSize::Large, lines);
}

fn render_embedded_browser(
    frame: &mut Frame<'_>,
    snapshot: Option<&TerminalProcessSnapshot>,
    theme: Theme,
    hovered: Option<ClickAction>,
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
        Paragraph::new(hint_line(EMBEDDED_HINTS, theme, hovered)),
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

fn render_confirmation(
    frame: &mut Frame<'_>,
    theme: Theme,
    title: &str,
    prompt: &str,
    hovered: Option<ClickAction>,
) {
    render_dialog(
        frame,
        theme,
        title,
        ModalSize::Standard,
        vec![
            Line::from(""),
            Line::from(Span::styled(format!("  {prompt}"), warning(theme))),
            Line::from(""),
            hint_line(CONFIRM_HINTS, theme, hovered),
        ],
    );
}

fn render_archive_unavailable(frame: &mut Frame<'_>, theme: Theme, hovered: Option<ClickAction>) {
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
            hint_line(ARCHIVE_UNAVAILABLE_HINTS, theme, hovered),
        ],
    );
}

fn render_stop_confirmation(
    frame: &mut Frame<'_>,
    app: &App,
    theme: Theme,
    hovered: Option<ClickAction>,
) {
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
            hint_line(STOP_HINTS, theme, hovered),
        ],
    );
}

fn render_resume_confirmation(
    frame: &mut Frame<'_>,
    app: &App,
    theme: Theme,
    hovered: Option<ClickAction>,
) {
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
            hint_line(RESUME_HINTS, theme, hovered),
        ],
    );
}

fn render_keybinds(frame: &mut Frame<'_>, theme: Theme, hovered: Option<ClickAction>) {
    // The binding rows start at the first inner row: with the full table plus the footer, a
    // leading blank would push the hint outside the modal and make it unclickable.
    let mut lines = Vec::with_capacity(MANAGEMENT_KEYBINDINGS.len() + 2);
    lines.extend(MANAGEMENT_KEYBINDINGS.iter().map(|binding| {
        Line::from(Span::styled(
            format!("  {:<27} {}", binding.keys, binding.action),
            chrome_style(
                theme,
                hovered == Some(ClickAction::Management(binding.command)),
                false,
                text(theme),
            ),
        ))
    }));
    lines.push(hint_line(BACK_HINTS, theme, hovered));
    render_dialog(frame, theme, " Keybinds ", ModalSize::Standard, lines);
}

/// Inner row the usage footer sits on. Fixed so the content above it can vary without moving the
/// clickable hints out from under the pointer.
const USAGE_HINT_LINE: u16 = 14;
const USAGE_PROVENANCE_LINE: usize = 13;
const USAGE_LABEL_WIDTH: usize = 14;
const USAGE_BAR_WIDTH: usize = 18;

fn render_usage(
    frame: &mut Frame<'_>,
    app: &App,
    theme: Theme,
    hovered: Option<ClickAction>,
    now_ms: u64,
) {
    let inner = dialog_inner(ModalSize::Standard, frame.area());
    let width = usize::from(inner.width);
    let mut lines = vec![
        Line::from(""),
        usage_tabs(app, theme, hovered),
        Line::from(""),
    ];

    match app.selected_usage() {
        Some(provider) => {
            lines.push(usage_header(provider, theme, width));
            lines.push(Line::from(""));
            lines.extend(usage_body(provider, theme, now_ms, width));
        }
        None => {
            lines.push(Line::from(Span::styled(
                "  No coding agent that publishes usage is installed.",
                theme.muted(),
            )));
        }
    }

    while lines.len() < USAGE_PROVENANCE_LINE {
        lines.push(Line::from(""));
    }
    lines.truncate(USAGE_PROVENANCE_LINE);
    lines.push(usage_provenance(app.selected_usage(), theme, now_ms, width));
    while lines.len() < usize::from(USAGE_HINT_LINE) {
        lines.push(Line::from(""));
    }
    lines.push(hint_line(USAGE_HINTS, theme, hovered));
    render_dialog(frame, theme, " Usage ", ModalSize::Standard, lines);
}

fn usage_tabs(app: &App, theme: Theme, hovered: Option<ClickAction>) -> Line<'static> {
    let selected = app.selected_usage().map(|provider| provider.kind);
    Line::from(
        app.usage()
            .providers
            .iter()
            .map(|provider| {
                Span::styled(
                    format!("  {}  ", provider.kind.label()),
                    chrome_style(
                        theme,
                        hovered == Some(ClickAction::UsageTab(provider.kind)),
                        selected == Some(provider.kind),
                        text(theme),
                    ),
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn usage_tab_area(kind: AgentKind, app: &App, inner: Rect, row: u16) -> Rect {
    let x = inner.x
        + app
            .usage()
            .providers
            .iter()
            .take_while(|provider| provider.kind != kind)
            .map(|provider| provider.kind.label().chars().count() as u16 + 4)
            .sum::<u16>();
    Rect::new(x, row, kind.label().chars().count() as u16 + 4, 1)
}

/// Plan on the left, probe state on the right.
fn usage_header(provider: &UsageProviderReport, theme: Theme, width: usize) -> Line<'static> {
    // The tab already names the provider, so the header carries only the plan when there is one.
    let plan = match &provider.report {
        UsageReport::Available(evidence) => evidence.plan.clone().unwrap_or_default(),
        _ => String::new(),
    };

    let state = if provider.refreshing {
        if provider.observed_at_ms.is_some() {
            "refreshing…"
        } else {
            "checking…"
        }
    } else {
        ""
    };

    let left = clip(
        format!("  {plan}"),
        width.saturating_sub(state.chars().count() + 2),
    );
    let gap = width
        .saturating_sub(left.chars().count())
        .saturating_sub(state.chars().count());
    Line::from(vec![
        Span::styled(left, accent(theme).add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(gap)),
        Span::styled(state.to_owned(), theme.muted()),
    ])
}

fn usage_body(
    provider: &UsageProviderReport,
    theme: Theme,
    now_ms: u64,
    width: usize,
) -> Vec<Line<'static>> {
    match &provider.report {
        UsageReport::Available(evidence) => {
            let mut lines: Vec<Line<'static>> = evidence
                .windows
                .iter()
                .map(|window| usage_window_line(window, theme, now_ms, width))
                .collect();
            if !evidence.notes.is_empty() {
                lines.push(Line::from(""));
                lines.extend(evidence.notes.iter().map(|note| {
                    Line::from(Span::styled(
                        clip(format!("  {note}"), width),
                        theme.muted(),
                    ))
                }));
            }
            lines
        }
        UsageReport::NotProbed => vec![Line::from(Span::styled("  Checking…", theme.muted()))],
        UsageReport::Unavailable(unavailable) => vec![
            Line::from(Span::styled(
                clip(format!("  {}", unavailable.message), width),
                warning(theme),
            )),
            Line::from(""),
            Line::from(Span::styled(
                clip("  Press [r] to check again.".to_owned(), width),
                theme.muted(),
            )),
        ],
    }
}

fn usage_window_line(
    window: &UsageWindow,
    theme: Theme,
    now_ms: u64,
    width: usize,
) -> Line<'static> {
    let percent = window.whole_percent();
    let label = format!(
        "  {:<USAGE_LABEL_WIDTH$}",
        clip_plain(&window.label, USAGE_LABEL_WIDTH)
    );
    let bar = usage_bar(window.used_tenths, USAGE_BAR_WIDTH);
    let reset = format_reset(window.resets_at_ms, now_ms);
    let detail = window
        .detail
        .as_deref()
        .filter(|_| !reset.is_empty())
        .map(|detail| format!(" · {detail}"))
        .unwrap_or_default();

    let used = label.chars().count() + bar.chars().count() + 6;
    let tail = clip_plain(&format!("{reset}{detail}"), width.saturating_sub(used));
    Line::from(vec![
        Span::styled(label, text(theme)),
        Span::styled(bar, usage_style(theme, percent)),
        Span::styled(format!(" {percent:>3}%  "), text(theme)),
        Span::styled(tail, theme.muted()),
    ])
}

/// A filled/empty bar. The numeric percentage always accompanies it, so the bar never carries
/// meaning on its own and the modal stays readable without colour.
fn usage_bar(used_tenths: u16, width: usize) -> String {
    let used = usize::from(used_tenths.min(1000));
    // Round down so the bar only reads as full at 100%, but never show an empty bar for usage
    // that has actually started.
    let mut filled = (used * width) / 1000;
    if used > 0 && filled == 0 {
        filled = 1;
    }
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn usage_style(theme: Theme, percent: u16) -> Style {
    match percent {
        0..=59 => success(theme),
        60..=89 => warning(theme),
        _ => danger(theme),
    }
}

/// Describe a reset as a countdown plus, when it is close enough to matter, the local clock time.
fn format_reset(resets_at_ms: Option<u64>, now_ms: u64) -> String {
    let Some(resets_at) = resets_at_ms else {
        return "reset time not reported".to_owned();
    };
    let Some(remaining) = resets_at.checked_sub(now_ms) else {
        return "resetting now".to_owned();
    };
    let minutes = remaining / 60_000;
    let (hours, days) = (minutes / 60, minutes / 1_440);
    let countdown = if days > 0 {
        format!("in {}d {}h", days, hours % 24)
    } else if hours > 0 {
        format!("in {}h {}m", hours, minutes % 60)
    } else {
        format!("in {minutes}m")
    };
    match local_clock(resets_at).filter(|_| days == 0) {
        Some(clock) => format!("resets {clock} ({countdown})"),
        None => format!("resets {countdown}"),
    }
}

/// Local wall-clock `HH:MM` for an instant, via libc so no date dependency is needed.
fn local_clock(unix_ms: u64) -> Option<String> {
    let seconds = libc::time_t::try_from(unix_ms / 1000).ok()?;
    // SAFETY: `localtime_r` writes into the caller-provided `tm`, which is fully initialised
    // before it is read, and takes the time by pointer without retaining it.
    let time = unsafe {
        let mut out: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&seconds, &mut out).is_null() {
            return None;
        }
        out
    };
    Some(format!("{:02}:{:02}", time.tm_hour, time.tm_min))
}

fn usage_provenance(
    provider: Option<&UsageProviderReport>,
    theme: Theme,
    now_ms: u64,
    width: usize,
) -> Line<'static> {
    let Some(provider) = provider else {
        return Line::from("");
    };
    let text = provider
        .observed_at_ms
        .map(|at| format!("  checked {} ago", elapsed_label(now_ms.saturating_sub(at))))
        .unwrap_or_default();
    Line::from(Span::styled(clip(text, width), theme.muted()))
}

fn elapsed_label(elapsed_ms: u64) -> String {
    let seconds = elapsed_ms / 1000;
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// `render_dialog` wraps rather than clips, so an over-long line would shift every row below it
/// and desynchronise hit-testing. Every dynamic line goes through here.
fn clip(text: String, width: usize) -> String {
    clip_plain(&text, width)
}

fn clip_plain(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn render_settings(frame: &mut Frame<'_>, app: &App, theme: Theme, hovered: Option<ClickAction>) {
    let tab = app.settings_tab();
    let inner = dialog_inner(ModalSize::Standard, frame.area());
    let mut lines = vec![
        Line::from(""),
        settings_tabs(app, theme, hovered),
        Line::from(""),
    ];
    match tab {
        SettingsTab::Appearance => {
            lines.push(Line::from(vec![
                Span::styled("  Theme", text(theme).add_modifier(Modifier::BOLD)),
                Span::styled(
                    "              ‹  ",
                    chrome_style(
                        theme,
                        hovered == Some(ClickAction::ThemePrevious),
                        false,
                        theme.muted(),
                    ),
                ),
                Span::styled(
                    app.theme().label(),
                    accent(theme).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "  ›",
                    chrome_style(
                        theme,
                        hovered == Some(ClickAction::ThemeNext),
                        false,
                        theme.muted(),
                    ),
                ),
            ]));
            lines.push(Line::from(""));
        }
        SettingsTab::Harnesses => {
            lines.extend(
                AgentKind::ALL
                    .iter()
                    .map(|kind| render_harness_line(*kind, app, theme, inner.width)),
            );
            lines.push(Line::from(""));
        }
    }
    lines.push(hint_line(settings_hints(tab), theme, hovered));
    render_dialog(frame, theme, " Settings ", ModalSize::Standard, lines);
}

fn render_harness_line(kind: AgentKind, app: &App, theme: Theme, width: u16) -> Line<'static> {
    let installed = app.harness_installed(kind);
    let status = if installed {
        "✓ installed"
    } else {
        "× not found"
    };
    let mut line = Line::from(vec![
        Span::styled("  ", text(theme)),
        Span::styled(kind.label(), text(theme)),
    ]);
    let gap = usize::from(width)
        .saturating_sub(line.width())
        .saturating_sub(Line::from(status).width());
    line.spans.push(Span::raw(" ".repeat(gap)));
    line.spans.push(Span::styled(
        status,
        if installed {
            success(theme)
        } else {
            warning(theme)
        },
    ));
    line
}

fn settings_tabs(app: &App, theme: Theme, hovered: Option<ClickAction>) -> Line<'static> {
    let mut spans = Vec::new();
    for tab in SettingsTab::ALL {
        let selected = app.settings_tab() == tab;
        spans.push(Span::styled(
            format!("  {}  ", tab.label()),
            chrome_style(
                theme,
                hovered == Some(ClickAction::SettingsTab(tab)),
                selected,
                text(theme),
            ),
        ));
    }
    Line::from(spans)
}

fn settings_tab_area(tab: SettingsTab, inner: Rect, row: u16) -> Rect {
    let x = inner.x
        + SettingsTab::ALL
            .iter()
            .take_while(|candidate| **candidate != tab)
            .map(|candidate| candidate.label().len() as u16 + 4)
            .sum::<u16>();
    Rect::new(x, row, tab.label().len() as u16 + 4, 1)
}

fn settings_hints(tab: SettingsTab) -> &'static [ActionHint] {
    match tab {
        SettingsTab::Appearance => SETTINGS_HINTS,
        SettingsTab::Harnesses => HARNESS_SETTINGS_HINTS,
    }
}

fn settings_hint_line(tab: SettingsTab) -> u16 {
    match tab {
        SettingsTab::Appearance => 5,
        SettingsTab::Harnesses => 3 + AgentKind::ALL.len() as u16 + 1,
    }
}

fn hint_line(hints: &[ActionHint], theme: Theme, hovered: Option<ClickAction>) -> Line<'static> {
    let mut spans = vec![Span::styled("  ", theme.muted())];
    for (index, hint) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", theme.muted()));
        }
        let style = if hovered == Some(hint.action) {
            theme.selected()
        } else {
            theme.muted()
        };
        spans.push(Span::styled(hint.text, style));
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

fn danger(theme: Theme) -> Style {
    Style::default().fg(theme.error)
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
            terminal_area(Rect::new(0, 0, 120, 40), SIDEBAR_WIDTH),
            Rect::new(SIDEBAR_WIDTH, 0, 120 - SIDEBAR_WIDTH, 40)
        );
        assert_eq!(
            terminal_area(Rect::new(0, 0, 120, 40), 0),
            Rect::new(0, 0, 120, 40)
        );
        assert_eq!(clamp_sidebar_width(1, 80), SIDEBAR_MIN_WIDTH);
        assert_eq!(clamp_sidebar_width(99, 80), 39);
        assert_eq!(clamp_sidebar_width(28, 200), 28);
    }

    #[test]
    fn sidebar_has_no_top_bar_and_collapses_to_status_circles() {
        let agent = AgentSnapshot {
            id: svarm_agent::AgentId::new(1),
            kind: svarm_agent::AgentKind::Codex,
            launch_directory: PathBuf::from("/tmp/plain-directory"),
            working_directory: None,
            status: SessionStatus::Running,
            exit: None,
            output_generation: 1,
            seen_generation: 1,
            completed_generation: 0,
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            conversation_title: Some("Live thread".into()),
            conversation_id: None,
            activity: AgentActivity::Idle,
            recognition: None,
            git: None,
        };
        let mut app = App::hydrate(
            SvarmSessionSnapshot {
                summary: SessionSummary {
                    id: SessionId(21),
                    running_agents: 1,
                    total_agents: 1,
                    attachment: None,
                    last_user_activity_ms: 1,
                    revision: SessionRevision(1),
                },
                selected_agent_id: Some(agent.id),
                rows: 24,
                cols: 80,
                agents: vec![agent],
                archived: vec![ArchivedConversation {
                    conversation_id: "archived-id".into(),
                    title: "Hidden archive title".into(),
                    kind: svarm_agent::AgentKind::Claude,
                    launch_directory: "/tmp/hidden-archive-directory".into(),
                }],
            },
            crate::theme::ThemeName::Monochrome,
            None,
        );

        let expanded = render_app_text(&app);
        assert!(expanded.contains("Live thread"));
        assert!(expanded.contains("Hidden archive title"));
        assert!(expanded.contains("Archived"));
        assert!(!expanded.contains(" svarm "));
        let top_left = expanded
            .chars()
            .take(SIDEBAR_WIDTH as usize)
            .collect::<String>();
        assert!(
            !top_left.contains('─'),
            "sidebar should not draw a top bar: {top_left:?}"
        );

        app.set_sidebar_width(SIDEBAR_MIN_WIDTH);
        let collapsed = render_app_text(&app);
        assert!(collapsed.contains('●'));
        assert!(!collapsed.contains("Live thread"));
        assert!(!collapsed.contains("Hidden archive title"));
        assert!(!collapsed.contains("Archived"));
        assert!(!collapsed.contains("New agent"));
        assert!(!collapsed.contains("Menu"));
        assert_eq!(agent_item_at(&app, Rect::new(0, 0, 80, 24), 1, 0), Some(0));
        assert_eq!(agent_item_at(&app, Rect::new(0, 0, 80, 24), 1, 1), Some(1));
        assert_eq!(
            click_action(&app, Rect::new(0, 0, 80, 24), 0, 0),
            Some(ClickAction::SidebarItem(0))
        );
        assert_eq!(
            click_action(&app, Rect::new(0, 0, 80, 24), SIDEBAR_MIN_WIDTH - 1, 0),
            Some(ClickAction::ResizeSidebar)
        );
    }

    #[test]
    fn menu_hit_areas_stay_at_the_bottom_of_the_sidebar() {
        let area = Rect::new(0, 0, 120, 40);
        let app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Monochrome,
            false,
            None,
        );
        assert_eq!(
            usage_button_area(area, SIDEBAR_WIDTH),
            Some(Rect::new(0, 37, 27, 1))
        );
        assert_eq!(
            new_agent_button_area(area, SIDEBAR_WIDTH),
            Some(Rect::new(0, 38, 27, 1))
        );
        assert_eq!(
            menu_button_area(area, SIDEBAR_WIDTH),
            Some(Rect::new(0, 39, 27, 1))
        );
        // The three buttons stack bottom-up with no gap, and none exists without a sidebar.
        assert_eq!(usage_button_area(area, 0), None);
        let popup = menu_popup_area(menu_button_area(area, SIDEBAR_WIDTH).unwrap());
        assert_eq!(popup, Rect::new(0, 33, 27, 6));
        assert_eq!(menu_item_at(&app, area, 2, 36), Some(MenuItem::Keybinds));
        assert_eq!(menu_item_at(&app, area, 2, 37), Some(MenuItem::Settings));
        assert_eq!(menu_item_at(&app, area, 50, 36), None);
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
                        selection: None,
                        toast: None,
                        embedded: None,
                        theme: app.theme().theme(false),
                        colors_enabled: false,
                        nerd_fonts: false,
                        now_ms: 0,
                        pointer: None,
                    },
                );
            })
            .unwrap();

        let area = Rect::new(0, 0, 80, 24);
        let buffer = terminal.backend().buffer();
        let row = |button: Rect| {
            (button.x..button.right())
                .map(|x| buffer[(x, button.y)].symbol())
                .collect::<String>()
        };
        let usage_button = row(usage_button_area(area, SIDEBAR_WIDTH).unwrap());
        let new_button = row(new_agent_button_area(area, SIDEBAR_WIDTH).unwrap());
        let menu_button = row(menu_button_area(area, SIDEBAR_WIDTH).unwrap());
        assert!(usage_button.starts_with(" % Usage"));
        assert!(usage_button.ends_with("^B u"));
        assert!(new_button.starts_with(" + New agent"));
        assert!(new_button.ends_with("^B n"));
        assert!(menu_button.starts_with(" ≡ Menu"));
        assert!(menu_button.ends_with("^B m"));
    }

    fn selected_tab(app: &App) -> Option<AgentKind> {
        app.selected_usage().map(|provider| provider.kind)
    }

    fn usage_window(label: &str, percent: f64, resets_at_ms: Option<u64>) -> UsageWindow {
        let mut window = UsageWindow::from_percent(label, percent);
        window.resets_at_ms = resets_at_ms;
        window
    }

    fn usage_provider(
        kind: AgentKind,
        report: UsageReport,
        refreshing: bool,
    ) -> UsageProviderReport {
        UsageProviderReport {
            kind,
            report,
            observed_at_ms: Some(1_000),
            refreshing,
        }
    }

    fn available_usage(windows: Vec<UsageWindow>) -> UsageReport {
        UsageReport::Available(svarm_agent::protocol::UsageEvidence {
            plan: Some("Max".into()),
            windows,
            notes: Vec::new(),
            source: "GET api.anthropic.com/api/oauth/usage".into(),
        })
    }

    fn usage_app(providers: Vec<UsageProviderReport>) -> App {
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Monochrome,
            false,
            None,
        );
        app.set_usage(svarm_agent::protocol::UsageOverview { providers });
        app.open_usage();
        app
    }

    fn two_provider_app() -> App {
        usage_app(vec![
            usage_provider(
                AgentKind::Codex,
                available_usage(vec![usage_window("Weekly", 8.0, Some(3_600_000))]),
                false,
            ),
            usage_provider(
                AgentKind::Claude,
                available_usage(vec![
                    usage_window("5-hour", 42.0, Some(4_500_000)),
                    usage_window("Weekly", 17.0, None),
                ]),
                false,
            ),
        ])
    }

    #[test]
    fn usage_modal_shows_each_window_with_its_percentage_and_reset_at_80x24() {
        let mut app = two_provider_app();
        app.select_usage_tab(AgentKind::Claude);
        let rendered = render_app_text(&app);

        assert!(rendered.contains("Claude Code"), "{rendered}");
        assert!(rendered.contains("5-hour"));
        assert!(
            rendered.contains("42%"),
            "the number must be shown, not only a bar"
        );
        assert!(rendered.contains("17%"));
        // 4_500_000ms from a zero clock is 1h15m away.
        assert!(rendered.contains("in 1h 15m"), "{rendered}");
        // A window the provider gave no reset for must say so rather than imply one.
        assert!(rendered.contains("reset time not reported"));
        assert!(
            !rendered.contains("api.anthropic.com"),
            "the probe source is not shown"
        );
        assert!(rendered.contains("checked"), "{rendered}");
    }

    #[test]
    fn usage_bars_never_stand_in_for_the_number() {
        // Empty and full are exact; anything in between is non-empty and not yet full.
        assert_eq!(usage_bar(0, 10), "░".repeat(10));
        assert_eq!(usage_bar(1000, 10), "█".repeat(10));
        assert_eq!(usage_bar(1, 10).chars().filter(|c| *c == '█').count(), 1);
        assert_eq!(usage_bar(999, 10).chars().filter(|c| *c == '░').count(), 1);
        for tenths in [0, 1, 250, 500, 999, 1000, 2000] {
            assert_eq!(usage_bar(tenths, 18).chars().count(), 18);
        }
    }

    #[test]
    fn resets_are_described_relatively_and_absence_is_stated() {
        assert_eq!(format_reset(None, 0), "reset time not reported");
        assert_eq!(format_reset(Some(0), 1_000), "resetting now");
        assert!(format_reset(Some(600_000), 0).contains("in 10m"));
        assert!(format_reset(Some(4_500_000), 0).contains("in 1h 15m"));
        // Beyond a day the clock time stops being useful, so only the countdown is shown.
        let far = format_reset(Some(280_800_000), 0);
        assert_eq!(far, "resets in 3d 6h");
    }

    #[test]
    fn usage_tabs_switch_by_click_and_by_key_to_the_same_place() {
        let area = Rect::new(0, 0, 80, 24);
        let inner = dialog_inner(ModalSize::Standard, area);
        let tab_row = inner.y + 1;

        let mut clicked = two_provider_app();
        let claude_tab = usage_tab_area(AgentKind::Claude, &clicked, inner, tab_row);
        assert_eq!(
            click_action(&clicked, area, claude_tab.x + 1, tab_row),
            Some(ClickAction::UsageTab(AgentKind::Claude))
        );
        clicked.select_usage_tab(AgentKind::Claude);

        let mut keyed = two_provider_app();
        keyed.move_usage_tab(1);

        assert_eq!(selected_tab(&keyed), Some(AgentKind::Claude));
        assert_eq!(render_app_text(&keyed), render_app_text(&clicked));
    }

    #[test]
    fn usage_hints_and_the_sidebar_button_are_clickable() {
        let app = two_provider_app();
        let area = Rect::new(0, 0, 80, 24);
        let inner = dialog_inner(ModalSize::Standard, area);
        assert_hint_clicks(&app, area, inner.x, inner.y + USAGE_HINT_LINE, USAGE_HINTS);

        let button = usage_button_area(area, SIDEBAR_WIDTH).unwrap();
        assert_eq!(
            click_action(&app, area, button.x + 2, button.y),
            Some(ClickAction::Management(ManagementCommand::OpenUsage))
        );
    }

    /// The keyboard and the mouse must reach the same canonical action: `apply_click_action`
    /// forwards `ClickAction::Management` straight into `handle_management_command`, so proving
    /// both produce the same command proves both paths do the same thing.
    #[test]
    fn clicking_the_usage_button_and_pressing_the_key_produce_one_command() {
        let app = two_provider_app();
        let area = Rect::new(0, 0, 80, 24);
        let button = usage_button_area(area, SIDEBAR_WIDTH).unwrap();

        let clicked = click_action(&app, area, button.x + 2, button.y);
        let keyed = crate::input::management_command(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('u'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(clicked, Some(ClickAction::Management(keyed)));
        assert_eq!(keyed, ManagementCommand::OpenUsage);
    }

    #[test]
    fn the_usage_button_is_covered_by_the_menu_popover() {
        let mut app = two_provider_app();
        app.set_mode(Mode::Menu);
        let area = Rect::new(0, 0, 80, 24);
        let button = usage_button_area(area, SIDEBAR_WIDTH).unwrap();
        assert_ne!(
            click_action(&app, area, button.x + 2, button.y),
            Some(ClickAction::Management(ManagementCommand::OpenUsage))
        );
    }

    #[test]
    fn a_refreshing_provider_keeps_its_numbers_and_says_it_is_rechecking() {
        let app = usage_app(vec![usage_provider(
            AgentKind::Claude,
            available_usage(vec![usage_window("5-hour", 42.0, None)]),
            true,
        )]);
        let rendered = render_app_text(&app);
        assert!(rendered.contains("42%"), "stale numbers stay on screen");
        assert!(rendered.contains("refreshing…"), "{rendered}");
    }

    #[test]
    fn a_provider_that_cannot_be_read_states_the_reason() {
        let app = usage_app(vec![usage_provider(
            AgentKind::Codex,
            UsageReport::Unavailable(svarm_agent::protocol::UsageUnavailable {
                reason: svarm_agent::protocol::UsageUnavailableReason::NotSignedIn,
                message: "Not signed in. Run `codex login`, then refresh.".into(),
                evidence: "codex app-server account/read reported no signed-in account".into(),
            }),
            false,
        )]);
        let rendered = render_app_text(&app);
        assert!(rendered.contains("Not signed in"), "{rendered}");
        assert!(rendered.contains("codex login"));
        assert!(
            !rendered.contains("app-server"),
            "probe evidence is not shown: {rendered}"
        );
        // No percentage may be shown for a provider that reported none. (The sidebar's
        // "% Usage" button is not a reading, so look for a digit before the sign.)
        let digits: Vec<char> = rendered.chars().collect();
        assert!(
            !digits
                .windows(2)
                .any(|pair| pair[0].is_ascii_digit() && pair[1] == '%'),
            "{rendered}"
        );
    }

    #[test]
    fn usage_modal_survives_having_no_providers_at_all() {
        let app = usage_app(Vec::new());
        let rendered = render_app_text(&app);
        assert!(rendered.contains("No coding agent"), "{rendered}");
    }

    /// `render_dialog` wraps rather than clips, so an over-long line would push the footer down
    /// and break hit-testing for every hint.
    #[test]
    fn usage_lines_never_exceed_the_modal_width() {
        let theme = crate::theme::ThemeName::Monochrome.theme(false);
        let width = usize::from(dialog_inner(ModalSize::Standard, Rect::new(0, 0, 80, 24)).width);
        let long = "x".repeat(400);
        let provider = UsageProviderReport {
            kind: AgentKind::Claude,
            report: UsageReport::Available(svarm_agent::protocol::UsageEvidence {
                plan: Some(long.clone()),
                windows: vec![usage_window(&long, 100.0, Some(u64::MAX))],
                notes: vec![long.clone()],
                source: long.clone(),
            }),
            observed_at_ms: Some(0),
            refreshing: true,
        };

        let mut lines = vec![usage_header(&provider, theme, width)];
        lines.extend(usage_body(&provider, theme, 0, width));
        lines.push(usage_provenance(Some(&provider), theme, 0, width));
        for line in lines {
            let rendered: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            assert!(
                rendered.chars().count() <= width,
                "line is {} wide, limit {width}: {rendered}",
                rendered.chars().count()
            );
        }
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
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            true,
            None,
        );
        app.open_new_agent(None, None, Vec::new());
        app.open_location_choices();

        let draw = |app: &App, width, height| {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render(
                        frame,
                        UiModel {
                            app,
                            screen: None,
                            scrolled: false,
                            selection: None,
                            toast: None,
                            embedded: None,
                            theme: app.theme().theme(true),
                            colors_enabled: true,
                            nerd_fonts: false,
                            now_ms: 0,
                            pointer: None,
                        },
                    );
                })
                .unwrap();
        };

        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            draw(&app, width, height);
        }
        app.begin_worktree(1);
        draw(&app, 80, 24);
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
                            selection: None,
                            toast: None,
                            embedded: None,
                            theme: app.theme().theme(true),
                            colors_enabled: true,
                            nerd_fonts: false,
                            now_ms: 0,
                            pointer: None,
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
                    None,
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

        let new_button = new_agent_button_area(area, SIDEBAR_WIDTH).unwrap();
        assert_eq!(
            click_action(&app, area, new_button.x, new_button.y),
            Some(ClickAction::Management(ManagementCommand::ChooseAgent))
        );
        let menu_button = menu_button_area(area, SIDEBAR_WIDTH).unwrap();
        assert_eq!(
            click_action(&app, area, menu_button.x, menu_button.y),
            Some(ClickAction::ToggleMenu)
        );
        let handle = resize_handle_area(&app, area).unwrap();
        assert_eq!(
            click_action(&app, area, handle.x, handle.y + 4),
            Some(ClickAction::ResizeSidebar)
        );
        app.toggle_sidebar();
        let collapsed_menu = menu_button_area(area, 0).unwrap();
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
        assert_eq!(
            click_action(&app, area, popup.right() + 4, popup.y),
            Some(ClickAction::Cancel)
        );
        assert_eq!(
            click_action(&app, area, area.width - 2, area.height - 2),
            Some(ClickAction::Cancel)
        );
        assert_eq!(click_action(&app, area, popup.x, popup.y), None);

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
            (2, NewAgentField::Location),
            (3, NewAgentField::Agent),
            (4, NewAgentField::Start),
        ] {
            assert_eq!(
                click_action(&app, area, compact.x, compact.y + line),
                Some(ClickAction::NewAgentField(field))
            );
        }
        assert_hint_clicks(&app, area, compact.x, compact.y + 6, FORM_HINTS);

        app.open_location_choices();
        for index in 0..Checkout::ALL.len() {
            assert_eq!(
                click_action(&app, area, compact.x, compact.y + 1 + index as u16),
                Some(ClickAction::Location(index))
            );
        }
        assert_hint_clicks(
            &app,
            area,
            compact.x,
            compact.y + Checkout::ALL.len() as u16 + 2,
            LOCATION_HINTS,
        );

        app.begin_worktree(1);
        assert_hint_clicks(&app, area, compact.x, compact.y + 4, CREATING_HINTS);
        app.cancel_worktree();

        app.open_workspace_choices();
        assert_eq!(
            click_action(&app, area, compact.x, compact.y + 1),
            Some(ClickAction::Workspace(0))
        );
        assert_hint_clicks(&app, area, compact.x, compact.y + 3, WORKSPACE_HINTS);

        app.open_agent_choices();
        for index in 0..app.available_harnesses().len() {
            assert_eq!(
                click_action(&app, area, compact.x, compact.y + 1 + index as u16),
                Some(ClickAction::AgentKind(index))
            );
        }
        assert_hint_clicks(
            &app,
            area,
            compact.x,
            compact.y + app.available_harnesses().len() as u16 + 2,
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
                click_action(&app, area, standard.x, standard.y + index as u16),
                Some(ClickAction::Management(binding.command))
            );
        }
        assert_hint_clicks(
            &app,
            area,
            standard.x,
            standard.y + MANAGEMENT_KEYBINDINGS.len() as u16,
            BACK_HINTS,
        );

        app.set_mode(Mode::Settings);
        assert_hint_clicks(&app, area, standard.x, standard.y + 5, SETTINGS_HINTS);
        assert_eq!(
            click_action(&app, area, standard.x + 2, standard.y + 1),
            Some(ClickAction::SettingsTab(SettingsTab::Appearance))
        );
        assert_eq!(
            click_action(&app, area, standard.x + 17, standard.y + 1),
            Some(ClickAction::SettingsTab(SettingsTab::Harnesses))
        );
        assert_eq!(
            click_action(&app, area, standard.x + 21, standard.y + 3),
            Some(ClickAction::ThemePrevious)
        );
        let next_theme = standard.x + 24 + app.theme().label().chars().count() as u16;
        assert_eq!(
            click_action(&app, area, next_theme, standard.y + 3),
            Some(ClickAction::ThemeNext)
        );

        app.set_mode(Mode::Terminal);
        app.toggle_sidebar();
        assert!(render_app_text(&app).contains("≡ Menu  ^B m"));
    }

    #[test]
    fn hover_targets_buttons_and_ignores_dismiss_regions() {
        let area = Rect::new(0, 0, 80, 24);
        let compact = dialog_inner(ModalSize::Compact, area);
        let standard = dialog_inner(ModalSize::Standard, area);
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            false,
            None,
        );

        let new_button = new_agent_button_area(area, SIDEBAR_WIDTH).unwrap();
        let menu_button = menu_button_area(area, SIDEBAR_WIDTH).unwrap();
        assert_eq!(
            hover_action(&app, area, new_button.x, new_button.y),
            Some(ClickAction::Management(ManagementCommand::ChooseAgent))
        );
        assert_eq!(
            hover_action(&app, area, menu_button.x, menu_button.y),
            Some(ClickAction::ToggleMenu)
        );
        assert_eq!(
            hover_action(&app, area, SIDEBAR_WIDTH + 1, 0),
            None,
            "empty terminal click-to-create is not a hoverable button"
        );

        app.set_mode(Mode::Menu);
        let popup = menu_popup_area(menu_button);
        assert_eq!(
            hover_action(&app, area, popup.x + 1, popup.y + 1),
            Some(ClickAction::MenuItem(MenuItem::Detach))
        );
        assert_eq!(
            hover_action(&app, area, popup.right() + 4, popup.y),
            None,
            "clicking outside the menu dismisses it but is not a button"
        );

        app.set_mode(Mode::Settings);
        assert_eq!(
            hover_action(&app, area, standard.x + 21, standard.y + 3),
            Some(ClickAction::ThemePrevious)
        );
        assert_eq!(hover_action(&app, area, compact.x + 2, compact.y + 5), None);
        assert_eq!(
            hover_action(&app, area, standard.x + 2, standard.y + 5),
            Some(ClickAction::ThemePrevious)
        );
    }

    #[test]
    fn hovered_buttons_use_the_selected_style() {
        let area = Rect::new(0, 0, 80, 24);
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Monochrome,
            false,
            None,
        );
        let menu = menu_button_area(area, SIDEBAR_WIDTH).unwrap();
        let new_button = new_agent_button_area(area, SIDEBAR_WIDTH).unwrap();

        let hovered = draw_app(&app, Some((menu.x + 1, menu.y)));
        assert!(
            hovered.backend().buffer()[(menu.x + 1, menu.y)]
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            !hovered.backend().buffer()[(new_button.x + 1, new_button.y)]
                .modifier
                .contains(Modifier::REVERSED)
        );

        let idle = draw_app(&app, None);
        assert!(
            !idle.backend().buffer()[(menu.x + 1, menu.y)]
                .modifier
                .contains(Modifier::REVERSED)
        );

        app.set_mode(Mode::Settings);
        let standard = dialog_inner(ModalSize::Standard, area);
        let hint = draw_app(&app, Some((standard.x + 2, standard.y + 5)));
        assert!(
            hint.backend().buffer()[(standard.x + 2, standard.y + 5)]
                .modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn settings_lists_every_harness_and_filters_the_new_agent_picker() {
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Monochrome,
            false,
            None,
        );
        app.set_available_harnesses(vec![AgentKind::Codex]);
        app.set_mode(Mode::Settings);
        app.select_settings_tab(SettingsTab::Harnesses);

        let settings = render_app_text(&app);
        assert!(settings.contains("Harnesses"));
        assert!(settings.contains("Codex"));
        assert!(settings.contains("✓ installed"));
        assert!(settings.contains("Claude Code"));
        assert!(settings.contains("× not found"));

        app.open_new_agent(None, None, Vec::new());
        app.open_agent_choices();
        let picker = render_app_text(&app);
        assert!(picker.contains("Codex"));
        assert!(!picker.contains("Claude Code"));

        app.set_available_harnesses(Vec::new());
        app.open_new_agent(None, None, Vec::new());
        app.open_agent_choices();
        let area = Rect::new(0, 0, 80, 24);
        let compact = dialog_inner(ModalSize::Compact, area);
        assert_eq!(
            click_action(&app, area, compact.x + 2, compact.y + 3),
            Some(ClickAction::Confirm)
        );
        assert_eq!(click_action(&app, area, compact.x + 2, compact.y + 2), None);
    }

    #[test]
    fn hovering_an_agent_thread_fills_the_whole_card() {
        let area = Rect::new(0, 0, 80, 24);
        let active = AgentSnapshot {
            id: svarm_agent::AgentId::new(1),
            kind: svarm_agent::AgentKind::Codex,
            launch_directory: PathBuf::from("/tmp/plain-directory"),
            working_directory: None,
            status: SessionStatus::Running,
            exit: None,
            output_generation: 1,
            seen_generation: 1,
            completed_generation: 0,
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            conversation_title: Some("Live thread".into()),
            conversation_id: None,
            activity: AgentActivity::Idle,
            recognition: None,
            git: None,
        };
        let app = App::hydrate(
            SvarmSessionSnapshot {
                summary: SessionSummary {
                    id: SessionId(10),
                    running_agents: 1,
                    total_agents: 1,
                    attachment: None,
                    last_user_activity_ms: 1,
                    revision: SessionRevision(1),
                },
                selected_agent_id: Some(active.id),
                rows: 24,
                cols: 80,
                agents: vec![active],
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

        assert_eq!(
            hover_action(&app, area, 2, 0),
            Some(ClickAction::SidebarItem(0))
        );
        assert_eq!(
            hover_action(&app, area, 2, 4),
            Some(ClickAction::SidebarItem(1))
        );

        let edge = sidebar_area(area, SIDEBAR_WIDTH).width.saturating_sub(2);
        let hovered_card = draw_app(&app, Some((2, 0)));
        for row in 0..3 {
            assert!(
                hovered_card.backend().buffer()[(edge, row)]
                    .modifier
                    .contains(Modifier::REVERSED),
                "active card row {row} should fill to the sidebar edge"
            );
        }
        assert!(
            !hovered_card.backend().buffer()[(edge, 4)]
                .modifier
                .contains(Modifier::REVERSED),
            "archived row should stay unfilled while another card is hovered"
        );

        let hovered_archived = draw_app(&app, Some((2, 4)));
        assert!(
            hovered_archived.backend().buffer()[(edge, 4)]
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            !hovered_archived.backend().buffer()[(edge, 3)]
                .modifier
                .contains(Modifier::REVERSED),
            "the Archived header is not a card"
        );
    }

    #[test]
    fn archive_button_is_a_distinct_hit_region_on_each_active_card() {
        let area = Rect::new(0, 0, 80, 24);
        let agent = |id: u64, title: &str| AgentSnapshot {
            id: svarm_agent::AgentId::new(id),
            kind: svarm_agent::AgentKind::Codex,
            launch_directory: PathBuf::from("/tmp/plain-directory"),
            working_directory: None,
            status: SessionStatus::Running,
            exit: None,
            output_generation: 1,
            seen_generation: 1,
            completed_generation: 0,
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            conversation_title: Some(title.into()),
            conversation_id: None,
            activity: AgentActivity::Idle,
            recognition: None,
            git: None,
        };
        let app = App::hydrate(
            SvarmSessionSnapshot {
                summary: SessionSummary {
                    id: SessionId(11),
                    running_agents: 2,
                    total_agents: 2,
                    attachment: None,
                    last_user_activity_ms: 1,
                    revision: SessionRevision(1),
                },
                selected_agent_id: Some(svarm_agent::AgentId::new(1)),
                rows: 24,
                cols: 80,
                agents: vec![agent(1, "First"), agent(2, "Second")],
                archived: Vec::new(),
            },
            crate::theme::ThemeName::Monochrome,
            None,
        );

        let agents_area = agent_list_area(&app, sidebar_area(area, SIDEBAR_WIDTH), SIDEBAR_WIDTH);
        let button_column = agents_area.right() - 1;

        assert_eq!(
            click_action(&app, area, button_column, agents_area.y),
            Some(ClickAction::ArchiveCard(0))
        );
        assert_eq!(
            click_action(&app, area, button_column, agents_area.y + AGENT_CARD_HEIGHT),
            Some(ClickAction::ArchiveCard(1))
        );
        // The rest of the title line still selects the card.
        assert_eq!(
            click_action(&app, area, agents_area.x + 2, agents_area.y),
            Some(ClickAction::SidebarItem(0))
        );
        // The button only appears on a card's first line.
        assert_eq!(
            click_action(&app, area, button_column, agents_area.y + 1),
            Some(ClickAction::SidebarItem(0))
        );

        let hovered = draw_app(&app, Some((button_column, agents_area.y)));
        assert!(
            hovered.backend().buffer()[(button_column, agents_area.y)]
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            !hovered.backend().buffer()[(agents_area.x + 2, agents_area.y)]
                .modifier
                .contains(Modifier::REVERSED),
            "hovering just the archive button should not fill the whole card"
        );
    }

    #[test]
    fn nerd_fonts_swap_icons_with_a_plain_unicode_default() {
        let active = AgentSnapshot {
            id: svarm_agent::AgentId::new(1),
            kind: svarm_agent::AgentKind::Codex,
            launch_directory: PathBuf::from("/tmp/plain-directory"),
            working_directory: None,
            status: SessionStatus::Running,
            exit: None,
            output_generation: 1,
            seen_generation: 1,
            completed_generation: 0,
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            conversation_title: Some("Live thread".into()),
            conversation_id: None,
            activity: AgentActivity::Idle,
            recognition: None,
            git: Some(GitContext {
                branch: "main".into(),
                worktree: "/tmp/plain-directory".into(),
                linked: true,
                additions: 0,
                deletions: 0,
                ahead: None,
                behind: None,
            }),
        };
        let app = App::hydrate(
            SvarmSessionSnapshot {
                summary: SessionSummary {
                    id: SessionId(12),
                    running_agents: 1,
                    total_agents: 1,
                    attachment: None,
                    last_user_activity_ms: 1,
                    revision: SessionRevision(1),
                },
                selected_agent_id: Some(active.id),
                rows: 24,
                cols: 80,
                agents: vec![active],
                archived: Vec::new(),
            },
            crate::theme::ThemeName::Monochrome,
            None,
        );

        let plain = render_app_text_with(&app, false);
        assert!(plain.contains(ARCHIVE_BUTTON_TEXT));
        assert!(plain.contains(LINKED_WORKTREE));
        assert!(!plain.contains(ARCHIVE_BUTTON_TEXT_NERD_FONT));
        assert!(!plain.contains(LINKED_WORKTREE_NERD_FONT));

        let nerd = render_app_text_with(&app, true);
        assert!(nerd.contains(ARCHIVE_BUTTON_TEXT_NERD_FONT));
        assert!(nerd.contains(LINKED_WORKTREE_NERD_FONT));
        assert!(!nerd.contains(ARCHIVE_BUTTON_TEXT));
        assert!(!nerd.contains(LINKED_WORKTREE));
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
        assert!(keybinds.contains("detach"));
        assert!(keybinds.contains("stop session"));

        app.set_mode(Mode::Menu);
        let menu = render_app_text(&app);
        assert!(menu.contains("Detach"));
        assert!(menu.contains("Stop session"));
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
            working_directory: None,
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
            working_directory: None,
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
                linked: false,
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
        assert!(rendered.contains("● 1 · Unnamed conversa…"));
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
                summary: summary.clone(),
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

        // An agent that moved into a linked worktree reports where it is, marked as linked, not
        // the directory it was launched in.
        let mut moved = agent(2, SessionStatus::Running, 2, 1);
        moved.launch_directory = PathBuf::from("/tmp/project-eight");
        moved.working_directory = Some(PathBuf::from("/tmp/worktrees/review-fix"));
        let git = moved.git.as_mut().unwrap();
        git.worktree = "/tmp/worktrees/review-fix".into();
        git.branch = "fixup".into();
        git.linked = true;
        let moved_rendered = render_app_text(&App::hydrate(
            SvarmSessionSnapshot {
                summary: summary.clone(),
                selected_agent_id: Some(moved.id),
                rows: 24,
                cols: 80,
                agents: vec![moved],
                archived: Vec::new(),
            },
            crate::theme::ThemeName::Monochrome,
            None,
        ));
        assert!(
            moved_rendered.contains("Codex · ⑂review-fix"),
            "{moved_rendered}"
        );
        assert!(moved_rendered.contains("fixup +557 -300 ↑2 ↓4"));
        assert!(!moved_rendered.contains("project-eight"));

        // Without git the live directory still describes the agent better than its launch one.
        let mut wandered = agent(2, SessionStatus::Running, 2, 1);
        wandered.working_directory = Some(PathBuf::from("/tmp/scratch-notes"));
        wandered.git = None;
        let wandered_rendered = render_app_text(&App::hydrate(
            SvarmSessionSnapshot {
                summary: summary.clone(),
                selected_agent_id: Some(wandered.id),
                rows: 24,
                cols: 80,
                agents: vec![wandered],
                archived: Vec::new(),
            },
            crate::theme::ThemeName::Monochrome,
            None,
        ));
        assert!(
            wandered_rendered.contains("Codex · scratch-notes"),
            "{wandered_rendered}"
        );

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
                        selection: None,
                        toast: None,
                        embedded: None,
                        theme,
                        colors_enabled: true,
                        nerd_fonts: false,
                        now_ms: 0,
                        pointer: None,
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
            working_directory: None,
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
        assert_eq!(agent_item_at(&app, Rect::new(0, 0, 80, 24), 2, 3), None);
        assert_eq!(agent_item_at(&app, Rect::new(0, 0, 80, 24), 2, 4), Some(1));
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
                working_directory: None,
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

        app.scroll_sidebar(-1, agent_list_page_size(&app, area), 3);
        assert_eq!(agent_item_at(&app, area, 2, 1), Some(0));
        let rendered = render_app_text(&app);
        assert!(!rendered.contains("8 · Conversation 8"));

        // Scrolling all the way up brings the first card fully into view.
        app.scroll_sidebar(-8, agent_list_page_size(&app, area), 3);
        let rendered = render_app_text(&app);
        assert!(rendered.contains("1 · Conversation 1"));
        assert_eq!(agent_item_at(&app, area, 2, 0), Some(0));
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
                        selection: None,
                        toast: None,
                        embedded: None,
                        theme: app.theme().theme(true),
                        colors_enabled: true,
                        nerd_fonts: false,
                        now_ms: 0,
                        pointer: None,
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
        let app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            false,
            None,
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        terminal
            .draw(|frame| {
                render_terminal(
                    frame,
                    UiModel {
                        app: &app,
                        screen: Some(&screen),
                        scrolled: true,
                        selection: None,
                        toast: None,
                        embedded: None,
                        theme: app.theme().theme(true),
                        colors_enabled: true,
                        nerd_fonts: false,
                        now_ms: 0,
                        pointer: None,
                    },
                    frame.area(),
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
                        selection: None,
                        toast: None,
                        embedded: None,
                        theme: app.theme().theme(true),
                        colors_enabled: true,
                        nerd_fonts: false,
                        now_ms: 0,
                        pointer: None,
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
                        selection: None,
                        toast: None,
                        embedded: Some(&snapshot),
                        theme: app.theme().theme(true),
                        colors_enabled: true,
                        nerd_fonts: false,
                        now_ms: 0,
                        pointer: None,
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

    fn draw_app(app: &App, pointer: Option<(u16, u16)>) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app,
                        screen: None,
                        scrolled: false,
                        selection: None,
                        toast: None,
                        embedded: None,
                        theme: app.theme().theme(false),
                        colors_enabled: false,
                        nerd_fonts: false,
                        now_ms: 0,
                        pointer,
                    },
                )
            })
            .unwrap();
        terminal
    }

    #[test]
    fn copied_toast_renders_in_the_top_right_corner_at_80x24() {
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
                        selection: None,
                        toast: Some("Copied 17 characters to clipboard"),
                        embedded: None,
                        theme: app.theme().theme(false),
                        colors_enabled: false,
                        nerd_fonts: false,
                        now_ms: 0,
                        pointer: None,
                    },
                )
            })
            .unwrap();

        let top = terminal.backend().buffer().content()[0..80]
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(top.ends_with("┐"));
        let second = terminal.backend().buffer().content()[80..160]
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(second.contains("Copied 17 characters to clipboard"));
    }

    fn render_app_text(app: &App) -> String {
        render_app_text_with(app, false)
    }

    fn render_app_text_with(app: &App, nerd_fonts: bool) -> String {
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
                        selection: None,
                        toast: None,
                        embedded: None,
                        theme: app.theme().theme(false),
                        colors_enabled: false,
                        nerd_fonts,
                        pointer: None,
                        now_ms: 0,
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
