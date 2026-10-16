use std::io::{IsTerminal, Read};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use mlua::Lua;

#[derive(Parser)]
#[command(
    name = "timon",
    version,
    about = "A self-contained agent harness: Lua over agent, session and tool APIs"
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
        code: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Eval { code } => eval_command(&code),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("timon: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn eval_command(code: &str) -> Result<()> {
    let source = if code == "-" {
        let mut source = String::new();
        std::io::stdin()
            .read_to_string(&mut source)
            .context("reading Lua source from standard input")?;
        source
    } else {
        code.to_owned()
    };
    let lua = Lua::new();
    let values = timon::eval(&lua, &source).map_err(|err| anyhow::anyhow!("{err}"))?;
    let pretty = std::io::stdout().is_terminal();
    for value in values {
        let text = timon::render(&lua, &value, pretty).map_err(|err| anyhow::anyhow!("{err}"))?;
        println!("{text}");
    }
    Ok(())
}
