//! Backend-independent terminal state shared by the runtime, wire protocol, and TUI.

use serde::{Deserialize, Serialize};

use crate::CursorStyle;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

impl TerminalSize {
    pub const fn new(rows: u16, cols: u16) -> Self {
        Self { rows, cols }
    }

    pub const fn cell_count(self) -> usize {
        self.rows as usize * self.cols as usize
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalPosition {
    pub row: u16,
    pub column: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TerminalColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb([u8; 3]),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalAttributes {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub blink: bool,
    pub hidden: bool,
    pub strikethrough: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalCell {
    pub contents: String,
    pub foreground: TerminalColor,
    pub background: TerminalColor,
    pub attributes: TerminalAttributes,
    pub wide: bool,
    pub wide_continuation: bool,
    /// One-based index into [`TerminalState::hyperlinks`]; zero is never used.
    pub hyperlink: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalCursor {
    pub position: TerminalPosition,
    pub visible: bool,
    pub style: CursorStyle,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalScrollback {
    pub position: usize,
    pub retained_rows: usize,
    pub capacity: usize,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalModes {
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    /// The child requested xterm alternate-scroll mode (DEC private mode 1007).
    pub mouse_alternate_scroll: bool,
    /// The child enabled the kitty keyboard protocol's disambiguate-escape-codes mode.
    pub keyboard_disambiguate: bool,
    pub mouse_protocol: MouseProtocol,
    pub mouse_encoding: MouseEncoding,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseProtocol {
    #[default]
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalProgressState {
    Hidden,
    Normal,
    Error,
    Indeterminate,
    Warning,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalProgress {
    pub state: TerminalProgressState,
    pub value: u8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalClipboardRequest {
    pub selector: Vec<u8>,
    /// The OSC 52 base64 payload, retained verbatim for forwarding to the host terminal.
    pub base64_payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalBells {
    pub audible: u64,
    pub visual: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalState {
    pub size: TerminalSize,
    pub cursor: TerminalCursor,
    pub alternate_screen: bool,
    pub scrollback: TerminalScrollback,
    pub modes: TerminalModes,
    pub title: String,
    pub working_directory: Option<String>,
    /// OSC 8 URIs referenced by cells, addressed by one-based cell hyperlink indices.
    pub hyperlinks: Vec<String>,
    pub progress: Option<TerminalProgress>,
    pub clipboard_request: Option<TerminalClipboardRequest>,
    pub bells: TerminalBells,
    pub shell_command: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalSnapshot {
    pub state: TerminalState,
    pub cells: Vec<TerminalCell>,
    pub wrapped_rows: Vec<bool>,
}

impl TerminalSnapshot {
    pub fn blank(size: TerminalSize) -> Self {
        Self {
            state: TerminalState {
                size,
                cursor: TerminalCursor {
                    visible: true,
                    ..TerminalCursor::default()
                },
                ..TerminalState::default()
            },
            cells: vec![TerminalCell::default(); size.cell_count()],
            wrapped_rows: vec![false; usize::from(size.rows)],
        }
    }

    pub fn validate(&self) -> Result<(), TerminalModelError> {
        if self.cells.len() != self.state.size.cell_count() {
            return Err(TerminalModelError::CellCount);
        }
        if self.wrapped_rows.len() != usize::from(self.state.size.rows) {
            return Err(TerminalModelError::WrappedRowCount);
        }
        if self.cells.iter().any(|cell| {
            cell.hyperlink
                .is_some_and(|id| id == 0 || id as usize > self.state.hyperlinks.len())
        }) {
            return Err(TerminalModelError::Hyperlink);
        }
        Ok(())
    }

    pub const fn size(&self) -> TerminalSize {
        self.state.size
    }

    pub fn cell(&self, row: u16, column: u16) -> Option<&TerminalCell> {
        let size = self.size();
        if row >= size.rows || column >= size.cols {
            return None;
        }
        self.cells
            .get(usize::from(row) * usize::from(size.cols) + usize::from(column))
    }

    pub fn cell_mut(&mut self, row: u16, column: u16) -> Option<&mut TerminalCell> {
        let size = self.size();
        if row >= size.rows || column >= size.cols {
            return None;
        }
        self.cells
            .get_mut(usize::from(row) * usize::from(size.cols) + usize::from(column))
    }

    pub fn rows(&self) -> impl Iterator<Item = String> + '_ {
        let cols = usize::from(self.size().cols);
        self.cells.chunks(cols.max(1)).map(|row| {
            row.iter()
                .filter(|cell| !cell.wide_continuation)
                .map(|cell| cell.contents.as_str())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
    }

    pub fn contents(&self) -> String {
        self.rows().collect::<Vec<_>>().join("\n")
    }

    pub fn diff(&self, next: &Self) -> Option<TerminalSnapshotDiff> {
        (self.size() == next.size()).then(|| TerminalSnapshotDiff {
            state: next.state.clone(),
            cells: self
                .cells
                .iter()
                .zip(&next.cells)
                .enumerate()
                .filter(|(_, (before, after))| before != after)
                .map(|(index, (_, cell))| TerminalCellPatch {
                    index: index as u32,
                    cell: cell.clone(),
                })
                .collect(),
            wrapped_rows: self
                .wrapped_rows
                .iter()
                .zip(&next.wrapped_rows)
                .enumerate()
                .filter(|(_, (before, after))| before != after)
                .map(|(row, (_, wrapped))| TerminalRowPatch {
                    row: row as u16,
                    wrapped: *wrapped,
                })
                .collect(),
        })
    }

    pub fn apply(&mut self, diff: &TerminalSnapshotDiff) -> Result<(), TerminalModelError> {
        *self = self.applied(diff)?;
        Ok(())
    }

    pub fn applied(&self, diff: &TerminalSnapshotDiff) -> Result<Self, TerminalModelError> {
        let mut next = self.clone();
        next.apply_in_place(diff)?;
        Ok(next)
    }

    fn apply_in_place(&mut self, diff: &TerminalSnapshotDiff) -> Result<(), TerminalModelError> {
        if self.size() != diff.state.size {
            return Err(TerminalModelError::Size);
        }
        if diff
            .cells
            .iter()
            .any(|patch| patch.index as usize >= self.cells.len())
        {
            return Err(TerminalModelError::CellIndex);
        }
        if diff
            .wrapped_rows
            .iter()
            .any(|patch| patch.row >= self.size().rows)
        {
            return Err(TerminalModelError::RowIndex);
        }

        for patch in &diff.cells {
            self.cells[patch.index as usize] = patch.cell.clone();
        }
        for patch in &diff.wrapped_rows {
            self.wrapped_rows[usize::from(patch.row)] = patch.wrapped;
        }
        self.state = diff.state.clone();
        self.validate()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalSnapshotDiff {
    pub state: TerminalState,
    pub cells: Vec<TerminalCellPatch>,
    pub wrapped_rows: Vec<TerminalRowPatch>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalCellPatch {
    pub index: u32,
    pub cell: TerminalCell,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalRowPatch {
    pub row: u16,
    pub wrapped: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalModelError {
    Size,
    CellCount,
    WrappedRowCount,
    CellIndex,
    RowIndex,
    Hyperlink,
}

/// A terminal emulator exposes its visible state through this narrow semantic boundary.
pub trait TerminalBackend: Send {
    fn process(&mut self, bytes: &[u8]);
    fn resize(&mut self, size: TerminalSize);
    fn cursor_position(&self) -> TerminalPosition;
    fn modes(&self, keyboard_disambiguate: bool, mouse_alternate_scroll: bool) -> TerminalModes;
    fn snapshot(&self, cursor_style: CursorStyle, modes: TerminalModes) -> TerminalSnapshot;
    fn viewport(
        &mut self,
        scrollback: usize,
        cursor_style: CursorStyle,
        modes: TerminalModes,
    ) -> TerminalSnapshot;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_snapshot_is_self_contained_and_unicode_safe() {
        let mut snapshot = TerminalSnapshot::blank(TerminalSize::new(1, 3));
        snapshot.cell_mut(0, 0).unwrap().contents = "é".into();
        snapshot.cell_mut(0, 1).unwrap().contents = "界".into();
        snapshot.cell_mut(0, 1).unwrap().wide = true;
        snapshot.cell_mut(0, 2).unwrap().wide_continuation = true;

        let encoded = serde_json::to_vec(&snapshot).unwrap();
        let decoded: TerminalSnapshot = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, snapshot);
        assert_eq!(decoded.rows().collect::<Vec<_>>(), ["é界"]);
        assert_eq!(decoded.validate(), Ok(()));
    }

    #[test]
    fn diff_contains_only_changes_and_reconstructs_the_next_snapshot() {
        let before = TerminalSnapshot::blank(TerminalSize::new(2, 4));
        let mut after = before.clone();
        after.cell_mut(1, 2).unwrap().contents = "x".into();
        after.state.cursor.position = TerminalPosition { row: 1, column: 3 };
        after.state.modes.bracketed_paste = true;

        let diff = before.diff(&after).unwrap();
        assert_eq!(diff.cells.len(), 1);
        let mut applied = before;
        applied.apply(&diff).unwrap();
        assert_eq!(applied, after);
    }

    #[test]
    fn invalid_or_wrong_sized_diffs_are_rejected_without_panicking() {
        let mut snapshot = TerminalSnapshot::blank(TerminalSize::new(1, 1));
        let mut diff = snapshot.diff(&snapshot).unwrap();
        diff.cells.push(TerminalCellPatch {
            index: 5,
            cell: TerminalCell::default(),
        });
        let original = snapshot.clone();
        assert_eq!(snapshot.apply(&diff), Err(TerminalModelError::CellIndex));
        assert_eq!(snapshot, original);

        diff.cells.clear();
        diff.state.size = TerminalSize::new(2, 1);
        assert_eq!(snapshot.apply(&diff), Err(TerminalModelError::Size));
    }

    #[test]
    fn a_small_interactive_change_is_smaller_than_a_full_frame() {
        let before = TerminalSnapshot::blank(TerminalSize::new(24, 80));
        let mut after = before.clone();
        after.cell_mut(20, 10).unwrap().contents = "x".into();
        let diff = before.diff(&after).unwrap();

        assert_eq!(diff.cells.len(), 1);
        assert!(
            serde_json::to_vec(&diff).unwrap().len()
                < serde_json::to_vec(&after).unwrap().len() / 4
        );
    }
}
