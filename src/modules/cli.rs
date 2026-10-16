//! `cli`: command-line options for scripts, declared like task options (see [`crate::flags`]).

use mlua::{Lua, MultiValue, Table, Value};

use crate::docs::{Func, Module};
use crate::error::Error;
use crate::flags::Spec;
use crate::modules::Installer;

pub const CLI_DOC: Module = Module {
    name: "cli",
    summary: "Command-line options for scripts, declared the way task options are: a type string such as \"integer?\", or { type, default, short, doc }. Flags follow babashka.cli: --key value, --key=value, --flag and --no-flag, a repeated flag for a list, -k for a short name, -- to stop.",
    functions: &[
        Func::new(
            "parse",
            "cli.parse(words, options) -> opts, rest",
            "Parses words (arg, in a script) against the declared options. opts has every option: given, defaulted, false or an empty list; rest has the other words in order, and every word after --. An unknown flag, a bad value or a missing required option raises usage, with did-you-mean; usage ends a run with exit code 2.",
            "local opts, rest = cli.parse({ \"--port\", \"9000\", \"-v\", \"notes.txt\" }, {\n  port = \"integer?\",\n  verbose = { type = \"boolean\", short = \"v\" },\n  mode = { type = '\"fast\" | \"full\"', default = \"fast\" },\n})\nreturn opts.port, opts.verbose, opts.mode, rest[1]  -- 9000, true, fast, notes.txt",
        ),
        Func::new(
            "help",
            "cli.help(options) -> string",
            "The Options section of a help text for declared options: flags, types, docs and defaults.",
            "print(cli.help({ base = { type = \"string\", default = \"main\", doc = \"the branch\" } }))",
        ),
    ],
};

fn spec(options: &Table) -> mlua::Result<Spec> {
    Spec::from_lua(options).map_err(|m| Error::new("bad_argument", m).into())
}

/// The words of a list, from 1: a script's `arg` keeps its path at 0, which is left out.
fn words(list: &Table) -> mlua::Result<Vec<String>> {
    let mut out = Vec::new();
    for i in 1..=list.raw_len() {
        match list.raw_get::<Value>(i)? {
            Value::String(s) => out.push(s.to_string_lossy()),
            Value::Integer(n) => out.push(n.to_string()),
            Value::Number(n) => out.push(n.to_string()),
            other => {
                return Err(Error::new(
                    "bad_argument",
                    format!("cli.parse: word {i} is a {}", other.type_name()),
                )
                .into());
            }
        }
    }
    Ok(out)
}

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let cli = installer.module(&CLI_DOC)?;
    installer.function(
        &CLI_DOC,
        Some(&cli),
        "parse",
        lua.create_function(|lua: &Lua, (list, options): (Table, Table)| {
            let parsed = spec(&options)?.parse(&words(&list)?)?;
            if parsed.help {
                return Err(Error::new("usage", "--help was given")
                    .with_hint("declare help handling with cli.help(options)")
                    .into());
            }
            let opts =
                crate::modules::json::decode(lua, &serde_json::Value::Object(parsed.values))?;
            let rest = crate::modules::json::array_from(lua, parsed.rest)?;
            Ok(MultiValue::from_iter([opts, Value::Table(rest)]))
        })?,
    )?;
    installer.function(
        &CLI_DOC,
        Some(&cli),
        "help",
        lua.create_function(|_, options: Table| Ok(spec(&options)?.help()))?,
    )?;
    Ok(())
}
