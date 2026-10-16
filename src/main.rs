use std::ffi::OsString;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mlua::MultiValue;
use timon::{Error, Failure, Runtime};

#[derive(Parser)]
#[command(
    name = "timon",
    version,
    about = "A self-contained agent harness: Lua over agent, session and tool APIs",
    after_help = "Run a script with `timon FILE.lua [ARGS]`.",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Evaluate Lua and print each value it returns
    Eval {
        /// Lua source; `-` reads it from standard input
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        code: Option<String>,
        /// Evaluate a file instead
        #[arg(short, long, value_name = "FILE")]
        file: Option<PathBuf>,
    },
    #[command(external_subcommand)]
    Script(Vec<OsString>),
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
    tokio::task::LocalSet::new().block_on(&rt, run(cli))
}

async fn run(cli: Cli) -> ExitCode {
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => return fail(&err.into()),
    };
    let result = match cli.command {
        Command::Eval { code, file } => eval(&runtime, code, file).await,
        Command::Script(argv) => script(&runtime, argv).await,
    };
    match result {
        Ok(values) => print(&values),
        Err(failure) => fail(&failure),
    }
}

async fn eval(
    runtime: &Runtime,
    code: Option<String>,
    file: Option<PathBuf>,
) -> Result<MultiValue, Failure> {
    if let Some(path) = file {
        return runtime.run_script(&path, &[]).await;
    }
    match code.as_deref() {
        Some("-") => {
            let mut source = String::new();
            std::io::stdin().read_to_string(&mut source).map_err(|e| {
                Failure::from(Error::new("io", format!("cannot read standard input: {e}")))
            })?;
            runtime.eval(&source, "=stdin").await
        }
        Some(code) => runtime.eval(code, "=eval").await,
        None => Ok(MultiValue::new()),
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
    path.extension().is_some_and(|ext| ext == "lua")
        || path.components().count() > 1
        || path.is_file()
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
