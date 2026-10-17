//! Windows console and ConPTY details (spec 006). On Unix nothing here has any effect.

use std::borrow::Cow;

/// What conhost sends first when the pseudo-console inherits the cursor position: a cursor
/// position request. It is conhost's own handshake, not output of the child.
const CURSOR_REQUEST: &[u8] = b"\x1b[6n";

/// Reply to [`CURSOR_REQUEST`]: the cursor is at row 1, column 1.
pub const CURSOR_REPLY: &[u8] = b"\x1b[1;1R";

/// Only the start of the output is searched; conhost sends the request before anything else.
const SEARCH_LIMIT: usize = 1024;

/// Removes conhost's cursor position request from the start of the output.
///
/// Assumes conhost writes the request in one piece, so a request split over two reads is not
/// recognized.
pub struct HandshakeFilter {
    remaining: usize,
}

impl Default for HandshakeFilter {
    fn default() -> Self {
        Self {
            remaining: SEARCH_LIMIT,
        }
    }
}

impl HandshakeFilter {
    /// Returns the bytes to pass on and whether the request was found (and has to be answered).
    pub fn filter<'a>(&mut self, chunk: &'a [u8]) -> (Cow<'a, [u8]>, bool) {
        if self.remaining == 0 {
            return (Cow::Borrowed(chunk), false);
        }
        if let Some(start) = chunk
            .windows(CURSOR_REQUEST.len())
            .position(|window| window == CURSOR_REQUEST)
        {
            self.remaining = 0;
            let mut rest = chunk.to_vec();
            rest.drain(start..start + CURSOR_REQUEST.len());
            return (Cow::Owned(rest), true);
        }
        self.remaining = self.remaining.saturating_sub(chunk.len());
        (Cow::Borrowed(chunk), false)
    }
}

/// Turns on virtual-terminal processing of this process's console while alive, so escape
/// sequences and special keys pass through classic conhost as they do on a Unix terminal, and
/// puts the previous modes back afterwards.
pub struct ConsoleModes {
    #[cfg(windows)]
    restore: Vec<(windows_sys::Win32::Foundation::HANDLE, u32)>,
}

#[cfg(not(windows))]
impl ConsoleModes {
    pub fn enable(_input: bool, _output: bool) -> Self {
        Self {}
    }

    pub fn enable_window(_input: bool, _output: bool) -> Self {
        Self {}
    }
}

/// Windows: stops this process's own standard handles from being inherited by a child it starts.
/// A background host that inherited the caller's output pipe would keep it open for as long as the
/// host lives, so anything that reads this command's output (a shell tool, `Command::output`) would
/// wait for the host to end instead of for the command to finish (spec 007).
pub fn stop_inheriting_std_handles() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::{
            Foundation::{HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation},
            System::Console::{
                GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            },
        };
        for device in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let handle = GetStdHandle(device);
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Windows: a host started in its own new console (`open`) was given NUL for its standard handles
/// so that it holds none of the caller's pipes. Point them at the console it owns instead, so the
/// window shows the hosted program and takes the person's keys. Does nothing when stdin already is
/// a console, or when there is no console.
pub fn adopt_own_console() {
    #[cfg(windows)]
    unsafe {
        use std::{fs::OpenOptions, os::windows::io::IntoRawHandle};
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            SetStdHandle,
        };
        let mut mode = 0_u32;
        if GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut mode) != 0 {
            return;
        }
        if let Ok(input) = OpenOptions::new().read(true).write(true).open("CONIN$") {
            SetStdHandle(STD_INPUT_HANDLE, input.into_raw_handle());
        }
        if let Ok(output) = OpenOptions::new().read(true).write(true).open("CONOUT$") {
            let handle = output.into_raw_handle();
            SetStdHandle(STD_OUTPUT_HANDLE, handle);
            SetStdHandle(STD_ERROR_HANDLE, handle);
        }
    }
}

/// Names this terminal window. On Windows it sets the console title; elsewhere it writes the
/// xterm "set window title" sequence, which terminals that do not know it ignore.
pub fn set_title(title: &str) {
    let clean: String = title.chars().filter(|c| !c.is_control()).collect();
    #[cfg(windows)]
    {
        use widestring::U16CString;
        if let Ok(wide) = U16CString::from_str(&clean) {
            unsafe { windows_sys::Win32::System::Console::SetConsoleTitleW(wide.as_ptr()) };
        }
    }
    #[cfg(not(windows))]
    {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]0;{clean}\x07");
        let _ = out.flush();
    }
}

/// Resizes this console window to `cols` x `rows` (the job of `mode con cols=.. lines=..`), so that a
/// window shows the whole hosted screen. Best effort and Windows only: another terminal is sized by
/// its own settings.
pub fn fit_window(cols: u16, rows: u16) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{
            COORD, GetStdHandle, SMALL_RECT, STD_OUTPUT_HANDLE, SetConsoleScreenBufferSize,
            SetConsoleWindowInfo,
        };
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        // The window must always fit inside the buffer: shrink it, resize the buffer, then grow it.
        let tiny = SMALL_RECT {
            Left: 0,
            Top: 0,
            Right: 1,
            Bottom: 1,
        };
        SetConsoleWindowInfo(handle, 1, &tiny);
        SetConsoleScreenBufferSize(
            handle,
            COORD {
                X: cols as i16,
                Y: rows as i16,
            },
        );
        let full = SMALL_RECT {
            Left: 0,
            Top: 0,
            Right: cols as i16 - 1,
            Bottom: rows as i16 - 1,
        };
        SetConsoleWindowInfo(handle, 1, &full);
    }
    #[cfg(not(windows))]
    let _ = (cols, rows);
}

#[cfg(windows)]
impl ConsoleModes {
    /// `input` and `output` say whether stdin and stdout are the console. Best effort: a handle
    /// that cannot be changed is left alone.
    pub fn enable(input: bool, output: bool) -> Self {
        Self::enable_with(input, output, false)
    }

    /// Like [`ConsoleModes::enable`] for a window a person watches (`host --stdio`, `attach`): also
    /// switches QuickEdit off. With QuickEdit on, one click in the window starts a text selection
    /// ("Select" in the title bar) and the console stops its process until Esc or Enter is pressed,
    /// which freezes the hosted program (spec 007).
    pub fn enable_window(input: bool, output: bool) -> Self {
        Self::enable_with(input, output, true)
    }

    fn enable_with(input: bool, output: bool, no_quick_edit: bool) -> Self {
        use windows_sys::Win32::System::Console::{
            ENABLE_EXTENDED_FLAGS, ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT,
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
            STD_OUTPUT_HANDLE, SetConsoleMode,
        };

        let mut restore = Vec::new();
        for (wanted, device, flag, is_input) in [
            (input, STD_INPUT_HANDLE, ENABLE_VIRTUAL_TERMINAL_INPUT, true),
            (
                output,
                STD_OUTPUT_HANDLE,
                ENABLE_VIRTUAL_TERMINAL_PROCESSING,
                false,
            ),
        ] {
            if !wanted {
                continue;
            }
            unsafe {
                let handle = GetStdHandle(device);
                let mut mode = 0_u32;
                if GetConsoleMode(handle, &mut mode) != 0 {
                    let mut wanted_mode = mode | flag;
                    if is_input && no_quick_edit {
                        // The extended-flags bit must be set for a change to the QuickEdit bit to apply.
                        wanted_mode =
                            (wanted_mode & !ENABLE_QUICK_EDIT_MODE) | ENABLE_EXTENDED_FLAGS;
                    }
                    if SetConsoleMode(handle, wanted_mode) != 0 {
                        restore.push((handle, mode));
                    }
                }
            }
        }
        Self { restore }
    }
}

#[cfg(windows)]
impl Drop for ConsoleModes {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::SetConsoleMode;
        for &(handle, mode) in self.restore.iter().rev() {
            unsafe { SetConsoleMode(handle, mode) };
        }
    }
}

/// Removes the sequences that set the window title (`ESC ] 0 ; text BEL`, also `1;`, `2;` and the `ESC \`
/// terminator) from the output shown in a window, so a title given with `--title` stays: many TUIs
/// and shells set their own, which would otherwise replace it (spec 007). Everything else, including
/// other `ESC ]` sequences, passes through unchanged. A sequence split over two reads is handled.
#[derive(Default)]
pub struct TitleFilter {
    state: TitleState,
    held: Vec<u8>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum TitleState {
    #[default]
    Plain,
    /// Saw ESC.
    Escape,
    /// Saw ESC ] and is reading the number.
    Number,
    /// In the text of a title sequence that is being dropped.
    Dropping,
    /// In the dropped text and saw ESC (a possible `ESC \` terminator).
    DroppingEscape,
}

impl TitleFilter {
    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len());
        for &byte in input {
            match self.state {
                TitleState::Plain => {
                    if byte == 0x1b {
                        self.held.clear();
                        self.held.push(byte);
                        self.state = TitleState::Escape;
                    } else {
                        output.push(byte);
                    }
                }
                TitleState::Escape => {
                    self.held.push(byte);
                    if byte == b']' {
                        self.state = TitleState::Number;
                    } else {
                        self.release(&mut output);
                        // The byte may itself start an escape sequence (ESC ESC ...).
                        if byte == 0x1b {
                            self.held.push(byte);
                            output.pop();
                            self.state = TitleState::Escape;
                        }
                    }
                }
                TitleState::Number => {
                    self.held.push(byte);
                    match byte {
                        b'0'..=b'9' if self.held.len() <= 6 => {}
                        b';' => {
                            let number = &self.held[2..self.held.len() - 1];
                            if matches!(number, b"0" | b"1" | b"2") {
                                self.held.clear();
                                self.state = TitleState::Dropping;
                            } else {
                                self.release(&mut output);
                            }
                        }
                        _ => self.release(&mut output),
                    }
                }
                TitleState::Dropping => match byte {
                    0x07 => self.state = TitleState::Plain,
                    0x1b => self.state = TitleState::DroppingEscape,
                    _ => {}
                },
                TitleState::DroppingEscape => {
                    self.state = if byte == b'\\' {
                        TitleState::Plain
                    } else {
                        TitleState::Dropping
                    };
                }
            }
        }
        output
    }

    /// Passes the held bytes through: they turned out not to be a title sequence.
    fn release(&mut self, output: &mut Vec<u8>) {
        output.append(&mut self.held);
        self.state = TitleState::Plain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_the_request_and_asks_for_a_reply() {
        let mut filter = HandshakeFilter::default();
        let (data, reply) = filter.filter(b"\x1b[6nhello");
        assert_eq!(&*data, b"hello");
        assert!(reply);
    }

    #[test]
    fn leaves_output_without_a_request_untouched() {
        let mut filter = HandshakeFilter::default();
        let (data, reply) = filter.filter(b"hello \x1b[1mworld");
        assert_eq!(&*data, b"hello \x1b[1mworld");
        assert!(!reply);
    }

    #[test]
    fn only_the_first_request_is_a_handshake() {
        let mut filter = HandshakeFilter::default();
        assert!(filter.filter(b"\x1b[6n").1);
        let (data, reply) = filter.filter(b"\x1b[6n");
        assert_eq!(&*data, b"\x1b[6n");
        assert!(!reply);
    }

    #[test]
    fn stops_looking_after_the_start_of_the_output() {
        let mut filter = HandshakeFilter::default();
        assert!(!filter.filter(&[b'x'; SEARCH_LIMIT]).1);
        let (data, reply) = filter.filter(b"\x1b[6n");
        assert_eq!(&*data, b"\x1b[6n");
        assert!(!reply);
    }

    fn filtered(chunks: &[&[u8]]) -> Vec<u8> {
        let mut filter = TitleFilter::default();
        chunks
            .iter()
            .flat_map(|chunk| filter.filter(chunk))
            .collect()
    }

    #[test]
    fn title_sequences_are_removed_and_text_is_kept() {
        assert_eq!(filtered(&[b"a\x1b]0;my title\x07b"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b]2;my title\x1b\\b"]), b"ab");
        assert_eq!(filtered(&[b"\x1b]1;icon\x07\x1b]2;win\x07x"]), b"x");
    }

    #[test]
    fn other_sequences_pass_through_untouched() {
        let colours: &[u8] = b"\x1b[31mred\x1b[0m";
        assert_eq!(filtered(&[colours]), colours);
        let hyperlink: &[u8] = b"\x1b]8;;http://x\x07link\x1b]8;;\x07";
        assert_eq!(filtered(&[hyperlink]), hyperlink);
        let other_osc: &[u8] = b"\x1b]52;c;abcd\x07";
        assert_eq!(filtered(&[other_osc]), other_osc);
        assert_eq!(filtered(&[b"\x1b\x1b[Kx"]), b"\x1b\x1b[Kx");
    }

    #[test]
    fn a_sequence_split_over_reads_is_still_removed() {
        assert_eq!(filtered(&[b"a\x1b", b"]0;ti", b"tle", b"\x07b"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b]2;t", b"\x1b", b"\\b"]), b"ab");
        assert_eq!(filtered(&[b"\x1b", b"[1mbold"]), b"\x1b[1mbold");
    }
}
