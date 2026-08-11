use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use tui_term::vt100::{MouseProtocolEncoding, MouseProtocolMode};

pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        [b"\x1b[200~".as_slice(), text.as_bytes(), b"\x1b[201~"].concat()
    } else {
        text.as_bytes().to_vec()
    }
}

pub fn encode_key(event: KeyEvent) -> Option<Vec<u8>> {
    if event.kind == KeyEventKind::Release {
        return None;
    }

    let modifiers = event.modifiers;
    let mut bytes = match event.code {
        KeyCode::Char(character) if modifiers.contains(KeyModifiers::CONTROL) => {
            encode_control(character).map(|byte| vec![byte])?
        }
        KeyCode::Char(character) => character.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Left => cursor_key(b'D', modifiers),
        KeyCode::Right => cursor_key(b'C', modifiers),
        KeyCode::Up => cursor_key(b'A', modifiers),
        KeyCode::Down => cursor_key(b'B', modifiers),
        KeyCode::Home => cursor_key(b'H', modifiers),
        KeyCode::End => cursor_key(b'F', modifiers),
        KeyCode::Insert => tilde_key(2, modifiers),
        KeyCode::Delete => tilde_key(3, modifiers),
        KeyCode::PageUp => tilde_key(5, modifiers),
        KeyCode::PageDown => tilde_key(6, modifiers),
        KeyCode::F(number) => function_key(number, modifiers)?,
        KeyCode::Null => vec![0],
        _ => return None,
    };

    if modifiers.contains(KeyModifiers::ALT)
        && matches!(
            event.code,
            KeyCode::Char(_) | KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace | KeyCode::Esc
        )
    {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

pub fn encode_mouse(
    event: MouseEvent,
    mode: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    let allowed = match mode {
        MouseProtocolMode::None => false,
        MouseProtocolMode::Press => matches!(
            event.kind,
            MouseEventKind::Down(_)
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight
        ),
        MouseProtocolMode::PressRelease => {
            !matches!(event.kind, MouseEventKind::Drag(_) | MouseEventKind::Moved)
        }
        MouseProtocolMode::ButtonMotion => !matches!(event.kind, MouseEventKind::Moved),
        MouseProtocolMode::AnyMotion => true,
    };
    if !allowed {
        return None;
    }

    let released = matches!(event.kind, MouseEventKind::Up(_));
    let mut code = match event.kind {
        MouseEventKind::Down(button)
        | MouseEventKind::Up(button)
        | MouseEventKind::Drag(button) => mouse_button(button),
        MouseEventKind::Moved => 3,
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        MouseEventKind::ScrollLeft => 66,
        MouseEventKind::ScrollRight => 67,
    };
    if matches!(event.kind, MouseEventKind::Drag(_) | MouseEventKind::Moved) {
        code += 32;
    }
    code += 4 * u16::from(event.modifiers.contains(KeyModifiers::SHIFT));
    code += 8 * u16::from(event.modifiers.contains(KeyModifiers::ALT));
    code += 16 * u16::from(event.modifiers.contains(KeyModifiers::CONTROL));

    let column = event.column.saturating_add(1);
    let row = event.row.saturating_add(1);
    match encoding {
        MouseProtocolEncoding::Sgr => Some(
            format!(
                "\x1b[<{code};{column};{row}{}",
                if released { 'm' } else { 'M' }
            )
            .into_bytes(),
        ),
        MouseProtocolEncoding::Default => {
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
        MouseProtocolEncoding::Utf8 => {
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

fn cursor_key(final_byte: u8, modifiers: KeyModifiers) -> Vec<u8> {
    let parameter = modifier_parameter(modifiers);
    if parameter == 1 {
        vec![0x1b, b'[', final_byte]
    } else {
        format!("\x1b[1;{parameter}{}", final_byte as char).into_bytes()
    }
}

fn tilde_key(number: u8, modifiers: KeyModifiers) -> Vec<u8> {
    let parameter = modifier_parameter(modifiers);
    if parameter == 1 {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{parameter}~").into_bytes()
    }
}

fn function_key(number: u8, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    let parameter = modifier_parameter(modifiers);
    let final_byte = match number {
        1 => Some('P'),
        2 => Some('Q'),
        3 => Some('R'),
        4 => Some('S'),
        _ => None,
    };
    if let Some(final_byte) = final_byte {
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

fn modifier_parameter(modifiers: KeyModifiers) -> u8 {
    1 + u8::from(modifiers.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(modifiers.contains(KeyModifiers::ALT))
        + 4 * u8::from(modifiers.contains(KeyModifiers::CONTROL))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn forwards_text_and_control_keys() {
        assert_eq!(
            encode_key(key(KeyCode::Char('x'), KeyModifiers::NONE)),
            Some(b"x".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('x'), KeyModifiers::ALT)),
            Some(b"\x1bx".to_vec())
        );
    }

    #[test]
    fn encodes_navigation_with_xterm_modifiers() {
        assert_eq!(
            encode_key(key(KeyCode::Up, KeyModifiers::NONE)),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Right, KeyModifiers::CONTROL)),
            Some(b"\x1b[1;5C".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::PageDown, KeyModifiers::SHIFT)),
            Some(b"\x1b[6;2~".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Left, KeyModifiers::ALT)),
            Some(b"\x1b[1;3D".to_vec())
        );
    }

    #[test]
    fn wraps_bracketed_paste() {
        assert_eq!(
            encode_paste("hello", true),
            b"\x1b[200~hello\x1b[201~".to_vec()
        );
        assert_eq!(encode_paste("hello", false), b"hello".to_vec());
    }

    #[test]
    fn forwards_sgr_mouse_events_when_the_agent_requests_them() {
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            encode_mouse(
                event,
                MouseProtocolMode::PressRelease,
                MouseProtocolEncoding::Sgr
            ),
            Some(b"\x1b[<0;5;3M".to_vec())
        );
        assert_eq!(
            encode_mouse(event, MouseProtocolMode::None, MouseProtocolEncoding::Sgr),
            None
        );
    }
}
