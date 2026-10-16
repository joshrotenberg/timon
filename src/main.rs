use std::ffi::OsString;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mlua::MultiValue;
use timon::dirs::Dirs;
use timon::layers::{self, Layers};
use timon::{Error, Failure, Runtime};

#[derive(Parser)]
#[command(
    name = "timon",
    version,
    about = "A self-contained agent harness: Lua over agent, session and tool APIs",
    after_help = "Run a script with `timon FILE.lua [ARGS]` or `timon FILE.fnl [ARGS]`.",
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
    },
    /// Run a named script from a scripts/ directory
    Run {
        name: String,
        /// Arguments for the script, available as `arg` and `...`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
        #[arg(long)]
        json: bool,
    },
    /// List and read skills: built in, user and project
    #[command(subcommand)]
    Skill(SkillCommand),
    /// Build the plugin directory from the binary
    #[command(subcommand)]
    Plugin(PluginCommand),
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
            json,
        } => return runs(&layers, command, since, limit, json),
        Command::Plugin(PluginCommand::Build { dir, check }) => return plugin_build(&dir, check),
        Command::Eval { .. }
        | Command::Run { .. }
        | Command::Script(_)
        | Command::Agent(_)
        | Command::Sessions(_) => {}
    }
    // The runtime must outlive the values it returns, which refer into its Lua state.
    let rt = match runtime(&layers) {
        Ok(rt) => rt,
        Err(failure) => return fail(&failure),
    };
    rt.set_limits(cli.max_runs, cli.max_usd);
    let command = match cli.command {
        Command::Sessions(command) => return sessions(&rt, command).await,
        command => command,
    };
    let result = match command {
        Command::Eval { code, file, fnl } => eval(&rt, code, file, fnl).await,
        Command::Run { name, args } => match layers.script(&name) {
            Ok(script) => rt.run_script(&script.path, &args).await,
            Err(err) => Err(err.into()),
        },
        Command::Script(argv) => script(&rt, argv).await,
        Command::Agent(AgentCommand::Run { .. }) => agent_run(&rt, command).await,
        Command::Scripts { .. }
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

async fn script(runtime: &Runtime, argv: Vec<OsString>) -> Result<MultiValue, Failure> {
    let mut argv = argv.into_iter();
    let first = argv.next().unwrap_or_default();
    let path = PathBuf::from(&first);
    if !looks_like_script(&path) {
        return Err(Error::new(
            "usage",
            format!("unknown command \"{}\"", first.to_string_lossy()),
        )
        .with_hint("run timon --help, or give the path to a .lua file")
        .into());
    }
    let args: Vec<String> = argv.map(|a| a.to_string_lossy().into_owned()).collect();
    runtime.run_script(&path, &args).await
}

fn looks_like_script(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext == "lua" || ext == "fnl")
        || path.components().count() > 1
        || path.is_file()
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
            since,
            limit,
            subagents,
            automated,
        } => (
            "list",
            vec![opts(
                &flags,
                json!({ "project": project, "since": since, "limit": limit,
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
        return match store.show(&id) {
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
    if failure.error.kind == "usage" {
        ExitCode::from(2)
    } else {
        ExitCode::FAILURE
    }
}
