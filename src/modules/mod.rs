//! The modules installed into every runtime, and the installer that registers them for `help()`.

pub mod fs;
pub mod json;
pub mod log;
pub mod tbl;
pub mod text;

use mlua::{Function, Lua, Table, Value};

use crate::docs::{self, Func, Module};
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

    /// Creates the global table for a module.
    pub(crate) fn module(&self, doc: &Module) -> mlua::Result<Table> {
        let table = self.lua().create_table()?;
        self.lua().globals().raw_set(doc.name, &table)?;
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
    log::install(&installer)?;
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
    lua.create_function(|lua, (kind, message): (String, String)| {
        error::to_table(lua, &Error::new(kind, message))
    })
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
