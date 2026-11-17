// Copyright © 2016-2026 The SENTIENT Authors

//! Keys to the bytes a terminal program expects (xterm's encoding): arrows
//! and the editing keys (application cursor mode too), function keys,
//! Ctrl with a letter, Alt as an Escape prefix, modifiers on the special
//! keys as `1;<m>`. Pure, so it is tested without a window.

/// The modifiers held.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { ctrl: false, alt: false, shift: false };

    /// xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }

    fn any(self) -> bool {
        self.ctrl || self.alt || self.shift
    }
}

/// What the terminal does with a key the dialog keeps for itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    Copy,
    Paste,
}

/// Ctrl+Shift+C copies, Ctrl+Shift+V pastes (Ctrl+C stays the program's).
pub fn shortcut(key: &str, m: Mods) -> Option<Shortcut> {
    if !(m.ctrl && m.shift) || m.alt {
        return None;
    }
    match key {
        "c" | "C" => Some(Shortcut::Copy),
        "v" | "V" => Some(Shortcut::Paste),
        _ => None,
    }
}

/// The bytes for a key. `key` is the key's name as GPUI says it ("up",
/// "enter", "a"), `key_char` what it types (None for most Ctrl keys),
/// `app_cursor` the program asked for application cursor keys (DECCKM).
/// None: nothing to send.
// VERIFY: GPUI's key names ("pageup", "pagedown", "insert", "f1"…,
// "space", "backspace") on Linux and Windows.
pub fn key_bytes(key: &str, key_char: Option<&str>, m: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    let csi = |s: &str| format!("\x1b[{s}").into_bytes();
    // Arrows, Home, End: SS3 in application mode without modifiers, CSI
    // otherwise; with modifiers CSI 1;m X.
    let cursor = |final_byte: char| {
        if m.any() {
            csi(&format!("1;{}{final_byte}", m.param()))
        } else if app_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            csi(&final_byte.to_string())
        }
    };
    // Insert, Delete, Page Up/Down, F5+: CSI n ~ (CSI n;m ~ with modifiers).
    let tilde = |n: u8| if m.any() { csi(&format!("{n};{}~", m.param())) } else { csi(&format!("{n}~")) };
    // F1–F4: SS3 P–S (CSI 1;m P with modifiers).
    let ss3 = |final_byte: char| if m.any() { csi(&format!("1;{}{final_byte}", m.param())) } else { format!("\x1bO{final_byte}").into_bytes() };
    let alt = |mut b: Vec<u8>| {
        if m.alt {
            b.insert(0, 0x1b);
        }
        b
    };
    let out = match key {
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => ss3('P'),
        "f2" => ss3('Q'),
        "f3" => ss3('R'),
        "f4" => ss3('S'),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        "enter" => alt(vec![b'\r']),
        "escape" => vec![0x1b],
        "tab" if m.shift => csi("Z"),
        "tab" => alt(vec![b'\t']),
        "backspace" if m.ctrl => alt(vec![0x08]),
        "backspace" => alt(vec![0x7f]),
        "space" if m.ctrl => alt(vec![0]),
        "space" => alt(vec![b' ']),
        _ if m.ctrl => {
            let c = key.chars().next().filter(|_| key.chars().count() == 1)?;
            let b = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                '@' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '-' | '7' => 0x1f,
                '?' | '8' => 0x7f,
                _ => return key_char.filter(|s| !s.is_empty()).map(|s| alt(s.as_bytes().to_vec())),
            };
            alt(vec![b])
        }
        _ => {
            let text = key_char.filter(|s| !s.is_empty())?;
            alt(text.as_bytes().to_vec())
        }
    };
    Some(out)
}

/// Pasted text as the program should get it: inside the bracketed-paste
/// markers when it asked for them (without any Escape inside, so the paste
/// cannot end itself early), line ends as Enter otherwise.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\n");
    if bracketed {
        let clean: String = text.chars().filter(|&c| c != '\x1b').collect();
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        text.replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Mods = Mods { ctrl: true, alt: false, shift: false };
    const ALT: Mods = Mods { ctrl: false, alt: true, shift: false };
    const SHIFT: Mods = Mods { ctrl: false, alt: false, shift: true };

    fn k(key: &str, ch: Option<&str>, m: Mods) -> Vec<u8> {
        key_bytes(key, ch, m, false).unwrap()
    }

    #[test]
    fn arrows_follow_the_cursor_mode() {
        assert_eq!(k("up", None, Mods::NONE), b"\x1b[A");
        assert_eq!(k("left", None, Mods::NONE), b"\x1b[D");
        assert_eq!(key_bytes("up", None, Mods::NONE, true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes("home", None, Mods::NONE, true).unwrap(), b"\x1bOH");
        // Modifiers always use CSI 1;m.
        assert_eq!(k("right", None, CTRL), b"\x1b[1;5C");
        assert_eq!(key_bytes("right", None, SHIFT, true).unwrap(), b"\x1b[1;2C");
        assert_eq!(k("end", None, Mods { ctrl: true, alt: false, shift: true }), b"\x1b[1;6F");
    }

    #[test]
    fn editing_and_function_keys() {
        assert_eq!(k("delete", None, Mods::NONE), b"\x1b[3~");
        assert_eq!(k("pageup", None, Mods::NONE), b"\x1b[5~");
        assert_eq!(k("pagedown", None, SHIFT), b"\x1b[6;2~");
        assert_eq!(k("f1", None, Mods::NONE), b"\x1bOP");
        assert_eq!(k("f10", None, Mods::NONE), b"\x1b[21~");
        assert_eq!(k("f12", None, Mods::NONE), b"\x1b[24~");
        assert_eq!(k("f2", None, CTRL), b"\x1b[1;5Q");
    }

    #[test]
    fn enter_tab_backspace_escape() {
        assert_eq!(k("enter", None, Mods::NONE), b"\r");
        assert_eq!(k("tab", Some("\t"), Mods::NONE), b"\t");
        assert_eq!(k("tab", None, SHIFT), b"\x1b[Z");
        assert_eq!(k("backspace", None, Mods::NONE), b"\x7f");
        assert_eq!(k("backspace", None, ALT), b"\x1b\x7f");
        assert_eq!(k("backspace", None, CTRL), b"\x08");
        assert_eq!(k("escape", None, Mods::NONE), b"\x1b");
        assert_eq!(k("space", Some(" "), Mods::NONE), b" ");
        assert_eq!(k("space", None, CTRL), b"\0");
    }

    #[test]
    fn ctrl_letters_are_control_bytes() {
        assert_eq!(k("c", None, CTRL), [0x03]);
        assert_eq!(k("d", None, CTRL), [0x04]);
        assert_eq!(k("z", None, CTRL), [0x1a]);
        assert_eq!(k("a", Some("a"), CTRL), [0x01]);
        assert_eq!(k("[", None, CTRL), [0x1b]);
        assert_eq!(k("\\", None, CTRL), [0x1c]);
        assert_eq!(k("c", None, Mods { ctrl: true, alt: true, shift: false }), [0x1b, 0x03]);
    }

    #[test]
    fn text_and_alt() {
        assert_eq!(k("a", Some("a"), Mods::NONE), b"a");
        assert_eq!(k("a", Some("A"), SHIFT), b"A");
        assert_eq!(k("b", Some("b"), ALT), b"\x1bb");
        assert_eq!(k("e", Some("é"), Mods::NONE), "é".as_bytes());
        // A modifier alone, or a key that types nothing.
        assert_eq!(key_bytes("shift", None, SHIFT, false), None);
        assert_eq!(key_bytes("f13x", None, Mods::NONE, false), None);
    }

    #[test]
    fn copy_and_paste_shortcuts() {
        let cs = Mods { ctrl: true, alt: false, shift: true };
        assert_eq!(shortcut("c", cs), Some(Shortcut::Copy));
        assert_eq!(shortcut("v", cs), Some(Shortcut::Paste));
        assert_eq!(shortcut("c", CTRL), None);
        assert_eq!(shortcut("x", cs), None);
    }

    #[test]
    fn paste_is_bracketed_when_asked() {
        assert_eq!(paste_bytes("ls\r\npwd\n", false), b"ls\rpwd\r");
        assert_eq!(paste_bytes("a\nb", true), b"\x1b[200~a\nb\x1b[201~");
        // An Escape inside cannot end the paste early.
        assert_eq!(paste_bytes("x\x1b[201~rm", true), b"\x1b[200~x[201~rm\x1b[201~");
    }
}
