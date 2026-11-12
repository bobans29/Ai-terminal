//! Keyboard and scroll mapping.
//!
//! Sequences follow xterm, which is what `TERM=xterm-256color` advertises.
//! Application cursor mode (vim, less) switches the unmodified arrow keys.
//! Copy and paste are intercepted here so they never reach the shell.

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Debug)]
pub enum KeyAction {
    Bytes(Vec<u8>),
    Scroll(Scroll),
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    SplitVertical,
    SplitHorizontal,
    NextPane,
    PreviousPane,
    ClosePane,
    Copy,
    Paste,
    None,
}

pub fn encode_key(key: &Key, mods: ModifiersState, mode: TermMode) -> KeyAction {
    if let Some(action) = pane_action(key, mods) {
        return action;
    }
    if let Some(action) = tab_action(key, mods) {
        return action;
    }
    if is_copy(key, mods) {
        return KeyAction::Copy;
    }
    if is_paste(key, mods) {
        return KeyAction::Paste;
    }
    if let Key::Named(NamedKey::Insert) = key {
        if mods.shift_key() && !mods.control_key() && !mods.alt_key() {
            return KeyAction::Paste;
        }
    }

    // Shift+PageUp/Down scrolls the window. Plain PageUp goes to the shell.
    if mods.shift_key() && !mods.control_key() && !mods.alt_key() {
        match key {
            Key::Named(NamedKey::PageUp) => return KeyAction::Scroll(Scroll::PageUp),
            Key::Named(NamedKey::PageDown) => return KeyAction::Scroll(Scroll::PageDown),
            _ => {}
        }
    }

    if let Some(bytes) = named_bytes(key, mods, mode) {
        return KeyAction::Bytes(bytes);
    }
    if let Key::Character(text) = key {
        return character_bytes(text, mods);
    }
    KeyAction::None
}

fn pane_action(key: &Key, mods: ModifiersState) -> Option<KeyAction> {
    let exact_ctrl_shift =
        mods.control_key() && mods.shift_key() && !mods.alt_key() && !mods.super_key();
    if let Key::Character(text) = key {
        if exact_ctrl_shift {
            return match text.to_ascii_lowercase().as_str() {
                "d" => Some(KeyAction::SplitVertical),
                "e" => Some(KeyAction::SplitHorizontal),
                "x" => Some(KeyAction::ClosePane),
                _ => None,
            };
        }
    }
    if mods.alt_key() && !mods.control_key() && !mods.shift_key() && !mods.super_key() {
        if let Key::Named(named) = key {
            return match named {
                NamedKey::ArrowRight => Some(KeyAction::NextPane),
                NamedKey::ArrowLeft => Some(KeyAction::PreviousPane),
                _ => None,
            };
        }
    }
    None
}

fn tab_action(key: &Key, mods: ModifiersState) -> Option<KeyAction> {
    if let Key::Named(NamedKey::Tab) = key {
        if mods.control_key() && !mods.alt_key() && !mods.super_key() {
            return Some(if mods.shift_key() {
                KeyAction::PreviousTab
            } else {
                KeyAction::NextTab
            });
        }
    }

    let Key::Character(text) = key else {
        return None;
    };
    if !text.eq_ignore_ascii_case("t") && !text.eq_ignore_ascii_case("w") {
        return None;
    }
    let new_tab = text.eq_ignore_ascii_case("t");
    let action = if new_tab {
        KeyAction::NewTab
    } else {
        KeyAction::CloseTab
    };
    if mods.control_key() && mods.shift_key() && !mods.alt_key() && !mods.super_key() {
        return Some(action);
    }
    #[cfg(target_os = "macos")]
    if mods.super_key() && !mods.control_key() && !mods.shift_key() && !mods.alt_key() {
        return Some(action);
    }
    None
}

/// Wheel notches (positive = content moves down, revealing lines above).
pub fn notch_to_lines(notches: f32) -> i32 {
    quantize(notches * 3.0)
}

/// Add pixel movement to a running remainder and return only whole cells.
///
/// Touchpads emit many one-pixel events. Rounding each one up to a line makes
/// the scroll distance depend on how the platform batches those events.
pub fn consume_pixel_scroll(pending_px: &mut f32, delta_px: f32, cell_height: f32) -> i32 {
    if cell_height < 1.0 {
        return 0;
    }
    *pending_px += delta_px;
    let lines = (*pending_px / cell_height).trunc();
    if lines == 0.0 {
        return 0;
    }
    *pending_px -= lines * cell_height;
    lines as i32
}

/// Bracketed paste must not contain ESC. A payload of `\x1b[201~` would end
/// paste mode, and the rest would be typed as shell input.
pub fn sanitize_paste(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\r');
            }
            '\n' => out.push('\r'),
            '\u{1b}' | '\u{9b}' => {}
            _ => out.push(ch),
        }
    }
    out
}

fn quantize(lines: f32) -> i32 {
    let rounded = lines.round() as i32;
    if rounded == 0 && lines != 0.0 {
        if lines > 0.0 {
            1
        } else {
            -1
        }
    } else {
        rounded
    }
}

/// Arrow key bytes. `up == true` is the up arrow (used for alt-screen wheel).
pub fn arrow_bytes(up: bool, mode: TermMode) -> Vec<u8> {
    let code = if up { 'A' } else { 'B' };
    let mods = ModifiersState::empty();
    csi_arrow(code, mods, mode.contains(TermMode::APP_CURSOR))
}

/// SGR or X10 mouse report. `col` and `row` are 0-based.
pub fn encode_mouse(code: u16, col: usize, row: usize, press: bool, mode: TermMode) -> Vec<u8> {
    let col = col.saturating_add(1);
    let row = row.saturating_add(1);
    if mode.contains(TermMode::SGR_MOUSE) {
        let action = if press { 'M' } else { 'm' };
        format!("\x1b[<{code};{col};{row}{action}").into_bytes()
    } else {
        // X10 has no release flag. Button code 3 is the release event.
        let reported = if press { code } else { 3 };
        let cb = (reported as u8).saturating_add(32);
        let cx = (col.min(223) as u8).saturating_add(32);
        let cy = (row.min(223) as u8).saturating_add(32);
        vec![0x1b, b'[', b'M', cb, cx, cy]
    }
}

pub fn mouse_button_code(button: winit::event::MouseButton) -> Option<u16> {
    match button {
        winit::event::MouseButton::Left => Some(0),
        winit::event::MouseButton::Middle => Some(1),
        winit::event::MouseButton::Right => Some(2),
        _ => None,
    }
}

fn is_copy(key: &Key, mods: ModifiersState) -> bool {
    is_clipboard_key(key, mods, "c")
}

fn is_paste(key: &Key, mods: ModifiersState) -> bool {
    is_clipboard_key(key, mods, "v")
}

fn is_clipboard_key(key: &Key, mods: ModifiersState, letter: &str) -> bool {
    let Key::Character(text) = key else {
        return false;
    };
    if !text.eq_ignore_ascii_case(letter) {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        mods.super_key() && !mods.control_key() && !mods.alt_key()
    }
    #[cfg(not(target_os = "macos"))]
    {
        mods.control_key() && mods.shift_key() && !mods.alt_key() && !mods.super_key()
    }
}

fn named_bytes(key: &Key, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
    let Key::Named(named) = key else {
        return None;
    };
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let bytes = match named {
        NamedKey::Enter => vec![b'\r'],
        NamedKey::Backspace => {
            if mods.control_key() {
                vec![0x17]
            } else if mods.alt_key() {
                vec![0x1b, 0x7f]
            } else {
                vec![0x7f]
            }
        }
        NamedKey::Tab => {
            if mods.shift_key() {
                b"\x1b[Z".to_vec()
            } else {
                vec![b'\t']
            }
        }
        NamedKey::Escape => vec![0x1b],
        NamedKey::Space => {
            if mods.control_key() {
                with_alt(vec![0x00], mods)
            } else {
                with_alt(vec![b' '], mods)
            }
        }
        NamedKey::ArrowUp => csi_arrow('A', mods, app_cursor),
        NamedKey::ArrowDown => csi_arrow('B', mods, app_cursor),
        NamedKey::ArrowRight => csi_arrow('C', mods, app_cursor),
        NamedKey::ArrowLeft => csi_arrow('D', mods, app_cursor),
        NamedKey::Home => csi_arrow('H', mods, app_cursor),
        NamedKey::End => csi_arrow('F', mods, app_cursor),
        NamedKey::Insert => csi_tilde(2, mods),
        NamedKey::Delete => csi_tilde(3, mods),
        NamedKey::PageUp => csi_tilde(5, mods),
        NamedKey::PageDown => csi_tilde(6, mods),
        NamedKey::F1 => function(1, mods),
        NamedKey::F2 => function(2, mods),
        NamedKey::F3 => function(3, mods),
        NamedKey::F4 => function(4, mods),
        NamedKey::F5 => function(5, mods),
        NamedKey::F6 => function(6, mods),
        NamedKey::F7 => function(7, mods),
        NamedKey::F8 => function(8, mods),
        NamedKey::F9 => function(9, mods),
        NamedKey::F10 => function(10, mods),
        NamedKey::F11 => function(11, mods),
        NamedKey::F12 => function(12, mods),
        _ => return None,
    };
    Some(bytes)
}

fn character_bytes(text: &str, mods: ModifiersState) -> KeyAction {
    let Some(ch) = text.chars().next() else {
        return KeyAction::None;
    };
    if text.chars().nth(1).is_some() && !mods.control_key() && !mods.alt_key() {
        return KeyAction::Bytes(text.as_bytes().to_vec());
    }
    if mods.control_key() {
        if let Some(code) = ctrl_code(ch) {
            return KeyAction::Bytes(with_alt(vec![code], mods));
        }
        return KeyAction::None;
    }
    KeyAction::Bytes(with_alt(text.as_bytes().to_vec(), mods))
}

fn with_alt(mut bytes: Vec<u8>, mods: ModifiersState) -> Vec<u8> {
    if mods.alt_key() {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn ctrl_code(ch: char) -> Option<u8> {
    match ch.to_ascii_lowercase() {
        'a'..='z' => Some((ch.to_ascii_lowercase() as u8) - b'a' + 1),
        ' ' | '@' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}

fn xterm_mod(mods: ModifiersState) -> u8 {
    let mut value = 1u8;
    if mods.shift_key() {
        value += 1;
    }
    if mods.alt_key() {
        value += 2;
    }
    if mods.control_key() {
        value += 4;
    }
    value
}

fn csi_arrow(code: char, mods: ModifiersState, app_cursor: bool) -> Vec<u8> {
    let modifier = xterm_mod(mods);
    if modifier == 1 {
        if app_cursor {
            format!("\x1bO{code}").into_bytes()
        } else {
            format!("\x1b[{code}").into_bytes()
        }
    } else {
        format!("\x1b[1;{modifier}{code}").into_bytes()
    }
}

fn csi_tilde(number: u8, mods: ModifiersState) -> Vec<u8> {
    let modifier = xterm_mod(mods);
    if modifier == 1 {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{modifier}~").into_bytes()
    }
}

fn function(n: u8, mods: ModifiersState) -> Vec<u8> {
    let modifier = xterm_mod(mods);
    if n <= 4 {
        let code = (b'P' + (n - 1)) as char;
        if modifier == 1 {
            format!("\x1bO{code}").into_bytes()
        } else {
            format!("\x1b[1;{modifier}{code}").into_bytes()
        }
    } else {
        // xterm skips 16 for F6.
        let number = match n {
            5 => 15,
            6 => 17,
            7 => 18,
            8 => 19,
            9 => 20,
            10 => 21,
            11 => 23,
            12 => 24,
            _ => 15,
        };
        csi_tilde(number, mods)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(key: Key, mods: ModifiersState, mode: TermMode) -> Vec<u8> {
        match encode_key(&key, mods, mode) {
            KeyAction::Bytes(bytes) => bytes,
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    #[test]
    fn ctrl_c_and_arrows() {
        let none = ModifiersState::empty();
        assert_eq!(
            bytes(
                Key::Character("c".into()),
                ModifiersState::CONTROL,
                TermMode::empty()
            ),
            vec![0x03]
        );
        assert_eq!(
            bytes(Key::Named(NamedKey::ArrowUp), none, TermMode::empty()),
            b"\x1b[A"
        );
        assert_eq!(
            bytes(Key::Named(NamedKey::ArrowUp), none, TermMode::APP_CURSOR),
            b"\x1bOA"
        );
        assert_eq!(
            bytes(Key::Named(NamedKey::F1), none, TermMode::empty()),
            b"\x1bOP"
        );
        assert_eq!(
            bytes(Key::Named(NamedKey::F5), none, TermMode::empty()),
            b"\x1b[15~"
        );
        assert_eq!(
            bytes(Key::Named(NamedKey::Backspace), none, TermMode::empty()),
            vec![0x7f]
        );
        assert_eq!(
            bytes(
                Key::Character("x".into()),
                ModifiersState::ALT,
                TermMode::empty()
            ),
            b"\x1bx"
        );
    }

    #[test]
    fn tab_shortcuts_are_actions_and_plain_ctrl_letters_stay_shell_controls() {
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert!(matches!(
            encode_key(&Key::Character("t".into()), ctrl_shift, TermMode::empty()),
            KeyAction::NewTab
        ));
        assert!(matches!(
            encode_key(&Key::Character("W".into()), ctrl_shift, TermMode::empty()),
            KeyAction::CloseTab
        ));
        assert!(matches!(
            encode_key(
                &Key::Named(NamedKey::Tab),
                ModifiersState::CONTROL,
                TermMode::empty()
            ),
            KeyAction::NextTab
        ));
        assert!(matches!(
            encode_key(&Key::Named(NamedKey::Tab), ctrl_shift, TermMode::empty()),
            KeyAction::PreviousTab
        ));
        assert_eq!(
            bytes(
                Key::Character("t".into()),
                ModifiersState::CONTROL,
                TermMode::empty()
            ),
            vec![0x14]
        );
        assert_eq!(
            bytes(
                Key::Character("w".into()),
                ModifiersState::CONTROL,
                TermMode::empty()
            ),
            vec![0x17]
        );
    }

    #[test]
    fn pane_shortcuts_use_exact_modifiers_and_preserve_shell_keys() {
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        for (letter, expected) in [
            ("d", KeyAction::SplitVertical),
            ("E", KeyAction::SplitHorizontal),
            ("x", KeyAction::ClosePane),
        ] {
            let actual = encode_key(
                &Key::Character(letter.into()),
                ctrl_shift,
                TermMode::empty(),
            );
            assert!(std::mem::discriminant(&actual) == std::mem::discriminant(&expected));
        }
        assert!(matches!(
            encode_key(
                &Key::Named(NamedKey::ArrowRight),
                ModifiersState::ALT,
                TermMode::empty()
            ),
            KeyAction::NextPane
        ));
        assert!(matches!(
            encode_key(
                &Key::Named(NamedKey::ArrowLeft),
                ModifiersState::ALT,
                TermMode::empty()
            ),
            KeyAction::PreviousPane
        ));

        // Extra modifiers keep these keys available to the terminal.
        for mods in [
            ctrl_shift | ModifiersState::ALT,
            ctrl_shift | ModifiersState::SUPER,
        ] {
            assert!(matches!(
                encode_key(&Key::Character("d".into()), mods, TermMode::empty()),
                KeyAction::Bytes(_)
            ));
        }
        assert!(matches!(
            encode_key(
                &Key::Named(NamedKey::ArrowRight),
                ModifiersState::ALT | ModifiersState::SHIFT,
                TermMode::empty()
            ),
            KeyAction::Bytes(_)
        ));
        for (letter, code) in [("d", 0x04), ("e", 0x05), ("x", 0x18)] {
            assert_eq!(
                bytes(
                    Key::Character(letter.into()),
                    ModifiersState::CONTROL,
                    TermMode::empty()
                ),
                vec![code]
            );
        }
        assert_eq!(
            bytes(
                Key::Named(NamedKey::ArrowRight),
                ModifiersState::empty(),
                TermMode::empty()
            ),
            b"\x1b[C"
        );
    }

    #[test]
    fn wheel_notches() {
        assert_eq!(notch_to_lines(1.0), 3);
        assert_eq!(notch_to_lines(-1.0), -3);
        assert_eq!(notch_to_lines(0.2), 1);
    }

    #[test]
    fn pixel_scroll_accumulates_sub_cell_deltas() {
        let mut pending = 0.0;
        let mut total = 0;
        for _ in 0..38 {
            total += consume_pixel_scroll(&mut pending, 1.0, 19.0);
        }
        assert_eq!(total, 2);
        assert!(pending.abs() < 0.01);
    }

    #[test]
    fn paste_cannot_end_bracketed_mode() {
        let payload = sanitize_paste("hi\u{1b}[201~echo pwned\r\n");
        assert!(!payload.contains('\u{1b}'));
        assert_eq!(payload, "hi[201~echo pwned\r");
    }

    #[test]
    fn legacy_mouse_release_uses_button_3() {
        let press = encode_mouse(0, 1, 2, true, TermMode::empty());
        let release = encode_mouse(0, 1, 2, false, TermMode::empty());
        assert_eq!(press[3], 32);
        assert_eq!(release[3], 35);
        let sgr = encode_mouse(0, 0, 0, false, TermMode::SGR_MOUSE);
        assert_eq!(sgr, b"\x1b[<0;1;1m");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn copy_is_not_sent_to_the_shell() {
        let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        match encode_key(&Key::Character("c".into()), mods, TermMode::empty()) {
            KeyAction::Copy => {}
            other => panic!("expected copy, got {other:?}"),
        }
        match encode_key(&Key::Character("v".into()), mods, TermMode::empty()) {
            KeyAction::Paste => {}
            other => panic!("expected paste, got {other:?}"),
        }
    }
}
