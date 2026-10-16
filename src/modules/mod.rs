//! The modules installed into every runtime, and the installer that registers them for `help()`.

pub mod agent;
pub mod cli;
pub mod concurrency;
pub mod formats;
pub mod fs;
pub mod gh;
pub mod git;
pub mod http;
pub mod json;
pub mod log;
pub mod proc;
pub mod schema;
pub mod sessions;
pub mod tbl;
pub mod text;

use mlua::{Function, Lua, Table, Value};

use crate::docs::{self, Func, Module, Opt};
use crate::error::{self, Error};
use crate::runtime::Runtime;
use crate::suggest;

/// Registry key of the table mapping installed functions and modules to their names.
const HELP_INDEX: &str = "timon.help_index";

/// Global functions.
pub const GLOBALS_DOC: Module = Module {
    name: docs::GLOBALS,
    summary: "Functions available everywhere without a module prefix.",
    functions: &[
        Func::new(
            "help",
            "help(topic) -> string",
            "Describes a module or function. Pass the value itself or its name; no argument lists the modules.",
            "help(json)\nhelp(json.encode)\nhelp(\"str.fmt\")",
        ),
        Func::new(
            "try",
            "try(f, ...) -> true, ... | false, err",
            "Calls f with the arguments. Returns true and its results, or false and the error as a table with kind, message and hint.",
            "local ok, err = try(fs.read, \"missing.txt\")\nif not ok then print(err.kind) end  --> not_found",
        ),
        Func::new(
            "pp",
            "pp(...) -> ...",
            "Pretty-prints each value to stderr and returns them unchanged, so it can wrap an expression.",
            "local prs = pp(json.decode(text))",
        ),
        Func::new(
            "scope",
            "scope(fn, { budget, reserve }) -> ...",
            "Runs fn(s) and returns its results once every task spawned on s has finished. If fn or any task raises, the others are cancelled and the first error is raised. s:spawn(f, ...) starts a task; s:cancel() cancels the running ones. scope() with no function returns a scope for local s <close> = scope(), which waits when the block ends.",
            "local review, history = scope(function(s)\n  local r = s:spawn(agent.run, { agent = \"reviewer\", prompt = \"Review PR 12\" })\n  local h = s:spawn(sessions.search, { query = \"PR 12\" })\n  return r:await(), h:await()\nend)",
        )
        .with_options(&[
            Opt::new("budget", "{ runs: integer?, usd: number? }", "limits for agent runs inside the scope, on top of every enclosing budget"),
            Opt::new("reserve", "boolean", "take budget.runs from the enclosing budget now, so the scope cannot run short; unused runs go back when it ends"),
        ]),
        Func::new(
            "spawn",
            "spawn(f, ...) -> task",
            "Starts f(...) as a task on the innermost scope; a script's main chunk is one, and the script waits for its tasks. task:await() returns the results or raises the error, task:cancel() stops it, task:done() tells whether it finished.",
            "local h = spawn(fs.read, \"big.json\")\n-- other work\nlocal text = h:await()",
        ),
        Func::new(
            "phase",
            "phase(name) -> previous | phase() -> current",
            "Names the phase of the current task: agent runs started in it record the phase and show it in progress lines. Tasks inherit the phase of the task that spawned them; s:spawn takes the phase of the scope's function. phase(nil) clears it.",
            "phase(\"review\")\nlocal reviews = par.map(prs, review_one)\nphase(\"verify\")",
        ),
        Func::new(
            "sleep",
            "sleep(duration)",
            "Waits without blocking other tasks. A number is seconds; a string takes ms, s, m or h.",
            "sleep(\"250ms\")\nsleep(2)",
        ),
    ],
};

/// Registers modules and functions, wrapping Rust functions so their errors reach Lua as tables.
pub(crate) struct Installer<'a> {
    runtime: &'a Runtime,
    index: Table,
}

impl<'a> Installer<'a> {
    fn new(runtime: &'a Runtime) -> mlua::Result<Self> {
        let index = runtime.lua().create_table()?;
        runtime.lua().set_named_registry_value(HELP_INDEX, &index)?;
        Ok(Self { runtime, index })
    }

    pub(crate) fn lua(&self) -> &Lua {
        self.runtime.lua()
    }

    /// Wraps a Rust function so its errors reach Lua as tables, without registering it.
    pub(crate) fn wrap(&self, f: Function) -> mlua::Result<Function> {
        self.runtime.host_function(f)
    }

    /// Creates the global table for a module. Reading a field it does not have raises
    /// `undefined_field` with the closest name; `rawget` still tests without raising.
    pub(crate) fn module(&self, doc: &Module) -> mlua::Result<Table> {
        let lua = self.lua();
        let table = lua.create_table()?;
        let name = doc.name;
        let missing =
            lua.create_function(move |_, (t, key): (Table, Value)| -> mlua::Result<Value> {
                let Value::String(key) = key else {
                    return Ok(Value::Nil);
                };
                let key = key.to_string_lossy();
                let fields: Vec<String> = t
                    .pairs::<String, Value>()
                    .filter_map(|pair| pair.ok().map(|(k, _)| k))
                    .collect();
                let err = Error::new("undefined_field", format!("{name} has no field \"{key}\""));
                Err(
                    match suggest::closest(&key, fields.iter().map(String::as_str)) {
                        Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
                        None => err.with_hint(format!("help({name}) lists what it has")),
                    }
                    .into(),
                )
            })?;
        let mt = lua.create_table()?;
        mt.raw_set("__index", self.wrap(missing)?)?;
        mt.raw_set("__name", format!("timon.module.{name}"))?;
        table.set_metatable(Some(mt))?;
        lua.globals().raw_set(doc.name, &table)?;
        self.index.raw_set(&table, doc.name)?;
        Ok(table)
    }

    /// Adds a Rust function to a module (or as a global, for the globals module).
    pub(crate) fn function(
        &self,
        doc: &Module,
        table: Option<&Table>,
        name: &str,
        f: Function,
    ) -> mlua::Result<Function> {
        let wrapped = self.runtime.host_function(f)?;
        self.add(doc, table, name, wrapped)
    }

    /// Adds a function written in Lua, which raises error tables itself.
    pub(crate) fn lua_function(
        &self,
        doc: &Module,
        table: Option<&Table>,
        name: &str,
        f: Function,
    ) -> mlua::Result<Function> {
        self.add(doc, table, name, f)
    }

    fn add(
        &self,
        doc: &Module,
        table: Option<&Table>,
        name: &str,
        f: Function,
    ) -> mlua::Result<Function> {
        let func = declared(doc, name)?;
        match table {
            Some(table) => table.raw_set(name, &f)?,
            None => self.lua().globals().raw_set(name, &f)?,
        }
        self.index.raw_set(&f, doc.path(func))?;
        Ok(f)
    }
}

fn declared<'m>(doc: &'m Module, name: &str) -> mlua::Result<&'m Func> {
    doc.functions
        .iter()
        .find(|f| f.name == name)
        .ok_or_else(|| {
            Error::new(
                "internal",
                format!("{}.{name} is installed but not declared", doc.name),
            )
            .into()
        })
}

/// Installs every module.
pub(crate) fn install(runtime: &Runtime) -> mlua::Result<()> {
    let installer = Installer::new(runtime)?;
    let lua = installer.lua();

    let try_fn: Function = lua.globals().raw_get("try")?;
    installer.lua_function(&GLOBALS_DOC, None, "try", try_fn)?;
    installer.function(&GLOBALS_DOC, None, "help", lua.create_function(help)?)?;

    json::install(&installer)?;
    text::install(&installer)?;
    tbl::install(&installer)?;
    fs::install(&installer)?;
    git::install(&installer)?;
    http::install(&installer)?;
    gh::install(&installer)?;
    log::install(&installer)?;
    concurrency::install(&installer)?;
    proc::install(&installer)?;
    cli::install(&installer)?;
    formats::install(&installer)?;
    crate::tasks::install(&installer)?;
    schema::install(&installer)?;
    agent::install(&installer)?;
    sessions::install(&installer)?;
    Ok(())
}

fn help(lua: &Lua, topic: Option<Value>) -> mlua::Result<String> {
    let index: Table = lua.named_registry_value(HELP_INDEX)?;
    let name = match &topic {
        None => return Ok(docs::overview()),
        Some(Value::String(s)) => Some(s.to_string_lossy()),
        Some(v @ (Value::Table(_) | Value::Function(_))) => {
            index.raw_get::<Option<String>>(v.clone())?
        }
        Some(_) => None,
    };
    let Some(name) = name else {
        return Err(Error::new("not_found", "no help for that value")
            .with_hint("pass a module, one of its functions, or a name such as \"json.encode\"")
            .into());
    };
    if let Some(module) = docs::module(&name) {
        return Ok(docs::describe_module(module));
    }
    if let Some((_, func)) = docs::function(&name) {
        return Ok(docs::describe_function(func));
    }
    let names: Vec<String> = docs::modules()
        .iter()
        .flat_map(|m| {
            std::iter::once(m.name.to_owned()).chain(m.functions.iter().map(|f| m.path(f)))
        })
        .collect();
    let err = Error::new("not_found", format!("no help for \"{name}\""));
    Err(
        match suggest::closest(&name, names.iter().map(String::as_str)) {
            Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
            None => err.with_hint("help() lists the modules"),
        }
        .into(),
    )
}

/// Builds an error table, for Lua-written modules.
pub(crate) fn fail_function(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(
        |lua, (kind, message, hint): (String, String, Option<String>)| {
            error::to_table(lua, &Error::new(kind, message).with_hint_opt(hint))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_function_is_installed() {
        let runtime = Runtime::new().unwrap();
        let index: Table = runtime.lua().named_registry_value(HELP_INDEX).unwrap();
        let installed: Vec<String> = index
            .pairs::<Value, String>()
            .map(|pair| pair.unwrap().1)
            .collect();
        for module in docs::modules() {
            for func in module.functions {
                let path = module.path(func);
                assert!(
                    installed.contains(&path),
                    "{path} is declared but not installed"
                );
            }
        }
    }
}
