//! `sh` and `proc`: running programs from tasks and scripts.
//!
//! `sh` is for the common case: output streams to the terminal and a nonzero exit raises
//! `command_failed`, like `set -e`. `sh.out` captures stdout for a value. `proc.run` returns a
//! record and never raises for an exit code. `proc.spawn` starts a background process that is
//! stopped when its scope ends.
//!
//! A command string is split into words without a shell. A string with shell syntax (pipes,
//! redirects, `&&`, `$VAR`, globs) raises unless `shell = true` runs it through `/bin/sh -c`.
//! Every process runs in its own process group, so a timeout, a cancelled task or Ctrl-C
//! stops the whole tree; one that keeps the terminal's stdin stays in timon's group, so it can
//! read the terminal.

use std::cell::{Cell, RefCell};
use std::io::{IsTerminal, Write};
use std::process::Stdio;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{Function, Lua, Table, UserData, UserDataMethods, Value};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};

use crate::docs::{Func, Module, Opt};
use crate::error::Error;
use crate::modules::{Installer, concurrency};
use crate::options;
use crate::runtime::INTERNAL_CHUNK;

pub const SH_DOC: Module = Module {
    name: "sh",
    summary: "Run a program. sh(cmd, opts) streams its output and raises command_failed on a nonzero exit; sh.out captures stdout. cmd is a string, split into words without a shell, or a list { \"cargo\", \"test\", cwd = \"x\" }. Pass shell = true for pipes, redirects and globs.",
    functions: &[
        Func::new(
            "run",
            "sh.run(cmd, opts) -> { code, ms, out, err }",
            "Runs cmd and waits. sh(cmd, opts) is the same call. Output goes to the terminal unless out or err is \"capture\" or \"null\". A nonzero exit raises command_failed with the code in error.data, unless check = false; a timeout raises timeout. The process and its children stop if the task is cancelled.",
            "sh(\"cargo fmt --all -- --check\")\nsh{ \"cargo\", \"test\", \"--\", \"--nocapture\", cwd = \"crates/core\" }\nlocal r = sh(\"cargo test\", { check = false })\nif r.code ~= 0 then log.warn(\"tests failed: \" .. r.code) end",
        )
        .with_options(&[
            Opt::new("cwd", "string", "the directory to run in"),
            Opt::new("env", "table<string, string | boolean>", "variables set over the inherited ones; false unsets one"),
            Opt::new("clear_env", "boolean", "start from no variables at all; set PATH in env if the program needs it"),
            Opt::new("input", "string", "text for stdin; default: the terminal's stdin"),
            Opt::new("timeout", "number | string", "seconds, or a duration such as \"10m\"; stops the process group"),
            Opt::new("out", "\"inherit\" | \"capture\" | \"null\"", "stdout: the terminal (default), the record's out, or nowhere"),
            Opt::new("err", "\"inherit\" | \"capture\" | \"null\"", "stderr, the same way"),
            Opt::new("check", "boolean", "false returns the record instead of raising on a nonzero exit"),
            Opt::new("shell", "boolean", "run cmd through /bin/sh -c, for pipes, redirects, globs and $VAR"),
            Opt::new("prefix", "string", "put this before every line of output, so concurrent commands do not interleave"),
        ]),
        Func::new(
            "out",
            "sh.out(cmd, opts) -> string",
            "Runs cmd and returns its stdout, trimmed. stderr goes to the terminal. A nonzero exit raises command_failed.",
            "local head = sh.out(\"git rev-parse HEAD\")\nlocal files = str.lines(sh.out{ \"git\", \"ls-files\", \"src\" })",
        )
        .with_options(&[
            Opt::new("cwd", "string", "the directory to run in"),
            Opt::new("env", "table<string, string | boolean>", "variables set over the inherited ones; false unsets one"),
            Opt::new("clear_env", "boolean", "start from no variables at all"),
            Opt::new("input", "string", "text for stdin"),
            Opt::new("timeout", "number | string", "seconds, or a duration such as \"10m\""),
            Opt::new("err", "\"inherit\" | \"capture\" | \"null\"", "stderr: the terminal (default), captured, or nowhere"),
            Opt::new("check", "boolean", "false returns the output even after a nonzero exit"),
            Opt::new("shell", "boolean", "run cmd through /bin/sh -c"),
        ]),
    ],
};

pub const PROC_DOC: Module = Module {
    name: "proc",
    summary: "Processes as data. proc.run returns { code, out, err, ms, timed_out } and never raises for an exit code; proc.spawn starts a background process that stops when its scope ends.",
    functions: &[
        Func::new(
            "run",
            "proc.run(cmd, opts) -> { code, out, err, ms, timed_out }",
            "Runs cmd and waits, capturing stdout and stderr by default. A nonzero exit or a timeout is in the record, not raised; only a program that cannot start raises.",
            "local r = proc.run{ \"cargo\", \"build\", \"--release\" }\nif r.code ~= 0 then print(r.err) end",
        )
        .with_options(&[
            Opt::new("cwd", "string", "the directory to run in"),
            Opt::new("env", "table<string, string | boolean>", "variables set over the inherited ones; false unsets one"),
            Opt::new("clear_env", "boolean", "start from no variables at all"),
            Opt::new("input", "string", "text for stdin; default: none"),
            Opt::new("timeout", "number | string", "seconds, or a duration such as \"10m\"; stops the process group"),
            Opt::new("out", "\"inherit\" | \"capture\" | \"null\"", "stdout: captured (default), the terminal, or nowhere"),
            Opt::new("err", "\"inherit\" | \"capture\" | \"null\"", "stderr, the same way"),
            Opt::new("shell", "boolean", "run cmd through /bin/sh -c"),
            Opt::new("prefix", "string", "with out or err = \"inherit\", put this before every line"),
        ]),
        Func::new(
            "spawn",
            "proc.spawn(cmd, opts) -> process",
            "Starts cmd in the background and returns at once. p:wait() returns { code, ms } without blocking other tasks; p:kill() stops it and its children; p:alive(); p:pid(). With out = \"pipe\", p:lines() iterates stdout and p:read() returns the rest. A process still running when its scope ends is stopped.",
            "local server = proc.spawn({ \"python3\", \"-m\", \"http.server\", \"8000\" }, { out = \"null\" })\nsleep(\"200ms\")\n-- ... use it ...\nserver:kill()",
        )
        .with_options(&[
            Opt::new("cwd", "string", "the directory to run in"),
            Opt::new("env", "table<string, string | boolean>", "variables set over the inherited ones; false unsets one"),
            Opt::new("clear_env", "boolean", "start from no variables at all"),
            Opt::new("input", "string", "text for stdin; default: none"),
            Opt::new("out", "\"inherit\" | \"pipe\" | \"null\"", "stdout: the terminal (default), readable with p:lines() and p:read(), or nowhere"),
            Opt::new("err", "\"inherit\" | \"null\"", "stderr: the terminal (default) or nowhere"),
            Opt::new("shell", "boolean", "run cmd through /bin/sh -c"),
        ]),
    ],
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stream {
    Inherit,
    Capture,
    Null,
    Pipe,
}

impl Stream {
    fn parse(value: Option<String>, default: Stream) -> Stream {
        match value.as_deref() {
            Some("inherit") => Stream::Inherit,
            Some("capture") => Stream::Capture,
            Some("null") => Stream::Null,
            Some("pipe") => Stream::Pipe,
            _ => default,
        }
    }
}

/// What to run, and how.
struct Spec {
    argv: Vec<String>,
    /// The command as the caller wrote it, for messages.
    display: String,
    cwd: Option<String>,
    env: Vec<(String, Option<String>)>,
    clear_env: bool,
    input: Option<String>,
    /// Keep the terminal's stdin when there is no input.
    inherit_stdin: bool,
    timeout: Option<Duration>,
    out: Stream,
    err: Stream,
    check: bool,
    prefix: Option<String>,
}

/// The first piece of shell syntax in an unquoted (or double-quoted) part of `s`.
pub(crate) fn shell_syntax(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let (mut single, mut double, mut escaped) = (false, false, false);
    let mut word_start = true;
    for (i, &c) in chars.iter().enumerate() {
        if escaped {
            escaped = false;
            word_start = false;
            continue;
        }
        match c {
            '\\' if !single => escaped = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            _ if single => {}
            '$' if chars.get(i + 1).is_some_and(|n| {
                *n == '(' || *n == '{' || n.is_ascii_alphabetic() || *n == '_'
            }) =>
            {
                return Some(format!("${}", chars[i + 1]));
            }
            '`' => return Some("`".into()),
            '|' | '&' | ';' | '<' | '>' | '*' if !double => return Some(c.to_string()),
            '~' if !double && word_start => return Some("~".into()),
            _ => {}
        }
        word_start = c.is_whitespace();
    }
    None
}

/// Reads the command and its options. `cmd` is a string or a list of words, which may also
/// hold the options as named fields.
fn spec(
    lua: &Lua,
    path: &str,
    cmd: Value,
    opts: Option<Table>,
    defaults: (Stream, Stream, bool),
) -> mlua::Result<Spec> {
    crate::tasks::refuse_while_loading(lua, path)?;
    let merged = lua.create_table()?;
    let mut argv: Option<Vec<String>> = None;
    let mut text: Option<String> = None;
    match cmd {
        Value::String(s) => text = Some(s.to_string_lossy()),
        Value::Table(t) => {
            let mut words = Vec::new();
            for pair in t.pairs::<Value, Value>() {
                let (k, v) = pair?;
                match k {
                    Value::Integer(_) => {}
                    Value::String(_) => merged.raw_set(k, v)?,
                    other => {
                        return Err(Error::new(
                            "bad_argument",
                            format!("{path}: a {} key in the command list", other.type_name()),
                        )
                        .into());
                    }
                }
            }
            for i in 1..=t.raw_len() {
                match t.raw_get::<Value>(i)? {
                    Value::String(s) => words.push(s.to_string_lossy()),
                    Value::Integer(n) => words.push(n.to_string()),
                    Value::Number(n) => words.push(n.to_string()),
                    other => {
                        return Err(Error::new(
                            "bad_argument",
                            format!("{path}: word {i} of the command is a {}", other.type_name()),
                        )
                        .into());
                    }
                }
            }
            argv = Some(words);
        }
        other => {
            return Err(Error::new(
                "bad_argument",
                format!(
                    "{path} takes a command string or a list of words, not a {}",
                    other.type_name()
                ),
            )
            .into());
        }
    }
    if let Some(opts) = &opts {
        for pair in opts.pairs::<Value, Value>() {
            let (k, v) = pair?;
            merged.raw_set(k, v)?;
        }
    }
    options::check(path, &merged)?;
    let get = |k: &str| merged.raw_get::<Option<String>>(k);
    let shell = merged.raw_get::<Option<bool>>("shell")?.unwrap_or(false);
    let display = match (&text, &argv) {
        (Some(t), _) => t.clone(),
        (None, Some(words)) => shell_words::join(words),
        _ => String::new(),
    };
    let argv = match (text, argv) {
        (Some(t), _) if shell => vec!["/bin/sh".into(), "-c".into(), t],
        (None, Some(words)) if shell => {
            vec!["/bin/sh".into(), "-c".into(), shell_words::join(&words)]
        }
        (Some(t), _) => {
            if let Some(syntax) = shell_syntax(&t) {
                return Err(Error::new(
                    "bad_argument",
                    format!("`{t}` uses shell syntax ({syntax}), which only a shell runs"),
                )
                .with_hint("pass shell = true, give a list of words, single-quote it to pass it as is, or use fs.glob for patterns")
                .into());
            }
            shell_words::split(&t).map_err(|e| {
                Error::new(
                    "bad_argument",
                    format!("cannot split `{t}` into words: {e}"),
                )
                .with_hint("check its quotes, or give a list of words")
            })?
        }
        (None, Some(words)) => words,
        (None, None) => Vec::new(),
    };
    if argv.is_empty() {
        return Err(Error::new("bad_argument", format!("{path}: the command is empty")).into());
    }
    let mut env = Vec::new();
    if let Some(t) = merged.raw_get::<Option<Table>>("env")? {
        for pair in t.pairs::<String, Value>() {
            let (k, v) = pair?;
            match v {
                Value::Boolean(false) => env.push((k, None)),
                Value::String(s) => env.push((k, Some(s.to_string_lossy()))),
                _ => {}
            }
        }
    }
    let timeout: Value = merged.raw_get("timeout")?;
    Ok(Spec {
        argv,
        display,
        cwd: get("cwd")?,
        env,
        clear_env: merged
            .raw_get::<Option<bool>>("clear_env")?
            .unwrap_or(false),
        input: get("input")?,
        inherit_stdin: defaults.2,
        timeout: if timeout.is_nil() {
            None
        } else {
            Some(concurrency::duration(&timeout)?)
        },
        out: Stream::parse(get("out")?, defaults.0),
        err: Stream::parse(get("err")?, defaults.1),
        check: merged.raw_get::<Option<bool>>("check")?.unwrap_or(true),
        prefix: get("prefix")?,
    })
}

/// Stops a process group when dropped, unless disarmed: a cancelled or timed-out run leaves
/// nothing behind. Without a group, only the process.
struct Stopper {
    pid: Option<u32>,
    group: bool,
    armed: Cell<bool>,
}

impl Stopper {
    fn signal(&self, sig: i32) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // SAFETY: signals a process, or the process group, this module started.
            unsafe {
                if self.group {
                    libc::killpg(pid as i32, sig);
                } else {
                    libc::kill(pid as i32, sig);
                }
            }
        }
    }

    /// TERM now, KILL shortly after from a helper thread if it is still there.
    fn stop(&self) {
        #[cfg(unix)]
        {
            self.signal(libc::SIGTERM);
            let (pid, group) = (self.pid, self.group);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(250));
                if let Some(pid) = pid {
                    // SAFETY: as above; it did not stop on TERM.
                    unsafe {
                        if group {
                            libc::killpg(pid as i32, libc::SIGKILL);
                        } else {
                            libc::kill(pid as i32, libc::SIGKILL);
                        }
                    }
                }
            });
        }
    }
}

impl Drop for Stopper {
    fn drop(&mut self) {
        if self.armed.get() {
            self.stop();
        }
    }
}

fn command(spec: &Spec, stdin_tty: bool) -> (Command, bool) {
    let mut cmd = Command::new(&spec.argv[0]);
    cmd.args(&spec.argv[1..]);
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    if spec.clear_env {
        cmd.env_clear();
    }
    for (k, v) in &spec.env {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
    let interactive = spec.input.is_none() && spec.inherit_stdin && stdin_tty;
    cmd.stdin(if spec.input.is_some() {
        Stdio::piped()
    } else if spec.inherit_stdin {
        Stdio::inherit()
    } else {
        Stdio::null()
    });
    let stdio = |s: Stream| match s {
        Stream::Inherit if spec.prefix.is_some() => Stdio::piped(),
        Stream::Inherit => Stdio::inherit(),
        Stream::Null => Stdio::null(),
        Stream::Capture | Stream::Pipe => Stdio::piped(),
    };
    cmd.stdout(stdio(spec.out));
    cmd.stderr(stdio(spec.err));
    cmd.kill_on_drop(true);
    // A program that reads the terminal must be in its foreground group; others get their own
    // group, so stopping them stops their children too.
    #[cfg(unix)]
    if !interactive {
        cmd.process_group(0);
    }
    (cmd, !interactive)
}

fn spawn_error(spec: &Spec, e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        Error::new(
            "not_found",
            format!(
                "cannot run `{}`: no program named {}",
                spec.display, spec.argv[0]
            ),
        )
        .with_hint("check its name and PATH, or give the full path")
    } else {
        Error::new("io", format!("cannot run `{}`: {e}", spec.display))
    }
}

/// How a finished run went.
struct Outcome {
    code: i64,
    out: Option<String>,
    err: Option<String>,
    ms: u64,
    timed_out: bool,
}

fn exit_code(status: std::process::ExitStatus) -> i64 {
    if let Some(code) = status.code() {
        return i64::from(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + i64::from(sig);
        }
    }
    -1
}

/// Reads a stream: into a string when capturing, or line by line to our own stdout or stderr
/// with a prefix.
async fn drain<R: AsyncRead + Unpin>(
    stream: Option<R>,
    capture: bool,
    prefix: Option<String>,
    to_stderr: bool,
) -> Option<String> {
    let stream = stream?;
    if capture {
        let mut bytes = Vec::new();
        let mut reader = stream;
        let _ = reader.read_to_end(&mut bytes).await;
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    let prefix = prefix.unwrap_or_default();
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let text = format!("{prefix}{line}\n");
        // One write per line, so concurrent commands do not interleave within a line.
        if to_stderr {
            let _ = std::io::stderr().lock().write_all(text.as_bytes());
        } else {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(text.as_bytes());
            let _ = out.flush();
        }
    }
    None
}

async fn execute(spec: &Spec) -> Result<Outcome, Error> {
    let started = Instant::now();
    let (mut cmd, group) = command(spec, std::io::stdin().is_terminal());
    let mut child = cmd.spawn().map_err(|e| spawn_error(spec, e))?;
    let stopper = Stopper {
        pid: child.id(),
        group,
        armed: Cell::new(true),
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let input = spec.input.clone();
    let write = async move {
        if let (Some(mut stdin), Some(input)) = (stdin, input) {
            let _ = stdin.write_all(input.as_bytes()).await;
        }
    };
    let out = drain(
        stdout,
        spec.out == Stream::Capture,
        spec.prefix.clone(),
        false,
    );
    let err = drain(
        stderr,
        spec.err == Stream::Capture,
        spec.prefix.clone(),
        true,
    );
    let run = async {
        let (_, out, err, status) = tokio::join!(write, out, err, child.wait());
        (out, err, status)
    };
    let finished = match spec.timeout {
        Some(limit) => tokio::time::timeout(limit, run).await.ok(),
        None => Some(run.await),
    };
    let ms = started.elapsed().as_millis() as u64;
    match finished {
        Some((out, err, status)) => {
            stopper.armed.set(false);
            let status = status.map_err(|e| {
                Error::new("io", format!("cannot wait for `{}`: {e}", spec.display))
            })?;
            Ok(Outcome {
                code: exit_code(status),
                out,
                err,
                ms,
                timed_out: false,
            })
        }
        None => {
            stopper.stop();
            stopper.armed.set(false);
            let _ = child.wait().await;
            Ok(Outcome {
                code: 124,
                out: None,
                err: None,
                ms,
                timed_out: true,
            })
        }
    }
}

fn outcome_table(lua: &Lua, o: &Outcome, with_timed_out: bool) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.raw_set("code", o.code)?;
    t.raw_set("ms", o.ms)?;
    if let Some(out) = &o.out {
        t.raw_set("out", out.as_str())?;
    }
    if let Some(err) = &o.err {
        t.raw_set("err", err.as_str())?;
    }
    if with_timed_out {
        t.raw_set("timed_out", o.timed_out)?;
    }
    Ok(t)
}

/// The raise for a run that did not succeed, when the caller wants one.
fn failure(spec: &Spec, o: &Outcome) -> Option<Error> {
    if o.timed_out {
        return Some(
            Error::new(
                "timeout",
                format!(
                    "`{}` did not finish in {:.1} s",
                    spec.display,
                    spec.timeout.unwrap_or_default().as_secs_f64()
                ),
            )
            .with_hint("raise timeout"),
        );
    }
    if o.code != 0 && spec.check {
        let tail: Option<String> = o.err.as_ref().map(|e| {
            let lines: Vec<&str> = e.trim_end().lines().collect();
            lines[lines.len().saturating_sub(5)..].join("\n")
        });
        let mut message = format!("`{}` exited {}", spec.display, o.code);
        if let Some(tail) = tail.as_ref().filter(|t| !t.is_empty()) {
            message.push_str(&format!(": {}", tail.replace('\n', " | ")));
        }
        return Some(
            Error::new("command_failed", message)
                .with_exit(o.code)
                .with_hint("pass check = false to handle the exit code yourself")
                .with_data(json!({ "code": o.code, "command": spec.display, "err": tail })),
        );
    }
    None
}

/// A process started with `proc.spawn`.
struct Process {
    display: String,
    pid: Option<u32>,
    started: Instant,
    child: tokio::sync::Mutex<Child>,
    stdout: tokio::sync::Mutex<Option<tokio::io::Lines<BufReader<ChildStdout>>>>,
    exit: RefCell<Option<(i64, u64)>>,
    stopper: Rc<Stopper>,
}

impl Process {
    async fn wait(&self) -> mlua::Result<(i64, u64)> {
        if let Some(done) = *self.exit.borrow() {
            return Ok(done);
        }
        let mut child = self.child.lock().await;
        let status = child
            .wait()
            .await
            .map_err(|e| Error::new("io", format!("cannot wait for `{}`: {e}", self.display)))?;
        let done = (exit_code(status), self.started.elapsed().as_millis() as u64);
        *self.exit.borrow_mut() = Some(done);
        self.stopper.armed.set(false);
        Ok(done)
    }

    fn alive(&self) -> bool {
        if self.exit.borrow().is_some() {
            return false;
        }
        let Ok(mut child) = self.child.try_lock() else {
            return true;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                *self.exit.borrow_mut() =
                    Some((exit_code(status), self.started.elapsed().as_millis() as u64));
                self.stopper.armed.set(false);
                false
            }
            _ => true,
        }
    }
}

#[derive(Clone)]
struct ProcessHandle(Rc<Process>);

impl UserData for ProcessHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method("wait", |lua, this, ()| async move {
            let (code, ms) = this.0.wait().await?;
            let t = lua.create_table()?;
            t.raw_set("code", code)?;
            t.raw_set("ms", ms)?;
            Ok(t)
        });
        methods.add_method("kill", |_, this, ()| {
            if this.0.alive() {
                this.0.stopper.stop();
            }
            Ok(())
        });
        methods.add_method("alive", |_, this, ()| Ok(this.0.alive()));
        methods.add_method("pid", |_, this, ()| Ok(this.0.pid));
        methods.add_async_method("read", |_, this, ()| async move {
            let mut guard = this.0.stdout.lock().await;
            let Some(lines) = guard.as_mut() else {
                return Err(not_piped().into());
            };
            let mut text = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                text.push_str(&line);
                text.push('\n');
            }
            Ok(text)
        });
        methods.add_method("lines", |lua, this, ()| {
            let process = this.0.clone();
            lua.create_async_function(move |_, ()| {
                let process = process.clone();
                async move {
                    let mut guard = process.stdout.lock().await;
                    let Some(lines) = guard.as_mut() else {
                        return Err(not_piped().into());
                    };
                    Ok(lines.next_line().await.ok().flatten())
                }
            })
        });
        methods.add_meta_method(mlua::MetaMethod::ToString, |_, this, ()| {
            Ok(format!("process({})", this.0.display))
        });
    }
}

fn not_piped() -> Error {
    Error::new("bad_argument", "this process's stdout is not piped")
        .with_hint("start it with proc.spawn(cmd, { out = \"pipe\" })")
}

fn spawn_process(
    lua: &Lua,
    thread: mlua::Thread,
    cmd: Value,
    opts: Option<Table>,
) -> mlua::Result<ProcessHandle> {
    let spec = spec(
        lua,
        "proc.spawn",
        cmd,
        opts,
        (Stream::Inherit, Stream::Inherit, false),
    )?;
    let (mut command, group) = command(&spec, false);
    let mut child = command.spawn().map_err(|e| spawn_error(&spec, e))?;
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), spec.input.clone()) {
        tokio::task::spawn_local(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
        });
    }
    let stdout = child.stdout.take().map(|s| BufReader::new(s).lines());
    let stopper = Rc::new(Stopper {
        pid: child.id(),
        group,
        armed: Cell::new(true),
    });
    let process = Rc::new(Process {
        display: spec.display,
        pid: child.id(),
        started: Instant::now(),
        child: tokio::sync::Mutex::new(child),
        stdout: tokio::sync::Mutex::new(stdout),
        exit: RefCell::new(None),
        stopper,
    });
    // Stopped when its scope ends, unless it has finished by then.
    let weak = Rc::downgrade(&process);
    concurrency::on_scope_end(
        lua,
        &thread,
        Box::new(move || {
            if let Some(p) = weak.upgrade()
                && p.alive()
            {
                p.stopper.stop();
                p.stopper.armed.set(false);
            }
        }),
    );
    Ok(ProcessHandle(process))
}

/// `sh(...)` calls `sh.run`; `proc.spawn` passes its caller's coroutine, which names the scope
/// the process belongs to.
const GLUE: &str = r#"
local sh, spawn_from, running = ...
setmetatable(sh, (function(mt)
  mt.__call = function(_, ...) return sh.run(...) end
  return mt
end)(getmetatable(sh)))
return function(cmd, opts) return spawn_from((running()), cmd, opts) end
"#;

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let sh = installer.module(&SH_DOC)?;
    installer.function(
        &SH_DOC,
        Some(&sh),
        "run",
        lua.create_async_function(|lua, (cmd, opts): (Value, Option<Table>)| async move {
            let spec = spec(
                &lua,
                "sh.run",
                cmd,
                opts,
                (Stream::Inherit, Stream::Inherit, true),
            )?;
            let o = execute(&spec).await?;
            if let Some(err) = failure(&spec, &o) {
                return Err(err.into());
            }
            outcome_table(&lua, &o, false)
        })?,
    )?;
    installer.function(
        &SH_DOC,
        Some(&sh),
        "out",
        lua.create_async_function(|lua, (cmd, opts): (Value, Option<Table>)| async move {
            let mut spec = spec(
                &lua,
                "sh.out",
                cmd,
                opts,
                (Stream::Capture, Stream::Inherit, false),
            )?;
            spec.out = Stream::Capture;
            let o = execute(&spec).await?;
            if let Some(err) = failure(&spec, &o) {
                return Err(err.into());
            }
            Ok(o.out.unwrap_or_default().trim().to_owned())
        })?,
    )?;
    let proc = installer.module(&PROC_DOC)?;
    installer.function(
        &PROC_DOC,
        Some(&proc),
        "run",
        lua.create_async_function(|lua, (cmd, opts): (Value, Option<Table>)| async move {
            let spec = spec(
                &lua,
                "proc.run",
                cmd,
                opts,
                (Stream::Capture, Stream::Capture, false),
            )?;
            let o = execute(&spec).await?;
            outcome_table(&lua, &o, true)
        })?,
    )?;
    let spawn_from = installer.wrap(lua.create_function(
        |lua, (thread, cmd, opts): (mlua::Thread, Value, Option<Table>)| {
            spawn_process(lua, thread, cmd, opts)
        },
    )?)?;
    let coroutine: Table = lua.globals().raw_get("coroutine")?;
    let running: Function = coroutine.raw_get("running")?;
    let spawn: Function = lua
        .load(GLUE)
        .set_name(INTERNAL_CHUNK)
        .call((sh, spawn_from, running))?;
    installer.lua_function(&PROC_DOC, Some(&proc), "spawn", spawn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::shell_syntax;

    #[test]
    fn shell_syntax_is_found_outside_single_quotes() {
        assert_eq!(shell_syntax("cargo test -- --nocapture"), None);
        assert_eq!(shell_syntax("ls | wc -l").as_deref(), Some("|"));
        assert_eq!(shell_syntax("a && b").as_deref(), Some("&"));
        assert_eq!(shell_syntax("echo $HOME").as_deref(), Some("$H"));
        assert_eq!(shell_syntax("ls *.rs").as_deref(), Some("*"));
        assert_eq!(shell_syntax("cd ~/x").as_deref(), Some("~"));
        assert_eq!(shell_syntax("echo 'a | b $HOME *'"), None);
        assert_eq!(shell_syntax("echo \"a | b\""), None);
        assert_eq!(shell_syntax("echo \"$HOME\"").as_deref(), Some("$H"));
        assert_eq!(shell_syntax("git log --format=%h~1"), None);
        assert_eq!(shell_syntax("curl https://x.test/?a=1"), None);
        assert_eq!(shell_syntax("echo a\\|b"), None);
    }
}
