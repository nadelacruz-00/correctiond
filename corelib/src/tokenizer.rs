//! Key classification and the in-progress word buffer.

const CONTROL_MASK: u32 = 1 << 2;
const MOD1_MASK: u32 = 1 << 3; // Alt
const SUPER_MASK: u32 = 1 << 26;
const HYPER_MASK: u32 = 1 << 27;
const META_MASK: u32 = 1 << 28;
const COMMAND_MASK: u32 = CONTROL_MASK | MOD1_MASK | SUPER_MASK | HYPER_MASK | META_MASK;

const KEY_BACKSPACE: u32 = 0xff08;
const KEY_TAB: u32 = 0xff09;
const KEY_RETURN: u32 = 0xff0d;
const KEY_KP_ENTER: u32 = 0xff8d;
const KEY_ISO_LEFT_TAB: u32 = 0xfe20;
const UNICODE_KEYSYM_BASE: u32 = 0x0100_0000;

/// How a key event affects the word being typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyClass {
    /// Extends the current word.
    Printable(char),
    /// Completes the word; carries the committable boundary char, if any.
    Boundary(Option<char>),
    /// Removes the last char of the word.
    Backspace,
    /// The word can no longer be trusted; discard it.
    Invalidate,
}

/// Classifies an X11/IBus keysym plus modifier state.
pub fn classify_key(keyval: u32, state: u32) -> KeyClass {
    if state & COMMAND_MASK != 0 {
        return KeyClass::Invalidate;
    }
    match keyval {
        KEY_BACKSPACE => return KeyClass::Backspace,
        KEY_RETURN | KEY_KP_ENTER | KEY_TAB | KEY_ISO_LEFT_TAB => return KeyClass::Boundary(None),
        _ => {}
    }
    match keysym_to_char(keyval) {
        Some(c @ (' ' | ',' | ';' | ':' | '!' | '?')) => KeyClass::Boundary(Some(c)),
        Some(c) => KeyClass::Printable(c),
        None => KeyClass::Invalidate,
    }
}

/// Maps a keysym to its printable char (Latin-1 range or Unicode keysym).
fn keysym_to_char(keyval: u32) -> Option<char> {
    let cp = match keyval {
        0x20..=0x7e | 0xa0..=0xff => keyval,
        k if k > UNICODE_KEYSYM_BASE => k - UNICODE_KEYSYM_BASE,
        _ => return None,
    };
    char::from_u32(cp).filter(|c| !c.is_control())
}

/// The characters typed since the last boundary.
#[derive(Debug, Default, Clone)]
pub struct WordBuffer {
    word: String,
}

impl WordBuffer {
    pub fn push(&mut self, c: char) {
        self.word.push(c);
    }

    /// Removes and returns the last char, or `None` when empty.
    pub fn pop(&mut self) -> Option<char> {
        self.word.pop()
    }

    pub fn clear(&mut self) {
        self.word.clear();
    }

    pub fn word(&self) -> &str {
        &self.word
    }

    pub fn len_chars(&self) -> usize {
        self.word.chars().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: u32 = 1 << 2;

    #[test]
    fn letter_is_printable() {
        assert_eq!(classify_key(0x61, 0), KeyClass::Printable('a'));
    }

    #[test]
    fn space_is_boundary() {
        assert_eq!(classify_key(0x20, 0), KeyClass::Boundary(Some(' ')));
    }

    #[test]
    fn return_is_charless_boundary() {
        assert_eq!(classify_key(0xff0d, 0), KeyClass::Boundary(None));
    }

    #[test]
    fn path_chars_are_printable() {
        for c in ['/', '-', '.', '_'] {
            assert_eq!(classify_key(c as u32, 0), KeyClass::Printable(c));
        }
    }

    #[test]
    fn ctrl_combo_invalidates() {
        assert_eq!(classify_key(0x61, CTRL), KeyClass::Invalidate);
    }

    #[test]
    fn hyper_combo_invalidates() {
        // IBus HYPER_MASK.
        assert_eq!(classify_key(0x61, 1 << 27), KeyClass::Invalidate);
    }

    #[test]
    fn arrow_invalidates() {
        assert_eq!(classify_key(0xff51, 0), KeyClass::Invalidate);
    }

    #[test]
    fn backspace_classified() {
        assert_eq!(classify_key(0xff08, 0), KeyClass::Backspace);
    }

    #[test]
    fn buffer_push_pop_word() {
        let mut b = WordBuffer::default();
        for c in ['t', 'e', 'h'] {
            b.push(c);
        }
        assert_eq!(b.word(), "teh");
        assert_eq!(b.pop(), Some('h'));
        assert_eq!(b.word(), "te");
        b.clear();
        assert_eq!(b.pop(), None);
    }

    #[test]
    fn buffer_handles_multibyte() {
        let mut b = WordBuffer::default();
        b.push('é');
        assert_eq!(b.len_chars(), 1);
        assert_eq!(b.pop(), Some('é'));
        assert_eq!(b.word(), "");
    }
}
