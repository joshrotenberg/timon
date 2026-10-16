//! Fennel: the pinned compiler, loaded on first use.
//!
//! Fennel source compiles to Lua, which then loads like any user chunk: under strict globals,
//! with the same modules. The compiler itself runs against `_G`.

use std::path::Path;

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::error::Error;
use crate::runtime::ENV;

const SOURCE: &str = include_str!("../vendor/fennel/fennel-1.6.1.lua");
const COMPILER: &str = "timon.fennel";
const COMPILER_CHUNK: &str = "=[fennel]";

/// The Fennel module, loaded once per runtime.
pub(crate) fn compiler(lua: &Lua) -> mlua::Result<Table> {
    if let Some(fennel) = lua.named_registry_value::<Option<Table>>(COMPILER)? {
        return Ok(fennel);
    }
    let fennel: Table = lua.load(SOURCE).set_name(COMPILER_CHUNK).call(())?;
    lua.set_named_registry_value(COMPILER, &fennel)?;
    Ok(fennel)
}

/// Compiles Fennel source to Lua. `name` is the chunk name used for both, such as `=eval` or
/// `@review.fnl`; correlation keeps Lua line numbers close to the Fennel ones.
pub(crate) fn compile(lua: &Lua, source: &str, name: &str) -> Result<String, Error> {
    let fennel = compiler(lua)?;
    let compile: Function = fennel.get("compileString")?;
    let opts = lua.create_table()?;
    opts.set("filename", name.trim_start_matches(['=', '@']))?;
    opts.set("correlate", true)?;
    compile
        .call::<String>((source, opts))
        .map_err(|err| Error::new("syntax", message(&err)))
}

/// Fennel raises plain strings; drop the Lua traceback mlua appends.
fn message(err: &mlua::Error) -> String {
    let text = match err {
        mlua::Error::RuntimeError(text) => text.clone(),
        mlua::Error::CallbackError { cause, .. } => return message(cause),
        other => other.to_string(),
    };
    match text.find("\nstack traceback:") {
        Some(end) => text[..end].trim_end().to_owned(),
        None => text.trim_end().to_owned(),
    }
}

/// Comments out a leading `#!` line, keeping line numbers.
pub(crate) fn strip_shebang(source: &str) -> std::borrow::Cow<'_, str> {
    if source.starts_with("#!") {
        format!(";{source}").into()
    } else {
        source.into()
    }
}

/// Adds a `package.searchers` entry so `require "name"` finds `name.fnl` next to where it
/// would find `name.lua`.
pub(crate) fn install_searcher(lua: &Lua) -> mlua::Result<()> {
    let searcher = lua.create_function(|lua, name: String| -> mlua::Result<MultiValue> {
        let package: Table = lua.globals().get("package")?;
        let path: String = package.get("path")?;
        let relative = name.replace('.', "/");
        let mut tried = String::new();
        for template in path.split(';') {
            let Some(stem) = template.strip_suffix(".lua") else {
                continue;
            };
            let candidate = format!("{}.fnl", stem.replace('?', &relative));
            if !Path::new(&candidate).is_file() {
                tried.push_str(&format!("\n\tno file '{candidate}'"));
                continue;
            }
            let source = std::fs::read_to_string(&candidate)
                .map_err(|e| Error::new("io", format!("cannot read {candidate}: {e}")))?;
            let chunk_name = format!("@{candidate}");
            let lua_source = compile(lua, &strip_shebang(&source), &chunk_name)?;
            let env: Table = lua.named_registry_value(ENV)?;
            let loader = lua
                .load(lua_source)
                .set_name(chunk_name)
                .set_environment(env)
                .into_function()?;
            return Ok(MultiValue::from_iter([
                Value::Function(loader),
                Value::String(lua.create_string(&candidate)?),
            ]));
        }
        Ok(MultiValue::from_iter([Value::String(
            lua.create_string(&tried)?,
        )]))
    })?;
    let package: Table = lua.globals().get("package")?;
    let searchers: Table = package.get("searchers")?;
    searchers.raw_insert(2, searcher)?;
    Ok(())
}
