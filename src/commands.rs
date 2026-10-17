//! Client commands (spec 007): everything that drives a running host through its control socket.
//!
//! Each command prefers the host's own operation (`status`, `wait`, `screen_json`, `stop`) and, when
//! the host is older and answers "unknown operation", does the same job itself with `send` and
//! `screen`, so a new client works with hosts that are already running.

use std::collections::HashSet;

use super::*;
use state::{State, WaitSpec};

/// A host that does not know the operation that was asked for (it predates it).
#[derive(Debug)]
pub(crate) struct UnknownOperation(pub String);

impl std::fmt::Display for UnknownOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "host rejected request: {}", self.0)
    }
}

impl std::error::Error for UnknownOperation {}

fn is_unknown_operation(error: &anyhow::Error) -> bool {
    error.downcast_ref::<UnknownOperation>().is_some()
}

/// Exit status of `ask` and `wait` when the time ran out (the same as `timeout(1)`).
const EXIT_TIMED_OUT: i32 = 124;
/// Exit status of `ask` and `wait` when the hosted program ended first.
const EXIT_PROGRAM_ENDED: i32 = 125;

fn send_bytes(socket: &Path, data: Vec<u8>) -> Result<()> {
    request(socket, json!({ "op": "send", "data": data }))?;
    Ok(())
}

/// `request` that gives up after `timeout`, for looking at hosts that may be hung.
fn request_with_timeout(socket: &Path, value: Value, timeout: Duration) -> Result<Value> {
    let (sender, receiver) = mpsc::channel();
    let socket = socket.to_path_buf();
    thread::spawn(move || {
        let _ = sender.send(request(&socket, value));
    });
    receiver
        .recv_timeout(timeout)
        .map_err(|_| anyhow!("no answer within {} ms", timeout.as_millis()))?
}

/// `Ok(None)` for a host without the `status` operation.
fn host_status(socket: &Path) -> Result<Option<Value>> {
    match request(socket, json!({ "op": "status" })) {
        Ok(value) => Ok(Some(value)),
        Err(error) if is_unknown_operation(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn application_cursor(socket: &Path) -> bool {
    host_status(socket)
        .ok()
        .flatten()
        .and_then(|status| status.get("application_cursor").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn read_screen(socket: &Path) -> Result<String> {
    let response = request(socket, json!({ "op": "screen" }))?;
    Ok(response
        .get("screen")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

/// The text to type: the argument, or the contents of `--file` without its final line break (the
/// Enter key is a separate decision).
fn text_argument(text: &str, file: &Option<PathBuf>) -> Result<String> {
    match file {
        Some(path) => {
            let mut content = fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            while content.ends_with('\n') || content.ends_with('\r') {
                content.pop();
            }
            Ok(content)
        }
        None => Ok(text.to_owned()),
    }
}

/// Types `text`, in pieces of `typing.chunk` characters when pacing is on. Pieces never split a
/// character.
fn type_text(socket: &Path, text: &str, typing: &Typing) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    if typing.chunk == 0 {
        return send_bytes(socket, text.as_bytes().to_vec());
    }
    let characters: Vec<char> = text.chars().collect();
    for (index, piece) in characters.chunks(typing.chunk).enumerate() {
        if index > 0 {
            thread::sleep(typing.pause);
        }
        send_bytes(socket, piece.iter().collect::<String>().into_bytes())?;
    }
    Ok(())
}

pub(crate) fn send(args: SendArgs) -> Result<i32> {
    let socket = args.connection.path()?;
    let text = text_argument(&args.text, &args.file)?;
    let typing = args.typing.resolve();
    let mut tail = Vec::new();
    if !args.keys.is_empty() {
        tail.extend(keys::encode(&args.keys, application_cursor(&socket))?);
    }
    if args.enter {
        tail.push(0x0d);
    }
    if args.ctrl_c {
        tail.push(0x03);
    }
    if args.ctrl_d {
        tail.push(0x04);
    }
    if typing.chunk == 0 && typing.enter_delay.is_zero() {
        // Everything in one write, exactly as before spec 007.
        let mut data = text.into_bytes();
        data.extend(tail);
        send_bytes(&socket, data)?;
        return Ok(0);
    }
    let typed = !text.is_empty();
    type_text(&socket, &text, &typing)?;
    if !tail.is_empty() {
        if typed {
            thread::sleep(typing.enter_delay);
        }
        send_bytes(&socket, tail)?;
    }
    Ok(0)
}

pub(crate) fn key(args: KeyArgs) -> Result<i32> {
    let socket = args.connection.path()?;
    let bytes = keys::encode(&args.keys, application_cursor(&socket))?;
    send_bytes(&socket, bytes)?;
    Ok(0)
}

enum Outcome {
    Met(String),
    TimedOut(String),
    /// The program ended; the last screen and, when known, its exit code.
    Ended(String, Option<i32>),
}

/// Waits on the host (one request). `min_wait` is how long to wait at least, whatever the screen shows.
fn wait_for(
    socket: &Path,
    spec: &WaitSpec,
    timeout: Duration,
    min_wait: Duration,
) -> Result<Outcome> {
    let request_body = json!({
        "op": "wait", "spec": spec,
        "timeout_ms": timeout.as_millis() as u64, "min_wait_ms": min_wait.as_millis() as u64,
    });
    match request(socket, request_body) {
        Ok(response) => {
            let screen = response
                .get("screen")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if response.get("met").and_then(Value::as_bool) == Some(true) {
                Ok(Outcome::Met(screen))
            } else if response.get("exited").and_then(Value::as_bool) == Some(true) {
                let code = response
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .map(|code| code as i32);
                Ok(Outcome::Ended(screen, code))
            } else {
                Ok(Outcome::TimedOut(screen))
            }
        }
        Err(error) if is_unknown_operation(&error) => poll_wait(socket, spec, timeout, min_wait),
        // The host went away while we waited: the program ended and the host with it.
        Err(_) if request(socket, json!({ "op": "screen" })).is_err() => {
            Ok(Outcome::Ended(String::new(), None))
        }
        Err(error) => Err(error),
    }
}

/// The same wait for hosts without the `wait` operation: it looks at the screen, and treats a
/// screen that has stopped changing as quiet.
fn poll_wait(
    socket: &Path,
    spec: &WaitSpec,
    timeout: Duration,
    min_wait: Duration,
) -> Result<Outcome> {
    let compiled = spec.compile()?;
    let begun = Instant::now();
    let mut last = String::new();
    let mut changed = Instant::now();
    loop {
        let screen = match read_screen(socket) {
            Ok(screen) => screen,
            Err(_) => return Ok(Outcome::Ended(last, None)),
        };
        if screen != last {
            last = screen.clone();
            changed = Instant::now();
        }
        if begun.elapsed() >= min_wait
            && compiled.met(&screen, changed.elapsed().as_millis() as u64)
        {
            return Ok(Outcome::Met(screen));
        }
        if begun.elapsed() >= timeout {
            return Ok(Outcome::TimedOut(screen));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn show(screen: &str, tail: usize) {
    if tail == 0 {
        println!("{screen}");
    } else {
        println!("{}", state::tail_lines(screen, tail));
    }
}

/// Prints the final screen and returns the exit status for an outcome.
fn finish(outcome: Outcome, tail: usize, quiet: bool) -> i32 {
    let (screen, code) = match outcome {
        Outcome::Met(screen) => (screen, 0),
        Outcome::TimedOut(screen) => {
            if !quiet {
                eprintln!("timed out waiting");
            }
            (screen, EXIT_TIMED_OUT)
        }
        Outcome::Ended(screen, exit_code) => {
            if !quiet {
                match exit_code {
                    Some(code) => eprintln!("the program ended with exit code {code}"),
                    None => eprintln!("the program ended"),
                }
            }
            (screen, EXIT_PROGRAM_ENDED)
        }
    };
    if !quiet && !screen.is_empty() {
        show(&screen, tail);
    }
    code
}

/// Sends `text` plus Enter, then waits: until the screen has been quiet for `idle_ms` (the default),
/// or until `--until` matches, or until a dialog shows (`--asking`).
pub(crate) fn ask(args: AskArgs) -> Result<i32> {
    let socket = args.connection.path()?;
    let text = text_argument(&args.text, &args.file)?;
    let typing = args.typing.resolve();
    let sent = !text.is_empty();
    if sent {
        type_text(&socket, &text, &typing)?;
        if !typing.enter_delay.is_zero() {
            thread::sleep(typing.enter_delay);
        }
        send_bytes(&socket, vec![0x0d])?;
    }
    let by_pattern = !args.until.is_empty() || args.asking;
    let spec = WaitSpec {
        idle_ms: (!by_pattern).then_some(args.idle_ms),
        patterns: args.until,
        asking: args.asking,
        gone: Vec::new(),
    };
    // After typing, give the program time to react before a quiet screen counts as its answer.
    let min_wait = if !sent {
        Duration::ZERO
    } else if by_pattern {
        Duration::from_millis(250)
    } else {
        Duration::from_millis(args.idle_ms)
    };
    let outcome = wait_for(&socket, &spec, Duration::from_secs(args.timeout), min_wait)?;
    Ok(finish(outcome, args.tail, false))
}

pub(crate) fn wait(args: WaitArgs) -> Result<i32> {
    let socket = args.connection.path()?;
    let spec = WaitSpec {
        idle_ms: args.idle_ms,
        patterns: args.patterns,
        asking: args.asking,
        gone: args.gone,
    };
    if spec.is_empty() {
        return Err(anyhow!(
            "wait needs a condition: --idle-ms, --for, --asking or --gone"
        ));
    }
    let outcome = wait_for(
        &socket,
        &spec,
        Duration::from_secs(args.timeout),
        Duration::ZERO,
    )?;
    Ok(finish(outcome, args.tail, args.quiet))
}

pub(crate) fn screen(args: ScreenArgs) -> Result<i32> {
    let socket = args.connection.path()?;
    if args.json {
        let value = match request(&socket, json!({ "op": "screen_json", "spans": args.spans })) {
            Ok(value) => value,
            Err(error) if is_unknown_operation(&error) => {
                // An older host: lines and nothing else.
                let text = read_screen(&socket)?;
                let lines: Vec<&str> = text.lines().collect();
                json!({ "ok": true, "legacy": true, "rows": lines.len(), "lines": lines })
            }
            Err(error) => return Err(error),
        };
        println!("{value}");
        return Ok(0);
    }
    let text = read_screen(&socket)?;
    match (args.tail, args.compact) {
        (Some(count), _) => println!("{}", state::tail_lines(&text, count)),
        (None, true) => println!("{}", state::tail_lines(&text, usize::MAX)),
        (None, false) => println!("{text}"),
    }
    Ok(0)
}

fn human_duration(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// What a look at a host showed.
struct Look {
    status: Option<Value>,
    screen: String,
    quiet_ms: u64,
}

impl Look {
    fn running(&self) -> bool {
        self.status
            .as_ref()
            .is_none_or(|status| status.get("running").and_then(Value::as_bool) != Some(false))
    }

    fn state(&self, stalled_ms: u64) -> State {
        if self.running() {
            state::classify(&self.screen, self.quiet_ms, stalled_ms)
        } else {
            State::Gone
        }
    }
}

/// Tells how long a screen has been unchanged, for hosts that cannot say how long they have been quiet.
struct Quiet {
    last: String,
    changed: Instant,
}

impl Quiet {
    fn new() -> Self {
        Self {
            last: String::new(),
            changed: Instant::now(),
        }
    }

    fn observe(&mut self, screen: &str) -> u64 {
        if screen != self.last {
            self.last = screen.to_owned();
            self.changed = Instant::now();
        }
        self.changed.elapsed().as_millis() as u64
    }
}

fn look(socket: &Path, quiet: &mut Quiet) -> Result<Look> {
    let status = host_status(socket)?;
    let screen = read_screen(socket)?;
    let observed = quiet.observe(&screen);
    let quiet_ms = status
        .as_ref()
        .and_then(|status| status.get("quiet_ms").and_then(Value::as_u64))
        .unwrap_or(observed);
    Ok(Look {
        status,
        screen,
        quiet_ms,
    })
}

pub(crate) fn status(args: StatusArgs) -> Result<i32> {
    let socket = args.connection.path()?;
    let mut quiet = Quiet::new();
    let look = look(&socket, &mut quiet)
        .with_context(|| format!("no session answers on {}", socket.display()))?;
    let state = look.state(args.stalled_ms);
    if args.json {
        let mut value = look
            .status
            .clone()
            .unwrap_or_else(|| json!({ "ok": true, "legacy": true }));
        value["state"] = json!(state.name());
        value["quiet_ms"] = json!(look.quiet_ms);
        println!("{value}");
        return Ok(0);
    }
    let field = |key: &str| look.status.as_ref().and_then(|status| status.get(key));
    println!(
        "state:    {} (no output for {})",
        state.name(),
        human_duration(look.quiet_ms)
    );
    match &look.status {
        Some(status) => {
            if let Some(name) = status.get("name").and_then(Value::as_str) {
                println!("name:     {name}");
            }
            println!(
                "size:     {}x{}  cursor {},{}",
                field("cols").and_then(Value::as_u64).unwrap_or(0),
                field("rows").and_then(Value::as_u64).unwrap_or(0),
                field("cursor")
                    .and_then(|c| c.get(0))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                field("cursor")
                    .and_then(|c| c.get(1))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
            println!(
                "host:     pid {}  up {}",
                field("pid").and_then(Value::as_u64).unwrap_or(0),
                human_duration(field("uptime_ms").and_then(Value::as_u64).unwrap_or(0))
            );
            if let Some(command) = field("command").and_then(Value::as_array) {
                let words: Vec<&str> = command.iter().filter_map(Value::as_str).collect();
                println!("program:  {}", words.join(" "));
            }
            if let Some(code) = field("exit_code").and_then(Value::as_i64) {
                println!("exited:   code {code}");
            }
        }
        None => println!(
            "note:     an older host: only the screen could be read (restart it for full status)"
        ),
    }
    Ok(0)
}

fn pad(text: &str, width: usize) -> String {
    let shortened: String = if text.chars().count() > width {
        text.chars()
            .take(width.saturating_sub(1))
            .chain(['…'])
            .collect()
    } else {
        text.to_owned()
    };
    format!("{shortened:<width$}")
}

pub(crate) fn list(args: ListArgs) -> Result<i32> {
    let entries = session::entries();
    let mut rows = Vec::new();
    for entry in entries {
        let socket = PathBuf::from(&entry.socket);
        let answer = request_with_timeout(
            &socket,
            json!({ "op": "screen" }),
            Duration::from_millis(1500),
        );
        let state = match &answer {
            Ok(response) => {
                let screen = response
                    .get("screen")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let quiet = host_status(&socket)
                    .ok()
                    .flatten()
                    .and_then(|status| status.get("quiet_ms").and_then(Value::as_u64));
                match quiet {
                    Some(quiet_ms) => state::classify(screen, quiet_ms, 8000).name(),
                    None if state::is_asking(screen) => "asking",
                    None => "up",
                }
            }
            Err(_) => "dead",
        };
        if state == "dead" && !args.all {
            session::forget(&entry.name);
            continue;
        }
        rows.push((entry, state));
    }
    if args.json {
        let items: Vec<Value> = rows
            .iter()
            .map(|(entry, state)| {
                let mut value = serde_json::to_value(entry).unwrap_or(Value::Null);
                value["state"] = json!(state);
                value
            })
            .collect();
        println!("{}", Value::Array(items));
        return Ok(0);
    }
    if rows.is_empty() {
        println!(
            "no sessions are running (start one with `agent-cli-lab open -n NAME -- command`)"
        );
        return Ok(0);
    }
    println!(
        "{}{}{}{}PROGRAM",
        pad("NAME", 16),
        pad("STATE", 9),
        pad("SIZE", 9),
        pad("PID", 8),
    );
    for (entry, state) in rows {
        println!(
            "{}{}{}{}{}",
            pad(&entry.name, 16),
            pad(state, 9),
            pad(&format!("{}x{}", entry.cols, entry.rows), 9),
            pad(&entry.pid.to_string(), 8),
            pad(&entry.command.join(" "), 60),
        );
    }
    Ok(0)
}

/// Ends the process tree that starts at `pid`. Used for hosts too old to understand `stop`.
fn kill_process_tree(pid: u32) -> Result<()> {
    #[cfg(windows)]
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()?
        .status;
    #[cfg(not(windows))]
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("could not end process {pid}"))
    }
}

pub(crate) fn stop(args: StopArgs) -> Result<i32> {
    let socket = args.connection.path()?;
    match request(&socket, json!({ "op": "stop" })) {
        Ok(_) => {}
        Err(error) if is_unknown_operation(&error) => {
            // An older host: end it by its process id, which a named session recorded.
            let pid = session::entries()
                .into_iter()
                .find(|entry| Path::new(&entry.socket) == socket)
                .map(|entry| entry.pid);
            match pid {
                Some(pid) => kill_process_tree(pid)?,
                None => {
                    return Err(anyhow!(
                        "this host is older than `stop` and was not started with --name, so it cannot be \
                         found; close its window or end its process"
                    ));
                }
            }
        }
        Err(error) => {
            return Err(error.context(format!("no session answers on {}", socket.display())));
        }
    }
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    while request(&socket, json!({ "op": "screen" })).is_ok() {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "the session is still answering after {} s",
                args.timeout
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
    if let Some(name) = &args.connection.name {
        session::forget(name);
    }
    println!(
        "stopped {}",
        args.connection
            .name
            .as_deref()
            .map_or_else(|| socket.display().to_string(), str::to_owned)
    );
    Ok(0)
}

fn emit(args: &WatchArgs, state: State, look: &Look, begun: Instant) {
    let tail = state::tail_lines(
        &look.screen,
        if args.tail == 0 {
            usize::MAX
        } else {
            args.tail
        },
    );
    if args.json {
        println!(
            "{}",
            json!({
                "state": state.name(), "quiet_ms": look.quiet_ms, "at_ms": now_ms() as u64,
                "after_ms": begun.elapsed().as_millis() as u64, "screen_tail": tail,
            })
        );
        return;
    }
    let seconds = begun.elapsed().as_secs();
    println!(
        "[+{:02}:{:02}] {} (no output for {})",
        seconds / 60,
        seconds % 60,
        state.name(),
        human_duration(look.quiet_ms)
    );
    for line in tail.lines() {
        println!("    {line}");
    }
}

/// Prints an event each time a session starts asking, stalls, or ends, and stays silent while it is
/// busy. A session that ends its turn with a question in plain words is `stalled`, not `asking`:
/// only a quiet screen tells, which is why this looks at output and not at the words.
pub(crate) fn watch(args: WatchArgs) -> Result<i32> {
    let socket = args.connection.path()?;
    let wanted: HashSet<String> = args
        .only
        .split(',')
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| !part.is_empty())
        .collect();
    for state in &wanted {
        if !["working", "asking", "stalled", "gone"].contains(&state.as_str()) {
            return Err(anyhow!(
                "unknown state {state:?}; choose from working, asking, stalled, gone"
            ));
        }
    }
    let begun = Instant::now();
    let mut quiet = Quiet::new();
    // The last asking or stalled screen that was reported, so that it is reported once however often the
    // program redraws a clock or a usage meter in between (which flips the state to working and back).
    let mut reported: Option<(State, u64)> = None;
    let mut previous_state: Option<State> = None;
    loop {
        let (state, look) = match look(&socket, &mut quiet) {
            Ok(look) => (look.state(args.stalled_ms), look),
            Err(_) => (
                State::Gone,
                Look {
                    status: None,
                    screen: String::new(),
                    quiet_ms: 0,
                },
            ),
        };
        let signature = screen_signature(&look.screen);
        let mut report = false;
        match state {
            State::Asking | State::Stalled => {
                // A new dialog or a new final screen is a new event even in the same state.
                if reported != Some((state, signature)) {
                    reported = Some((state, signature));
                    report = wanted.contains(state.name());
                }
            }
            State::Working => {
                // Real new output (not just changing digits) makes the next stall a new event.
                if reported.is_some_and(|(_, last)| last != signature) {
                    reported = None;
                }
                report = previous_state != Some(State::Working) && wanted.contains("working");
            }
            State::Gone => {
                if wanted.contains("gone") {
                    emit(&args, state, &look, begun);
                }
                return Ok(0);
            }
        }
        previous_state = Some(state);
        if report {
            emit(&args, state, &look, begun);
            if args.once {
                return Ok(0);
            }
        }
        thread::sleep(Duration::from_millis(args.interval_ms.max(50)));
    }
}

/// Identifies what a screen says, ignoring digits: a clock, a countdown or a usage meter that keeps
/// ticking in an idle program must not make the same screen look new.
fn screen_signature(screen: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let tail: String = state::tail_lines(screen, state::ASKING_WINDOW)
        .chars()
        .map(|character| {
            if character.is_ascii_digit() {
                '#'
            } else {
                character
            }
        })
        .collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tail.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn open(args: OpenArgs) -> Result<i32> {
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| format!("lab-{}", std::process::id()));
    session::validate_name(&name)?;
    let socket = session::socket_for(&name)?;
    prepare_socket_parent(&socket)?;
    if request(&socket, json!({ "op": "screen" })).is_ok() {
        return Err(anyhow!(
            "a session named {name:?} is already running; stop it or pick another name"
        ));
    }
    let exe = env::current_exe().context("failed to resolve agent-cli-lab executable")?;
    let title = args
        .title
        .clone()
        .unwrap_or_else(|| format!("agent-cli-lab: {name}"));
    let window_here = cfg!(windows) && !args.detached;

    let mut host: Vec<OsString> = vec![
        "host".into(),
        "--name".into(),
        name.clone().into(),
        "-C".into(),
        args.cols.to_string().into(),
        "-R".into(),
        args.rows.to_string().into(),
        "--title".into(),
        title.into(),
    ];
    if let Some(cwd) = &args.cwd {
        host.extend(["--cwd".into(), cwd.clone().into_os_string()]);
    }
    for (variable, value) in &args.env {
        host.extend(["--env".into(), format!("{variable}={value}").into()]);
    }
    if let Some(record) = &args.record {
        host.extend(["-r".into(), record.clone().into_os_string()]);
    }
    if window_here {
        // The window is the host's own console: the person types into it and sees it directly.
        host.extend(["--stdio".into(), "--fit-window".into()]);
        if args.quick_edit {
            host.push("--quick-edit".into());
        }
    }
    host.push("--".into());
    host.extend(args.command.iter().cloned());

    let mut command = Command::new(&exe);
    command.args(&host);
    // Whoever reads this command's output must not be kept waiting by the host that outlives it.
    conpty::stop_inheriting_std_handles();
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(if window_here {
            CREATE_NEW_CONSOLE
        } else {
            CREATE_NO_WINDOW
        });
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().context("failed to start the host")?;

    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    loop {
        if request(&socket, json!({ "op": "screen" })).is_ok() {
            break;
        }
        if let Some(status) = child.try_wait()? {
            return Err(anyhow!(
                "the host ended before it was ready ({status}); run the same command with `agent-cli-lab host` to see why"
            ));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(anyhow!(
                "the session {name:?} did not answer within {} s",
                args.timeout
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
    #[cfg(not(windows))]
    if !args.detached {
        open_attach_terminal(&socket, args.terminal.as_deref())?;
    }
    if args.json {
        println!(
            "{}",
            json!({ "name": name, "socket": socket, "pid": child.id(), "window": !args.detached })
        );
    } else {
        println!(
            "opened {name} (socket {}; reach it with -n {name})",
            socket.display()
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_is_one_write_unless_paced() {
        let plain = TypingArgs::default().resolve();
        assert_eq!(plain.chunk, 0);
        assert!(plain.enter_delay.is_zero());
        let paced = TypingArgs {
            paced: true,
            ..Default::default()
        }
        .resolve();
        assert_eq!(
            (paced.chunk, paced.pause, paced.enter_delay),
            (50, Duration::from_millis(150), Duration::from_millis(800))
        );
        let custom = TypingArgs {
            paced: true,
            chunk: Some(10),
            pause_ms: Some(5),
            enter_delay_ms: Some(0),
        }
        .resolve();
        assert_eq!(
            (custom.chunk, custom.pause, custom.enter_delay),
            (10, Duration::from_millis(5), Duration::ZERO)
        );
    }

    #[test]
    fn durations_are_short_and_readable() {
        assert_eq!(human_duration(900), "0s");
        assert_eq!(human_duration(12_300), "12s");
        assert_eq!(human_duration(125_000), "2m 5s");
        assert_eq!(human_duration(3_900_000), "1h 5m");
    }

    #[test]
    fn a_file_loses_only_its_final_line_break() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("brief.txt");
        fs::write(&path, "line one\r\nline two\r\n\r\n").unwrap();
        assert_eq!(
            text_argument("ignored", &Some(path)).unwrap(),
            "line one\r\nline two"
        );
        assert_eq!(text_argument("typed", &None).unwrap(), "typed");
        assert!(text_argument("", &Some(directory.path().join("missing.txt"))).is_err());
    }

    #[test]
    fn a_ticking_digit_does_not_make_a_screen_new() {
        let idle = "report done\nusage 12%  resets in 3h17\n";
        let later = "report done\nusage 13%  resets in 3h16\n";
        assert_eq!(screen_signature(idle), screen_signature(later));
        assert_ne!(
            screen_signature(idle),
            screen_signature("a new report\nusage 12%  resets in 3h17\n")
        );
        assert_ne!(screen_signature(idle), screen_signature(""));
    }

    #[test]
    fn pad_shortens_with_an_ellipsis() {
        assert_eq!(pad("abc", 5), "abc  ");
        assert_eq!(pad("abcdefgh", 5), "abcd…");
    }

    #[test]
    fn quiet_time_restarts_when_the_screen_changes() {
        let mut quiet = Quiet::new();
        quiet.observe("a");
        thread::sleep(Duration::from_millis(30));
        assert!(quiet.observe("a") >= 25);
        assert!(quiet.observe("b") < 25);
    }
}
