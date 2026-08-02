//! WIN32_INPUT_MODE encoder for keys written to a Windows ConPTY child.
//!
//! Microsoft's win32-input-mode (negotiated with `\x1b[?9001h`) wraps every
//! key event in `\x1b[Vk;Sc;Uc;Kd;Cs;Rc_`. The child's ConPTY decodes that
//! into a full `INPUT_RECORD` — virtual-key code and modifiers preserved —
//! instead of guessing them from a lossy legacy byte.
//!
//! Spec: <https://learn.microsoft.com/en-us/windows/console/key-event-record-str>.
//! Reference impl adapted from wezterm's `encode_win32_input_mode`.

use zellij_utils::data::{BareKey, KeyModifier, KeyWithModifier};

// dwControlKeyState bits — Microsoft KEY_EVENT_RECORD.
const RIGHT_ALT_PRESSED: u32 = 0x0001;
const LEFT_ALT_PRESSED: u32 = 0x0002;
const RIGHT_CTRL_PRESSED: u32 = 0x0004;
const LEFT_CTRL_PRESSED: u32 = 0x0008;
const SHIFT_PRESSED: u32 = 0x0010;
const ENHANCED_KEY: u32 = 0x0100;

#[derive(Debug, Clone, Copy)]
struct Win32KeyRecord {
    vk: u16,
    scan: u16,
    unicode: u16,
    key_down: bool,
    control_key_state: u32,
    repeat: u16,
}

pub fn encode_key_for_child(key: &KeyWithModifier) -> Vec<u8> {
    fn key_to_records(key: &KeyWithModifier) -> Vec<Win32KeyRecord> {
        fn keycode_to_vk(key: &KeyWithModifier) -> Option<u16> {
            match key.bare_key {
                BareKey::Backspace => Some(0x08),
                BareKey::Tab => Some(0x09),
                BareKey::Enter => Some(0x0D),
                BareKey::Pause => Some(0x13),
                BareKey::CapsLock => Some(0x14),
                BareKey::Esc => Some(0x1B),
                BareKey::PageUp => Some(0x21),
                BareKey::PageDown => Some(0x22),
                BareKey::End => Some(0x23),
                BareKey::Home => Some(0x24),
                BareKey::Left => Some(0x25),
                BareKey::Up => Some(0x26),
                BareKey::Right => Some(0x27),
                BareKey::Down => Some(0x28),
                BareKey::PrintScreen => Some(0x2C),
                BareKey::Insert => Some(0x2D),
                BareKey::Delete => Some(0x2E),
                BareKey::Menu => Some(0x5D),
                BareKey::F(n) if (1..=24).contains(&n) => Some(0x70 + (n as u16 - 1)),
                BareKey::F(_) => None,
                BareKey::NumLock => Some(0x90),
                BareKey::ScrollLock => Some(0x91),
                BareKey::Char(' ') => Some(0x20),
                BareKey::Char(c) if c.is_ascii_alphabetic() => Some(c.to_ascii_uppercase() as u16),
                BareKey::Char(c) if c.is_ascii_digit() => Some(c as u16),
                // Symbols: their VK is layout-specific (VK_OEM_*). Leave 0 and
                // rely on the unicode field for the literal char.
                BareKey::Char(_) => Some(0),
            }
        }

        fn keycode_to_scan(key: &KeyWithModifier) -> u16 {
            // US-layout nominal scan codes. Apps reading INPUT_RECORDs almost
            // always branch on vk + modifiers, so a stable nominal value is
            // enough; 0 is also acceptable for keys we don't enumerate.
            match key.bare_key {
                BareKey::Esc => 0x01,
                BareKey::Backspace => 0x0E,
                BareKey::Tab => 0x0F,
                BareKey::Enter => 0x1C,
                BareKey::Char(' ') => 0x39,
                BareKey::Char(c) if c.is_ascii_alphabetic() => match c.to_ascii_lowercase() {
                    'q' => 0x10, 'w' => 0x11, 'e' => 0x12, 'r' => 0x13,
                    't' => 0x14, 'y' => 0x15, 'u' => 0x16, 'i' => 0x17,
                    'o' => 0x18, 'p' => 0x19,
                    'a' => 0x1E, 's' => 0x1F, 'd' => 0x20, 'f' => 0x21,
                    'g' => 0x22, 'h' => 0x23, 'j' => 0x24, 'k' => 0x25,
                    'l' => 0x26,
                    'z' => 0x2C, 'x' => 0x2D, 'c' => 0x2E, 'v' => 0x2F,
                    'b' => 0x30, 'n' => 0x31, 'm' => 0x32,
                    _ => 0,
                },
                BareKey::Char(c) if c.is_ascii_digit() => match c {
                    '1' => 0x02, '2' => 0x03, '3' => 0x04, '4' => 0x05,
                    '5' => 0x06, '6' => 0x07, '7' => 0x08, '8' => 0x09,
                    '9' => 0x0A, '0' => 0x0B,
                    _ => 0,
                },
                BareKey::F(n) if (1..=10).contains(&n) => 0x3A + n as u16,
                BareKey::F(11) => 0x57,
                BareKey::F(12) => 0x58,
                BareKey::Home => 0x47,
                BareKey::Up => 0x48,
                BareKey::PageUp => 0x49,
                BareKey::Left => 0x4B,
                BareKey::Right => 0x4D,
                BareKey::End => 0x4F,
                BareKey::Down => 0x50,
                BareKey::PageDown => 0x51,
                BareKey::Insert => 0x52,
                BareKey::Delete => 0x53,
                _ => 0,
            }
        }

        fn keycode_to_unicode(key: &KeyWithModifier) -> u16 {
            // UTF-16 code unit the key would type. Must mirror what real
            // Windows hardware produces, because consumers like fzf (tcell on
            // Windows) treat this field as the authoritative "did the user
            // type a character?" answer and only fall back to the VK + modifier
            // for non-printable values. Ctrl + ascii-letter must therefore
            // collapse to the standard control char (Ctrl+A=0x01..Ctrl+Z=0x1A)
            // — otherwise fzf would see Ctrl+D as a printable 'd' arriving with
            // a stray modifier and type it into the filter.
            match key.bare_key {
                BareKey::Char(c) if c.is_ascii_alphabetic() => {
                    let has_ctrl = key.key_modifiers.contains(&KeyModifier::Ctrl);
                    let has_alt = key.key_modifiers.contains(&KeyModifier::Alt);
                    let has_shift = key.key_modifiers.contains(&KeyModifier::Shift);
                    if has_ctrl && !has_alt {
                        // Ctrl(+Shift) + letter → control char in 0x01..=0x1A.
                        (c.to_ascii_uppercase() as u16) - ('A' as u16) + 1
                    } else if has_shift {
                        c.to_ascii_uppercase() as u16
                    } else {
                        c as u16
                    }
                },
                BareKey::Char(c) => c as u16,
                BareKey::Enter => 0x0D,
                BareKey::Tab => 0x09,
                BareKey::Esc => 0x1B,
                BareKey::Backspace => 0x08,
                _ => 0,
            }
        }

        fn modifiers_to_control_key_state(key: &KeyWithModifier) -> u32 {
            let mut state = 0;
            if key.key_modifiers.contains(&KeyModifier::Shift) {
                state |= SHIFT_PRESSED;
            }
            if key.key_modifiers.contains(&KeyModifier::Ctrl) {
                state |= LEFT_CTRL_PRESSED;
            }
            if key.key_modifiers.contains(&KeyModifier::Alt) {
                state |= LEFT_ALT_PRESSED;
            }
            // ENHANCED_KEY identifies keys outside the main typewriter block
            // (arrows, nav cluster, numpad-Enter). ConPTY consumers use this
            // to e.g. distinguish numpad Enter from main Enter.
            if matches!(
                key.bare_key,
                BareKey::Left
                    | BareKey::Right
                    | BareKey::Up
                    | BareKey::Down
                    | BareKey::Home
                    | BareKey::End
                    | BareKey::PageUp
                    | BareKey::PageDown
                    | BareKey::Insert
                    | BareKey::Delete
            ) {
                state |= ENHANCED_KEY;
            }
            // Silence dead_code on the right-side constants we keep for parity
            // with KEY_EVENT_RECORD even though zellij's KeyModifier doesn't
            // carry left/right distinction.
            let _ = (RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED);
            state
        }

        let Some(vk) = keycode_to_vk(key) else {
            return Vec::new();
        };
        let scan = keycode_to_scan(key);
        let unicode = keycode_to_unicode(key);
        let control_key_state = modifiers_to_control_key_state(key);

        // Single key-down record. Most consumers (crossterm, PSReadLine,
        // GNU readline) only react to KeyDown. Add a key-up record if a
        // specific app proves to need matched pairs.
        vec![Win32KeyRecord {
            vk,
            scan,
            unicode,
            key_down: true,
            control_key_state,
            repeat: 1,
        }]
    }

    fn serialize_record(rec: &Win32KeyRecord) -> String {
        format!(
            "\u{1b}[{};{};{};{};{};{}_",
            rec.vk,
            rec.scan,
            rec.unicode,
            rec.key_down as u8,
            rec.control_key_state,
            rec.repeat,
        )
    }

    key_to_records(key)
        .iter()
        .map(serialize_record)
        .collect::<String>()
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn k(bare: BareKey, mods: &[KeyModifier]) -> KeyWithModifier {
        KeyWithModifier {
            bare_key: bare,
            key_modifiers: mods.iter().copied().collect::<BTreeSet<_>>(),
        }
    }

    #[test]
    fn ctrl_j_carries_vk_j_not_vk_return() {
        // The exact bug this module fixes: legacy encoding writes 0x0A which
        // ConPTY decodes as VK_RETURN + Ctrl. With win32 mode the encoded
        // record names VK_J (0x4A) with LEFT_CTRL_PRESSED — distinguishable
        // from Enter at the receiver. UnicodeChar is the control char 0x0A,
        // matching what real Windows hardware produces.
        let bytes = encode_key_for_child(&k(BareKey::Char('j'), &[KeyModifier::Ctrl]));
        let s = String::from_utf8(bytes).unwrap();
        assert_eq!(s, "\u{1b}[74;36;10;1;8;1_");
    }

    #[test]
    fn ctrl_d_unicode_is_control_char_not_printable_d() {
        // fzf treats the UnicodeChar as authoritative for "did the user type
        // a printable?" — if we sent 'd' (0x64), Ctrl+D would arrive as a
        // printable letter with a stray Ctrl flag and end up typed into the
        // filter. Real hardware sends 0x04 (EOT, the Ctrl+D control char).
        let bytes = encode_key_for_child(&k(BareKey::Char('d'), &[KeyModifier::Ctrl]));
        let s = String::from_utf8(bytes).unwrap();
        assert_eq!(s, "\u{1b}[68;32;4;1;8;1_");
    }

    #[test]
    fn ctrl_s_unicode_is_control_char() {
        let bytes = encode_key_for_child(&k(BareKey::Char('s'), &[KeyModifier::Ctrl]));
        let s = String::from_utf8(bytes).unwrap();
        assert_eq!(s, "\u{1b}[83;31;19;1;8;1_");
    }

    #[test]
    fn plain_enter_is_distinct_from_ctrl_j() {
        let bytes = encode_key_for_child(&k(BareKey::Enter, &[]));
        let s = String::from_utf8(bytes).unwrap();
        assert_eq!(s, "\u{1b}[13;28;13;1;0;1_");
    }

    #[test]
    fn arrow_keys_get_enhanced_bit() {
        let bytes = encode_key_for_child(&k(BareKey::Left, &[]));
        let s = String::from_utf8(bytes).unwrap();
        // vk=0x25 (VK_LEFT), scan=0x4B, uni=0, kd=1, cs=ENHANCED_KEY(0x100), rep=1
        assert_eq!(s, "\u{1b}[37;75;0;1;256;1_");
    }
}
