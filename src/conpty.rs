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
}

#[cfg(windows)]
impl ConsoleModes {
    /// `input` and `output` say whether stdin and stdout are the console. Best effort: a handle
    /// that cannot be changed is left alone.
    pub fn enable(input: bool, output: bool) -> Self {
        use windows_sys::Win32::System::Console::{
            ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode,
            GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
        };

        let mut restore = Vec::new();
        for (wanted, device, flag) in [
            (input, STD_INPUT_HANDLE, ENABLE_VIRTUAL_TERMINAL_INPUT),
            (
                output,
                STD_OUTPUT_HANDLE,
                ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            ),
        ] {
            if !wanted {
                continue;
            }
            unsafe {
                let handle = GetStdHandle(device);
                let mut mode = 0_u32;
                if GetConsoleMode(handle, &mut mode) != 0
                    && SetConsoleMode(handle, mode | flag) != 0
                {
                    restore.push((handle, mode));
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
}
