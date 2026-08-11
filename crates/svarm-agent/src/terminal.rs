use serde::{Deserialize, Serialize};
use terminal_colorsaurus::ColorPalette;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalPalette {
    foreground: [u16; 3],
    background: [u16; 3],
}

impl TerminalPalette {
    pub fn detect() -> Option<Self> {
        terminal_colorsaurus::color_palette(Default::default())
            .ok()
            .map(Self::from)
    }

    fn response(self, query: ColorQuery) -> String {
        let (slot, [red, green, blue]) = match query {
            ColorQuery::Foreground => (10, self.foreground),
            ColorQuery::Background => (11, self.background),
        };
        format!("\x1b]{slot};rgb:{red:04x}/{green:04x}/{blue:04x}\x1b\\")
    }
}

impl From<ColorPalette> for TerminalPalette {
    fn from(palette: ColorPalette) -> Self {
        Self {
            foreground: [
                palette.foreground.r,
                palette.foreground.g,
                palette.foreground.b,
            ],
            background: [
                palette.background.r,
                palette.background.g,
                palette.background.b,
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ColorQuery {
    Foreground,
    Background,
}

#[derive(Default)]
pub(crate) struct ColorQueryDetector {
    pending: Vec<u8>,
}

impl ColorQueryDetector {
    pub(crate) fn process(&mut self, bytes: &[u8]) -> Vec<ColorQuery> {
        const QUERIES: [(&[u8], ColorQuery); 4] = [
            (b"\x1b]10;?\x1b\\", ColorQuery::Foreground),
            (b"\x1b]11;?\x1b\\", ColorQuery::Background),
            (b"\x1b]10;?\x07", ColorQuery::Foreground),
            (b"\x1b]11;?\x07", ColorQuery::Background),
        ];

        self.pending.extend_from_slice(bytes);
        let mut detected = Vec::new();
        while let Some((position, pattern, query)) = QUERIES
            .iter()
            .filter_map(|(pattern, query)| {
                self.pending
                    .windows(pattern.len())
                    .position(|window| window == *pattern)
                    .map(|position| (position, *pattern, *query))
            })
            .min_by_key(|(position, _, _)| *position)
        {
            detected.push(query);
            self.pending.drain(..position + pattern.len());
        }

        const MAX_PARTIAL_QUERY_LEN: usize = 7;
        if self.pending.len() > MAX_PARTIAL_QUERY_LEN {
            self.pending
                .drain(..self.pending.len() - MAX_PARTIAL_QUERY_LEN);
        }
        detected
    }
}

pub(crate) fn color_query_responses(
    detector: &mut ColorQueryDetector,
    palette: Option<TerminalPalette>,
    bytes: &[u8],
) -> Vec<String> {
    let Some(palette) = palette else {
        detector.process(bytes);
        return Vec::new();
    };
    detector
        .process(bytes)
        .into_iter()
        .map(|query| palette.response(query))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_formats_osc_color_responses() {
        let palette = TerminalPalette {
            foreground: [0xaaaa, 0xbbbb, 0xcccc],
            background: [0x1111, 0x2222, 0x3333],
        };

        assert_eq!(
            palette.response(ColorQuery::Foreground),
            "\x1b]10;rgb:aaaa/bbbb/cccc\x1b\\"
        );
        assert_eq!(
            palette.response(ColorQuery::Background),
            "\x1b]11;rgb:1111/2222/3333\x1b\\"
        );
    }

    #[test]
    fn detector_reports_exact_queries_split_across_reads() {
        let mut detector = ColorQueryDetector::default();

        assert!(detector.process(b"before\x1b]10;?\x1b").is_empty());
        assert_eq!(
            detector.process(b"\\middle\x1b]11;?\x07after"),
            [ColorQuery::Foreground, ColorQuery::Background]
        );
    }

    #[test]
    fn detector_does_not_infer_queries_from_partial_evidence() {
        let mut detector = ColorQueryDetector::default();

        assert!(detector.process(b"\x1b]10;?").is_empty());
        assert!(detector.process(b"not-a-terminator").is_empty());
    }
}
