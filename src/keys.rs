//! Named keys for `key` and `send --key` (spec 007).
//!
//! Replaces hand-written escape sequences such as `ESC [ B` with names a person can read:
//! `Down`, `Enter`, `Ctrl-C`, `Alt-x`, `F5`, and `Down*3` to repeat. A single character stands for
//! itself, so `key y Enter` answers a `(y/n)` prompt.

use anyhow::{Result, anyhow};

/// Most repeats one `Name*N` may ask for. A typo such as `Down*100000` should not flood a TUI.
const MAX_REPEAT: usize = 100;

/// Encodes `names` into the bytes a terminal would send. `application_cursor` is the child's DECCKM
/// mode: a program that turned it on expects `ESC O A` for the Up arrow instead of `ESC [ A`.
pub fn encode(names: &[String], application_cursor: bool) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for name in names {
        let (key, repeat) = split_repeat(name)?;
        let one = encode_one(key, application_cursor)?;
        for _ in 0..repeat {
            bytes.extend_from_slice(&one);
        }
    }
    Ok(bytes)
}

fn split_repeat(name: &str) -> Result<(&str, usize)> {
    // `*` alone is the character `*`; `Down*3` repeats.
    if let Some((key, count)) = name.rsplit_once('*')
        && !key.is_empty()
    {
        let count: usize = count
            .parse()
            .map_err(|_| anyhow!("invalid repeat count in {name:?}; write for example Down*3"))?;
        if !(1..=MAX_REPEAT).contains(&count) {
            return Err(anyhow!(
                "repeat count in {name:?} must be 1 to {MAX_REPEAT}"
            ));
        }
        return Ok((key, count));
    }
    Ok((name, 1))
}

fn encode_one(name: &str, application_cursor: bool) -> Result<Vec<u8>> {
    let lower = name.to_ascii_lowercase();
    let arrow = |letter: char| -> Vec<u8> {
        let introducer = if application_cursor { 'O' } else { '[' };
        format!("\x1b{introducer}{letter}").into_bytes()
    };
    let fixed: Option<&str> = match lower.as_str() {
        "enter" | "return" | "cr" => Some("\r"),
        "esc" | "escape" => Some("\x1b"),
        "tab" => Some("\t"),
        "shift-tab" | "backtab" => Some("\x1b[Z"),
        "space" => Some(" "),
        "backspace" | "bs" => Some("\x7f"),
        "delete" | "del" => Some("\x1b[3~"),
        "insert" | "ins" => Some("\x1b[2~"),
        "pageup" | "pgup" => Some("\x1b[5~"),
        "pagedown" | "pgdn" => Some("\x1b[6~"),
        "f1" => Some("\x1bOP"),
        "f2" => Some("\x1bOQ"),
        "f3" => Some("\x1bOR"),
        "f4" => Some("\x1bOS"),
        "f5" => Some("\x1b[15~"),
        "f6" => Some("\x1b[17~"),
        "f7" => Some("\x1b[18~"),
        "f8" => Some("\x1b[19~"),
        "f9" => Some("\x1b[20~"),
        "f10" => Some("\x1b[21~"),
        "f11" => Some("\x1b[23~"),
        "f12" => Some("\x1b[24~"),
        _ => None,
    };
    if let Some(sequence) = fixed {
        return Ok(sequence.as_bytes().to_vec());
    }
    match lower.as_str() {
        "up" => return Ok(arrow('A')),
        "down" => return Ok(arrow('B')),
        "right" => return Ok(arrow('C')),
        "left" => return Ok(arrow('D')),
        "home" => return Ok(arrow('H')),
        "end" => return Ok(arrow('F')),
        _ => {}
    }
    if let Some(rest) = lower
        .strip_prefix("ctrl-")
        .or_else(|| lower.strip_prefix("c-"))
    {
        return control(rest, name);
    }
    if let Some(rest) = name
        .strip_prefix("Alt-")
        .or_else(|| name.strip_prefix("alt-"))
        .or_else(|| name.strip_prefix("M-"))
    {
        let mut bytes = vec![0x1b];
        bytes.extend_from_slice(&encode_one(rest, application_cursor)?);
        return Ok(bytes);
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(only), None) => Ok(only.to_string().into_bytes()),
        _ => Err(anyhow!(
            "unknown key {name:?}; try Enter, Esc, Tab, Up, Down, Left, Right, Home, End, PageUp, PageDown, \
             Backspace, Delete, Space, F1-F12, Ctrl-X, Alt-X, a single character, or Name*N to repeat"
        )),
    }
}

/// `Ctrl-A` to `Ctrl-Z` and the punctuation keys that have a control code.
fn control(rest: &str, original: &str) -> Result<Vec<u8>> {
    let mut chars = rest.chars();
    let (Some(letter), None) = (chars.next(), chars.next()) else {
        return Err(anyhow!(
            "unknown key {original:?}; write Ctrl- followed by one character"
        ));
    };
    let code = match letter {
        'a'..='z' => letter as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' | '/' => 0x1f,
        '?' => 0x7f,
        _ => {
            return Err(anyhow!(
                "unknown key {original:?}; Ctrl- takes a letter or one of @ [ \\ ] ^ _ ?"
            ));
        }
    };
    Ok(vec![code])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str], application: bool) -> Vec<u8> {
        encode(
            &names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
            application,
        )
        .unwrap()
    }

    #[test]
    fn named_keys_match_a_terminal() {
        assert_eq!(keys(&["Enter"], false), b"\r");
        assert_eq!(keys(&["esc"], false), b"\x1b");
        assert_eq!(keys(&["TAB"], false), b"\t");
        assert_eq!(keys(&["Shift-Tab"], false), b"\x1b[Z");
        assert_eq!(keys(&["Backspace"], false), b"\x7f");
        assert_eq!(keys(&["Delete", "PageDown"], false), b"\x1b[3~\x1b[6~");
        assert_eq!(keys(&["F1", "F5", "F12"], false), b"\x1bOP\x1b[15~\x1b[24~");
    }

    #[test]
    fn arrows_follow_the_application_cursor_mode() {
        assert_eq!(keys(&["Down"], false), b"\x1b[B");
        assert_eq!(keys(&["Down"], true), b"\x1bOB");
        assert_eq!(
            keys(&["Up", "Left", "Right", "Home", "End"], false),
            b"\x1b[A\x1b[D\x1b[C\x1b[H\x1b[F"
        );
    }

    #[test]
    fn control_alt_and_plain_characters() {
        assert_eq!(keys(&["Ctrl-C"], false), [0x03]);
        assert_eq!(keys(&["ctrl-d"], false), [0x04]);
        assert_eq!(keys(&["Ctrl-]"], false), [0x1d]);
        assert_eq!(keys(&["Alt-x"], false), b"\x1bx");
        assert_eq!(keys(&["Alt-Enter"], false), b"\x1b\r");
        assert_eq!(keys(&["y", "é"], false), "yé".as_bytes());
        assert_eq!(keys(&["*"], false), b"*");
    }

    #[test]
    fn repeats_are_bounded() {
        assert_eq!(keys(&["Down*3", "Enter"], false), b"\x1b[B\x1b[B\x1b[B\r");
        assert!(encode(&["Down*0".into()], false).is_err());
        assert!(encode(&["Down*101".into()], false).is_err());
        assert!(encode(&["Down*x".into()], false).is_err());
    }

    #[test]
    fn unknown_keys_are_errors_not_guesses() {
        let error = encode(&["Hyper".into()], false).unwrap_err().to_string();
        assert!(error.contains("unknown key"), "{error}");
        assert!(encode(&["Ctrl-".into()], false).is_err());
        assert!(encode(&["Ctrl-ab".into()], false).is_err());
        assert!(encode(&["Ctrl-é".into()], false).is_err());
    }
}
