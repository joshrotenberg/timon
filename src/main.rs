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
        Command::Runs {
            command,
            since,
            limit,
            json,
        } => return runs(&layers, command, since, limit, json),
        Command::Plugin(PluginCommand::Build { dir, check }) => return plugin_build(&dir, check),
        Command::Eval { .. } | Command::Run { .. } | Command::Script(_) => {}
    }
    // The runtime must outlive the values it returns, which refer into its Lua state.
    let rt = match runtime(&layers) {
        Ok(rt) => rt,
        Err(failure) => return fail(&failure),
    };
    rt.set_limits(cli.max_runs, cli.max_usd);
    let result = match cli.command {
        Command::Eval { code, file, fnl } => eval(&rt, code, file, fnl).await,
        Command::Run { name, args } => match layers.script(&name) {
            Ok(script) => rt.run_script(&script.path, &args).await,
            Err(err) => Err(err.into()),
        },
        Command::Script(argv) => script(&rt, argv).await,
        Command::Scripts { .. }
        | Command::Trust { .. }
        | Command::Defs { .. }
        | Command::Skill(_)
        | Command::Runs { .. }
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
