//! Backend-independent terminal state shared by the runtime, wire protocol, and TUI.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};

use crate::CursorStyle;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct TerminalPosition {
    pub row: u16,
    pub column: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TerminalColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb([u8; 3]),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum TerminalText {
    #[default]
    Empty,
    Scalar(char),
    Grapheme(Box<String>),
}

impl TerminalText {
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    pub fn with_str<T>(&self, read: impl FnOnce(&str) -> T) -> T {
        match self {
            Self::Empty => read(""),
            Self::Scalar(character) => {
                let mut bytes = [0; 4];
                read(character.encode_utf8(&mut bytes))
            }
            Self::Grapheme(text) => read(text),
        }
    }

    fn push_to(&self, output: &mut String) {
        match self {
            Self::Empty => {}
            Self::Scalar(character) => output.push(*character),
            Self::Grapheme(text) => output.push_str(text),
        }
    }
}

impl From<&str> for TerminalText {
    fn from(text: &str) -> Self {
        let mut characters = text.chars();
        match (characters.next(), characters.next()) {
            (None, _) => Self::Empty,
            (Some(character), None) => Self::Scalar(character),
            _ => Self::Grapheme(Box::new(text.to_owned())),
        }
    }
}

impl From<String> for TerminalText {
    fn from(text: String) -> Self {
        let mut characters = text.chars();
        match (characters.next(), characters.next()) {
            (None, _) => Self::Empty,
            (Some(character), None) => Self::Scalar(character),
            _ => Self::Grapheme(Box::new(text)),
        }
    }
}

impl PartialEq<&str> for TerminalText {
    fn eq(&self, other: &&str) -> bool {
        self.with_str(|text| text == *other)
    }
}

impl Serialize for TerminalText {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.with_str(|text| serializer.serialize_str(text))
    }
}

impl<'de> Deserialize<'de> for TerminalText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Into::into)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalCell {
    #[serde(default, rename = "t", skip_serializing_if = "TerminalText::is_empty")]
    pub contents: TerminalText,
    #[serde(default, rename = "f", skip_serializing_if = "is_default_color")]
    pub foreground: TerminalColor,
    #[serde(default, rename = "b", skip_serializing_if = "is_default_color")]
    pub background: TerminalColor,
    #[serde(default, rename = "a", skip_serializing_if = "is_default_attributes")]
    pub attributes: TerminalAttributes,
    #[serde(default, rename = "w", skip_serializing_if = "is_false")]
    pub wide: bool,
    #[serde(default, rename = "c", skip_serializing_if = "is_false")]
    pub wide_continuation: bool,
    /// One-based index into [`TerminalState::hyperlinks`]; zero is never used.
    #[serde(default, rename = "h", skip_serializing_if = "Option::is_none")]
    pub hyperlink: Option<u32>,
}

fn is_default_color(color: &TerminalColor) -> bool {
    *color == TerminalColor::Default
}

fn is_default_attributes(attributes: &TerminalAttributes) -> bool {
    *attributes == TerminalAttributes::default()
}

const fn is_false(value: &bool) -> bool {
    !*value
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSnapshot {
    pub state: TerminalState,
    pub cells: Vec<TerminalCell>,
    pub wrapped_rows: Vec<bool>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct CellStyle {
    foreground: TerminalColor,
    background: TerminalColor,
    attributes: TerminalAttributes,
    hyperlink: Option<u32>,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            foreground: TerminalColor::Default,
            background: TerminalColor::Default,
            attributes: TerminalAttributes::default(),
            hyperlink: None,
        }
    }
}

impl From<&TerminalCell> for CellStyle {
    fn from(cell: &TerminalCell) -> Self {
        Self {
            foreground: cell.foreground,
            background: cell.background,
            attributes: cell.attributes,
            hyperlink: cell.hyperlink,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WireSnapshot {
    #[serde(rename = "s")]
    state: TerminalState,
    #[serde(rename = "y")]
    styles: Vec<WireStyle>,
    #[serde(rename = "r")]
    rows: Vec<WireRow>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct WireStyle {
    #[serde(default, rename = "f", skip_serializing_if = "is_default_color")]
    foreground: TerminalColor,
    #[serde(default, rename = "b", skip_serializing_if = "is_default_color")]
    background: TerminalColor,
    #[serde(default, rename = "a", skip_serializing_if = "is_zero_u8")]
    attributes: u8,
    #[serde(default, rename = "h", skip_serializing_if = "Option::is_none")]
    hyperlink: Option<u32>,
}

impl From<CellStyle> for WireStyle {
    fn from(style: CellStyle) -> Self {
        Self {
            foreground: style.foreground,
            background: style.background,
            attributes: attribute_flags(style.attributes),
            hyperlink: style.hyperlink,
        }
    }
}

impl From<WireStyle> for CellStyle {
    fn from(style: WireStyle) -> Self {
        Self {
            foreground: style.foreground,
            background: style.background,
            attributes: attributes(style.attributes),
            hyperlink: style.hyperlink,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WireRow {
    #[serde(rename = "r")]
    runs: Vec<WireRun>,
    #[serde(default, rename = "w", skip_serializing_if = "is_false")]
    wrapped: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct WireRun {
    #[serde(default, rename = "s", skip_serializing_if = "is_zero_u32")]
    style: u32,
    #[serde(default, rename = "f", skip_serializing_if = "is_zero_u8")]
    flags: u8,
    #[serde(default, rename = "e", skip_serializing_if = "is_zero_u16")]
    empty: u16,
    #[serde(default, rename = "t", skip_serializing_if = "String::is_empty")]
    text: String,
    #[serde(default, rename = "g", skip_serializing_if = "Vec::is_empty")]
    graphemes: Vec<String>,
}

const fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

const fn is_zero_u16(value: &u16) -> bool {
    *value == 0
}

const fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

const fn attribute_flags(attributes: TerminalAttributes) -> u8 {
    (attributes.bold as u8)
        | ((attributes.dim as u8) << 1)
        | ((attributes.italic as u8) << 2)
        | ((attributes.underline as u8) << 3)
        | ((attributes.inverse as u8) << 4)
        | ((attributes.blink as u8) << 5)
        | ((attributes.hidden as u8) << 6)
        | ((attributes.strikethrough as u8) << 7)
}

const fn attributes(flags: u8) -> TerminalAttributes {
    TerminalAttributes {
        bold: flags & 1 != 0,
        dim: flags & 2 != 0,
        italic: flags & 4 != 0,
        underline: flags & 8 != 0,
        inverse: flags & 16 != 0,
        blink: flags & 32 != 0,
        hidden: flags & 64 != 0,
        strikethrough: flags & 128 != 0,
    }
}

const fn cell_flags(cell: &TerminalCell) -> u8 {
    (cell.wide as u8) | ((cell.wide_continuation as u8) << 1)
}

impl Serialize for TerminalSnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        WireSnapshot::try_from(self)
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TerminalSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_from(WireSnapshot::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

impl TryFrom<&TerminalSnapshot> for WireSnapshot {
    type Error = &'static str;

    fn try_from(snapshot: &TerminalSnapshot) -> Result<Self, Self::Error> {
        snapshot
            .validate()
            .map_err(|_| "invalid terminal snapshot")?;
        let mut style_ids = HashMap::from([(CellStyle::default(), 0)]);
        let mut styles = vec![WireStyle::from(CellStyle::default())];
        let cols = usize::from(snapshot.size().cols);
        let mut rows = Vec::with_capacity(usize::from(snapshot.size().rows));

        for row_index in 0..usize::from(snapshot.size().rows) {
            let row_start = row_index * cols;
            let cells = &snapshot.cells[row_start..row_start + cols];
            let mut runs = Vec::new();
            let mut column = 0;
            while column < cells.len() {
                let cell = &cells[column];
                let style = CellStyle::from(cell);
                let style_id = if let Some(style_id) = style_ids.get(&style) {
                    *style_id
                } else {
                    let style_id = u32::try_from(styles.len()).map_err(|_| "too many styles")?;
                    styles.push(style.into());
                    style_ids.insert(style, style_id);
                    style_id
                };
                let flags = cell_flags(cell);
                let start = column;
                match &cell.contents {
                    TerminalText::Empty => {
                        while column < cells.len()
                            && cells[column].contents.is_empty()
                            && CellStyle::from(&cells[column]) == style
                            && cell_flags(&cells[column]) == flags
                        {
                            column += 1;
                        }
                        runs.push(WireRun {
                            style: style_id,
                            flags,
                            empty: u16::try_from(column - start)
                                .map_err(|_| "terminal row is too wide")?,
                            ..WireRun::default()
                        });
                    }
                    TerminalText::Scalar(_) => {
                        let mut text = String::new();
                        while column < cells.len()
                            && matches!(cells[column].contents, TerminalText::Scalar(_))
                            && CellStyle::from(&cells[column]) == style
                            && cell_flags(&cells[column]) == flags
                        {
                            cells[column].contents.push_to(&mut text);
                            column += 1;
                        }
                        runs.push(WireRun {
                            style: style_id,
                            flags,
                            text,
                            ..WireRun::default()
                        });
                    }
                    TerminalText::Grapheme(_) => {
                        let mut graphemes = Vec::new();
                        while column < cells.len()
                            && matches!(cells[column].contents, TerminalText::Grapheme(_))
                            && CellStyle::from(&cells[column]) == style
                            && cell_flags(&cells[column]) == flags
                        {
                            graphemes.push(cells[column].contents.with_str(str::to_owned));
                            column += 1;
                        }
                        runs.push(WireRun {
                            style: style_id,
                            flags,
                            graphemes,
                            ..WireRun::default()
                        });
                    }
                }
            }
            rows.push(WireRow {
                runs,
                wrapped: snapshot.wrapped_rows[row_index],
            });
        }
        Ok(Self {
            state: snapshot.state.clone(),
            styles,
            rows,
        })
    }
}

impl TryFrom<WireSnapshot> for TerminalSnapshot {
    type Error = &'static str;

    fn try_from(wire: WireSnapshot) -> Result<Self, Self::Error> {
        if wire.rows.len() != usize::from(wire.state.size.rows) || wire.styles.is_empty() {
            return Err("terminal snapshot dimensions are invalid");
        }
        let styles = wire
            .styles
            .into_iter()
            .map(CellStyle::from)
            .collect::<Vec<_>>();
        let mut cells = Vec::with_capacity(wire.state.size.cell_count());
        let mut wrapped_rows = Vec::with_capacity(wire.rows.len());
        for row in wire.rows {
            let row_start = cells.len();
            for run in row.runs {
                let style = *styles
                    .get(run.style as usize)
                    .ok_or("terminal snapshot references an unknown style")?;
                if run.flags & !3 != 0 {
                    return Err("terminal snapshot contains invalid cell flags");
                }
                let variants = usize::from(run.empty > 0)
                    + usize::from(!run.text.is_empty())
                    + usize::from(!run.graphemes.is_empty());
                if variants != 1 {
                    return Err("terminal snapshot run has invalid contents");
                }
                let run_len = if run.empty > 0 {
                    usize::from(run.empty)
                } else if !run.text.is_empty() {
                    run.text.chars().count()
                } else {
                    run.graphemes.len()
                };
                if cells.len() - row_start + run_len > usize::from(wire.state.size.cols) {
                    return Err("terminal snapshot row is too wide");
                }
                if run.empty > 0 {
                    cells.extend(
                        std::iter::repeat_with(|| {
                            terminal_cell(TerminalText::Empty, style, run.flags)
                        })
                        .take(usize::from(run.empty)),
                    );
                } else if !run.text.is_empty() {
                    cells.extend(run.text.chars().map(|character| {
                        terminal_cell(TerminalText::Scalar(character), style, run.flags)
                    }));
                } else {
                    if run.graphemes.iter().any(String::is_empty) {
                        return Err("terminal snapshot contains an empty grapheme");
                    }
                    cells.extend(
                        run.graphemes
                            .into_iter()
                            .map(|text| terminal_cell(text.into(), style, run.flags)),
                    );
                }
            }
            if cells.len() - row_start != usize::from(wire.state.size.cols) {
                return Err("terminal snapshot row has the wrong width");
            }
            wrapped_rows.push(row.wrapped);
        }
        let snapshot = Self {
            state: wire.state,
            cells,
            wrapped_rows,
        };
        snapshot
            .validate()
            .map_err(|_| "terminal snapshot is invalid")?;
        Ok(snapshot)
    }
}

const fn terminal_cell(text: TerminalText, style: CellStyle, flags: u8) -> TerminalCell {
    TerminalCell {
        contents: text,
        foreground: style.foreground,
        background: style.background,
        attributes: style.attributes,
        wide: flags & 1 != 0,
        wide_continuation: flags & 2 != 0,
        hyperlink: style.hyperlink,
    }
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
            let mut text = String::new();
            for cell in row.iter().filter(|cell| !cell.wide_continuation) {
                cell.contents.push_to(&mut text);
            }
            text.truncate(text.trim_end().len());
            text
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
        let hyperlink_count = diff.state.hyperlinks.len();
        if diff
            .cells
            .iter()
            .any(|patch| !valid_hyperlink(patch.cell.hyperlink, hyperlink_count))
        {
            return Err(TerminalModelError::Hyperlink);
        }
        if self.cells.iter().enumerate().any(|(index, cell)| {
            !valid_hyperlink(cell.hyperlink, hyperlink_count)
                && !diff.cells.iter().any(|patch| patch.index as usize == index)
        }) {
            return Err(TerminalModelError::Hyperlink);
        }

        for patch in &diff.cells {
            self.cells[patch.index as usize] = patch.cell.clone();
        }
        for patch in &diff.wrapped_rows {
            self.wrapped_rows[usize::from(patch.row)] = patch.wrapped;
        }
        self.state = diff.state.clone();
        debug_assert_eq!(self.validate(), Ok(()));
        Ok(())
    }
}

fn valid_hyperlink(hyperlink: Option<u32>, count: usize) -> bool {
    hyperlink.is_none_or(|id| id != 0 && id as usize <= count)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalSnapshotDiff {
    #[serde(rename = "s")]
    pub state: TerminalState,
    #[serde(rename = "c")]
    pub cells: Vec<TerminalCellPatch>,
    #[serde(rename = "w")]
    pub wrapped_rows: Vec<TerminalRowPatch>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalCellPatch {
    #[serde(rename = "i")]
    pub index: u32,
    #[serde(rename = "c")]
    pub cell: TerminalCell,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalRowPatch {
    #[serde(rename = "r")]
    pub row: u16,
    #[serde(rename = "w")]
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
        let mut snapshot = TerminalSnapshot::blank(TerminalSize::new(1, 5));
        snapshot.cell_mut(0, 0).unwrap().contents = "e\u{301}".into();
        snapshot.cell_mut(0, 1).unwrap().contents = "界".into();
        snapshot.cell_mut(0, 1).unwrap().wide = true;
        snapshot.cell_mut(0, 2).unwrap().wide_continuation = true;
        snapshot.cell_mut(0, 3).unwrap().contents = "x".into();
        snapshot.cell_mut(0, 3).unwrap().foreground = TerminalColor::Indexed(2);
        snapshot.cell_mut(0, 3).unwrap().attributes.bold = true;
        snapshot.cell_mut(0, 3).unwrap().hyperlink = Some(1);
        snapshot.state.hyperlinks = vec!["https://example.com".into()];

        let encoded = serde_json::to_vec(&snapshot).unwrap();
        let decoded: TerminalSnapshot = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, snapshot);
        assert_eq!(decoded.rows().collect::<Vec<_>>(), ["e\u{301}界x"]);
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

        let mut diff = snapshot.diff(&snapshot).unwrap();
        diff.cells.push(TerminalCellPatch {
            index: 0,
            cell: TerminalCell {
                hyperlink: Some(1),
                ..TerminalCell::default()
            },
        });
        assert_eq!(snapshot.apply(&diff), Err(TerminalModelError::Hyperlink));
        assert_eq!(snapshot, original);
    }

    #[test]
    fn a_small_interactive_change_is_smaller_than_a_full_frame() {
        let before = TerminalSnapshot::blank(TerminalSize::new(24, 80));
        let mut after = before.clone();
        after.cell_mut(20, 10).unwrap().contents = "x".into();
        let diff = before.diff(&after).unwrap();

        assert_eq!(diff.cells.len(), 1);
        let diff_len = serde_json::to_vec(&diff).unwrap().len();
        let full_len = serde_json::to_vec(&after).unwrap().len();
        assert!(diff_len < full_len, "diff {diff_len}, full {full_len}");
    }

    #[test]
    fn full_grid_wire_format_is_compact_and_cell_text_is_inline_when_possible() {
        let mut snapshot = TerminalSnapshot::blank(TerminalSize::new(24, 80));
        for row in 0..24 {
            for column in 0..80 {
                snapshot.cell_mut(row, column).unwrap().contents =
                    char::from(b'a' + (column % 26) as u8).to_string().into();
            }
        }

        let encoded = serde_json::to_vec(&snapshot).unwrap();
        assert!(encoded.len() < 10_000, "encoded {} bytes", encoded.len());
        assert!(std::mem::size_of::<TerminalText>() <= 16);
        assert!(std::mem::size_of::<TerminalCell>() <= 48);
    }

    #[test]
    fn malformed_row_runs_are_rejected() {
        let snapshot = TerminalSnapshot::blank(TerminalSize::new(1, 2));
        let mut wire = serde_json::to_value(snapshot).unwrap();
        wire["r"][0]["r"][0]["s"] = 99.into();

        assert!(serde_json::from_value::<TerminalSnapshot>(wire).is_err());
    }
}
