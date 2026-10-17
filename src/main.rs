use std::ffi::OsString;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use clap::{CommandFactory, Parser, Subcommand};
use mlua::MultiValue;
use timon::dirs::Dirs;
use timon::layers::{self, Layers};
use timon::modules::agent::Progress;
use timon::{Error, Failure, Runtime};

// stdout for the CLI. These shadow std's macros in this file: when the reader stops early
// (`timon tasks --json | head`), timon ends quietly instead of panicking on the broken pipe
// (#115). Nothing is running by the time the CLI prints, so exiting here leaves nothing behind.
macro_rules! println {
    () => {
        out(format_args!("\n"))
    };
    ($($arg:tt)*) => {
        out(format_args!("{}\n", format_args!($($arg)*)))
    };
}

macro_rules! print {
    ($($arg:tt)*) => {
        out(format_args!($($arg)*))
    };
}

fn out(args: std::fmt::Arguments) {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = stdout.write_fmt(args).and_then(|()| stdout.flush()) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        eprintln!("timon: cannot write to stdout: {e}");
        std::process::exit(1);
    }
}

#[derive(Parser)]
#[command(
    name = "timon",
    version,
    about = "A task runner with agents built in: tasks and scripts in Lua or Fennel",
    after_help = "Run a task with `timon NAME [WORDS]` (`timon tasks` lists them), or a script \
                  with `timon FILE.lua [ARGS]` or `timon FILE.fnl [ARGS]`.",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Most agent runs this invocation may start
    #[arg(long, global = true, value_name = "N")]
    max_runs: Option<u64>,
    /// Most USD this invocation's Claude runs may spend
    #[arg(long, global = true, value_name = "USD")]
    max_usd: Option<f64>,
    /// Agent run progress on stderr; default: text when stderr is a terminal, else none
    #[arg(long, global = true, value_parser = ["text", "json", "none"])]
    progress: Option<String>,
    /// Run the script again, reusing the agent runs of this earlier script run that can be
    /// reused (read-only, idempotent, or in an unchanged worktree)
    #[arg(long, global = true, value_name = "SCRIPT_RUN")]
    resume: Option<String>,
    /// Answer yes to every confirm question of the tasks that run
    #[arg(long, global = true)]
    yes: bool,
    /// Most of a task's independent dependencies running at once
    #[arg(long, short = 'j', global = true, value_name = "N", default_value_t = 1,
          value_parser = clap::value_parser!(u16).range(1..))]
    jobs: u16,
    /// Print a task's plan instead of running it
    #[arg(long, global = true)]
    dry_run: bool,
}

// Built once per process from the command line, so a large variant costs nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// Evaluate Lua and print each value it returns
    Eval {
        /// Lua source; `-` reads it from standard input
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        code: Option<String>,
        /// Evaluate a file instead; a .fnl file is Fennel
        #[arg(short, long, value_name = "FILE")]
        file: Option<PathBuf>,
        /// Treat CODE as Fennel
        #[arg(long)]
        fnl: bool,
        /// Bind `input` to the lines of stdin, as a list
        #[arg(short = 'i', long, conflicts_with = "in_json")]
        in_lines: bool,
        /// Bind `input` to the JSON values on stdin (one document, or JSON lines), as a list
        #[arg(short = 'I', long)]
        in_json: bool,
        /// Print the returned list one element per line: strings as they are, the rest as JSON
        #[arg(short = 'o', long, conflicts_with = "out_json")]
        out_lines: bool,
        /// Print the returned list as JSON lines
        #[arg(short = 'O', long)]
        out_json: bool,
        /// With -i or -I, run once per line or value, with `input` bound to it; nil prints nothing
        #[arg(long)]
        stream: bool,
    },
    /// Run a task after its dependencies, even one a timon command shares a name with
    #[command(alias = "run")]
    Task {
        name: String,
        /// Words for the task (`opts.args`), or a script's arguments (`arg` and `...`)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List the tasks in reach: tasks.lua and scripts/, in the user and project layers
    Tasks {
        /// Include private tasks and timon's own commands
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print a shell completion script for bash, zsh or fish
    Completions {
        #[arg(value_parser = ["bash", "zsh", "fish"])]
        shell: String,
    },
    /// Completion candidates for the words after timon; the completion scripts call it
    #[command(name = "__complete", hide = true)]
    Complete {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        words: Vec<String>,
    },
    /// List the scripts in reach
    Scripts {
        #[arg(long)]
        json: bool,
    },
    /// Trust a project's .timon directory so its scripts, lib and settings load
    Trust {
        /// The project root; defaults to the nearest one with a .timon directory
        dir: Option<PathBuf>,
        /// Stop trusting it
        #[arg(long)]
        revoke: bool,
    },
    /// Show the layers, settings, agents, scripts and lib directories in effect
    Defs {
        #[arg(long)]
        json: bool,
    },
    /// Run a Claude or Codex agent and print the run record
    #[command(subcommand)]
    Agent(AgentCommand),
    /// List agent definitions from agents/ in the user and project layers
    Agents {
        #[arg(long)]
        json: bool,
    },
    /// Find, read, search and summarize Claude Code and Codex sessions
    #[command(subcommand)]
    Sessions(SessionsCommand),
    /// List recorded agent runs, newest first, or show one
    Runs {
        #[command(subcommand)]
        command: Option<RunsCommand>,
        /// Only runs started within this long, such as 1d or 2h
        #[arg(long)]
        since: Option<String>,
        /// Most runs to list
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// List script runs instead of agent runs
        #[arg(long)]
        scripts: bool,
        #[arg(long)]
        json: bool,
    },
    /// List and read skills: built in, user and project
    #[command(subcommand)]
    Skill(SkillCommand),
    /// Build the plugin directory from the binary
    #[command(subcommand)]
    Plugin(PluginCommand),
    /// Use MCP servers: the named ones in mcp/, a server's tools, a tool call
    #[command(subcommand)]
    Mcp(McpCommand),
    /// Serve this project's tasks as MCP tools over stdio, with eval and the skills
    Serve,
    /// Run a Claude Code or Codex hook: reads the event's JSON on stdin, runs the handlers
    /// init.lua's hooks setting names, and prints the host's JSON
    Hook {
        /// The event, such as SessionStart; default: the input's hook_event_name
        event: Option<String>,
    },
    #[command(external_subcommand)]
    Script(Vec<OsString>),
}

#[derive(Subcommand)]
enum AgentCommand {
    /// One run, with the agent.run options as flags; prints the record as JSON
    Run {
        /// What to do; `-` reads it from standard input
        prompt: String,
        /// A definition from agents/NAME.lua to start from
        #[arg(long)]
        agent: Option<String>,
        /// claude (default) or codex
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        /// read (default), write or full
        #[arg(long)]
        access: Option<String>,
        /// Claude's tool list, comma-separated
        #[arg(long, value_delimiter = ',')]
        tools: Option<Vec<String>>,
        #[arg(long)]
        cwd: Option<String>,
        /// Instructions added to the system prompt
        #[arg(long)]
        system: Option<String>,
        /// A contract name; the answer is validated against it
        #[arg(long)]
        output: Option<String>,
        /// Seconds, or a duration such as 10m
        #[arg(long)]
        timeout: Option<String>,
        /// Most USD this run may spend (claude)
        #[arg(long)]
        budget_usd: Option<f64>,
        /// Most turns this run may take (claude)
        #[arg(long)]
        budget_turns: Option<u32>,
        /// A session id timon started, to continue
        #[arg(long)]
        resume: Option<String>,
        /// A session id to branch from (claude)
        #[arg(long)]
        fork: Option<String>,
        /// Add the agent's text, reasoning, tool calls and tool results to the record
        #[arg(long)]
        events: bool,
        /// Run in a new git worktree
        #[arg(long)]
        worktree: bool,
        /// An extra provider argument; repeat for more
        #[arg(long = "native", allow_hyphen_values = true)]
        native: Vec<String>,
    },
}

/// Flags every `timon sessions` command shares.
#[derive(clap::Args)]
struct SessionFlags {
    /// "claude" or "codex"
    #[arg(long)]
    backend: Option<String>,
    /// Print the result as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum SessionsCommand {
    /// List sessions, newest first
    List {
        #[command(flatten)]
        flags: SessionFlags,
        /// Only sessions whose directory contains this
        #[arg(long)]
        project: Option<String>,
        /// Only sessions that ran in this directory or below it; faster than --project
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
        /// A duration back (7d, 2h), a date, or an RFC 3339 time
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        limit: Option<i64>,
        /// Include subagent sessions
        #[arg(long)]
        subagents: bool,
        /// Include sessions started by timon or other automation
        #[arg(long)]
        automated: bool,
    },
    /// Show one session: where it is, its project and how to resume it
    Show {
        /// A session id or an unambiguous prefix
        id: String,
        #[command(flatten)]
        flags: SessionFlags,
    },
    /// Show the session timon is running inside
    Current {
        #[arg(long)]
        json: bool,
    },
    /// Print a page of a session's transcript
    Transcript {
        id: String,
        #[command(flatten)]
        flags: SessionFlags,
        /// The first record index
        #[arg(long)]
        offset: Option<i64>,
        #[arg(long)]
        limit: Option<i64>,
        /// Only these record kinds: user, assistant, tool_call, tool_result, reasoning,
        /// attachment, system, omitted
        #[arg(long = "kind", value_name = "KIND")]
        kinds: Vec<String>,
    },
    /// Search the text of every session
    Search {
        query: String,
        #[command(flatten)]
        flags: SessionFlags,
        /// terms (default), phrase, fts or regex
        #[arg(long)]
        mode: Option<String>,
        /// Only sessions whose directory contains this
        #[arg(long)]
        project: Option<String>,
        /// A duration back (7d, 2h), a date, or an RFC 3339 time
        #[arg(long)]
        since: Option<String>,
        /// The same forms as --since
        #[arg(long)]
        until: Option<String>,
        /// Most sessions returned
        #[arg(long)]
        limit: Option<i64>,
        /// Most matches shown per session
        #[arg(long)]
        per_session: Option<i64>,
        /// relevance (default) or recent
        #[arg(long)]
        order: Option<String>,
        #[arg(long)]
        subagents: bool,
        /// Include sessions written in the last 10 minutes
        #[arg(long)]
        active: bool,
        #[arg(long)]
        automated: bool,
        /// Leave out this session instead of the current one
        #[arg(long, value_name = "ID")]
        exclude: Option<String>,
        /// Leave out no session, not even the current one
        #[arg(long, conflicts_with = "exclude")]
        include_current: bool,
    },
    /// Summarize a session with Claude; spends tokens unless cached
    Summarize {
        id: String,
        #[command(flatten)]
        flags: SessionFlags,
        /// The Claude model (default haiku)
        #[arg(long)]
        model: Option<String>,
        /// Summarize again even when the cache is current
        #[arg(long)]
        force: bool,
    },
    /// Bring the session index fully up to date, with no time limit
    Index {
        /// Delete the index and build it from nothing
        #[arg(long)]
        rebuild: bool,
    },
}

#[derive(Subcommand)]
enum RunsCommand {
    /// Print a run's request and result
    Show {
        /// The run id, or an unambiguous prefix
        id: String,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// The named servers in the layers' mcp/ directories
    Servers {
        #[arg(long)]
        json: bool,
    },
    /// A server's tools, with their descriptions
    Tools {
        /// A server named in mcp/
        server: String,
        #[arg(long)]
        json: bool,
    },
    /// Call a tool and print its result
    Call {
        /// A server named in mcp/
        server: String,
        tool: String,
        /// Arguments as key=value; a value that parses as JSON is taken as JSON
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Write the plugin's skills/ from the skills built into this binary
    Build {
        /// The plugin directory
        #[arg(default_value = "plugin")]
        dir: PathBuf,
        /// Report drift and exit 1 instead of writing
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum SkillCommand {
    /// List the skills in reach
    List {
        #[arg(long)]
        json: bool,
    },
    /// Print a skill, or one of its references
    Show {
        name: String,
        /// The reference to print instead of SKILL.md
        #[arg(long = "ref", value_name = "REF")]
        reference: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("timon: cannot start the async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    tokio::task::LocalSet::new().block_on(&rt, async {
        tokio::select! {
            code = run(cli) => code,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("timon: interrupted");
                ExitCode::from(130)
            }
        }
    })
}

async fn run(cli: Cli) -> ExitCode {
    let layers = match discover() {
        Ok(layers) => layers,
        Err(err) => return fail(&err.into()),
    };
    let how = timon::tasks::How {
        yes: cli.yes,
        jobs: cli.jobs as usize,
        dry_run: cli.dry_run,
    };
    match cli.command {
        Command::Scripts { json } => return scripts(&layers, json),
        Command::Trust { dir, revoke } => return trust(&layers, dir, revoke),
        Command::Defs { json } => return defs(&layers, json),
        Command::Skill(command) => return skill(&layers, command),
        Command::Agents { json } => return agents(&layers, json),
        Command::Sessions(SessionsCommand::Index { rebuild }) => {
            return sessions_index(&layers, rebuild);
        }
        Command::Runs {
            command,
            since,
            limit,
            scripts,
            json,
        } => {
            if scripts {
                return script_runs(&layers, limit, json);
            }
            return runs(&layers, command, since, limit, json);
        }
        Command::Plugin(PluginCommand::Build { dir, check }) => return plugin_build(&dir, check),
        Command::Hook { event } => return hook(&layers, event).await,
        Command::Serve => {
            return match timon::serve::serve(layers, builtins()).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(failure) => fail(&failure),
            };
        }
        Command::Completions { shell } => {
            print!("{}", timon::complete::script(&shell).unwrap_or_default());
            return ExitCode::SUCCESS;
        }
        Command::Eval { .. }
        | Command::Task { .. }
        | Command::Tasks { .. }
        | Command::Complete { .. }
        | Command::Script(_)
        | Command::Agent(_)
        | Command::Mcp(_)
        | Command::Sessions(_) => {}
    }
    // The runtime must outlive the values it returns, which refer into its Lua state.
    let rt = match runtime(&layers) {
        Ok(rt) => rt,
        Err(failure) => return fail(&failure),
    };
    rt.set_limits(cli.max_runs, cli.max_usd);
    rt.set_resume(cli.resume.clone());
    rt.set_progress(match cli.progress.as_deref() {
        Some("text") => Progress::Text,
        Some("json") => Progress::Json,
        Some(_) => Progress::Off,
        None if std::io::stderr().is_terminal() => Progress::Text,
        None => Progress::Off,
    });
    let command = match cli.command {
        Command::Sessions(command) => return sessions(&rt, command).await,
        Command::Tasks { all, json } => return tasks(&rt, &layers, all, json),
        Command::Complete { words } => return complete(&rt, &layers, words),
        command => command,
    };
    let result = match command {
        Command::Eval {
            code,
            file,
            fnl,
            in_lines,
            in_json,
            out_lines,
            out_json,
            stream,
        } if in_lines || in_json || out_lines || out_json || stream => {
            let io = EvalIo {
                in_lines,
                in_json,
                out_lines,
                out_json,
                stream,
            };
            // With an input or output flag, a file is evaluated like CODE: its value is printed
            // the way the flag says, and no script run is recorded.
            return match (code, file) {
                (Some(code), None) => eval_io(&rt, &code, "=eval", fnl, io).await,
                (None, Some(path)) => match std::fs::read_to_string(&path) {
                    Ok(source) => {
                        let fennel = fnl || path.extension().is_some_and(|e| e == "fnl");
                        let name = format!("@{}", path.display());
                        eval_io(&rt, &source, &name, fennel, io).await
                    }
                    Err(e) => fail(
                        &Error::new("io", format!("cannot read {}: {e}", path.display())).into(),
                    ),
                },
                _ => fail(&Error::new("usage", "give CODE or -f FILE").into()),
            };
        }
        Command::Eval {
            code, file, fnl, ..
        } => eval(&rt, code, file, fnl).await,
        Command::Task { name, args } => match catalog(&rt, &layers) {
            Ok(catalog) => timon::tasks::run(&rt, catalog, &name, &args, how).await,
            Err(failure) => Err(failure),
        },
        Command::Script(argv) => external(&rt, &layers, argv, how).await,
        Command::Agent(AgentCommand::Run { .. }) => agent_run(&rt, command).await,
        Command::Mcp(command) => mcp(&rt, command).await,
        Command::Scripts { .. }
        | Command::Tasks { .. }
        | Command::Completions { .. }
        | Command::Complete { .. }
        | Command::Serve
        | Command::Hook { .. }
        | Command::Trust { .. }
        | Command::Defs { .. }
        | Command::Skill(_)
        | Command::Runs { .. }
        | Command::Agents { .. }
        | Command::Sessions(_)
        | Command::Plugin(_) => Ok(MultiValue::new()),
    };
    match result {
        Ok(values) => print(&values),
        Err(failure) => fail(&failure),
    }
}

/// `timon hook`: always exits 0, so a failing hook never stops the host.
async fn hook(layers: &Layers, event: Option<String>) -> ExitCode {
    let mut input = String::new();
    if !std::io::stdin().is_terminal()
        && let Err(e) = std::io::stdin().read_to_string(&mut input)
    {
        eprintln!("timon hook: cannot read the input: {e}");
    }
    timon::hook::run(layers, &builtins(), event, &input).await;
    ExitCode::SUCCESS
}

fn discover() -> Result<Layers, Error> {
    let cwd = std::env::current_dir()
        .map_err(|e| Error::new("io", format!("cannot read the working directory: {e}")))?;
    Ok(Layers::discover(Dirs::from_env()?, &cwd))
}

fn runtime(layers: &Layers) -> Result<Runtime, Failure> {
    let runtime = Runtime::new()?;
    runtime.configure(layers)?;
    Ok(runtime)
}

async fn eval(
    runtime: &Runtime,
    code: Option<String>,
    file: Option<PathBuf>,
    fnl: bool,
) -> Result<MultiValue, Failure> {
    if let Some(path) = file {
        return runtime.run_script(&path, &[]).await;
    }
    let (source, name) = match code.as_deref() {
        Some("-") => {
            let mut source = String::new();
            std::io::stdin().read_to_string(&mut source).map_err(|e| {
                Failure::from(Error::new("io", format!("cannot read standard input: {e}")))
            })?;
            (source, "=stdin")
        }
        Some(code) => (code.to_owned(), "=eval"),
        None => return Ok(MultiValue::new()),
    };
    if fnl {
        runtime.eval_fennel(&source, name).await
    } else {
        runtime.eval(&source, name).await
    }
}

/// `timon mcp`: the same calls a script makes, with the arguments bound as a global.
async fn mcp(rt: &Runtime, command: McpCommand) -> Result<MultiValue, Failure> {
    let (cli, code) = match command {
        McpCommand::Servers { json } => (
            serde_json::json!({ "json": json }),
            r#"local list = mcp.servers()
               if __mcp_cli.json then return list end
               local out = {}
               for _, s in ipairs(list) do
                 out[#out + 1] = s.name .. "  " .. s.layer .. "  " .. (s.description or s.error or "")
               end
               return table.concat(out, "\n")"#,
        ),
        McpCommand::Tools { server, json } => (
            serde_json::json!({ "server": server, "json": json }),
            r#"local tools = mcp.connect(__mcp_cli.server):tools()
               if __mcp_cli.json then return tools end
               local out = {}
               for _, t in ipairs(tools) do out[#out + 1] = t.name .. "  " .. (t.description or "") end
               return table.concat(out, "\n")"#,
        ),
        McpCommand::Call { server, tool, args } => {
            let mut map = serde_json::Map::new();
            for arg in args {
                let Some((k, v)) = arg.split_once('=') else {
                    return Err(Error::new("usage", format!("\"{arg}\" is not key=value"))
                        .with_hint("timon mcp call SERVER TOOL name=mlua limit=5")
                        .into());
                };
                let value = serde_json::from_str(v)
                    .unwrap_or_else(|_| serde_json::Value::String(v.to_owned()));
                map.insert(k.to_owned(), value);
            }
            (
                serde_json::json!({ "server": server, "tool": tool, "args": map }),
                "return mcp.connect(__mcp_cli.server):call(__mcp_cli.tool, __mcp_cli.args)",
            )
        }
    };
    rt.set_global("__mcp_cli", &cli)?;
    rt.eval(code, "=mcp").await
}

/// `timon eval`'s input and output flags, after bb's `-i`, `-I`, `-o`, `-O` and `--stream`.
#[derive(Clone, Copy)]
struct EvalIo {
    in_lines: bool,
    in_json: bool,
    out_lines: bool,
    out_json: bool,
    stream: bool,
}

/// `timon eval` with input bound from stdin or output printed as lines.
async fn eval_io(rt: &Runtime, code: &str, name: &str, fnl: bool, io: EvalIo) -> ExitCode {
    match eval_io_inner(rt, code, name, fnl, io).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => fail(&failure),
    }
}

async fn eval_io_inner(
    rt: &Runtime,
    code: &str,
    name: &str,
    fnl: bool,
    io: EvalIo,
) -> Result<(), Failure> {
    use std::io::BufRead;
    if code == "-" && (io.in_lines || io.in_json) {
        return Err(Error::new(
            "usage",
            "stdin is the input, so the code cannot come from it",
        )
        .with_hint("give the code as an argument")
        .into());
    }
    if io.stream && !(io.in_lines || io.in_json) {
        return Err(Error::new("usage", "--stream needs -i or -I").into());
    }
    let f = rt.compile_eval(code, name, fnl)?;
    let io_err =
        |e: std::io::Error| Failure::from(Error::new("io", format!("cannot read stdin: {e}")));
    let json_err =
        |e: serde_json::Error| Failure::from(Error::new("bad_json", format!("stdin: {e}")));
    let stdin = std::io::stdin();
    if io.stream {
        if io.in_lines {
            for line in stdin.lock().lines() {
                rt.set_global("input", &serde_json::Value::String(line.map_err(io_err)?))?;
                print_out(&rt.call(f.clone(), MultiValue::new()).await?, io, true)?;
            }
        } else {
            for value in serde_json::Deserializer::from_reader(stdin.lock()).into_iter() {
                rt.set_global("input", &value.map_err(json_err)?)?;
                print_out(&rt.call(f.clone(), MultiValue::new()).await?, io, true)?;
            }
        }
        return Ok(());
    }
    if io.in_lines {
        let lines: Vec<serde_json::Value> = stdin
            .lock()
            .lines()
            .map(|l| l.map(serde_json::Value::String))
            .collect::<Result<_, _>>()
            .map_err(io_err)?;
        rt.set_global("input", &serde_json::Value::Array(lines))?;
    } else if io.in_json {
        let values: Vec<serde_json::Value> = serde_json::Deserializer::from_reader(stdin.lock())
            .into_iter()
            .collect::<Result<_, _>>()
            .map_err(json_err)?;
        rt.set_global("input", &serde_json::Value::Array(values))?;
    }
    print_out(&rt.call(f, MultiValue::new()).await?, io, false)
}

/// Prints eval's values: a list one element per line with -o or -O, else as `print` does. In a
/// stream, nil prints nothing and a value that is not a list prints as one line.
fn print_out(values: &MultiValue, io: EvalIo, stream: bool) -> Result<(), Failure> {
    use mlua::Value;
    let line = |v: &Value| -> Result<String, Failure> {
        Ok(match v {
            Value::String(s) if !io.out_json => s.to_string_lossy(),
            other => {
                serde_json::to_string(&timon::value::to_json(other, timon::value::Mode::Strict)?)
                    .unwrap_or_default()
            }
        })
    };
    if !(io.out_lines || io.out_json) {
        if stream {
            for v in values.iter().filter(|v| !v.is_nil()) {
                println!("{}", timon::value::render(v, false)?);
            }
            return Ok(());
        }
        match print(values) {
            ExitCode::SUCCESS => return Ok(()),
            _ => return Err(Error::new("io", "cannot print the result").into()),
        }
    }
    for v in values.iter() {
        match v {
            Value::Nil => {}
            Value::Table(t) if t.raw_len() > 0 || t.pairs::<Value, Value>().next().is_none() => {
                for item in t.sequence_values::<Value>() {
                    println!("{}", line(&item.map_err(|e| Error::from_lua(&e))?)?);
                }
            }
            other if stream => println!("{}", line(other)?),
            other => {
                return Err(Error::new(
                    "usage",
                    format!(
                        "-o and -O print a list; the code returned a {}",
                        other.type_name()
                    ),
                )
                .into());
            }
        }
    }
    Ok(())
}

/// timon's own command names and aliases, which take precedence over a task's name.
fn builtins() -> Vec<String> {
    let mut names = vec!["help".to_owned()];
    for c in Cli::command().get_subcommands() {
        names.push(c.get_name().to_owned());
        names.extend(c.get_all_aliases().map(str::to_owned));
    }
    names
}

fn catalog(rt: &Runtime, layers: &Layers) -> Result<Rc<timon::tasks::Catalog>, Failure> {
    timon::tasks::Catalog::load(rt, layers, &builtins()).map(Rc::new)
}

/// `timon NAME [WORDS]` or `timon FILE [ARGS]`: a path runs as a script, a name as a task.
async fn external(
    rt: &Runtime,
    layers: &Layers,
    argv: Vec<OsString>,
    how: timon::tasks::How,
) -> Result<MultiValue, Failure> {
    let mut argv = argv.into_iter();
    let first = argv.next().unwrap_or_default();
    let words: Vec<String> = argv.map(|a| a.to_string_lossy().into_owned()).collect();
    let path = PathBuf::from(&first);
    if is_path(&path) {
        return rt.run_script(&path, &words).await;
    }
    let name = first.to_string_lossy().into_owned();
    let catalog = catalog(rt, layers)?;
    if catalog.resolve(&name).is_some() {
        return timon::tasks::run(rt, catalog, &name, &words, how).await;
    }
    if path.is_file() {
        return rt.run_script(&path, &words).await;
    }
    match catalog.find(&name) {
        Err(err) if err.kind == "untrusted" => Err(err.into()),
        _ => {
            let builtins = builtins();
            let names = builtins
                .iter()
                .map(String::as_str)
                .chain(catalog.tasks.iter().map(|t| t.name.as_str()));
            let err = Error::new("usage", format!("unknown command or task \"{name}\""));
            Err(match timon::suggest::closest(&name, names) {
                Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
                None => err.with_hint("timon --help lists the commands and timon tasks the tasks"),
            }
            .into())
        }
    }
}

/// Completion candidates, one per line. Nothing is printed for a tasks file that fails to load.
fn complete(rt: &Runtime, layers: &Layers, words: Vec<String>) -> ExitCode {
    let words: Vec<String> = match words.first().map(String::as_str) {
        Some("--") => words[1..].to_vec(),
        _ => words,
    };
    let catalog = catalog(rt, layers).ok();
    for candidate in timon::complete::complete(&Cli::command(), catalog.as_deref(), &words) {
        println!("{candidate}");
    }
    ExitCode::SUCCESS
}

/// A script path rather than a task name: it has a .lua or .fnl extension or a directory.
fn is_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext == "lua" || ext == "fnl")
        || path.components().count() > 1
}

fn tasks(rt: &Runtime, layers: &Layers, all: bool, json: bool) -> ExitCode {
    let catalog = match catalog(rt, layers) {
        Ok(catalog) => catalog,
        Err(failure) => return fail(&failure),
    };
    let root = Cli::command();
    if json {
        let mut list = catalog.to_json(all);
        if all && let serde_json::Value::Array(items) = &mut list {
            items.extend(timon::complete::builtins(&root));
        }
        println!("{list}");
    } else {
        print!(
            "{}",
            catalog.listing(all, &timon::complete::top_level(&root))
        );
    }
    if let Some(note) = catalog.untrusted_note() {
        eprintln!("{note}");
    }
    ExitCode::SUCCESS
}

fn scripts(layers: &Layers, json: bool) -> ExitCode {
    let scripts = layers.scripts();
    if json {
        let list: Vec<serde_json::Value> = scripts
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "layer": s.layer.name(),
                    "path": s.path,
                    "description": s.description,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(list));
        return ExitCode::SUCCESS;
    }
    let width = scripts.iter().map(|s| s.name.len()).max().unwrap_or(0);
    for s in &scripts {
        println!("{:width$}  {:7}  {}", s.name, s.layer.name(), s.description);
    }
    if let Some(project) = layers.project.as_ref().filter(|p| !p.trusted) {
        eprintln!(
            "timon: {} is not trusted, so its scripts are not listed",
            project.root.display()
        );
    }
    ExitCode::SUCCESS
}

fn skill(layers: &Layers, command: SkillCommand) -> ExitCode {
    match command {
        SkillCommand::List { json } => {
            let skills = timon::skills::list(layers);
            if json {
                let list: Vec<serde_json::Value> = skills
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "name": s.name,
                            "layer": s.layer.name(),
                            "description": s.description,
                            "references": s.references(),
                        })
                    })
                    .collect();
                println!("{}", serde_json::Value::Array(list));
            } else {
                let width = skills.iter().map(|s| s.name.len()).max().unwrap_or(0);
                for s in &skills {
                    println!("{:width$}  {:7}  {}", s.name, s.layer.name(), s.description);
                }
            }
            ExitCode::SUCCESS
        }
        SkillCommand::Show { name, reference } => {
            let text = timon::skills::find(layers, &name).and_then(|skill| match reference {
                Some(r) => skill.reference(&r),
                None => skill.text().map(|text| {
                    let refs = skill.references();
                    if refs.is_empty() {
                        text
                    } else {
                        format!(
                            "{}\n\nReferences: {} (timon skill show {} --ref NAME)\n",
                            text.trim_end(),
                            refs.join(", "),
                            skill.name
                        )
                    }
                }),
            });
            match text {
                Ok(text) => {
                    print!("{text}");
                    if !text.ends_with('\n') {
                        println!();
                    }
                    ExitCode::SUCCESS
                }
                Err(err) => fail(&err.into()),
            }
        }
    }
}

/// `timon agent run`: the flags become a spec table for the same agent.run that Lua calls, so
/// merging, checks, budgets and records cannot differ between the two.
async fn agent_run(rt: &Runtime, command: Command) -> Result<MultiValue, Failure> {
    let Command::Agent(AgentCommand::Run {
        prompt,
        agent,
        provider,
        model,
        effort,
        access,
        tools,
        cwd,
        system,
        output,
        timeout,
        budget_usd,
        budget_turns,
        resume,
        fork,
        worktree,
        events,
        native,
    }) = command
    else {
        return Ok(MultiValue::new());
    };
    let prompt = if prompt == "-" {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text).map_err(|e| {
            Failure::from(Error::new("io", format!("cannot read standard input: {e}")))
        })?;
        text
    } else {
        prompt
    };
    let build = || -> mlua::Result<mlua::Table> {
        let lua = rt.lua();
        let spec = lua.create_table()?;
        spec.raw_set("prompt", prompt)?;
        for (key, value) in [
            ("agent", agent),
            ("provider", provider),
            ("model", model),
            ("effort", effort),
            ("access", access),
            ("cwd", cwd),
            ("system", system),
            ("output", output),
            ("timeout", timeout),
            ("resume", resume),
            ("fork", fork),
        ] {
            if let Some(value) = value {
                spec.raw_set(key, value)?;
            }
        }
        if let Some(tools) = tools {
            spec.raw_set("tools", lua.create_sequence_from(tools)?)?;
        }
        if !native.is_empty() {
            spec.raw_set("native", lua.create_sequence_from(native)?)?;
        }
        if budget_usd.is_some() || budget_turns.is_some() {
            let budget = lua.create_table()?;
            budget.raw_set("usd", budget_usd)?;
            budget.raw_set("turns", budget_turns)?;
            spec.raw_set("budget", budget)?;
        }
        if events {
            spec.raw_set("events", true)?;
        }
        if worktree {
            spec.raw_set("worktree", true)?;
        }
        Ok(spec)
    };
    let spec = build().map_err(|e| Failure::from(Error::from_lua(&e)))?;
    let run: mlua::Function = rt
        .lua()
        .globals()
        .get::<mlua::Table>("agent")
        .and_then(|agent| agent.get("run"))
        .map_err(|e| Failure::from(Error::from_lua(&e)))?;
    rt.call(run, MultiValue::from_iter([mlua::Value::Table(spec)]))
        .await
}

/// `timon sessions`: the same functions as Lua's `sessions`, called through the runtime so
/// options are checked the same way and summaries spend through `agent.run`.
async fn sessions(rt: &Runtime, command: SessionsCommand) -> ExitCode {
    use serde_json::json;
    use timon::sessions::text;
    let flag = |on: bool| on.then_some(true);
    let opts = |flags: &SessionFlags, mut more: serde_json::Value| {
        more["backend"] = json!(flags.backend);
        more
    };
    let (name, args, json, format): (&str, Vec<serde_json::Value>, bool, Format) = match command {
        SessionsCommand::List {
            flags,
            project,
            dir,
            since,
            limit,
            subagents,
            automated,
        } => (
            "list",
            vec![opts(
                &flags,
                json!({ "project": project, "dir": dir, "since": since, "limit": limit,
                    "subagents": flag(subagents), "automated": flag(automated) }),
            )],
            flags.json,
            text::list,
        ),
        SessionsCommand::Show { id, flags } => (
            "show",
            vec![json!(id), opts(&flags, json!({}))],
            flags.json,
            |r| (text::show(r), None),
        ),
        SessionsCommand::Current { json } => ("current", vec![], json, |r| (text::show(r), None)),
        SessionsCommand::Transcript {
            id,
            flags,
            offset,
            limit,
            kinds,
        } => (
            "transcript",
            vec![
                json!(id),
                opts(
                    &flags,
                    json!({ "offset": offset, "limit": limit,
                        "kinds": (!kinds.is_empty()).then_some(kinds) }),
                ),
            ],
            flags.json,
            text::transcript,
        ),
        SessionsCommand::Search {
            query,
            flags,
            mode,
            project,
            since,
            until,
            limit,
            per_session,
            order,
            subagents,
            active,
            automated,
            exclude,
            include_current,
        } => (
            "search",
            vec![opts(
                &flags,
                json!({ "query": query, "mode": mode, "project": project, "since": since, "until": until,
                        "limit": limit, "per_session": per_session, "order": order,
                        "subagents": flag(subagents), "active": flag(active),
                        "automated": flag(automated),
                    "exclude": if include_current { json!(false) } else { json!(exclude) } }),
            )],
            flags.json,
            text::search,
        ),
        SessionsCommand::Summarize {
            id,
            flags,
            model,
            force,
        } => (
            "summarize",
            vec![
                json!(id),
                opts(&flags, json!({ "model": model, "force": flag(force) })),
            ],
            flags.json,
            text::summary,
        ),
        SessionsCommand::Index { .. } => return ExitCode::SUCCESS,
    };
    let call = async {
        let lua = rt.lua();
        let lua_err = |e: mlua::Error| Failure::from(Error::from_lua(&e));
        let f: mlua::Function = lua
            .globals()
            .get::<mlua::Table>("sessions")
            .and_then(|sessions| sessions.get(name))
            .map_err(lua_err)?;
        let args = args
            .iter()
            .map(|a| timon::value::from_json(lua, &without_nulls(a)))
            .collect::<mlua::Result<Vec<_>>>()
            .map_err(lua_err)?;
        let values = rt.call(f, MultiValue::from_iter(args)).await?;
        match values.into_iter().next() {
            None | Some(mlua::Value::Nil) => Err(Failure::from(
                Error::new("not_found", "timon is not running inside a session")
                    .with_hint("Claude Code sets CLAUDE_CODE_SESSION_ID and Codex CODEX_THREAD_ID"),
            )),
            Some(value) => {
                timon::value::to_json(&value, timon::value::Mode::Display).map_err(Failure::from)
            }
        }
    };
    let result = match call.await {
        Ok(result) => result,
        Err(failure) => return fail(&failure),
    };
    if json {
        let pretty = std::io::stdout().is_terminal();
        let text = if pretty {
            serde_json::to_string_pretty(&result)
        } else {
            serde_json::to_string(&result)
        };
        println!("{}", text.unwrap_or_default());
        return ExitCode::SUCCESS;
    }
    let (out, note) = format(&result);
    if !out.is_empty() {
        println!("{out}");
    }
    if let Some(note) = note {
        eprintln!("{note}");
    }
    ExitCode::SUCCESS
}

type Format = fn(&serde_json::Value) -> (String, Option<String>);

/// Drops null fields, which stand for flags not given.
fn without_nulls(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn sessions_index(layers: &Layers, rebuild: bool) -> ExitCode {
    let result = timon::sessions::Roots::from_env(layers.dirs.home.as_deref()).and_then(|roots| {
        let paths = timon::sessions::Paths::new(&layers.dirs.cache, &roots);
        timon::sessions::search::reindex(&paths, &timon::sessions::Limits::default(), rebuild)
    });
    match result {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(err) => fail(&err.into()),
    }
}

fn agents(layers: &Layers, json: bool) -> ExitCode {
    let rt = match runtime(layers) {
        Ok(rt) => rt,
        Err(failure) => return fail(&failure),
    };
    let dirs = timon::agents::dirs(layers);
    let list: Vec<serde_json::Value> = timon::agents::list(&dirs)
        .iter()
        .map(|a| {
            let description = timon::agents::describe(rt.lua(), &dirs, &a.name)
                .unwrap_or_else(|err| format!("(cannot load: {err})"));
            serde_json::json!({
                "name": a.name,
                "layer": a.layer.name(),
                "path": a.path,
                "description": description,
            })
        })
        .collect();
    if json {
        println!("{}", serde_json::Value::Array(list));
        return ExitCode::SUCCESS;
    }
    let width = list
        .iter()
        .map(|a| a["name"].as_str().unwrap_or("").len())
        .max()
        .unwrap_or(0);
    for a in &list {
        println!(
            "{:width$}  {:7}  {}",
            a["name"].as_str().unwrap_or(""),
            a["layer"].as_str().unwrap_or(""),
            a["description"].as_str().unwrap_or("")
        );
    }
    ExitCode::SUCCESS
}

fn runs(
    layers: &Layers,
    command: Option<RunsCommand>,
    since: Option<String>,
    limit: usize,
    json: bool,
) -> ExitCode {
    let store = timon::agent::records::Store::new(&layers.dirs.state);
    if let Some(RunsCommand::Show { id }) = command {
        let shown = if id.starts_with("s_") {
            timon::scripts::show(&layers.dirs.state, &id)
        } else {
            store.show(&id)
        };
        return match shown {
            Ok(run) => {
                println!("{}", serde_json::to_string_pretty(&run).unwrap_or_default());
                ExitCode::SUCCESS
            }
            Err(err) => fail(&err.into()),
        };
    }
    let since = match since.map(|s| timon::modules::concurrency::parse_duration(&s)) {
        None => None,
        Some(Ok(back)) => match chrono::Duration::from_std(back) {
            Ok(back) => Some(chrono::Utc::now() - back),
            Err(e) => return fail(&Error::new("bad_option", e.to_string()).into()),
        },
        Some(Err(err)) => return fail(&err.into()),
    };
    let entries = match store.list(since, limit) {
        Ok(entries) => entries,
        Err(err) => return fail(&err.into()),
    };
    if json {
        println!("{}", serde_json::Value::Array(entries));
        return ExitCode::SUCCESS;
    }
    for e in &entries {
        let usd = e["usd"]
            .as_f64()
            .map(|u| format!("${u:.4}"))
            .unwrap_or_else(|| "-".to_owned());
        println!(
            "{}  {}  {:6}  {:9}  {:>7}ms  {:>8}  {}",
            e["id"].as_str().unwrap_or("?"),
            e["started_at"].as_str().unwrap_or("?"),
            e["provider"].as_str().unwrap_or("?"),
            e["status"].as_str().unwrap_or("?"),
            e["ms"].as_u64().unwrap_or(0),
            usd,
            e["prompt"].as_str().unwrap_or(""),
        );
    }
    ExitCode::SUCCESS
}

fn script_runs(layers: &Layers, limit: usize, json: bool) -> ExitCode {
    let entries = timon::scripts::list(&layers.dirs.state, limit);
    if json {
        println!("{}", serde_json::Value::Array(entries));
        return ExitCode::SUCCESS;
    }
    for e in &entries {
        let resumed = e["resumed_from"]
            .as_str()
            .map(|r| format!("  (resumed {r})"))
            .unwrap_or_default();
        println!(
            "{}  {}  {:11}  {:>3} runs, {} cached  ${:.4}  {}{resumed}",
            e["id"].as_str().unwrap_or("?"),
            e["started_at"].as_str().unwrap_or("?"),
            e["status"].as_str().unwrap_or("?"),
            e["runs"].as_u64().unwrap_or(0),
            e["cached"].as_u64().unwrap_or(0),
            e["usd"].as_f64().unwrap_or(0.0),
            e["name"].as_str().unwrap_or(""),
        );
    }
    ExitCode::SUCCESS
}

fn plugin_build(dir: &Path, check: bool) -> ExitCode {
    if check {
        return match timon::plugin::check(dir) {
            Ok(drift) if drift.is_empty() => ExitCode::SUCCESS,
            Ok(drift) => {
                for (what, paths) in [
                    ("missing", &drift.missing),
                    ("changed", &drift.changed),
                    ("extra", &drift.extra),
                ] {
                    for p in paths {
                        eprintln!("{what}: {}", dir.join(p).display());
                    }
                }
                fail(
                    &Error::new(
                        "plugin_drift",
                        "the plugin's skills differ from this binary's",
                    )
                    .with_hint(format!("run: timon plugin build {}", dir.display()))
                    .into(),
                )
            }
            Err(err) => fail(&err.into()),
        };
    }
    match timon::plugin::build(dir) {
        Ok(n) => {
            println!("wrote {n} files under {}", dir.join("skills").display());
            ExitCode::SUCCESS
        }
        Err(err) => fail(&err.into()),
    }
}

fn trust(layers: &Layers, dir: Option<PathBuf>, revoke: bool) -> ExitCode {
    let root = match dir.or_else(|| layers.project_root.clone()) {
        Some(root) => root,
        None => {
            return fail(
                &Error::new("not_found", "no .timon directory here or above")
                    .with_hint("give the project root: timon trust DIR")
                    .into(),
            );
        }
    };
    match layers::set_trust(&layers.dirs, &root, revoke) {
        Ok(root) => {
            let verb = if revoke {
                "no longer trusted"
            } else {
                "trusted"
            };
            println!("{verb}: {}", root.display());
            ExitCode::SUCCESS
        }
        Err(err) => fail(&err.into()),
    }
}

fn defs(layers: &Layers, json: bool) -> ExitCode {
    let settings = match runtime(layers).and_then(|rt| rt.settings().map_err(Failure::from)) {
        Ok(settings) => settings,
        Err(f) => return fail(&f),
    };
    let layer_json = |l: &layers::Layer| {
        serde_json::json!({
            "layer": l.kind.name(),
            "root": l.root,
            "exists": l.root.is_dir(),
            "trusted": l.trusted,
        })
    };
    let report = serde_json::json!({
        "layers": layers.project.iter().chain(std::iter::once(&layers.user)).map(layer_json).collect::<Vec<_>>(),
        "settings": settings,
        "scripts": layers.scripts().iter().map(|s| serde_json::json!({
            "name": s.name, "layer": s.layer.name(), "path": s.path,
        })).collect::<Vec<_>>(),
        "agents": timon::agents::list(&timon::agents::dirs(layers)).iter().map(|a| serde_json::json!({
            "name": a.name, "layer": a.layer.name(), "path": a.path,
        })).collect::<Vec<_>>(),
        "lib": layers.lib_dirs(),
        "state": layers.dirs.state,
        "cache": layers.dirs.cache,
    });
    if json || !std::io::stdout().is_terminal() {
        println!("{report}");
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    }
    ExitCode::SUCCESS
}

fn print(values: &MultiValue) -> ExitCode {
    let pretty = std::io::stdout().is_terminal();
    for value in values {
        match timon::value::render(value, pretty) {
            Ok(text) => println!("{text}"),
            Err(err) => return fail(&err.into()),
        }
    }
    ExitCode::SUCCESS
}

fn fail(failure: &Failure) -> ExitCode {
    eprintln!("{}", failure.report());
    if let Some(exit) = failure.error.exit {
        ExitCode::from(exit)
    } else if failure.error.kind == "usage" {
        ExitCode::from(2)
    } else {
        ExitCode::FAILURE
    }
}
