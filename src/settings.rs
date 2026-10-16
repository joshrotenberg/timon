//! `init.lua`: per-layer settings, loaded without effects and merged key by key.

use std::path::PathBuf;

use mlua::{Lua, Table, Value};

use crate::error::Error;
use crate::layers::Kind;
use crate::suggest;

/// Top-level settings an `init.lua` may return. Each is a table merged key by key.
pub const KEYS: &[&str] = &["defaults", "limits", "serve"];

/// Globals a definition file can use: pure functions only, no I/O.
const SAFE_GLOBALS: &[&str] = &[
    "assert", "error", "ipairs", "next", "pairs", "select", "tonumber", "tostring", "type", "math",
    "string", "table", "utf8", "str", "tbl", "json", "schema",
];

/// An environment for definition files: the pure standard library and timon's pure modules.
pub(crate) fn definition_env(lua: &Lua) -> mlua::Result<Table> {
    let env = lua.create_table()?;
    let globals = lua.globals();
    for name in SAFE_GLOBALS {
        env.raw_set(*name, globals.raw_get::<Value>(*name)?)?;
    }
    Ok(env)
}

fn bad(path: &std::path::Path, message: impl std::fmt::Display) -> Error {
    Error::new("bad_config", format!("{}: {message}", path.display()))
}

/// Loads `files` lowest layer first and merges them: for each setting, a later file's keys
/// replace an earlier file's.
pub(crate) fn load(lua: &Lua, files: &[(Kind, PathBuf)]) -> Result<Table, Error> {
    let merged = lua.create_table()?;
    for (_, path) in files {
        let source = std::fs::read_to_string(path).map_err(|e| bad(path, e))?;
        let chunk = lua
            .load(source)
            .set_name(format!("@{}", path.display()))
            .set_environment(definition_env(lua)?)
            .into_function()
            .map_err(|e| bad(path, Error::from_lua(&e).message))?;
        let returned = chunk
            .call::<Value>(())
            .map_err(|e| bad(path, Error::from_lua(&e).message))?;
        let Value::Table(settings) = returned else {
            return Err(bad(
                path,
                format!("must return a table, not a {}", returned.type_name()),
            ));
        };
        for pair in settings.pairs::<Value, Value>() {
            let (key, value) = pair?;
            let key = match key {
                Value::String(s) => s.to_string_lossy(),
                other => {
                    return Err(bad(
                        path,
                        format!("a {} key is not a setting", other.type_name()),
                    ));
                }
            };
            if !KEYS.contains(&key.as_str()) {
                let err = bad(path, format!("unknown setting \"{key}\""));
                return Err(match suggest::closest(&key, KEYS.iter().copied()) {
                    Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
                    None => err.with_hint(format!("settings are {}", KEYS.join(", "))),
                });
            }
            let Value::Table(values) = value else {
                return Err(bad(
                    path,
                    format!(
                        "setting \"{key}\" must be a table, not a {}",
                        value.type_name()
                    ),
                ));
            };
            let target = match merged.raw_get::<Option<Table>>(key.as_str())? {
                Some(t) => t,
                None => {
                    let t = lua.create_table()?;
                    merged.raw_set(key.as_str(), &t)?;
                    t
                }
            };
            for pair in values.pairs::<Value, Value>() {
                let (k, v) = pair?;
                target.raw_set(k, v)?;
            }
        }
    }
    Ok(merged)
}
