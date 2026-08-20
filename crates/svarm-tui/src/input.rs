use crossterm::event::{
    KeyCode as HostKeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton as HostMouseButton,
    MouseEvent, MouseEventKind,
};
use svarm_agent::protocol::{
    InputModifiers, KeyCode, KeyInput, MouseButton, MouseInput, MouseKind,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagementCommand {
    LiteralPrefix,
    NextAgent,
    PreviousAgent,
    ScrollTerminalUp,
    ScrollTerminalDown,
    ChooseAgent,
    CloseAgent,
    ArchiveAgent,
    ResumeArchived,
    Detach,
    ConfirmQuit,
    ToggleSidebar,
    NarrowSidebar,
    WidenSidebar,
    OpenMenu,
    OpenKeybinds,
    OpenUsage,
    OpenHarnessUpdates,
    OpenReview,
    SelectAgent(usize),
    Cancel,
    Unknown,
}

pub(crate) struct Keybinding {
    pub keys: &'static str,
    pub action: &'static str,
    pub command: ManagementCommand,
}

pub(crate) const MANAGEMENT_KEYBINDINGS: &[Keybinding] = &[
    Keybinding {
        keys: "Ctrl+B, j/k or arrows",
        action: "next/previous agent",
        command: ManagementCommand::NextAgent,
    },
    Keybinding {
        keys: "Ctrl+B, 1..9",
        action: "select conversation",
        command: ManagementCommand::SelectAgent(0),
    },
    Keybinding {
        keys: "Ctrl+B, PageUp/PageDown",
        action: "scroll agent history",
        command: ManagementCommand::ScrollTerminalUp,
    },
    Keybinding {
        keys: "Ctrl+B, n",
        action: "start an agent",
        command: ManagementCommand::ChooseAgent,
    },
    Keybinding {
        keys: "Ctrl+B, x",
        action: "close selected agent",
        command: ManagementCommand::CloseAgent,
    },
    Keybinding {
        keys: "Ctrl+B, a",
        action: "archive selected conversation",
        command: ManagementCommand::ArchiveAgent,
    },
    Keybinding {
        keys: "Ctrl+B, r",
        action: "reactivate archived conversation",
        command: ManagementCommand::ResumeArchived,
    },
    Keybinding {
        keys: "Ctrl+B, d",
        action: "detach",
        command: ManagementCommand::Detach,
    },
    Keybinding {
        keys: "Ctrl+B, q",
        action: "stop session",
        command: ManagementCommand::ConfirmQuit,
    },
    Keybinding {
        keys: "Ctrl+B, b",
        action: "toggle sidebar",
        command: ManagementCommand::ToggleSidebar,
    },
    Keybinding {
        keys: "Ctrl+B, h/l",
        action: "narrow/widen sidebar",
        command: ManagementCommand::NarrowSidebar,
    },
    Keybinding {
        keys: "Ctrl+B, m",
        action: "open menu",
        command: ManagementCommand::OpenMenu,
    },
    Keybinding {
        keys: "Ctrl+B, u",
        action: "show subscription usage",
        command: ManagementCommand::OpenUsage,
    },
    Keybinding {
        keys: "Ctrl+B, s",
        action: "open harness updates",
        command: ManagementCommand::OpenHarnessUpdates,
    },
    Keybinding {
        keys: "Ctrl+B, v",
        action: "review changes with Hunk",
        command: ManagementCommand::OpenReview,
    },
    Keybinding {
        keys: "Ctrl+B, ?",
        action: "open keybinds",
        command: ManagementCommand::OpenKeybinds,
    },
    Keybinding {
        keys: "Ctrl+B, Ctrl+B",
        action: "send Ctrl+B to agent",
        command: ManagementCommand::LiteralPrefix,
    },
];

pub(crate) fn is_management_prefix(key: KeyEvent) -> bool {
    key.code == HostKeyCode::Char('b') && key.modifiers == KeyModifiers::CONTROL
}

pub(crate) fn management_command(key: KeyEvent) -> ManagementCommand {
    match key.code {
        HostKeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            ManagementCommand::LiteralPrefix
        }
        HostKeyCode::Char('j') | HostKeyCode::Down => ManagementCommand::NextAgent,
        HostKeyCode::Char('k') | HostKeyCode::Up => ManagementCommand::PreviousAgent,
        HostKeyCode::PageUp => ManagementCommand::ScrollTerminalUp,
        HostKeyCode::PageDown => ManagementCommand::ScrollTerminalDown,
        HostKeyCode::Char('n') => ManagementCommand::ChooseAgent,
        HostKeyCode::Char('x') => ManagementCommand::CloseAgent,
        HostKeyCode::Char('a') => ManagementCommand::ArchiveAgent,
        HostKeyCode::Char('r') => ManagementCommand::ResumeArchived,
        HostKeyCode::Char('d') => ManagementCommand::Detach,
        HostKeyCode::Char('q') => ManagementCommand::ConfirmQuit,
        HostKeyCode::Char('b') => ManagementCommand::ToggleSidebar,
        HostKeyCode::Char('h') | HostKeyCode::Left => ManagementCommand::NarrowSidebar,
        HostKeyCode::Char('l') | HostKeyCode::Right => ManagementCommand::WidenSidebar,
        HostKeyCode::Char('m') => ManagementCommand::OpenMenu,
        HostKeyCode::Char('?') => ManagementCommand::OpenKeybinds,
        HostKeyCode::Char('u') => ManagementCommand::OpenUsage,
        HostKeyCode::Char('s') => ManagementCommand::OpenHarnessUpdates,
        HostKeyCode::Char('v') => ManagementCommand::OpenReview,
        HostKeyCode::Char(digit @ '1'..='9') => {
            ManagementCommand::SelectAgent(digit as usize - '1' as usize)
        }
        HostKeyCode::Esc => ManagementCommand::Cancel,
        _ => ManagementCommand::Unknown,
    }
}

pub(crate) fn key_input(event: KeyEvent) -> Option<KeyInput> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let code = match event.code {
        HostKeyCode::Char(character) => KeyCode::Character(character),
        HostKeyCode::Enter => KeyCode::Enter,
        HostKeyCode::Tab => KeyCode::Tab,
        HostKeyCode::BackTab => KeyCode::BackTab,
        HostKeyCode::Backspace => KeyCode::Backspace,
        HostKeyCode::Esc => KeyCode::Escape,
        HostKeyCode::Up => KeyCode::Up,
        HostKeyCode::Down => KeyCode::Down,
        HostKeyCode::Left => KeyCode::Left,
        HostKeyCode::Right => KeyCode::Right,
        HostKeyCode::Home => KeyCode::Home,
        HostKeyCode::End => KeyCode::End,
        HostKeyCode::PageUp => KeyCode::PageUp,
        HostKeyCode::PageDown => KeyCode::PageDown,
        HostKeyCode::Insert => KeyCode::Insert,
        HostKeyCode::Delete => KeyCode::Delete,
        HostKeyCode::F(number) => KeyCode::Function(number),
        _ => return None,
    };
    Some(KeyInput {
        code,
        modifiers: input_modifiers(event.modifiers),
    })
}

pub(crate) fn mouse_input(event: MouseEvent) -> MouseInput {
    MouseInput {
        kind: match event.kind {
            MouseEventKind::Down(button) => MouseKind::Down(mouse_button(button)),
            MouseEventKind::Up(button) => MouseKind::Up(mouse_button(button)),
            MouseEventKind::Drag(button) => MouseKind::Drag(mouse_button(button)),
            MouseEventKind::Moved => MouseKind::Moved,
            MouseEventKind::ScrollUp => MouseKind::ScrollUp,
            MouseEventKind::ScrollDown => MouseKind::ScrollDown,
            MouseEventKind::ScrollLeft => MouseKind::ScrollLeft,
            MouseEventKind::ScrollRight => MouseKind::ScrollRight,
        },
        column: event.column,
        row: event.row,
        modifiers: input_modifiers(event.modifiers),
    }
}

const fn input_modifiers(modifiers: KeyModifiers) -> InputModifiers {
    InputModifiers {
        shift: modifiers.contains(KeyModifiers::SHIFT),
        alt: modifiers.contains(KeyModifiers::ALT),
        control: modifiers.contains(KeyModifiers::CONTROL),
    }
}

const fn mouse_button(button: HostMouseButton) -> MouseButton {
    match button {
        HostMouseButton::Left => MouseButton::Left,
        HostMouseButton::Middle => MouseButton::Middle,
        HostMouseButton::Right => MouseButton::Right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: HostKeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn management_keys_have_one_canonical_mapping() {
        assert_eq!(
            management_command(key(HostKeyCode::Char('j'), KeyModifiers::NONE)),
            ManagementCommand::NextAgent
        );
        assert_eq!(
            management_command(key(HostKeyCode::Char('d'), KeyModifiers::NONE)),
            ManagementCommand::Detach
        );
        assert_eq!(
            management_command(key(HostKeyCode::Char('q'), KeyModifiers::NONE)),
            ManagementCommand::ConfirmQuit
        );
        assert_eq!(
            management_command(key(HostKeyCode::PageUp, KeyModifiers::NONE)),
            ManagementCommand::ScrollTerminalUp
        );
        assert_eq!(
            management_command(key(HostKeyCode::Char('h'), KeyModifiers::NONE)),
            ManagementCommand::NarrowSidebar
        );
        assert_eq!(
            management_command(key(HostKeyCode::Char('l'), KeyModifiers::NONE)),
            ManagementCommand::WidenSidebar
        );
        assert_eq!(
            management_command(key(HostKeyCode::Char('a'), KeyModifiers::NONE)),
            ManagementCommand::ArchiveAgent
        );
        assert_eq!(
            management_command(key(HostKeyCode::Char('r'), KeyModifiers::NONE)),
            ManagementCommand::ResumeArchived
        );
        assert_eq!(
            management_command(key(HostKeyCode::Char('u'), KeyModifiers::NONE)),
            ManagementCommand::OpenUsage
        );
        assert!(
            MANAGEMENT_KEYBINDINGS
                .iter()
                .any(|binding| binding.keys == "Ctrl+B, d" && binding.action == "detach")
        );
    }

    /// Every command reachable by key must also be listed, because the keybinds modal renders
    /// that table and each row doubles as the click target for its command.
    #[test]
    fn every_documented_binding_is_reachable_by_its_own_key() {
        for binding in MANAGEMENT_KEYBINDINGS {
            let Some(letter) = binding
                .keys
                .strip_prefix("Ctrl+B, ")
                .filter(|rest| rest.chars().count() == 1)
                .and_then(|rest| rest.chars().next())
            else {
                continue;
            };
            assert_eq!(
                management_command(key(HostKeyCode::Char(letter), KeyModifiers::NONE)),
                binding.command,
                "Ctrl+B, {letter} should invoke the command it is documented under"
            );
        }
    }

    #[test]
    fn translates_host_keys_without_encoding_terminal_modes() {
        assert_eq!(
            key_input(key(HostKeyCode::Up, KeyModifiers::CONTROL)),
            Some(KeyInput {
                code: KeyCode::Up,
                modifiers: InputModifiers {
                    control: true,
                    ..InputModifiers::default()
                },
            })
        );
    }

    #[test]
    fn translates_mouse_coordinates_without_guessing_child_modes() {
        assert_eq!(
            mouse_input(MouseEvent {
                kind: MouseEventKind::Down(HostMouseButton::Left),
                column: 4,
                row: 2,
                modifiers: KeyModifiers::NONE,
            }),
            MouseInput {
                kind: MouseKind::Down(MouseButton::Left),
                column: 4,
                row: 2,
                modifiers: InputModifiers::default(),
            }
        );
    }
}
