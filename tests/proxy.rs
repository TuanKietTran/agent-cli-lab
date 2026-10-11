use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Generous on purpose: PowerShell takes seconds to start on a cold CI runner. It is only reached
/// when a test fails, so that a failure is reported instead of hanging the job.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The child programs the scenarios run, per platform. Each does the same job on both.
#[cfg(unix)]
mod os {
    /// End of line as typed on this platform's terminal.
    pub const ENTER: &[u8] = b"\n";
    /// Prints `pty-ok` when its stdin is a terminal.
    pub const TTY_PROBE: &[&str] = &["sh", "-c", "test -t 0 && printf pty-ok"];
    /// Echoes its input and ends when its input ends.
    pub const UNTIL_END_OF_INPUT: &[&str] = &["cat"];
    /// Prints `ready`, reads a line, prints `got:<line>`, then waits for one more line.
    pub const TWO_LINES: &[&str] = &[
        "sh",
        "-c",
        "printf ready; IFS= read -r line; printf '\\ngot:%s\\n' \"$line\"; IFS= read -r line",
    ];
    /// Prints `recorded` and exits with status 7.
    pub const EXIT_SEVEN: &[&str] = &["sh", "-c", "printf recorded; exit 7"];
}

#[cfg(windows)]
mod os {
    pub const ENTER: &[u8] = b"\r";
    pub const TTY_PROBE: &[&str] = &[
        "powershell",
        "-NoProfile",
        "-Command",
        "if (-not [Console]::IsInputRedirected -and -not [Console]::IsOutputRedirected) { [Console]::Write('pty-ok') }",
    ];
    pub const UNTIL_END_OF_INPUT: &[&str] = &["sort"];
    pub const TWO_LINES: &[&str] = &[
        "powershell",
        "-NoProfile",
        "-Command",
        "[Console]::Write('ready'); $l = [Console]::ReadLine(); [Console]::WriteLine(''); [Console]::WriteLine('got:' + $l); [void][Console]::ReadLine()",
    ];
    pub const EXIT_SEVEN: &[&str] = &["cmd", "/c", "echo recorded & exit 7"];
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_agent-cli-lab")
}

fn wait_with_timeout(child: &mut Child) -> ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if start.elapsed() > TIMEOUT {
            let _ = child.kill();
            panic!("process did not exit within {TIMEOUT:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < TIMEOUT, "condition timed out");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Runs the proxy to completion. `stdin` is written and closed; without it stdin is null.
fn run_proxy(args: &[&str], child_command: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut proxy = Command::new(binary())
        .args(args)
        .arg("--")
        .args(child_command)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("proxy should start");
    if let (Some(bytes), Some(mut pipe)) = (stdin, proxy.stdin.take()) {
        pipe.write_all(bytes).unwrap();
    }
    let mut stdout = proxy.stdout.take().unwrap();
    let mut stderr = proxy.stderr.take().unwrap();
    let stdout = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let stderr = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let status = wait_with_timeout(&mut proxy);
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

/// A running `host`. It is killed when dropped so that a failing test leaks nothing.
struct Host {
    process: Child,
    socket: PathBuf,
    _directory: tempfile::TempDir,
}

impl Host {
    fn start(child_command: &[&str]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("control.sock");
        let process = Command::new(binary())
            .args(["host", "--socket"])
            .arg(&socket)
            .args(["--cols", "60", "--rows", "10", "--"])
            .args(child_command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("host should start");
        Self {
            process,
            socket,
            _directory: directory,
        }
    }

    fn call(&self, args: &[&str]) -> Output {
        Command::new(binary())
            .arg(args[0])
            .arg("--socket")
            .arg(&self.socket)
            .args(&args[1..])
            .output()
            .unwrap()
    }

    /// The rendered screen, or `None` while the host is not reachable.
    fn screen(&self) -> Option<String> {
        let output = self.call(&["screen"]);
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn wait_until_ready(&self) {
        wait_until(|| self.screen().is_some());
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[test]
fn child_receives_a_tty_and_output_is_forwarded() {
    let output = run_proxy(&["run"], os::TTY_PROBE, None);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("pty-ok"));
}

#[test]
fn piped_input_reaches_child_and_eof_terminates_it() {
    let input = [b"agent input", os::ENTER].concat();
    let output = run_proxy(&["run"], os::UNTIL_END_OF_INPUT, Some(&input));

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("agent input"));
}

/// Without a final newline the end-of-input key alone only completes the line, so the proxy has
/// to send one more to end the child (on Unix this is the writer's final newline and end-of-file).
#[test]
fn piped_input_without_a_final_newline_still_ends_the_child() {
    let output = run_proxy(&["run"], os::UNTIL_END_OF_INPUT, Some(b"agent input"));

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("agent input"));
}

#[test]
fn separate_calls_can_control_and_capture_a_hosted_tui() {
    let mut host = Host::start(os::TWO_LINES);
    host.wait_until_ready();

    assert!(
        host.call(&["send", "--enter", "hello agent"])
            .status
            .success()
    );

    let mut captured = String::new();
    wait_until(|| {
        captured = host.screen().unwrap_or_default();
        captured.contains("got:hello agent")
    });
    assert!(captured.contains("ready"));

    assert!(host.call(&["send", "--enter", "finish"]).status.success());
    assert!(wait_with_timeout(&mut host.process).success());
    assert!(host.screen().is_none(), "the host should be gone");
    #[cfg(unix)]
    assert!(
        !host.socket.exists(),
        "host should remove its socket on exit"
    );
}

#[test]
fn a_second_host_on_the_same_socket_is_refused() {
    let host = Host::start(os::TWO_LINES);
    host.wait_until_ready();

    let output = Command::new(binary())
        .args(["host", "--socket"])
        .arg(&host.socket)
        .arg("--")
        .args(os::TWO_LINES)
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to bind socket"));
    assert!(host.screen().is_some(), "the first host must keep working");
}

#[cfg(unix)]
#[test]
fn the_socket_is_private_to_the_owner() {
    use std::os::unix::fs::PermissionsExt;

    let host = Host::start(os::TWO_LINES);
    host.wait_until_ready();

    let mode = fs::metadata(&host.socket).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// conhost's cursor position request is its own handshake, not output of the child.
#[cfg(windows)]
#[test]
fn the_conpty_handshake_is_not_forwarded() {
    let output = run_proxy(&["run"], &["cmd", "/c", "echo visible"], None);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("visible"));
    assert!(!stdout.contains("\x1b[6n"), "{stdout:?}");
}

#[test]
fn exit_status_and_jsonl_recording_are_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let recording = directory.path().join("session.jsonl");
    let output = run_proxy(
        &["run", "--record", recording.to_str().unwrap()],
        os::EXIT_SEVEN,
        None,
    );

    assert_eq!(output.status.code(), Some(7));
    let events: Vec<serde_json::Value> = fs::read_to_string(recording)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line must be JSON"))
        .collect();
    assert_eq!(events.first().unwrap()["type"], "header");
    let recorded: Vec<u8> = events
        .iter()
        .filter(|event| event["type"] == "chunk" && event["stream"] == "output")
        .flat_map(|event| event["data"].as_array().unwrap())
        .map(|byte| byte.as_u64().unwrap() as u8)
        .collect();
    assert!(String::from_utf8_lossy(&recorded).contains("recorded"));
    assert_eq!(events.last().unwrap()["type"], "exit");
    assert_eq!(events.last().unwrap()["code"], 7);
}
