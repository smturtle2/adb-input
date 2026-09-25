// SPDX-License-Identifier: EUPL-1.2
//! Small original ANSI/termios frontend. No external UI code or UI dependencies.
use std::{
    collections::VecDeque,
    io::{self, IsTerminal, Read, Write},
    time::Duration,
};

#[derive(Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
    Backspace,
    Enter,
    Tab,
    Escape,
    Interrupt,
    Char(char),
    Paste(String),
}
#[derive(Default)]
pub struct Decoder {
    bytes: VecDeque<u8>,
}
impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.bytes.extend(bytes);
    }
    pub fn next(&mut self, expire_escape: bool) -> Option<Key> {
        let first = *self.bytes.front()?;
        if first == 27 {
            let bytes: Vec<_> = self.bytes.iter().copied().collect();
            if bytes.starts_with(b"\x1b[200~") {
                if bytes.contains(&3) {
                    self.bytes.clear();
                    return Some(Key::Interrupt);
                }
                if let Some(end) = bytes[6..].windows(6).position(|w| w == b"\x1b[201~") {
                    let text = String::from_utf8_lossy(&bytes[6..6 + end]).into_owned();
                    self.bytes.drain(..12 + end);
                    return Some(Key::Paste(text));
                }
                if bytes.len() > 8192 {
                    self.bytes.clear();
                }
                return None;
            }
            for (sequence, key) in [
                (b"\x1b[A".as_slice(), Key::Up),
                (b"\x1b[B", Key::Down),
                (b"\x1b[C", Key::Right),
                (b"\x1b[D", Key::Left),
                (b"\x1b[H", Key::Home),
                (b"\x1b[F", Key::End),
                (b"\x1b[3~", Key::Delete),
                (b"\x1bOH", Key::Home),
                (b"\x1bOF", Key::End),
                (b"\x1bOA", Key::Up),
                (b"\x1bOB", Key::Down),
            ] {
                if bytes.starts_with(sequence) {
                    self.bytes.drain(..sequence.len());
                    return Some(key);
                }
            }
            if !expire_escape {
                return None;
            }
            self.bytes.pop_front();
            return Some(Key::Escape);
        }
        let key = match first {
            3 => Key::Interrupt,
            4 => Key::Interrupt,
            10 | 13 => Key::Enter,
            9 => Key::Tab,
            8 | 127 => Key::Backspace,
            1 => Key::Home,
            5 => Key::End,
            b if b < 32 => {
                self.bytes.pop_front();
                return self.next(expire_escape);
            }
            _ => {
                let n = if first < 128 {
                    1
                } else if first < 224 {
                    2
                } else if first < 240 {
                    3
                } else {
                    4
                };
                if self.bytes.len() < n {
                    return None;
                }
                let bytes: Vec<_> = self.bytes.iter().take(n).copied().collect();
                match std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|s| s.chars().next())
                {
                    Some(ch) => {
                        self.bytes.drain(..n);
                        return Some(Key::Char(ch));
                    }
                    None => {
                        self.bytes.pop_front();
                        return self.next(expire_escape);
                    }
                }
            }
        };
        self.bytes.pop_front();
        Some(key)
    }
}

#[derive(Clone, Copy, Default)]
pub enum Style {
    #[default]
    Normal,
    Muted,
    Accent,
    Error,
    Strong,
}
pub struct Line {
    pub text: String,
    pub style: Style,
}
impl Line {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}
pub struct Screen {
    pub lines: Vec<Line>,
    pub footer: String,
}
pub struct Terminal {
    saved: libc::termios,
    decoder: Decoder,
    last: String,
    color: bool,
}
impl Terminal {
    pub fn enter() -> io::Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(io::Error::other(
                "interactive mode needs a terminal; use --help for commands",
            ));
        }
        let mut saved = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut saved) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = saved;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let terminal = Self {
            saved,
            decoder: Decoder::default(),
            last: String::new(),
            color: std::env::var_os("NO_COLOR").is_none()
                && std::env::var("TERM").unwrap_or_default() != "dumb",
        };
        // The guard already exists, so a failed write also restores termios.
        let mut stdout = io::stdout().lock();
        stdout.write_all(b"\x1b[?1049h\x1b[?25l\x1b[?2004h")?;
        stdout.flush()?;
        Ok(terminal)
    }
    pub fn size(&self) -> (usize, usize) {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } == 0
            && size.ws_col > 0
            && size.ws_row > 0
        {
            (size.ws_col as usize, size.ws_row as usize)
        } else {
            (80, 24)
        }
    }
    pub fn draw(&mut self, screen: Screen) -> io::Result<()> {
        let (width, height) = self.size();
        let mut frame = String::from("\x1b[H");
        if width < 30 || height < 10 {
            for y in 0..height {
                frame.push_str("\x1b[0m\x1b[2K");
                if y == 1 {
                    frame.push_str(&clip(
                        "Resize terminal (minimum 30 x 10)",
                        width.saturating_sub(1),
                    ));
                }
                if y == 3 {
                    frame.push_str(&clip("Esc to go back", width.saturating_sub(1)));
                }
                if y + 1 < height {
                    frame.push_str("\r\n");
                }
            }
        } else {
            let margin = if width >= 50 { 2 } else { 1 };
            for y in 0..height {
                frame.push_str("\x1b[0m\x1b[2K");
                let (text, style) =
                    if y == 1 {
                        (
                            format!(
                                "adb-input{}v{}",
                                " ".repeat(width.saturating_sub(
                                    2 * margin + 11 + env!("CARGO_PKG_VERSION").len()
                                )),
                                env!("CARGO_PKG_VERSION")
                            ),
                            Style::Strong,
                        )
                    } else if y == height - 2 {
                        (screen.footer.clone(), Style::Muted)
                    } else if y >= 3 && y < height - 3 {
                        screen
                            .lines
                            .get(y - 3)
                            .map(|l| (l.text.clone(), l.style))
                            .unwrap_or_default()
                    } else {
                        (String::new(), Style::Normal)
                    };
                frame.push_str(&" ".repeat(margin));
                frame.push_str(if self.color {
                    match style {
                        Style::Normal => "\x1b[0m",
                        Style::Muted => "\x1b[90m",
                        Style::Accent => "\x1b[1;36m",
                        Style::Error => "\x1b[31m",
                        Style::Strong => "\x1b[1m",
                    }
                } else {
                    ""
                });
                frame.push_str(&clip(&text, width.saturating_sub(margin * 2 + 1)));
                if y + 1 < height {
                    frame.push_str("\r\n");
                }
            }
        }
        frame.push_str("\x1b[0m");
        if frame != self.last {
            io::stdout().write_all(frame.as_bytes())?;
            io::stdout().flush()?;
            self.last = frame;
        }
        Ok(())
    }
    pub fn key(&mut self, timeout: Duration) -> io::Result<Option<Key>> {
        if let Some(key) = self.decoder.next(false) {
            return Ok(Some(key));
        }
        let mut poll = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe {
            libc::poll(
                &mut poll,
                1,
                timeout.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if result < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(io::Error::last_os_error());
        }
        if result == 0 {
            return Ok(self.decoder.next(true));
        }
        let mut bytes = [0; 1024];
        let count = io::stdin().read(&mut bytes)?;
        if count == 0 {
            return Ok(Some(Key::Interrupt));
        }
        self.decoder.push(&bytes[..count]);
        Ok(self.decoder.next(false))
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = io::stdout().write_all(b"\x1b[0m\x1b[?2004l\x1b[?25h\x1b[?1049l");
        let _ = io::stdout().flush();
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.saved);
        }
    }
}
// Restrict external device/error strings to printable ASCII. This prevents escape
// injection and gives deterministic cell widths without a Unicode dependency.
pub fn clip(text: &str, width: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_control() {
                c
            } else {
                '?'
            }
        })
        .collect();
    if clean.len() <= width {
        return clean;
    }
    if width < 3 {
        return clean.chars().take(width).collect();
    }
    format!("{}...", &clean[..width - 3])
}
#[derive(Default)]
pub struct Editor {
    pub text: Vec<char>,
    pub cursor: usize,
    selected: bool,
}
impl Editor {
    pub fn selected(value: &str) -> Self {
        let text: Vec<_> = value.chars().collect();
        Self {
            cursor: text.len(),
            selected: !text.is_empty(),
            text,
        }
    }
    pub fn value(&self) -> String {
        self.text.iter().collect()
    }
    pub fn key(&mut self, key: Key) {
        if self.selected {
            match &key {
                Key::Char(c) if c.is_ascii() && !c.is_control() => {
                    self.text.clear();
                    self.cursor = 0;
                    self.selected = false;
                }
                Key::Paste(_) => {} // first printable pasted character replaces the selection
                Key::Delete | Key::Backspace => {
                    self.text.clear();
                    self.cursor = 0;
                    self.selected = false;
                    return;
                }
                Key::Left | Key::Home => {
                    self.cursor = 0;
                    self.selected = false;
                    return;
                }
                Key::Right | Key::End => {
                    self.cursor = self.text.len();
                    self.selected = false;
                    return;
                }
                _ => {}
            }
        }
        match key {
            Key::Char(c) if c.is_ascii() && !c.is_control() && self.text.len() < 256 => {
                self.text.insert(self.cursor, c);
                self.cursor += 1;
            }
            Key::Paste(s) => {
                for c in s.chars().filter(|c| c.is_ascii() && !c.is_control()) {
                    self.key(Key::Char(c));
                }
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.text.len()),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = self.text.len(),
            Key::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.cursor);
            }
            Key::Delete if self.cursor < self.text.len() => {
                self.text.remove(self.cursor);
            }
            _ => {}
        }
    }
    pub fn display(&self, masked: bool, width: usize) -> String {
        if self.selected {
            let value = if masked {
                "*".repeat(self.text.len())
            } else {
                self.value()
            };
            return clip(&format!("[{value}]"), width);
        }
        let start = self.cursor.saturating_sub(width.saturating_sub(2));
        let mut output = String::new();
        for index in start..=self.text.len() {
            if index == self.cursor {
                output.push('|');
            }
            if let Some(c) = self.text.get(index) {
                output.push(if masked { '*' } else { *c });
            }
        }
        clip(&output, width)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_keys_paste_and_editing() {
        let mut decoder = Decoder::default();
        decoder.push(b"\x1b[");
        assert_eq!(decoder.next(false), None);
        decoder.push(b"A");
        assert_eq!(decoder.next(false), Some(Key::Up));
        decoder.push(b"\x1b[200~127.0.0.1:5555\n\x1b[201~");
        let mut editor = Editor::default();
        editor.key(decoder.next(false).unwrap());
        editor.key(Key::Left);
        editor.key(Key::Delete);
        editor.key(Key::Char('6'));
        assert_eq!(editor.value(), "127.0.0.1:5556");
        assert!(!editor.display(true, 12).contains('5'));
        decoder.push(b"\x1b");
        assert_eq!(decoder.next(true), Some(Key::Escape));
        assert_eq!(clip("bad\x1b[2J\n", 20), "bad?[2J?");
        let mut selected = Editor::selected("5555");
        assert_eq!(selected.display(false, 20), "[5555]");
        selected.key(Key::Paste("43210".into()));
        assert_eq!(selected.value(), "43210");
        let mut selected = Editor::selected("5555");
        selected.key(Key::Left);
        selected.key(Key::Delete);
        assert_eq!(selected.value(), "555");
    }
}
