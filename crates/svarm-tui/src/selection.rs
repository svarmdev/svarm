use std::collections::BTreeMap;

use svarm_agent::{
    AgentId,
    protocol::MouseInput,
    terminal_model::{TerminalCell, TerminalSnapshot},
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct CellPosition {
    row: i64,
    column: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VisibleSelection {
    start: CellPosition,
    end: CellPosition,
    scrollback: usize,
}

impl VisibleSelection {
    pub(crate) fn contains(self, row: u16, column: u16) -> bool {
        let position = CellPosition {
            row: i64::from(row) - self.scrollback as i64,
            column,
        };
        position >= self.start && position <= self.end
    }
}

#[derive(Clone, Debug)]
struct CapturedRow {
    cells: Vec<TerminalCell>,
    wrapped: bool,
}

#[derive(Clone, Debug)]
enum Phase {
    Pending {
        down: MouseInput,
        forward_to_child: bool,
    },
    Dragging,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScrollDirection {
    Older,
    Newer,
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalSelection {
    agent_id: AgentId,
    anchor: CellPosition,
    head: CellPosition,
    pointer_column: u16,
    pointer_row: u16,
    phase: Phase,
    rows: BTreeMap<i64, CapturedRow>,
    viewport_rows: u16,
    viewport_columns: u16,
    scrollback: usize,
    retained_rows: usize,
}

impl TerminalSelection {
    pub(crate) fn begin(
        agent_id: AgentId,
        column: u16,
        row: u16,
        down: MouseInput,
        forward_to_child: bool,
        screen: &TerminalSnapshot,
    ) -> Self {
        let mut selection = Self {
            agent_id,
            anchor: CellPosition { row: 0, column: 0 },
            head: CellPosition { row: 0, column: 0 },
            pointer_column: column,
            pointer_row: row,
            phase: Phase::Pending {
                down,
                forward_to_child,
            },
            rows: BTreeMap::new(),
            viewport_rows: screen.size().rows,
            viewport_columns: screen.size().cols,
            scrollback: screen.state.scrollback.position,
            retained_rows: screen.state.scrollback.retained_rows,
        };
        selection.absorb(screen);
        let position = selection.position(column, row, screen);
        selection.anchor = position;
        selection.head = position;
        selection
    }

    pub(crate) const fn agent_id(&self) -> AgentId {
        self.agent_id
    }

    pub(crate) fn is_dragging(&self) -> bool {
        matches!(self.phase, Phase::Dragging)
    }

    pub(crate) fn drag(&mut self, column: u16, row: u16, screen: &TerminalSnapshot) {
        self.phase = Phase::Dragging;
        self.pointer_column = column.min(screen.size().cols.saturating_sub(1));
        self.pointer_row = row.min(screen.size().rows.saturating_sub(1));
        self.absorb(screen);
        self.head = self.position(self.pointer_column, self.pointer_row, screen);
    }

    pub(crate) fn absorb(&mut self, screen: &TerminalSnapshot) {
        self.viewport_rows = screen.size().rows;
        self.viewport_columns = screen.size().cols;
        self.scrollback = screen.state.scrollback.position;
        self.retained_rows = screen.state.scrollback.retained_rows;
        let columns = usize::from(self.viewport_columns);
        for row in 0..self.viewport_rows {
            let start = usize::from(row) * columns;
            let end = start + columns;
            let global_row = i64::from(row) - self.scrollback as i64;
            self.rows.insert(
                global_row,
                CapturedRow {
                    cells: screen.cells[start..end].to_vec(),
                    wrapped: screen.wrapped_rows[usize::from(row)],
                },
            );
        }
        if self.is_dragging() {
            self.head = self.position(self.pointer_column, self.pointer_row, screen);
        }
    }

    pub(crate) fn visible(&self, screen: &TerminalSnapshot) -> Option<VisibleSelection> {
        self.is_dragging().then(|| {
            let (start, end) = ordered(self.anchor, self.head);
            VisibleSelection {
                start,
                end,
                scrollback: screen.state.scrollback.position,
            }
        })
    }

    pub(crate) fn scroll_direction(&self) -> Option<ScrollDirection> {
        if !self.is_dragging() || self.viewport_rows == 0 {
            return None;
        }
        if self.pointer_row == 0 && self.scrollback < self.retained_rows {
            Some(ScrollDirection::Older)
        } else if self.pointer_row >= self.viewport_rows.saturating_sub(1) && self.scrollback > 0 {
            Some(ScrollDirection::Newer)
        } else {
            None
        }
    }

    pub(crate) fn pending_click(&self) -> Option<(&MouseInput, bool)> {
        match &self.phase {
            Phase::Pending {
                down,
                forward_to_child,
            } => Some((down, *forward_to_child)),
            Phase::Dragging => None,
        }
    }

    pub(crate) fn text(&self) -> Option<String> {
        if !self.is_dragging() {
            return None;
        }
        let (start, end) = ordered(self.anchor, self.head);
        let mut output = String::new();
        for row_index in start.row..=end.row {
            let row = self.rows.get(&row_index)?;
            let first = if row_index == start.row {
                usize::from(start.column)
            } else {
                0
            };
            let last = if row_index == end.row {
                usize::from(end.column)
            } else {
                row.cells.len().saturating_sub(1)
            };
            let mut line = String::new();
            let mut meaningful_bytes = 0;
            for cell in row.cells.get(first..=last)? {
                if cell.wide_continuation {
                    continue;
                }
                cell.contents.with_str(|contents| {
                    if contents.is_empty() {
                        line.push(' ');
                    } else {
                        line.push_str(contents);
                        meaningful_bytes = line.len();
                    }
                });
            }
            line.truncate(meaningful_bytes);
            output.push_str(&line);
            if row_index != end.row && !row.wrapped {
                output.push('\n');
            }
        }
        Some(output)
    }

    fn position(&self, column: u16, row: u16, screen: &TerminalSnapshot) -> CellPosition {
        let row = row.min(screen.size().rows.saturating_sub(1));
        let mut column = column.min(screen.size().cols.saturating_sub(1));
        if screen
            .cell(row, column)
            .is_some_and(|cell| cell.wide_continuation)
        {
            column = column.saturating_sub(1);
        }
        CellPosition {
            row: i64::from(row) - screen.state.scrollback.position as i64,
            column,
        }
    }
}

fn ordered(first: CellPosition, second: CellPosition) -> (CellPosition, CellPosition) {
    if first <= second {
        (first, second)
    } else {
        (second, first)
    }
}

#[cfg(test)]
mod tests {
    use svarm_agent::{
        protocol::{InputModifiers, MouseButton, MouseKind},
        terminal_model::TerminalSize,
    };

    use super::*;

    fn mouse(column: u16, row: u16) -> MouseInput {
        MouseInput {
            kind: MouseKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: InputModifiers::default(),
        }
    }

    fn screen(rows: &[&str], wrapped: &[bool], scrollback: usize) -> TerminalSnapshot {
        let columns = rows
            .iter()
            .map(|row| row.chars().count())
            .max()
            .unwrap_or(1) as u16;
        let mut screen = TerminalSnapshot::blank(TerminalSize::new(rows.len() as u16, columns));
        screen.state.scrollback.position = scrollback;
        screen.state.scrollback.retained_rows = 20;
        for (row, text) in rows.iter().enumerate() {
            for (column, character) in text.chars().enumerate() {
                screen.cell_mut(row as u16, column as u16).unwrap().contents =
                    character.to_string().into();
            }
        }
        screen.wrapped_rows.copy_from_slice(wrapped);
        screen
    }

    #[test]
    fn copies_forward_and_backward_across_physical_lines() {
        let screen = screen(&["alpha", "bet"], &[false, false], 0);
        let mut selection =
            TerminalSelection::begin(AgentId::new(1), 1, 0, mouse(1, 0), false, &screen);
        selection.drag(2, 1, &screen);
        assert_eq!(selection.text().as_deref(), Some("lpha\nbet"));

        let mut reverse =
            TerminalSelection::begin(AgentId::new(1), 2, 1, mouse(2, 1), false, &screen);
        reverse.drag(1, 0, &screen);
        assert_eq!(reverse.text().as_deref(), Some("lpha\nbet"));
    }

    #[test]
    fn joins_wrapped_rows_and_preserves_graphemes_and_wide_cells() {
        let mut screen = screen(&["ab  ", "cd  "], &[true, false], 0);
        screen.cell_mut(0, 1).unwrap().contents = "e\u{301}".into();
        screen.cell_mut(0, 2).unwrap().contents = "界".into();
        screen.cell_mut(0, 2).unwrap().wide = true;
        screen.cell_mut(0, 3).unwrap().wide_continuation = true;
        let mut selection =
            TerminalSelection::begin(AgentId::new(1), 0, 0, mouse(0, 0), false, &screen);
        selection.drag(1, 1, &screen);
        assert_eq!(selection.text().as_deref(), Some("ae\u{301}界cd"));
    }

    #[test]
    fn trims_blank_padding_but_preserves_spaces_written_by_the_child() {
        let written_space = screen(&["a "], &[false], 0);
        let mut selection =
            TerminalSelection::begin(AgentId::new(1), 0, 0, mouse(0, 0), false, &written_space);
        selection.drag(1, 0, &written_space);
        assert_eq!(selection.text().as_deref(), Some("a "));

        let padded = screen(&["a", "bb"], &[false, false], 0);
        let mut selection =
            TerminalSelection::begin(AgentId::new(1), 0, 0, mouse(0, 0), false, &padded);
        selection.drag(1, 0, &padded);
        assert_eq!(selection.text().as_deref(), Some("a"));
    }

    #[test]
    fn combines_overlapping_scrollback_viewports_with_stable_rows() {
        let live = screen(&["four!", "five!", "six!!"], &[false; 3], 0);
        let mut selection =
            TerminalSelection::begin(AgentId::new(1), 4, 1, mouse(4, 1), false, &live);
        selection.drag(0, 0, &live);

        let older = screen(&["two!!", "three", "four!"], &[false; 3], 2);
        selection.absorb(&older);
        assert_eq!(
            selection.text().as_deref(),
            Some("two!!\nthree\nfour!\nfive!")
        );
        assert_eq!(selection.scroll_direction(), Some(ScrollDirection::Older));
    }

    #[test]
    fn scroll_direction_follows_the_pointer_at_either_viewport_edge() {
        let live = screen(&["one!!", "two!!", "three"], &[false; 3], 0);
        let mut older = TerminalSelection::begin(AgentId::new(1), 0, 1, mouse(0, 1), false, &live);
        older.drag(0, 0, &live);
        assert_eq!(older.scroll_direction(), Some(ScrollDirection::Older));

        older.drag(2, 1, &live);
        assert_eq!(older.scroll_direction(), None);

        let history = screen(&["one!!", "two!!", "three"], &[false; 3], 4);
        let mut newer =
            TerminalSelection::begin(AgentId::new(1), 0, 1, mouse(0, 1), false, &history);
        newer.drag(0, 2, &history);
        assert_eq!(newer.scroll_direction(), Some(ScrollDirection::Newer));

        newer.drag(0, 1, &history);
        assert_eq!(newer.scroll_direction(), None);
    }

    #[test]
    fn pending_click_is_not_a_visible_or_copyable_selection() {
        let screen = screen(&["text"], &[false], 0);
        let selection = TerminalSelection::begin(AgentId::new(1), 0, 0, mouse(0, 0), true, &screen);
        assert!(selection.visible(&screen).is_none());
        assert!(selection.text().is_none());
        assert!(
            selection
                .pending_click()
                .is_some_and(|(_, forward)| forward)
        );
    }
}
