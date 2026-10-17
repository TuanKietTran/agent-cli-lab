//! What a hosted screen means (spec 007): when it is waiting for an answer, when it has gone quiet,
//! and the conditions `wait` and `ask --until` block on.
//!
//! The same code runs inside the host (so a wait costs one request) and in the client (the
//! fallback for hosts that predate the `wait` operation), so both always agree.

use std::sync::OnceLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Phrases of the confirmation and choice dialogs that agent TUIs and command-line tools show.
/// Matching is case-insensitive and looks only at the end of the screen (see [`ASKING_WINDOW`]).
const ASKING: &str = r"(?i)enter to select|esc to cancel|do you want to|would you like to|\(y/n\)|\[y/n\]|\(yes/no\)|press enter to continue|allow (this|once|always)|wants to (select|use|navigate|access|run)|needs your permission|❯ ?1\.";

/// How many of the last non-empty lines may hold a dialog. A dialog sits at the bottom; the same
/// words higher up are earlier conversation.
pub const ASKING_WINDOW: usize = 20;

/// A screen that stays unchanged this long (milliseconds) is quiet; below it a dialog may still be drawing.
pub const SETTLE_MS: u64 = 400;

pub fn asking_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(ASKING).expect("the asking pattern is valid"))
}

/// The last `count` non-empty lines of `screen`, trailing spaces removed.
pub fn tail_lines(screen: &str, count: usize) -> String {
    let lines: Vec<&str> = screen
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

/// Whether the bottom of `screen` shows a question or confirmation dialog.
pub fn is_asking(screen: &str) -> bool {
    asking_regex().is_match(&tail_lines(screen, ASKING_WINDOW))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Output arrived recently: the program is drawing or computing.
    Working,
    /// A dialog is waiting for an answer.
    Asking,
    /// No output for a while and no dialog: the program finished its turn and waits for input.
    Stalled,
    /// The host is gone.
    Gone,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Asking => "asking",
            State::Stalled => "stalled",
            State::Gone => "gone",
        }
    }
}

/// Classifies a screen from how long it has been quiet (no output) for.
pub fn classify(screen: &str, quiet_ms: u64, stalled_ms: u64) -> State {
    if quiet_ms >= SETTLE_MS && is_asking(screen) {
        State::Asking
    } else if quiet_ms >= stalled_ms {
        State::Stalled
    } else {
        State::Working
    }
}

/// What `wait` blocks on. All conditions that are set must hold at the same moment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WaitSpec {
    /// No output for this many milliseconds.
    #[serde(default)]
    pub idle_ms: Option<u64>,
    /// Every regular expression must match somewhere on the screen.
    #[serde(default)]
    pub patterns: Vec<String>,
    /// A dialog is showing (see [`is_asking`]).
    #[serde(default)]
    pub asking: bool,
    /// None of these regular expressions may match any more.
    #[serde(default)]
    pub gone: Vec<String>,
}

pub struct Compiled {
    idle_ms: Option<u64>,
    patterns: Vec<Regex>,
    asking: bool,
    gone: Vec<Regex>,
}

impl WaitSpec {
    pub fn is_empty(&self) -> bool {
        self.idle_ms.is_none() && self.patterns.is_empty() && !self.asking && self.gone.is_empty()
    }

    pub fn compile(&self) -> Result<Compiled> {
        let build = |sources: &[String]| -> Result<Vec<Regex>> {
            sources
                .iter()
                .map(|source| {
                    Regex::new(source)
                        .with_context(|| format!("invalid regular expression {source:?}"))
                })
                .collect()
        };
        Ok(Compiled {
            idle_ms: self.idle_ms,
            patterns: build(&self.patterns)?,
            asking: self.asking,
            gone: build(&self.gone)?,
        })
    }
}

impl Compiled {
    /// Whether the condition holds for `screen`, which has had no output for `quiet_ms`.
    pub fn met(&self, screen: &str, quiet_ms: u64) -> bool {
        if let Some(idle) = self.idle_ms
            && quiet_ms < idle
        {
            return false;
        }
        if self.asking && !(quiet_ms >= SETTLE_MS && is_asking(screen)) {
            return false;
        }
        self.patterns.iter().all(|pattern| pattern.is_match(screen))
            && !self.gone.iter().any(|pattern| pattern.is_match(screen))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIALOG: &str = "history line\n\nTwo browsers are connected. Which one?\n❯ 1. Browser 1\n  2. Browser 2\nEnter to select · ↑/↓ to navigate · Esc to cancel\n";

    #[test]
    fn recognises_dialogs_at_the_bottom() {
        assert!(is_asking(DIALOG));
        assert!(is_asking("Overwrite the file? (y/n)"));
        assert!(is_asking("Continue [Y/n]"));
        assert!(is_asking(
            "Claude in Chrome wants to select a browser\n 1. Allow\n 2. Deny"
        ));
        assert!(!is_asking("compiling...\nfinished in 3s"));
    }

    #[test]
    fn the_same_words_far_above_are_only_history() {
        let mut screen = String::from("Do you want to continue? (y/n)\n");
        for index in 0..30 {
            screen.push_str(&format!("later line {index}\n"));
        }
        assert!(!is_asking(&screen));
    }

    #[test]
    fn tail_keeps_the_last_non_empty_lines() {
        assert_eq!(tail_lines("a\n\nb  \n\nc\n\n", 2), "b\nc");
        assert_eq!(tail_lines("only", 5), "only");
        assert_eq!(tail_lines("", 3), "");
    }

    #[test]
    fn classification_uses_quiet_time_and_the_dialog() {
        assert_eq!(classify("thinking", 100, 8000), State::Working);
        assert_eq!(classify("thinking", 7999, 8000), State::Working);
        assert_eq!(classify("report ends here", 8000, 8000), State::Stalled);
        assert_eq!(classify(DIALOG, 1000, 8000), State::Asking);
        // a dialog that is still being drawn is not yet "asking"
        assert_eq!(classify(DIALOG, 100, 8000), State::Working);
        assert_eq!(State::Stalled.name(), "stalled");
    }

    #[test]
    fn a_prose_question_is_stalled_not_asking() {
        // The owner's two stalls: the worker ended its turn with a question in plain words.
        let screen = "Decision for you: should I continue with the docs-only parts, or un-snap the window?\n";
        assert_eq!(classify(screen, 12_000, 8000), State::Stalled);
    }

    #[test]
    fn wait_conditions_are_anded() {
        let spec = WaitSpec {
            idle_ms: Some(500),
            patterns: vec!["ready".into()],
            ..Default::default()
        };
        let compiled = spec.compile().unwrap();
        assert!(!compiled.met("ready", 100));
        assert!(!compiled.met("not yet", 900));
        assert!(compiled.met("all ready now", 900));
    }

    #[test]
    fn gone_means_no_longer_on_screen() {
        let spec = WaitSpec {
            gone: vec!["Working".into()],
            ..Default::default()
        };
        let compiled = spec.compile().unwrap();
        assert!(!compiled.met("Working... 3s", 0));
        assert!(compiled.met("done", 0));
    }

    #[test]
    fn asking_needs_the_screen_to_settle() {
        let compiled = WaitSpec {
            asking: true,
            ..Default::default()
        }
        .compile()
        .unwrap();
        assert!(!compiled.met(DIALOG, 50));
        assert!(compiled.met(DIALOG, SETTLE_MS));
    }

    #[test]
    fn a_bad_pattern_is_reported() {
        let error = WaitSpec {
            patterns: vec!["(".into()],
            ..Default::default()
        }
        .compile()
        .err()
        .unwrap();
        assert!(error.to_string().contains("invalid regular expression"));
        assert!(WaitSpec::default().is_empty());
    }
}
