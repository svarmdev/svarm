use std::path::{Path, PathBuf};

use portable_pty::PtySize;

use crate::{
    AgentKind, Mode,
    session::{AgentSession, Result, SessionStatus, TerminalPalette},
    settings::{Settings, settings_path},
    theme::ThemeName,
};

pub struct AgentEntry {
    pub session: AgentSession,
    seen_generation: u64,
}

impl AgentEntry {
    pub fn has_unseen_output(&self) -> bool {
        self.session.generation() > self.seen_generation
    }

    fn mark_seen(&mut self) {
        self.seen_generation = self.session.generation();
    }
}

pub struct App {
    pub agents: Vec<AgentEntry>,
    pub selected: usize,
    pub mode: Mode,
    pub sidebar_visible: bool,
    pub menu_selected: usize,
    pub theme: ThemeName,
    pub notice: Option<String>,
    pub quit: bool,
    pub cwd: PathBuf,
    next_id: u64,
    pty_size: PtySize,
    terminal_palette: Option<TerminalPalette>,
    settings_path: Option<PathBuf>,
}

impl App {
    pub fn new(
        kind: Option<AgentKind>,
        cwd: PathBuf,
        pty_size: PtySize,
        terminal_palette: Option<TerminalPalette>,
    ) -> Result<Self> {
        let settings_path = settings_path();
        let (theme, settings_error) = match settings_path.as_deref() {
            Some(path) => match Settings::load(path) {
                Ok(settings) => (settings.theme, None),
                Err(error) => (
                    ThemeName::default(),
                    Some(format!("could not load {}: {error}", path.display())),
                ),
            },
            None => (ThemeName::default(), None),
        };
        let mut app = Self {
            agents: Vec::new(),
            selected: 0,
            mode: if kind.is_some() {
                Mode::Terminal
            } else {
                Mode::ChooseAgent
            },
            sidebar_visible: true,
            menu_selected: 0,
            theme,
            notice: None,
            quit: false,
            cwd,
            next_id: 1,
            pty_size,
            terminal_palette,
            settings_path,
        };
        if let Some(kind) = kind {
            app.spawn(kind)?;
        }
        app.notice = settings_error;
        Ok(app)
    }

    pub fn cycle_theme(&mut self, delta: isize) {
        self.theme.cycle(delta);
        let Some(path) = &self.settings_path else {
            self.notice = Some("could not save settings: HOME is not set".into());
            return;
        };
        if let Err(error) = (Settings { theme: self.theme }).save(path) {
            self.notice = Some(format!("could not save {}: {error}", path.display()));
        } else {
            self.notice = None;
        }
    }

    pub fn spawn(&mut self, kind: AgentKind) -> Result<()> {
        let session = AgentSession::spawn(
            self.next_id,
            kind,
            &self.cwd,
            self.pty_size,
            self.terminal_palette,
        )?;
        self.next_id += 1;
        self.agents.push(AgentEntry {
            session,
            seen_generation: 0,
        });
        self.selected = self.agents.len() - 1;
        self.mode = Mode::Terminal;
        self.notice = None;
        Ok(())
    }

    pub fn current(&self) -> Option<&AgentEntry> {
        self.agents.get(self.selected)
    }

    pub fn current_mut(&mut self) -> Option<&mut AgentEntry> {
        self.agents.get_mut(self.selected)
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

    pub fn select_next_menu_item(&mut self) {
        self.menu_selected = (self.menu_selected + 1) % 2;
    }

    pub fn select_previous_menu_item(&mut self) {
        self.menu_selected = (self.menu_selected + 1) % 2;
    }

    pub fn close_selected(&mut self) -> Result<()> {
        if self.agents.is_empty() {
            self.mode = Mode::Terminal;
            return Ok(());
        }
        self.agents[self.selected].session.stop()?;
        self.agents.remove(self.selected);
        self.selected = self.selected.min(self.agents.len().saturating_sub(1));
        self.mode = Mode::Terminal;
        self.mark_selected_seen();
        Ok(())
    }

    pub fn mark_selected_seen(&mut self) {
        if let Some(agent) = self.agents.get_mut(self.selected) {
            agent.mark_seen();
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let size = pty_size(rows, cols);
        if size == self.pty_size {
            return Ok(());
        }
        self.pty_size = size;
        for agent in &self.agents {
            agent.session.resize(size.rows, size.cols)?;
        }
        Ok(())
    }

    pub fn poll(&mut self) -> bool {
        let before = self.output_stamp();
        let notice = self.notice.clone();
        for agent in &mut self.agents {
            if let Err(error) = agent.session.poll_status() {
                self.notice = Some(error.to_string());
            } else if let Some(error) = agent.session.read_error() {
                self.notice = Some(error);
            }
        }
        before != self.output_stamp() || notice != self.notice
    }

    pub fn output_stamp(&self) -> Vec<(u64, SessionStatus)> {
        self.agents
            .iter()
            .map(|agent| (agent.session.generation(), agent.session.status()))
            .collect()
    }

    pub fn workspace_name(&self) -> &str {
        self.cwd
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| path_label(&self.cwd))
    }
}

pub fn pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn path_label(path: &Path) -> &str {
    path.to_str().unwrap_or("workspace")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_never_has_a_zero_dimension() {
        assert_eq!(pty_size(0, 0).rows, 1);
        assert_eq!(pty_size(0, 0).cols, 1);
    }
}
