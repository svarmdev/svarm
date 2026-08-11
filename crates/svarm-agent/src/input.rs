use crate::protocol::{
    InputModifiers, KeyCode, KeyInput, MouseButton, MouseEncoding, MouseInput, MouseKind,
    MouseProtocol, TerminalModes,
};

pub fn encode_key(input: &KeyInput, modes: TerminalModes) -> Option<Vec<u8>> {
    let modifiers = input.modifiers;
    let mut bytes = match input.code {
        KeyCode::Character(character) if modifiers.control => vec![encode_control(character)?],
        KeyCode::Character(character) => character.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Escape => vec![0x1b],
        KeyCode::Up => cursor_key(b'A', modifiers, modes.application_cursor),
        KeyCode::Down => cursor_key(b'B', modifiers, modes.application_cursor),
        KeyCode::Right => cursor_key(b'C', modifiers, modes.application_cursor),
        KeyCode::Left => cursor_key(b'D', modifiers, modes.application_cursor),
        KeyCode::Home => cursor_key(b'H', modifiers, modes.application_cursor),
        KeyCode::End => cursor_key(b'F', modifiers, modes.application_cursor),
        KeyCode::PageUp => tilde_key(5, modifiers),
        KeyCode::PageDown => tilde_key(6, modifiers),
        KeyCode::Insert => tilde_key(2, modifiers),
        KeyCode::Delete => tilde_key(3, modifiers),
        KeyCode::Function(number) => function_key(number, modifiers)?,
    };
    if modifiers.alt && !matches!(input.code, KeyCode::Escape) {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

pub fn encode_paste(text: &str, modes: TerminalModes) -> Vec<u8> {
    if modes.bracketed_paste {
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        text.as_bytes().to_vec()
    }
}

pub fn encode_mouse(event: &MouseInput, modes: TerminalModes) -> Option<Vec<u8>> {
    let allowed = match modes.mouse_protocol {
        MouseProtocol::None => false,
        MouseProtocol::Press => matches!(
            event.kind,
            MouseKind::Down(_) | MouseKind::ScrollUp | MouseKind::ScrollDown
        ),
        MouseProtocol::PressRelease => !matches!(event.kind, MouseKind::Moved | MouseKind::Drag(_)),
        MouseProtocol::ButtonMotion => !matches!(event.kind, MouseKind::Moved),
        MouseProtocol::AnyMotion => true,
    };
    if !allowed {
        return None;
    }

    let released = matches!(event.kind, MouseKind::Up(_));
    let mut code = match event.kind {
        MouseKind::Down(button) | MouseKind::Up(button) | MouseKind::Drag(button) => {
            mouse_button(button)
        }
        MouseKind::Moved => 3,
        MouseKind::ScrollUp => 64,
        MouseKind::ScrollDown => 65,
        MouseKind::ScrollLeft => 66,
        MouseKind::ScrollRight => 67,
    };
    if matches!(event.kind, MouseKind::Drag(_) | MouseKind::Moved) {
        code += 32;
    }
    code += modifier_code(event.modifiers);
    let column = event.column.saturating_add(1);
    let row = event.row.saturating_add(1);
    match modes.mouse_encoding {
        MouseEncoding::Sgr => Some(
            format!(
                "\x1b[<{code};{column};{row}{}",
                if released { 'm' } else { 'M' }
            )
            .into_bytes(),
        ),
        MouseEncoding::Default => {
            if column > 223 || row > 223 {
                return None;
            }
            let code = if released { 3 } else { code };
            Some(vec![
                0x1b,
                b'[',
                b'M',
                (code + 32) as u8,
                (column + 32) as u8,
                (row + 32) as u8,
            ])
        }
        MouseEncoding::Utf8 => {
            let code = if released { 3 } else { code };
            Some(
                format!(
                    "\x1b[M{}{}{}",
                    char::from_u32(u32::from(code + 32))?,
                    char::from_u32(u32::from(column + 32))?,
                    char::from_u32(u32::from(row + 32))?
                )
                .into_bytes(),
            )
        }
    }
}

const fn mouse_button(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

const fn modifier_code(modifiers: InputModifiers) -> u16 {
    4 * modifiers.shift as u16 + 8 * modifiers.alt as u16 + 16 * modifiers.control as u16
}

fn encode_control(character: char) -> Option<u8> {
    match character.to_ascii_lowercase() {
        'a'..='z' => Some(character.to_ascii_lowercase() as u8 - b'a' + 1),
        ' ' | '@' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

fn cursor_key(final_byte: u8, modifiers: InputModifiers, application_cursor: bool) -> Vec<u8> {
    let parameter = modifier_parameter(modifiers);
    if parameter == 1 {
        vec![
            0x1b,
            if application_cursor { b'O' } else { b'[' },
            final_byte,
        ]
    } else {
        format!("\x1b[1;{parameter}{}", final_byte as char).into_bytes()
    }
}

fn tilde_key(number: u8, modifiers: InputModifiers) -> Vec<u8> {
    let parameter = modifier_parameter(modifiers);
    if parameter == 1 {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{parameter}~").into_bytes()
    }
}

fn function_key(number: u8, modifiers: InputModifiers) -> Option<Vec<u8>> {
    let parameter = modifier_parameter(modifiers);
    if let Some(final_byte) = match number {
        1 => Some('P'),
        2 => Some('Q'),
        3 => Some('R'),
        4 => Some('S'),
        _ => None,
    } {
        return Some(if parameter == 1 {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[1;{parameter}{final_byte}").into_bytes()
        });
    }
    let number = match number {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return None,
    };
    Some(tilde_key(number, modifiers))
}

const fn modifier_parameter(modifiers: InputModifiers) -> u8 {
    1 + modifiers.shift as u8 + 2 * modifiers.alt as u8 + 4 * modifiers.control as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyInput {
        KeyInput {
            code,
            modifiers: InputModifiers::default(),
        }
    }

    #[test]
    fn cursor_keys_follow_the_authoritative_application_mode() {
        assert_eq!(
            encode_key(&key(KeyCode::Up), TerminalModes::default()),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(
                &key(KeyCode::Up),
                TerminalModes {
                    application_cursor: true,
                    ..TerminalModes::default()
                }
            ),
            Some(b"\x1bOA".to_vec())
        );
    }

    #[test]
    fn paste_is_wrapped_only_when_the_agent_requested_it() {
        assert_eq!(encode_paste("hello", TerminalModes::default()), b"hello");
        assert_eq!(
            encode_paste(
                "hello",
                TerminalModes {
                    bracketed_paste: true,
                    ..TerminalModes::default()
                }
            ),
            b"\x1b[200~hello\x1b[201~"
        );
    }

    #[test]
    fn mouse_is_suppressed_until_the_agent_requests_it() {
        let event = MouseInput {
            kind: MouseKind::Down(MouseButton::Left),
            column: 4,
            row: 2,
            modifiers: InputModifiers::default(),
        };
        assert_eq!(encode_mouse(&event, TerminalModes::default()), None);
        assert_eq!(
            encode_mouse(
                &event,
                TerminalModes {
                    mouse_protocol: MouseProtocol::PressRelease,
                    mouse_encoding: MouseEncoding::Sgr,
                    ..TerminalModes::default()
                }
            ),
            Some(b"\x1b[<0;5;3M".to_vec())
        );
    }
}
