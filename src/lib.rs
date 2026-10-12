mod conpty;
mod ipc;

use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::{self, BufRead, BufReader, IsTerminal, Read, Write},
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
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// What `run` sends to the child when its piped stdin ends: Ctrl-D on a Unix terminal. A Windows
/// console ignores Ctrl-D; it ends input on Ctrl-Z followed by Enter.
#[cfg(not(windows))]
const END_OF_INPUT: &[u8] = &[0x04];
#[cfg(windows)]
const END_OF_INPUT: &[u8] = &[0x1a, 0x0d];

#[derive(Debug, Parser)]
#[command(
    name = "agent-cli-lab",
    version,
    about = "Test and control interactive CLI programs through a pseudo-terminal",
    after_help = "EXAMPLES:\n  agent-cli-lab run -- python3 -i\n  agent-cli-lab host -r pi.jsonl -- pi\n  agent-cli-lab send -e 'Hello'\n  agent-cli-lab screen\n  agent-cli-lab attach\n\nRecordings contain all input and output and may contain secrets."
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
    /// Attach this terminal to a hosted PTY.
    Attach(AttachArgs),
    /// Resolve the socket path used by the shorthand commands.
    Socket(SocketAliasArgs),
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
    /// Local socket used by `send` and `screen`. On Windows this is only a name: it is mapped to
    /// a named pipe that just the current user can open, and no file is created.
    #[arg(long, short = 's', value_name = "PATH", default_value_os_t = default_socket())]
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
    #[arg(long, short = 'r', value_name = "PATH")]
    record: Option<PathBuf>,
    /// Terminal width in columns.
    #[arg(long, short = 'C', default_value_t = 80)]
    cols: u16,
    /// Terminal height in rows.
    #[arg(long, short = 'R', default_value_t = 24)]
    rows: u16,
}

#[derive(Debug, Args)]
struct SocketArgs {
    /// Local socket of the running host (the same value that was given to `host`).
    #[arg(long, short = 's', value_name = "PATH", default_value_os_t = default_socket())]
    socket: PathBuf,
}

#[derive(Debug, Args)]
struct SocketAliasArgs {
    /// Print only the path, without explanatory text.
    #[arg(long, short = 'p')]
    path_only: bool,
}

#[derive(Debug, Args)]
struct AttachArgs {
    #[command(flatten)]
    connection: SocketArgs,
    /// Milliseconds between screen refreshes.
    #[arg(long, default_value_t = 100)]
    interval_ms: u64,
}

#[derive(Debug, Args)]
struct SendArgs {
    #[command(flatten)]
    connection: SocketArgs,
    /// Text to type. Exact UTF-8 bytes are sent.
    #[arg(default_value = "")]
    text: String,
    /// Append a terminal Enter key (carriage return).
    #[arg(long, short = 'e')]
    enter: bool,
    /// Append Ctrl-C.
    #[arg(long, short = 'c')]
    ctrl_c: bool,
    /// Append Ctrl-D.
    #[arg(long, short = 'd')]
    ctrl_d: bool,
}

pub fn execute(cli: Cli) -> Result<i32> {
    match cli.command {
        Commands::Run(args) => run(args),
        Commands::Host(args) => host(args),
        Commands::Send(args) => send(args),
        Commands::Screen(args) => screen(args),
        Commands::Attach(args) => attach(args),
        Commands::Socket(args) => print_default_socket(args),
    }
}

fn default_socket() -> PathBuf {
    default_socket_from(
        env::var_os("AGENT_CLI_LAB_SOCKET"),
        env::var_os("XDG_RUNTIME_DIR"),
        env::var_os("TMPDIR"),
        env::temp_dir(),
    )
}

fn default_socket_from(
    explicit: Option<OsString>,
    xdg_runtime_dir: Option<OsString>,
    tmpdir: Option<OsString>,
    system_temp: PathBuf,
) -> PathBuf {
    if let Some(path) = explicit.filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }
    let base = xdg_runtime_dir
        .or(tmpdir)
        .map(PathBuf::from)
        .unwrap_or(system_temp);
    base.join("agent-cli-lab.sock")
}

fn prepare_socket_parent(socket: &Path) -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    if let Some(parent) = socket
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create socket directory {}", parent.display()))?;
    }
    Ok(())
}

fn print_default_socket(args: SocketAliasArgs) -> Result<i32> {
    let socket = default_socket();
    if args.path_only {
        println!("{}", socket.display());
    } else {
        println!("default socket: {}", socket.display());
        println!("override with AGENT_CLI_LAB_SOCKET or --socket PATH");
    }
    Ok(0)
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

struct Pty {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Box<dyn Read + Send>,
    /// Must stay alive until the child has exited: on Windows, dropping it closes the
    /// pseudo-console and kills the child.
    master: Box<dyn MasterPty + Send>,
}

/// The PTY and the writer for its input. Dropping the last reference to the writer makes it send
/// a final newline and end-of-file to the child, so the caller decides who owns it.
fn open_pty(args: &TerminalArgs, child_command: &[OsString]) -> Result<(Pty, SharedWriter)> {
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
    let pty = Pty {
        child,
        reader,
        master: pair.master,
    };
    Ok((pty, Arc::new(Mutex::new(writer))))
}

fn run(args: RunArgs) -> Result<i32> {
    validate_terminal(&args.terminal)?;
    let recorder = create_recorder(&args.terminal, &args.command)?;
    let (pty, writer) = open_pty(&args.terminal, &args.command)?;
    let handshake_input = handshake_input(&writer);

    let stdin_is_terminal = io::stdin().is_terminal();
    let _raw_mode = RawModeGuard::new(stdin_is_terminal)?;
    let _console = conpty::ConsoleModes::enable(stdin_is_terminal, io::stdout().is_terminal());
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
            let mut output = writer.lock().unwrap();
            if output.write_all(&buffer[..count]).is_err() || output.flush().is_err() {
                return;
            }
        }
        if !stdin_is_terminal {
            let mut output = writer.lock().unwrap();
            let _ = output.write_all(END_OF_INPUT);
            let _ = output.flush();
        }
    });

    let mut stdout = io::stdout().lock();
    let code = pty.pump(handshake_input, |bytes| {
        stdout.write_all(bytes)?;
        stdout.flush()?;
        if let Some(recorder) = &recorder {
            recorder.lock().unwrap().chunk("output", bytes)?;
        }
        Ok(())
    })?;
    record_exit(recorder, code)
}

fn host(args: HostArgs) -> Result<i32> {
    validate_terminal(&args.terminal)?;
    let recorder = create_recorder(&args.terminal, &args.command)?;
    let (pty, writer) = open_pty(&args.terminal, &args.command)?;
    let parser = Arc::new(Mutex::new(vt100::Parser::new(
        args.terminal.rows,
        args.terminal.cols,
        0,
    )));
    let stopped = Arc::new(AtomicBool::new(false));
    prepare_socket_parent(&args.socket)?;
    let listener = ipc::bind(&args.socket)?;
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

    let _console = conpty::ConsoleModes::enable(
        args.stdio && stdin_is_terminal,
        args.stdio && io::stdout().is_terminal(),
    );

    let mut stdout = io::stdout().lock();
    let code = pty.pump(handshake_input(&writer), |bytes| {
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

    let code = record_exit(recorder, code)?;
    stopped.store(true, Ordering::Release);
    let _ = listener_thread.join();
    Ok(code)
}

/// The writer that answers conhost's handshake, which is needed on Windows only. It must not be
/// kept elsewhere: on Unix, dropping the last reference to the writer is what sends the child its
/// final end-of-file, and that has to stay with whoever owns the writer (the stdin thread in `run`).
fn handshake_input(writer: &SharedWriter) -> Option<SharedWriter> {
    cfg!(windows).then(|| writer.clone())
}

impl Pty {
    /// Passes the child's output to `consume` until it ends, then reaps the child and returns its
    /// exit code. `input` is where the answer to conhost's handshake goes (see [`handshake_input`]).
    fn pump(
        self,
        input: Option<SharedWriter>,
        mut consume: impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<i32> {
        let Pty {
            mut child,
            mut reader,
            master,
        } = self;
        let mut handshake = conpty::HandshakeFilter::default();
        let mut consume = |bytes: &[u8]| -> Result<()> {
            if !cfg!(windows) {
                return consume(bytes);
            }
            let (bytes, reply) = handshake.filter(bytes);
            if reply && let Some(input) = &input {
                let mut input = input.lock().unwrap();
                input.write_all(conpty::CURSOR_REPLY)?;
                input.flush()?;
            }
            if bytes.is_empty() {
                return Ok(());
            }
            consume(&bytes)
        };

        // Windows: the output only ends once the pseudo-console is closed, which also kills the
        // child, so it is closed from here as soon as the child has exited.
        #[cfg(windows)]
        let status = {
            let waiter = thread::spawn(move || {
                let status = child.wait();
                drop(master);
                status
            });
            copy_output(&mut reader, &mut consume)?;
            waiter
                .join()
                .map_err(|_| anyhow!("child wait thread panicked"))?
        };
        #[cfg(not(windows))]
        let status = {
            copy_output(&mut reader, &mut consume)?;
            let status = child.wait();
            drop(master);
            status
        };
        let status = status.context("failed to wait for child command")?;
        Ok(status.exit_code() as i32)
    }
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

fn record_exit(recorder: Option<Arc<Mutex<Recorder>>>, code: i32) -> Result<i32> {
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
    listener: ipc::Listener,
    writer: SharedWriter,
    parser: Arc<Mutex<vt100::Parser>>,
    recorder: Option<Arc<Mutex<Recorder>>>,
    stopped: Arc<AtomicBool>,
) {
    while !stopped.load(Ordering::Acquire) {
        match ipc::accept(&listener) {
            Ok(stream) => {
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
    stream: ipc::Stream,
    writer: &SharedWriter,
    parser: &Arc<Mutex<vt100::Parser>>,
    recorder: &Option<Arc<Mutex<Recorder>>>,
) -> Result<()> {
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
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
    let mut reply = serde_json::to_vec(&response)?;
    reply.push(b'\n');
    (&stream).write_all(&reply)?;
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

fn attach(args: AttachArgs) -> Result<i32> {
    let socket = args.connection.socket;
    let interval = Duration::from_millis(args.interval_ms.max(16));
    let stopped = Arc::new(AtomicBool::new(false));
    let input_stopped = stopped.clone();
    let input_socket = socket.clone();

    eprintln!("attached to {}; press Ctrl-] to detach", socket.display());
    let stdin_is_terminal = io::stdin().is_terminal();
    let stdout_is_terminal = io::stdout().is_terminal();
    let _console = conpty::ConsoleModes::enable(stdin_is_terminal, stdout_is_terminal);
    let _raw_mode = RawModeGuard::new(stdin_is_terminal)?;
    let mut stdout = io::stdout().lock();
    let mut last_screen = String::new();
    render_attached_screen(&socket, &mut stdout, &mut last_screen)?;

    let input_thread = thread::spawn(move || -> Result<()> {
        let mut stdin = io::stdin().lock();
        let mut buffer = [0_u8; 1024];
        while !input_stopped.load(Ordering::Acquire) {
            let count = stdin.read(&mut buffer).context("failed to read stdin")?;
            if count == 0 {
                input_stopped.store(true, Ordering::Release);
                break;
            }
            if let Some(position) = buffer[..count].iter().position(|byte| *byte == 0x1d) {
                if position > 0 {
                    request(&input_socket, json!({ "op": "send", "data": &buffer[..position] }))?;
                }
                input_stopped.store(true, Ordering::Release);
                break;
            }
            request(&input_socket, json!({ "op": "send", "data": &buffer[..count] }))?;
        }
        Ok(())
    });

    while !stopped.load(Ordering::Acquire) {
        if let Err(error) = render_attached_screen(&socket, &mut stdout, &mut last_screen) {
            stopped.store(true, Ordering::Release);
            if !last_screen.is_empty() {
                writeln!(stdout)?;
            }
            eprintln!("detached: {error:#}");
            break;
        }
        thread::sleep(interval);
    }
    stopped.store(true, Ordering::Release);
    if input_thread.is_finished() {
        input_thread.join().unwrap()?;
    }
    Ok(0)
}

fn render_attached_screen(socket: &Path, stdout: &mut impl Write, last_screen: &mut String) -> Result<()> {
    let response = request(socket, json!({ "op": "screen" }))?;
    if let Some(contents) = response.get("screen").and_then(Value::as_str) {
        if contents != last_screen {
            write!(stdout, "\x1b[2J\x1b[H{contents}")?;
            stdout.flush()?;
            last_screen.clear();
            last_screen.push_str(contents);
        }
    }
    Ok(())
}

fn request(socket: &Path, value: Value) -> Result<Value> {
    let stream = ipc::connect(socket)
        .with_context(|| format!("failed to connect to {}", socket.display()))?;
    let mut message = serde_json::to_vec(&value)?;
    message.push(b'\n');
    (&stream).write_all(&message)?;
    (&stream).flush()?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
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
        let cli =
            Cli::try_parse_from(["lab", "send", "-s", "/tmp/test.sock", "-e", "hello"]).unwrap();
        match cli.command {
            Commands::Send(args) => {
                assert_eq!(args.text, "hello");
                assert!(args.enter);
            }
            _ => panic!("expected send command"),
        }
    }

    #[test]
    fn resolves_default_socket_precedence() {
        assert_eq!(
            default_socket_from(
                Some(OsString::from("/chosen.sock")),
                Some(OsString::from("/xdg")),
                Some(OsString::from("/tmpdir")),
                PathBuf::from("/system"),
            ),
            PathBuf::from("/chosen.sock")
        );
        assert_eq!(
            default_socket_from(
                Some(OsString::new()),
                Some(OsString::from("/xdg")),
                Some(OsString::from("/tmpdir")),
                PathBuf::from("/system"),
            ),
            PathBuf::from("/xdg/agent-cli-lab.sock")
        );
        assert_eq!(
            default_socket_from(
                None,
                None,
                Some(OsString::from("/tmpdir")),
                PathBuf::from("/system")
            ),
            PathBuf::from("/tmpdir/agent-cli-lab.sock")
        );
        assert_eq!(
            default_socket_from(None, None, None, PathBuf::from("/system")),
            PathBuf::from("/system/agent-cli-lab.sock")
        );
    }

    #[test]
    fn defaults_socket_for_host_and_control_commands() {
        let cli = Cli::try_parse_from(["lab", "host", "--", "tool"]).unwrap();
        match cli.command {
            Commands::Host(args) => assert!(args.socket.ends_with("agent-cli-lab.sock")),
            _ => panic!("expected host command"),
        }

        let cli = Cli::try_parse_from(["lab", "send", "hello"]).unwrap();
        match cli.command {
            Commands::Send(args) => assert!(args.connection.socket.ends_with("agent-cli-lab.sock")),
            _ => panic!("expected send command"),
        }

        let cli = Cli::try_parse_from(["lab", "attach"]).unwrap();
        match cli.command {
            Commands::Attach(args) => assert!(args.connection.socket.ends_with("agent-cli-lab.sock")),
            _ => panic!("expected attach command"),
        }
    }

    #[test]
    fn parses_short_control_flags() {
        let cli = Cli::try_parse_from(["lab", "send", "-e", "-c", "-d", "hello"]).unwrap();
        match cli.command {
            Commands::Send(args) => {
                assert_eq!(args.text, "hello");
                assert!(args.enter);
                assert!(args.ctrl_c);
                assert!(args.ctrl_d);
            }
            _ => panic!("expected send command"),
        }

        let cli = Cli::try_parse_from([
            "lab",
            "host",
            "-r",
            "out.jsonl",
            "-C",
            "100",
            "-R",
            "30",
            "--",
            "tool",
        ])
        .unwrap();
        match cli.command {
            Commands::Host(args) => {
                assert_eq!(args.terminal.record, Some(PathBuf::from("out.jsonl")));
                assert_eq!(args.terminal.cols, 100);
                assert_eq!(args.terminal.rows, 30);
            }
            _ => panic!("expected host command"),
        }
    }

    #[test]
    fn requires_a_child_command() {
        assert!(Cli::try_parse_from(["lab", "run"]).is_err());
        assert!(Cli::try_parse_from(["lab", "host"]).is_err());
    }
}
