//! Strict globals for user code.
//!
//! User chunks run with an environment table that reads and writes through to `_G`, so globals
//! are shared, but refuses two things: reading a global that does not exist, and creating a new
//! global from inside a function. Library code runs against `_G` directly and is unaffected.

use mlua::{Lua, Table, Value};

use crate::error::Error;
use crate::runtime::INTERNAL_CHUNK;
use crate::suggest;

/// Creates the strict environment.
pub(crate) fn environment(lua: &Lua) -> mlua::Result<Table> {
    let env = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.set(
        "__index",
        lua.create_function(|lua, (_env, key): (Table, Value)| {
            let globals = lua.globals();
            let value: Value = globals.raw_get(key.clone())?;
            if !value.is_nil() {
                return Ok(value);
            }
            match key {
                Value::String(name) => Err(undefined(&globals, &name.to_string_lossy()).into()),
                _ => Ok(Value::Nil),
            }
        })?,
    )?;
    mt.set(
        "__newindex",
        lua.create_function(|lua, (_env, key, value): (Table, Value, Value)| {
            let globals = lua.globals();
            let exists = !globals.raw_get::<Value>(key.clone())?.is_nil();
            if exists || caller_is_main_chunk(lua) {
                return globals.raw_set(key, value);
            }
            let name = key.to_string().unwrap_or_else(|_| "?".to_owned());
            Err(Error::new(
                "undeclared_global",
                format!("assignment to undeclared global \"{name}\" inside a function"),
            )
            .with_hint("declare it with local, or assign it at the top level of the script first")
            .into())
        })?,
    )?;
    mt.set("__name", "timon.environment")?;
    env.set_metatable(Some(mt))?;
    Ok(env)
}

fn undefined(globals: &Table, name: &str) -> Error {
    let names: Vec<String> = globals
        .pairs::<Value, Value>()
        .filter_map(|pair| match pair {
            Ok((Value::String(k), _)) => Some(k.to_string_lossy()),
            _ => None,
        })
        .collect();
    let err = Error::new("undefined_global", format!("undefined global \"{name}\""));
    match suggest::closest(name, names.iter().map(String::as_str)) {
        Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
        None => err.with_hint("declare it with local, or check the spelling"),
    }
}

/// Whether the Lua code that triggered the metamethod is a chunk's top level, skipping C frames
/// and timon's own Lua glue.
fn caller_is_main_chunk(lua: &Lua) -> bool {
    for level in 1.. {
        let frame = lua.inspect_stack(level, |d| {
            let source = d.source();
            let internal = source.source.as_deref() == Some(INTERNAL_CHUNK);
            (source.what, internal)
        });
        match frame {
            None => return false,
            Some(("C", _)) | Some((_, true)) => continue,
            Some((what, false)) => return what == "main",
        }
    }
    false
}
