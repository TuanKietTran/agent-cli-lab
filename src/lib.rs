use std::{
    ffi::OsString,
    fs::File,
    io::{self, BufRead, BufReader, IsTerminal, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(
    name = "agent-cli-lab",
    version,
    about = "Test and control interactive CLI programs through a pseudo-terminal",
    after_help = "EXAMPLES:\n  agent-cli-lab run -- python3 -i\n  agent-cli-lab host --socket /tmp/pi.sock -- pi\n  agent-cli-lab send --socket /tmp/pi.sock --enter 'Hello'\n  agent-cli-lab screen --socket /tmp/pi.sock\n\nRecordings contain all input and output and may contain secrets."
)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run a command and proxy this process's input/output.
    Run(RunArgs),
    /// Host a PTY that separate agent-cli-lab calls can control.
    Host(HostArgs),
    /// Send text or control keys to a hosted PTY.
    Send(SendArgs),
    /// Print the current rendered screen of a hosted PTY.
    Screen(SocketArgs),
}

#[derive(Debug, Args)]
struct RunArgs {
    #[command(flatten)]
    terminal: TerminalArgs,
    /// Command and arguments. Place these after `--`.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<OsString>,
}

#[derive(Debug, Args)]
struct HostArgs {
    #[command(flatten)]
    terminal: TerminalArgs,
    /// Unix socket used by `send` and `screen`.
    #[arg(long, value_name = "PATH")]
    socket: PathBuf,
    /// Also display the TUI and accept keyboard input in this terminal.
    #[arg(long)]
    stdio: bool,
    /// Command and arguments. Place these after `--`.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<OsString>,
}

#[derive(Debug, Args)]
struct TerminalArgs {
    /// Write input, output, and exit events as JSON Lines.
    #[arg(long, value_name = "PATH")]
    record: Option<PathBuf>,
    /// Terminal width in columns.
    #[arg(long, default_value_t = 80)]
    cols: u16,
    /// Terminal height in rows.
    #[arg(long, default_value_t = 24)]
    rows: u16,
}

#[derive(Debug, Args)]
struct SocketArgs {
    /// Unix socket of the running host.
    #[arg(long, value_name = "PATH")]
    socket: PathBuf,
}

#[derive(Debug, Args)]
struct SendArgs {
    #[command(flatten)]
    connection: SocketArgs,
    /// Text to type. Exact UTF-8 bytes are sent.
    #[arg(default_value = "")]
    text: String,
    /// Append a terminal Enter key (carriage return).
    #[arg(long)]
    enter: bool,
    /// Append Ctrl-C.
    #[arg(long)]
    ctrl_c: bool,
    /// Append Ctrl-D.
    #[arg(long)]
    ctrl_d: bool,
}

pub fn execute(cli: Cli) -> Result<i32> {
    match cli.command {
        Commands::Run(args) => run(args),
        Commands::Host(args) => host(args),
        Commands::Send(args) => send(args),
        Commands::Screen(args) => screen(args),
    }
}

fn validate_terminal(args: &TerminalArgs) -> Result<()> {
    if args.cols == 0 || args.rows == 0 {
        return Err(anyhow!(
            "terminal rows and columns must be greater than zero"
        ));
    }
    Ok(())
}

fn create_recorder(
    args: &TerminalArgs,
    command: &[OsString],
) -> Result<Option<Arc<Mutex<Recorder>>>> {
    let recorder = args
        .record
        .clone()
        .map(Recorder::create)
        .transpose()?
        .map(|value| Arc::new(Mutex::new(value)));
    if let Some(recorder) = &recorder {
        recorder.lock().unwrap().event(json!({
            "type": "header",
            "command": command.iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>(),
            "cols": args.cols,
            "rows": args.rows,
        }))?;
    }
    Ok(recorder)
}

fn open_pty(
    args: &TerminalArgs,
    child_command: &[OsString],
) -> Result<(
    Box<dyn portable_pty::Child + Send + Sync>,
    Box<dyn Read + Send>,
    Box<dyn Write + Send>,
)> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: args.rows,
            cols: args.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("failed to open pseudo-terminal")?;
    let mut command = CommandBuilder::new(&child_command[0]);
    command.cwd(std::env::current_dir().context("failed to resolve current directory")?);
    for arg in &child_command[1..] {
        command.arg(arg);
    }
    let child = pair
        .slave
        .spawn_command(command)
        .with_context(|| format!("failed to start {:?}", child_command[0]))?;
    drop(pair.slave);
    let reader = pair
        .master
        .try_clone_reader()
        .context("failed to open PTY output")?;
    let writer = pair
        .master
        .take_writer()
        .context("failed to open PTY input")?;
    Ok((child, reader, writer))
}

fn run(args: RunArgs) -> Result<i32> {
    validate_terminal(&args.terminal)?;
    let recorder = create_recorder(&args.terminal, &args.command)?;
    let (mut child, mut reader, mut writer) = open_pty(&args.terminal, &args.command)?;

    let stdin_is_terminal = io::stdin().is_terminal();
    let _raw_mode = RawModeGuard::new(stdin_is_terminal)?;
    let input_recorder = recorder.clone();
    thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buffer = [0_u8; 8192];
        loop {
            let count = match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            };
            if let Some(recorder) = &input_recorder {
                let _ = recorder.lock().unwrap().chunk("input", &buffer[..count]);
            }
            if writer.write_all(&buffer[..count]).is_err() || writer.flush().is_err() {
                return;
            }
        }
        if !stdin_is_terminal {
            let _ = writer.write_all(&[0x04]);
            let _ = writer.flush();
        }
    });

    let mut stdout = io::stdout().lock();
    copy_output(&mut reader, |bytes| {
        stdout.write_all(bytes)?;
        stdout.flush()?;
        if let Some(recorder) = &recorder {
            recorder.lock().unwrap().chunk("output", bytes)?;
        }
        Ok(())
    })?;
    finish_child(&mut *child, recorder)
}

fn host(args: HostArgs) -> Result<i32> {
    validate_terminal(&args.terminal)?;
    let recorder = create_recorder(&args.terminal, &args.command)?;
    let (mut child, mut reader, writer) = open_pty(&args.terminal, &args.command)?;
    let writer = Arc::new(Mutex::new(writer));
    let parser = Arc::new(Mutex::new(vt100::Parser::new(
        args.terminal.rows,
        args.terminal.cols,
        0,
    )));
    let stopped = Arc::new(AtomicBool::new(false));
    let socket_guard = SocketGuard::bind(&args.socket)?;
    let listener = socket_guard.listener.try_clone()?;
    let listener_writer = writer.clone();
    let listener_parser = parser.clone();
    let listener_recorder = recorder.clone();
    let listener_stopped = stopped.clone();
    let listener_thread = thread::spawn(move || {
        serve_socket(
            listener,
            listener_writer,
            listener_parser,
            listener_recorder,
            listener_stopped,
        )
    });

    let stdin_is_terminal = io::stdin().is_terminal();
    let _raw_mode = RawModeGuard::new(args.stdio && stdin_is_terminal)?;
    if args.stdio {
        let terminal_writer = writer.clone();
        let terminal_recorder = recorder.clone();
        thread::spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut buffer = [0_u8; 8192];
            loop {
                let count = match stdin.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                if let Some(recorder) = &terminal_recorder {
                    let _ = recorder.lock().unwrap().chunk("input", &buffer[..count]);
                }
                let mut output = terminal_writer.lock().unwrap();
                if output.write_all(&buffer[..count]).is_err() || output.flush().is_err() {
                    break;
                }
            }
        });
    }

    let mut stdout = io::stdout().lock();
    copy_output(&mut reader, |bytes| {
        parser.lock().unwrap().process(bytes);
        if args.stdio {
            stdout.write_all(bytes)?;
            stdout.flush()?;
        }
        if let Some(recorder) = &recorder {
            recorder.lock().unwrap().chunk("output", bytes)?;
        }
        Ok(())
    })?;

    let code = finish_child(&mut *child, recorder)?;
    stopped.store(true, Ordering::Release);
    let _ = listener_thread.join();
    drop(socket_guard);
    Ok(code)
}

fn copy_output(reader: &mut dyn Read, mut consume: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => consume(&buffer[..count])?,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error).context("failed to read PTY output"),
        }
    }
    Ok(())
}

fn finish_child(
    child: &mut dyn portable_pty::Child,
    recorder: Option<Arc<Mutex<Recorder>>>,
) -> Result<i32> {
    let status = child.wait().context("failed to wait for child command")?;
    let code = status.exit_code() as i32;
    if let Some(recorder) = &recorder {
        recorder
            .lock()
            .unwrap()
            .event(json!({ "type": "exit", "code": code }))?;
    }
    Ok(code)
}

#[derive(Debug, Deserialize)]
struct ControlRequest {
    op: String,
    #[serde(default)]
    data: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct ControlResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    screen: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn serve_socket(
    listener: UnixListener,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    parser: Arc<Mutex<vt100::Parser>>,
    recorder: Option<Arc<Mutex<Recorder>>>,
    stopped: Arc<AtomicBool>,
) {
    let _ = listener.set_nonblocking(true);
    while !stopped.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = handle_control(stream, &writer, &parser, &recorder);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
}

fn handle_control(
    mut stream: UnixStream,
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
    parser: &Arc<Mutex<vt100::Parser>>,
    recorder: &Option<Arc<Mutex<Recorder>>>,
) -> Result<()> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let response = match serde_json::from_str::<ControlRequest>(&line) {
        Ok(request) if request.op == "send" => {
            let result = (|| -> Result<()> {
                let mut output = writer.lock().unwrap();
                output.write_all(&request.data)?;
                output.flush()?;
                if let Some(recorder) = recorder {
                    recorder.lock().unwrap().chunk("input", &request.data)?;
                }
                Ok(())
            })();
            match result {
                Ok(()) => ControlResponse {
                    ok: true,
                    screen: None,
                    error: None,
                },
                Err(error) => ControlResponse {
                    ok: false,
                    screen: None,
                    error: Some(error.to_string()),
                },
            }
        }
        Ok(request) if request.op == "screen" => ControlResponse {
            ok: true,
            screen: Some(parser.lock().unwrap().screen().contents()),
            error: None,
        },
        Ok(request) => ControlResponse {
            ok: false,
            screen: None,
            error: Some(format!("unknown operation: {}", request.op)),
        },
        Err(error) => ControlResponse {
            ok: false,
            screen: None,
            error: Some(format!("invalid request: {error}")),
        },
    };
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")?;
    Ok(())
}

fn send(args: SendArgs) -> Result<i32> {
    let mut data = args.text.into_bytes();
    if args.enter {
        data.push(0x0d);
    }
    if args.ctrl_c {
        data.push(0x03);
    }
    if args.ctrl_d {
        data.push(0x04);
    }
    request(
        &args.connection.socket,
        json!({ "op": "send", "data": data }),
    )?;
    Ok(0)
}

fn screen(args: SocketArgs) -> Result<i32> {
    let response = request(&args.socket, json!({ "op": "screen" }))?;
    if let Some(contents) = response.get("screen").and_then(Value::as_str) {
        println!("{contents}");
    }
    Ok(0)
}

fn request(socket: &Path, value: Value) -> Result<Value> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("failed to connect to {}", socket.display()))?;
    serde_json::to_writer(&mut stream, &value)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let response: Value = serde_json::from_str(&line).context("invalid host response")?;
    if !response.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return Err(anyhow!(
            "host rejected request: {}",
            response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        ));
    }
    Ok(response)
}

struct SocketGuard {
    path: PathBuf,
    listener: UnixListener,
}

impl SocketGuard {
    fn bind(path: &Path) -> Result<Self> {
        let listener = UnixListener::bind(path)
            .with_context(|| format!("failed to bind socket {}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            path: path.to_owned(),
            listener,
        })
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct RawModeGuard(bool);

impl RawModeGuard {
    fn new(enabled: bool) -> Result<Self> {
        if enabled {
            crossterm::terminal::enable_raw_mode().context("failed to enable terminal raw mode")?;
        }
        Ok(Self(enabled))
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.0 {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

struct Recorder {
    file: File,
    started_ms: u128,
}

impl Recorder {
    fn create(path: PathBuf) -> Result<Self> {
        Ok(Self {
            file: File::create(&path)
                .with_context(|| format!("failed to create recording {}", path.display()))?,
            started_ms: now_ms(),
        })
    }

    fn chunk(&mut self, stream: &str, bytes: &[u8]) -> Result<()> {
        self.event(json!({ "type": "chunk", "stream": stream, "data": bytes }))
    }

    fn event(&mut self, mut value: Value) -> Result<()> {
        value["at_ms"] = json!(now_ms().saturating_sub(self.started_ms));
        serde_json::to_writer(&mut self.file, &value)?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        Ok(())
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_child_flags_without_interpreting_them() {
        let cli = Cli::try_parse_from(["lab", "run", "--", "tool", "--flag", "value"])
            .expect("arguments should parse");
        match cli.command {
            Commands::Run(args) => assert_eq!(args.command, ["tool", "--flag", "value"]),
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn send_enter_uses_carriage_return() {
        let cli = Cli::try_parse_from([
            "lab",
            "send",
            "--socket",
            "/tmp/test.sock",
            "--enter",
            "hello",
        ])
        .unwrap();
        match cli.command {
            Commands::Send(args) => {
                assert_eq!(args.text, "hello");
                assert!(args.enter);
            }
            _ => panic!("expected send command"),
        }
    }

    #[test]
    fn requires_a_child_command() {
        assert!(Cli::try_parse_from(["lab", "run"]).is_err());
        assert!(Cli::try_parse_from(["lab", "host", "--socket", "/tmp/x"]).is_err());
    }
}
