//! Spec 007: named sessions, `open`, `list`, `status`, `stop`, `key`, `wait`, `watch` and the extended
//! `send`, `ask` and `screen`, each against a real hosted program.

use std::{
    fs,
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

/// Generous: PowerShell takes seconds to start on a cold CI runner. Only reached when a test fails.
const TIMEOUT: Duration = Duration::from_secs(60);

#[cfg(unix)]
mod os {
    /// Prints `ready`, reads a line, prints `got:<line>`, then waits for one more line.
    pub const TWO_LINES: &[&str] = &[
        "sh",
        "-c",
        "printf ready; IFS= read -r line; printf '\\ngot:%s\\n' \"$line\"; IFS= read -r line",
    ];
    /// Asks a yes/no question, reads a line and prints `answer:<line>`.
    pub const PROMPT: &[&str] = &[
        "sh",
        "-c",
        "printf 'Continue? (y/n) '; IFS= read -r line; printf '\\nanswer:%s\\n' \"$line\"; IFS= read -r line",
    ];
    /// Prints the value of VV_TEST and the working directory.
    pub const ENVIRONMENT: &[&str] = &[
        "sh",
        "-c",
        "printf 'env=%s dir=%s\\n' \"$VV_TEST\" \"$(basename \"$(pwd)\")\"; IFS= read -r line",
    ];
}

#[cfg(windows)]
mod os {
    pub const TWO_LINES: &[&str] = &[
        "powershell",
        "-NoProfile",
        "-Command",
        "[Console]::Write('ready'); $l = [Console]::ReadLine(); [Console]::WriteLine(''); [Console]::WriteLine('got:' + $l); [void][Console]::ReadLine()",
    ];
    pub const PROMPT: &[&str] = &[
        "powershell",
        "-NoProfile",
        "-Command",
        "[Console]::Write('Continue? (y/n) '); $l = [Console]::ReadLine(); [Console]::WriteLine(''); [Console]::WriteLine('answer:' + $l); [void][Console]::ReadLine()",
    ];
    pub const ENVIRONMENT: &[&str] = &[
        "powershell",
        "-NoProfile",
        "-Command",
        "[Console]::WriteLine('env=' + $env:VV_TEST + ' dir=' + (Split-Path -Leaf (Get-Location))); [void][Console]::ReadLine()",
    ];
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_agent-cli-lab")
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < TIMEOUT, "condition timed out");
        thread::sleep(Duration::from_millis(30));
    }
}

/// An isolated session directory and the hosts started in it. Everything is killed when dropped.
struct Lab {
    directory: tempfile::TempDir,
    hosts: Vec<Child>,
}

impl Lab {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().unwrap(),
            hosts: Vec::new(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(binary());
        command
            .env("AGENT_CLI_LAB_DIR", self.directory.path())
            .env_remove("AGENT_CLI_LAB_SOCKET")
            .env_remove("AGENT_CLI_LAB_NAME");
        command.args(args);
        command
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.command(args).stdin(Stdio::null()).output().unwrap()
    }

    fn host(&mut self, name: &str, extra: &[&str], program: &[&str]) {
        let mut command = self.command(&["host", "--name", name, "--cols", "60", "--rows", "10"]);
        command
            .args(extra)
            .arg("--")
            .args(program)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.hosts.push(command.spawn().expect("host should start"));
        wait_until(|| self.cli(&["status", "-n", name]).status.success());
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?}: {error}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        for host in &mut self.hosts {
            let _ = host.kill();
            let _ = host.wait();
        }
    }
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_named_session_is_listed_reachable_and_stopped() {
    let mut lab = Lab::new();
    lab.host("alpha", &[], os::TWO_LINES);

    let listed = lab.json(&["list", "--json"]);
    let entries = listed.as_array().unwrap();
    assert_eq!(entries.len(), 1, "{listed}");
    assert_eq!(entries[0]["name"], "alpha");
    assert_eq!(entries[0]["cols"], 60);
    assert!(entries[0]["pid"].as_u64().unwrap() > 0);

    let answer = lab.cli(&["ask", "-n", "alpha", "hello"]);
    assert!(
        answer.status.success(),
        "{}",
        String::from_utf8_lossy(&answer.stderr)
    );
    assert!(text(&answer).contains("got:hello"), "{}", text(&answer));

    let stopped = lab.cli(&["stop", "-n", "alpha"]);
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    wait_until(|| lab.json(&["list", "--json"]).as_array().unwrap().is_empty());
    assert!(
        !lab.cli(&["status", "-n", "alpha"]).status.success(),
        "the session is gone"
    );
}

#[test]
fn wait_matches_a_pattern_and_times_out_with_124() {
    let mut lab = Lab::new();
    lab.host("w", &[], os::TWO_LINES);

    let found = lab.cli(&["wait", "-n", "w", "--for", "rea+dy", "--tail", "3"]);
    assert!(found.status.success());
    assert!(text(&found).contains("ready"));

    let missing = lab.cli(&[
        "wait",
        "-n",
        "w",
        "--for",
        "never-on-screen",
        "-t",
        "1",
        "--quiet",
    ]);
    assert_eq!(missing.status.code(), Some(124));
    assert!(missing.stdout.is_empty(), "--quiet prints nothing");

    let nothing = lab.cli(&["wait", "-n", "w"]);
    assert!(
        !nothing.status.success(),
        "a wait without a condition is an error"
    );
    assert!(String::from_utf8_lossy(&nothing.stderr).contains("needs a condition"));
}

#[test]
fn a_dialog_is_detected_and_answered_with_named_keys() {
    let mut lab = Lab::new();
    lab.host("p", &[], os::PROMPT);

    let asking = lab.cli(&["wait", "-n", "p", "--asking", "-t", "30"]);
    assert!(
        asking.status.success(),
        "{}",
        String::from_utf8_lossy(&asking.stderr)
    );
    assert!(text(&asking).contains("(y/n)"));
    assert_eq!(
        lab.json(&["status", "-n", "p", "--json"])["state"],
        "asking"
    );

    assert!(lab.cli(&["key", "-n", "p", "y", "Enter"]).status.success());
    let answered = lab.cli(&["wait", "-n", "p", "--for", "answer:y", "-t", "30"]);
    assert!(answered.status.success(), "{}", text(&answered));
}

#[test]
fn status_tells_working_from_stalled_and_reports_the_host() {
    let mut lab = Lab::new();
    lab.host("s", &[], os::TWO_LINES);
    assert!(
        lab.cli(&["wait", "-n", "s", "--for", "ready", "-q"])
            .status
            .success()
    );
    thread::sleep(Duration::from_millis(700));

    let status = lab.json(&["status", "-n", "s", "--json", "--stalled-ms", "300"]);
    assert_eq!(status["state"], "stalled", "{status}");
    assert_eq!(status["running"], true);
    assert_eq!(
        (status["cols"].as_u64(), status["rows"].as_u64()),
        (Some(60), Some(10))
    );
    assert_eq!(status["name"], "s");
    assert!(status["quiet_ms"].as_u64().unwrap() >= 300);

    let busy = lab.json(&["status", "-n", "s", "--json", "--stalled-ms", "3600000"]);
    assert_eq!(
        busy["state"], "working",
        "a long stall limit keeps it working: {busy}"
    );

    let plain = lab.cli(&["status", "-n", "s"]);
    assert!(text(&plain).contains("state:"), "{}", text(&plain));
}

#[test]
fn watch_reports_a_stall_once_and_exits() {
    let mut lab = Lab::new();
    lab.host("w2", &[], os::TWO_LINES);
    let output = lab.cli(&[
        "watch",
        "-n",
        "w2",
        "--once",
        "--only",
        "stalled",
        "--stalled-ms",
        "400",
        "--interval-ms",
        "100",
        "--json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let event: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["state"], "stalled");
    assert!(event["screen_tail"].as_str().unwrap().contains("ready"));
}

#[test]
fn screen_can_be_trimmed_and_read_as_json() {
    let mut lab = Lab::new();
    lab.host("j", &[], os::TWO_LINES);
    assert!(
        lab.cli(&["wait", "-n", "j", "--for", "ready", "-q"])
            .status
            .success()
    );

    let json = lab.json(&["screen", "-n", "j", "--json", "--spans"]);
    assert_eq!(json["rows"], 10);
    assert_eq!(json["cols"], 60);
    assert_eq!(json["lines"].as_array().unwrap().len(), 10);
    assert!(json["lines"][0].as_str().unwrap().contains("ready"));
    assert!(json["cursor"].is_array());
    assert!(json["spans"].is_array());

    let tail = lab.cli(&["screen", "-n", "j", "--tail", "1"]);
    assert_eq!(
        text(&tail).trim(),
        text(&tail).trim_end(),
        "no trailing spaces"
    );
    assert!(text(&tail).contains("ready"));
    assert_eq!(text(&tail).trim_end().lines().count(), 1);

    let compact = lab.cli(&["screen", "-n", "j", "--compact"]);
    assert!(
        !text(&compact).lines().any(|line| line.trim().is_empty()),
        "{}",
        text(&compact)
    );
}

#[test]
fn paced_typing_and_a_text_file_reach_the_program_intact() {
    let mut lab = Lab::new();
    lab.host("t", &[], os::TWO_LINES);
    let paced = lab.cli(&[
        "ask",
        "-n",
        "t",
        "--paced",
        "--chunk",
        "3",
        "--pause-ms",
        "10",
        "--enter-delay-ms",
        "10",
        "ünïcode text",
    ]);
    assert!(
        paced.status.success(),
        "{}",
        String::from_utf8_lossy(&paced.stderr)
    );
    assert!(
        text(&paced).contains("got:ünïcode text"),
        "no character split in the middle: {}",
        text(&paced)
    );

    let mut lab = Lab::new();
    lab.host("f", &[], os::TWO_LINES);
    let file = lab.directory.path().join("brief.txt");
    fs::write(&file, "from a file\n").unwrap();
    let asked = lab.cli(&["ask", "-n", "f", "--file", file.to_str().unwrap()]);
    assert!(text(&asked).contains("got:from a file"), "{}", text(&asked));
}

#[test]
fn ask_exits_124_when_the_pattern_never_shows() {
    let mut lab = Lab::new();
    lab.host("a", &[], os::TWO_LINES);
    let output = lab.cli(&[
        "ask",
        "-n",
        "a",
        "--until",
        "this never appears",
        "-t",
        "1",
        "--tail",
        "2",
    ]);
    assert_eq!(output.status.code(), Some(124));
    assert!(String::from_utf8_lossy(&output.stderr).contains("timed out"));
}

#[test]
fn cwd_and_env_reach_the_hosted_program() {
    let mut lab = Lab::new();
    let work = lab.directory.path().join("workdir");
    fs::create_dir(&work).unwrap();
    lab.host(
        "e",
        &["--env", "VV_TEST=marker", "--cwd", work.to_str().unwrap()],
        os::ENVIRONMENT,
    );
    let seen = lab.cli(&[
        "wait",
        "-n",
        "e",
        "--for",
        "env=marker dir=workdir",
        "-t",
        "30",
    ]);
    assert!(seen.status.success(), "{}", text(&seen));
}

#[test]
fn open_detached_names_a_session_that_list_and_stop_can_reach() {
    let lab = Lab::new();
    let mut arguments = vec![
        "open",
        "--detached",
        "-n",
        "beta",
        "-C",
        "70",
        "-R",
        "12",
        "--",
    ];
    arguments.extend_from_slice(os::TWO_LINES);
    let opened = lab.cli(&arguments);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    assert!(text(&opened).contains("opened beta"), "{}", text(&opened));

    let status = lab.json(&["status", "-n", "beta", "--json"]);
    assert_eq!(
        (status["cols"].as_u64(), status["rows"].as_u64()),
        (Some(70), Some(12))
    );
    assert_eq!(lab.json(&["list", "--json"])[0]["name"], "beta");

    let mut again = vec!["open", "--detached", "-n", "beta", "--"];
    again.extend_from_slice(os::TWO_LINES);
    let refused = lab.cli(&again);
    assert!(
        !refused.status.success(),
        "a second open with the same name is refused"
    );
    assert!(String::from_utf8_lossy(&refused.stderr).contains("already running"));

    assert!(lab.cli(&["stop", "-n", "beta"]).status.success());
    wait_until(|| lab.json(&["list", "--json"]).as_array().unwrap().is_empty());
}

#[test]
fn bad_names_and_missing_sessions_fail_clearly() {
    let lab = Lab::new();
    let bad = lab.cli(&["status", "-n", "../escape"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("invalid session name"));

    let missing = lab.cli(&["stop", "-n", "nobody"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("no session answers"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );

    let clash = lab.cli(&["send", "-s", "/tmp/x.sock", "-n", "both", "text"]);
    assert!(!clash.status.success(), "-s and -n cannot be combined");
}

#[test]
fn a_stale_registry_entry_is_pruned_by_list() {
    let lab = Lab::new();
    let stale = serde_json::json!({
        "name": "ghost", "pid": 1, "socket": lab.directory.path().join("ghost.sock").display().to_string(),
        "command": ["nothing"], "cols": 80, "rows": 24, "started_ms": 1,
    });
    fs::write(lab.directory.path().join("ghost.json"), stale.to_string()).unwrap();
    let shown = lab.json(&["list", "--json", "--all"]);
    assert_eq!(shown[0]["name"], "ghost");
    assert_eq!(shown[0]["state"], "dead");
    let pruned = lab.json(&["list", "--json"]);
    assert!(pruned.as_array().unwrap().is_empty(), "{pruned}");
    assert!(
        !Path::new(&lab.directory.path().join("ghost.json")).exists(),
        "the dead entry is removed"
    );
}
