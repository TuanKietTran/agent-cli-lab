#![cfg(unix)]

use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_agent-cli-lab")
}

#[test]
fn child_receives_a_tty_and_output_is_forwarded() {
    let output = Command::new(binary())
        .args(["run", "--", "sh", "-c", "test -t 0 && printf pty-ok"])
        .stdin(Stdio::null())
        .output()
        .expect("proxy should run");

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("pty-ok"));
}

#[test]
fn piped_input_reaches_child_and_eof_terminates_it() {
    let mut child = Command::new(binary())
        .args(["run", "--", "cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("proxy should start");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"agent input\n")
        .unwrap();

    let output = child.wait_with_output().expect("proxy should finish");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("agent input"));
}

#[test]
fn separate_calls_can_control_and_capture_a_hosted_tui() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("control.sock");
    let mut host = Command::new(binary())
        .args([
            "host",
            "--socket",
            socket.to_str().unwrap(),
            "--cols",
            "60",
            "--rows",
            "10",
            "--",
            "sh",
            "-c",
            "printf ready; IFS= read -r line; printf '\\ngot:%s\\n' \"$line\"; IFS= read -r line",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("host should start");

    wait_until(Duration::from_secs(3), || socket.exists());
    let sent = Command::new(binary())
        .args([
            "send",
            "--socket",
            socket.to_str().unwrap(),
            "--enter",
            "hello agent",
        ])
        .status()
        .unwrap();
    assert!(sent.success());

    let mut captured = String::new();
    wait_until(Duration::from_secs(3), || {
        let output = Command::new(binary())
            .args(["screen", "--socket", socket.to_str().unwrap()])
            .output()
            .unwrap();
        captured = String::from_utf8_lossy(&output.stdout).into_owned();
        captured.contains("got:hello agent")
    });
    assert!(captured.contains("ready"));

    let finished = Command::new(binary())
        .args([
            "send",
            "--socket",
            socket.to_str().unwrap(),
            "--enter",
            "finish",
        ])
        .status()
        .unwrap();
    assert!(finished.success());
    assert!(host.wait().unwrap().success());
    assert!(!socket.exists(), "host should remove its socket on exit");
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < timeout, "condition timed out");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn exit_status_and_jsonl_recording_are_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let recording = directory.path().join("session.jsonl");
    let status = Command::new(binary())
        .args([
            "run",
            "--record",
            recording.to_str().unwrap(),
            "--",
            "sh",
            "-c",
            "printf recorded; exit 7",
        ])
        .stdin(Stdio::null())
        .status()
        .expect("proxy should run");

    assert_eq!(status.code(), Some(7));
    let events: Vec<serde_json::Value> = fs::read_to_string(recording)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line must be JSON"))
        .collect();
    assert_eq!(events.first().unwrap()["type"], "header");
    assert!(events.iter().any(|event| event["type"] == "chunk"));
    assert_eq!(events.last().unwrap()["type"], "exit");
    assert_eq!(events.last().unwrap()["code"], 7);
}
