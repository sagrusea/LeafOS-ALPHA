#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Tab,
    Space,
    LeftShift,
    RightShift,
    LeftCtrl,
    RightCtrl,
    LeftAlt,
    RightAlt,
    CapsLock,
    NumLock,
    ScrollLock,
    F(u8),
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Unknown(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub state: KeyState,
    pub ascii: Option<u8>,
}

#[derive(Default)]
pub struct Keyboard {
    extended: bool,
    pause_remaining: u8,
    left_shift: bool,
    right_shift: bool,
    left_ctrl: bool,
    right_ctrl: bool,
    left_alt: bool,
    right_alt: bool,
    caps_lock: bool,
    num_lock: bool,
    scroll_lock: bool,
}

impl Keyboard {
    pub const fn new() -> Self {
        Self {
            extended: false,
            pause_remaining: 0,
            left_shift: false,
            right_shift: false,
            left_ctrl: false,
            right_ctrl: false,
            left_alt: false,
            right_alt: false,
            caps_lock: false,
            num_lock: false,
            scroll_lock: false,
        }
    }

    pub fn process_scancode(&mut self, scancode: u8) -> Option<KeyEvent> {
        if self.pause_remaining > 0 {
            self.pause_remaining -= 1;
            return None;
        }

        if scancode == 0xE1 {
            self.extended = false;
            self.pause_remaining = 5;
            return None;
        }

        if scancode == 0xE0 {
            self.extended = true;
            return None;
        }

        let extended = core::mem::replace(&mut self.extended, false);
        let released = scancode & 0x80 != 0;
        let code = scancode & 0x7F;

        let state = if released {
            KeyState::Released
        } else {
            KeyState::Pressed
        };

        let key = if extended {
            Self::extended_key(code)
        } else {
            Self::normal_key(code)
        };

        match key {
            KeyCode::LeftShift => self.left_shift = !released,
            KeyCode::RightShift => self.right_shift = !released,
            KeyCode::LeftCtrl => self.left_ctrl = !released,
            KeyCode::RightCtrl => self.right_ctrl = !released,
            KeyCode::LeftAlt => self.left_alt = !released,
            KeyCode::RightAlt => self.right_alt = !released,
            KeyCode::CapsLock if !released => self.caps_lock = !self.caps_lock,
            KeyCode::NumLock if !released => self.num_lock = !self.num_lock,
            KeyCode::ScrollLock if !released => self.scroll_lock = !self.scroll_lock,
            _ => {}
        }

        let ascii = if released { None } else { self.to_ascii(key) };

        Some(KeyEvent {
            code: key,
            state,
            ascii,
        })
    }

    fn normal_key(code: u8) -> KeyCode {
        use KeyCode::*;

        match code {
            0x01 => Escape,
            0x02 => Char('1'),
            0x03 => Char('2'),
            0x04 => Char('3'),
            0x05 => Char('4'),
            0x06 => Char('5'),
            0x07 => Char('6'),
            0x08 => Char('7'),
            0x09 => Char('8'),
            0x0A => Char('9'),
            0x0B => Char('0'),
            0x0C => Char('-'),
            0x0D => Char('='),
            0x0E => Backspace,
            0x0F => Tab,
            0x10 => Char('q'),
            0x11 => Char('w'),
            0x12 => Char('e'),
            0x13 => Char('r'),
            0x14 => Char('t'),
            0x15 => Char('y'),
            0x16 => Char('u'),
            0x17 => Char('i'),
            0x18 => Char('o'),
            0x19 => Char('p'),
            0x1A => Char('['),
            0x1B => Char(']'),
            0x1C => Enter,
            0x1D => LeftCtrl,
            0x1E => Char('a'),
            0x1F => Char('s'),
            0x20 => Char('d'),
            0x21 => Char('f'),
            0x22 => Char('g'),
            0x23 => Char('h'),
            0x24 => Char('j'),
            0x25 => Char('k'),
            0x26 => Char('l'),
            0x27 => Char(';'),
            0x28 => Char('\''),
            0x29 => Char('`'),
            0x2A => LeftShift,
            0x2B => Char('\\'),
            0x2C => Char('z'),
            0x2D => Char('x'),
            0x2E => Char('c'),
            0x2F => Char('v'),
            0x30 => Char('b'),
            0x31 => Char('n'),
            0x32 => Char('m'),
            0x33 => Char(','),
            0x34 => Char('.'),
            0x35 => Char('/'),
            0x36 => RightShift,
            0x37 => Char('*'),
            0x38 => LeftAlt,
            0x39 => Space,
            0x3A => CapsLock,
            0x3B..=0x44 => F(code - 0x3A),
            0x45 => NumLock,
            0x46 => ScrollLock,
            0x47 => Char('7'),
            0x48 => Char('8'),
            0x49 => Char('9'),
            0x4A => Char('-'),
            0x4B => Char('4'),
            0x4C => Char('5'),
            0x4D => Char('6'),
            0x4E => Char('+'),
            0x4F => Char('1'),
            0x50 => Char('2'),
            0x51 => Char('3'),
            0x52 => Char('0'),
            0x53 => Char('.'),
            0x57 => F(11),
            0x58 => F(12),
            _ => Unknown(code),
        }
    }

    fn extended_key(code: u8) -> KeyCode {
        use KeyCode::*;

        match code {
            0x1C => Enter,
            0x1D => RightCtrl,
            0x35 => Char('/'),
            0x38 => RightAlt,
            0x47 => Home,
            0x48 => ArrowUp,
            0x49 => PageUp,
            0x4B => ArrowLeft,
            0x4D => ArrowRight,
            0x4F => End,
            0x50 => ArrowDown,
            0x51 => PageDown,
            0x52 => Insert,
            0x53 => Delete,
            _ => Unknown(code | 0x80),
        }
    }

    fn to_ascii(&self, key: KeyCode) -> Option<u8> {
        let shifted = self.left_shift || self.right_shift;

        match key {
            KeyCode::Char(c) if c.is_ascii_alphabetic() => {
                let upper = shifted ^ self.caps_lock;
                Some(if upper {
                    c.to_ascii_uppercase() as u8
                } else {
                    c as u8
                })
            }
            KeyCode::Char(c) => {
                let c = if shifted {
                    match c {
                        '1' => '!',
                        '2' => '@',
                        '3' => '#',
                        '4' => '$',
                        '5' => '%',
                        '6' => '^',
                        '7' => '&',
                        '8' => '*',
                        '9' => '(',
                        '0' => ')',
                        '-' => '_',
                        '=' => '+',
                        '[' => '{',
                        ']' => '}',
                        '\\' => '|',
                        ';' => ':',
                        '\'' => '"',
                        '`' => '~',
                        ',' => '<',
                        '.' => '>',
                        '/' => '?',
                        other => other,
                    }
                } else {
                    c
                };

                let mut byte = c as u8;

                if self.left_ctrl || self.right_ctrl {
                    if byte.is_ascii_alphabetic() {
                        byte = byte.to_ascii_uppercase() - b'A' + 1;
                    }
                }

                Some(byte)
            }
            KeyCode::Space => Some(b' '),
            KeyCode::Enter => Some(b'\n'),
            KeyCode::Tab => Some(b'\t'),
            KeyCode::Backspace => Some(0x08),
            KeyCode::Escape => Some(0x1B),
            _ => None,
        }
    }
}
