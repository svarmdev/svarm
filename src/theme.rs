use ratatui::style::{Color, Modifier, Style};

// Sele's palettes, kept local because Svarm only needs in-memory UI preferences.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemeName {
    #[default]
    Dark,
    Light,
    HighContrast,
    Monochrome,
    SolarizedDark,
    SolarizedLight,
    CatppuccinMocha,
    CatppuccinLatte,
    TokyoNight,
    GruvboxDark,
    Nord,
}

impl ThemeName {
    pub const ALL: &[Self] = &[
        Self::Dark,
        Self::Light,
        Self::HighContrast,
        Self::Monochrome,
        Self::SolarizedDark,
        Self::SolarizedLight,
        Self::CatppuccinMocha,
        Self::CatppuccinLatte,
        Self::TokyoNight,
        Self::GruvboxDark,
        Self::Nord,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::HighContrast => "High Contrast",
            Self::Monochrome => "Monochrome",
            Self::SolarizedDark => "Solarized Dark",
            Self::SolarizedLight => "Solarized Light",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::CatppuccinLatte => "Catppuccin Latte",
            Self::TokyoNight => "Tokyo Night",
            Self::GruvboxDark => "Gruvbox Dark",
            Self::Nord => "Nord",
        }
    }

    pub fn cycle(&mut self, delta: isize) {
        let current = Self::ALL
            .iter()
            .position(|theme| theme == self)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(Self::ALL.len() as isize) as usize;
        *self = Self::ALL[next];
    }

    pub fn theme(self) -> Theme {
        if std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
            return Self::Monochrome.palette();
        }
        self.palette()
    }

    fn palette(self) -> Theme {
        match self {
            Self::Dark => Theme {
                bg: Color::Reset,
                surface: Color::Reset,
                selection: Color::DarkGray,
                on_selection: Color::White,
                border: Color::DarkGray,
                text: Color::Reset,
                dim: Color::DarkGray,
                strong: Color::White,
                accent: Color::Cyan,
                ok: Color::Green,
                warn: Color::Yellow,
            },
            Self::Light => Theme {
                bg: Color::Reset,
                surface: Color::Reset,
                selection: Color::Gray,
                on_selection: Color::Black,
                border: Color::Gray,
                text: Color::Reset,
                dim: Color::Gray,
                strong: Color::Black,
                accent: Color::Blue,
                ok: Color::Green,
                warn: Color::Magenta,
            },
            Self::HighContrast => Theme {
                bg: Color::Black,
                surface: Color::Black,
                selection: Color::Blue,
                on_selection: Color::White,
                border: Color::White,
                text: Color::White,
                dim: Color::Gray,
                strong: Color::White,
                accent: Color::LightCyan,
                ok: Color::LightGreen,
                warn: Color::LightYellow,
            },
            Self::Monochrome => Theme {
                bg: Color::Reset,
                surface: Color::Reset,
                selection: Color::Reset,
                on_selection: Color::Reset,
                border: Color::Reset,
                text: Color::Reset,
                dim: Color::Reset,
                strong: Color::Reset,
                accent: Color::Reset,
                ok: Color::Reset,
                warn: Color::Reset,
            },
            Self::SolarizedDark => Theme {
                bg: rgb(0x002b36),
                surface: rgb(0x073642),
                selection: rgb(0x586e75),
                on_selection: rgb(0xfdf6e3),
                border: rgb(0x586e75),
                text: rgb(0x839496),
                dim: rgb(0x586e75),
                strong: rgb(0x93a1a1),
                accent: rgb(0x268bd2),
                ok: rgb(0x859900),
                warn: rgb(0xb58900),
            },
            Self::SolarizedLight => Theme {
                bg: rgb(0xfdf6e3),
                surface: rgb(0xeee8d5),
                selection: rgb(0x93a1a1),
                on_selection: rgb(0x002b36),
                border: rgb(0x93a1a1),
                text: rgb(0x657b83),
                dim: rgb(0x93a1a1),
                strong: rgb(0x586e75),
                accent: rgb(0x268bd2),
                ok: rgb(0x859900),
                warn: rgb(0xcb4b16),
            },
            Self::CatppuccinMocha => Theme {
                bg: rgb(0x1e1e2e),
                surface: rgb(0x313244),
                selection: rgb(0x45475a),
                on_selection: rgb(0xcdd6f4),
                border: rgb(0x585b70),
                text: rgb(0xbac2de),
                dim: rgb(0x7f849c),
                strong: rgb(0xcdd6f4),
                accent: rgb(0x89b4fa),
                ok: rgb(0xa6e3a1),
                warn: rgb(0xf9e2af),
            },
            Self::CatppuccinLatte => Theme {
                bg: rgb(0xeff1f5),
                surface: rgb(0xe6e9ef),
                selection: rgb(0xccd0da),
                on_selection: rgb(0x4c4f69),
                border: rgb(0xacb0be),
                text: rgb(0x5c5f77),
                dim: rgb(0x8c8fa1),
                strong: rgb(0x4c4f69),
                accent: rgb(0x1e66f5),
                ok: rgb(0x40a02b),
                warn: rgb(0xdf8e1d),
            },
            Self::TokyoNight => Theme {
                bg: rgb(0x1a1b26),
                surface: rgb(0x24283b),
                selection: rgb(0x364a82),
                on_selection: rgb(0xc0caf5),
                border: rgb(0x3b4261),
                text: rgb(0xa9b1d6),
                dim: rgb(0x565f89),
                strong: rgb(0xc0caf5),
                accent: rgb(0x7aa2f7),
                ok: rgb(0x9ece6a),
                warn: rgb(0xe0af68),
            },
            Self::GruvboxDark => Theme {
                bg: rgb(0x282828),
                surface: rgb(0x3c3836),
                selection: rgb(0x504945),
                on_selection: rgb(0xfbf1c7),
                border: rgb(0x665c54),
                text: rgb(0xebdbb2),
                dim: rgb(0x928374),
                strong: rgb(0xfbf1c7),
                accent: rgb(0x83a598),
                ok: rgb(0xb8bb26),
                warn: rgb(0xfabd2f),
            },
            Self::Nord => Theme {
                bg: rgb(0x2e3440),
                surface: rgb(0x3b4252),
                selection: rgb(0x434c5e),
                on_selection: rgb(0xeceff4),
                border: rgb(0x4c566a),
                text: rgb(0xd8dee9),
                dim: rgb(0x616e88),
                strong: rgb(0xeceff4),
                accent: rgb(0x88c0d0),
                ok: rgb(0xa3be8c),
                warn: rgb(0xebcb8b),
            },
        }
    }
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb(
        (hex >> 16) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Theme {
    pub bg: Color,
    pub surface: Color,
    pub selection: Color,
    pub on_selection: Color,
    pub border: Color,
    pub text: Color,
    pub dim: Color,
    pub strong: Color,
    pub accent: Color,
    pub ok: Color,
    pub warn: Color,
}

impl Theme {
    pub fn page(self) -> Style {
        Style::default().bg(self.bg).fg(self.text)
    }

    pub fn surface(self) -> Style {
        Style::default().bg(self.surface).fg(self.text)
    }

    pub fn muted(self) -> Style {
        if self.dim == Color::Reset {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(self.dim)
        }
    }

    pub fn selected(self) -> Style {
        if self.selection == Color::Reset {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default()
                .bg(self.selection)
                .fg(self.on_selection)
                .add_modifier(Modifier::BOLD)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_cycle_wraps() {
        let mut theme = ThemeName::Dark;
        theme.cycle(-1);
        assert_eq!(theme, ThemeName::Nord);
        theme.cycle(1);
        assert_eq!(theme, ThemeName::Dark);
    }

    #[test]
    fn every_theme_has_a_unique_palette() {
        for (index, name) in ThemeName::ALL.iter().enumerate() {
            let palette = name.palette();
            assert!(
                !ThemeName::ALL[index + 1..]
                    .iter()
                    .any(|other| other.palette() == palette)
            );
        }
    }
}
