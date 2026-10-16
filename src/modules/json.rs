//! `json`: encode and decode JSON, with shapes that survive a round trip.

use std::sync::LazyLock;

use mlua::{Lua, Table, Value};

use crate::docs::{Func, Module, Opt, Ty};
use crate::error::Error;
use crate::modules::Installer;
use crate::options;
use crate::value::{self, ARRAY_NAME, Mode, NULL_NAME, OBJECT_NAME};

/// JMESPath with the standard functions and every jpx extension, built on first use.
static QUERY: LazyLock<jpx_core::Runtime> = LazyLock::new(|| {
    jpx_core::Runtime::builder()
        .with_standard()
        .with_all_extensions()
        .build()
});

const ARRAY_MT: &str = "timon.json.array_mt";
const OBJECT_MT: &str = "timon.json.object_mt";
const NULL: &str = "timon.json.null";

pub const DOC: Module = Module {
    name: "json",
    summary: "Encode and decode JSON. json.null stands for null.",
    functions: &[
        Func::new(
            "encode",
            "json.encode(value, { pretty }) -> string",
            "Encodes a value. A table with keys 1..n is an array, any other table an object with sorted keys. Functions, cycles and arrays with holes are errors.",
            "json.encode({ name = \"timon\", tags = { \"lua\", \"agents\" } })\n--> {\"name\":\"timon\",\"tags\":[\"lua\",\"agents\"]}",
        )
        .with_options(&[Opt::new("pretty", Ty::Boolean, "indent with two spaces")]),
        Func::new(
            "decode",
            "json.decode(text) -> value",
            "Decodes JSON text. Arrays and objects keep their shape when encoded again, even when empty; null becomes json.null.",
            "local data = json.decode('{\"prs\": [12, 14]}')\nreturn data.prs[2]  --> 14",
        ),
        Func::new(
            "array",
            "json.array(t) -> t",
            "Marks t, or a new table, as an array so it encodes as [] when empty.",
            "json.encode({ items = json.array{} })  --> {\"items\":[]}",
        ),
        Func::new(
            "query",
            "json.query(value, expression) -> value",
            "Evaluates a JMESPath expression against a value, with jpx's extension functions (strings, dates, math, hashing and more). Lists come back as arrays and null as json.null.",
            "json.query(prs, \"[?draft == `false`].number\")\njson.query(data, \"items[*].name | sort(@) | join(', ', @)\")\njson.query(run, \"upper(status)\")",
        ),
        Func::new(
            "object",
            "json.object(t) -> t",
            "Marks t, or a new table, as an object so it encodes as {} when empty.",
            "json.encode(json.object{})  --> {}",
        ),
    ],
};

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let json = installer.module(&DOC)?;

    for (key, name) in [(ARRAY_MT, ARRAY_NAME), (OBJECT_MT, OBJECT_NAME)] {
        let mt = lua.create_table()?;
        mt.raw_set("__name", name)?;
        lua.set_named_registry_value(key, mt)?;
    }
    let null = lua.create_table()?;
    let null_mt = lua.create_table()?;
    null_mt.raw_set("__name", NULL_NAME)?;
    null_mt.raw_set("__tostring", lua.create_function(|_, _: Value| Ok("null"))?)?;
    null_mt.raw_set(
        "__newindex",
        lua.create_function(|_, _: mlua::MultiValue| -> mlua::Result<()> {
            Err(Error::new("bad_argument", "json.null cannot be changed").into())
        })?,
    )?;
    null.set_metatable(Some(null_mt))?;
    lua.set_named_registry_value(NULL, &null)?;
    json.raw_set("null", null)?;

    installer.function(
        &DOC,
        Some(&json),
        "encode",
        lua.create_function(|_, (v, opts): (Value, Option<Table>)| {
            options::check_opt("json.encode", opts.as_ref())?;
            let pretty = match &opts {
                Some(o) => o.raw_get::<Option<bool>>("pretty")?.unwrap_or(false),
                None => false,
            };
            let json = value::to_json(&v, Mode::Strict)?;
            let text = if pretty {
                serde_json::to_string_pretty(&json)
            } else {
                serde_json::to_string(&json)
            };
            Ok(text.map_err(|e| Error::new("not_json", e.to_string()))?)
        })?,
    )?;
    installer.function(
        &DOC,
        Some(&json),
        "decode",
        lua.create_function(|lua, text: mlua::LuaString| {
            let parsed: serde_json::Value = serde_json::from_slice(&text.as_bytes())
                .map_err(|e| Error::new("bad_json", format!("invalid JSON: {e}")))?;
            decode(lua, &parsed)
        })?,
    )?;
    installer.function(
        &DOC,
        Some(&json),
        "query",
        lua.create_function(|lua, (v, expression): (Value, String)| {
            let data = value::to_json(&v, Mode::Strict)?;
            let compiled = QUERY.compile(&expression).map_err(query_error)?;
            let result = compiled.search(&data).map_err(query_error)?;
            decode(lua, &result)
        })?,
    )?;
    installer.function(
        &DOC,
        Some(&json),
        "array",
        lua.create_function(|lua, t: Option<Table>| array(lua, t))?,
    )?;
    installer.function(
        &DOC,
        Some(&json),
        "object",
        lua.create_function(|lua, t: Option<Table>| mark(lua, t, OBJECT_MT))?,
    )?;
    Ok(())
}

fn query_error(err: jpx_core::JmespathError) -> Error {
    let kind = match err.reason {
        jpx_core::ErrorReason::Parse(_) => "bad_query",
        _ => "query_failed",
    };
    Error::new(kind, err.to_string())
}

/// Marks `t`, or a new table, as a JSON array. Host functions use it for every list they return.
pub(crate) fn array(lua: &Lua, t: Option<Table>) -> mlua::Result<Table> {
    mark(lua, t, ARRAY_MT)
}

/// A new array holding `items`.
pub(crate) fn array_from<T: mlua::IntoLua>(
    lua: &Lua,
    items: impl IntoIterator<Item = T>,
) -> mlua::Result<Table> {
    let t = array(lua, None)?;
    for (i, item) in items.into_iter().enumerate() {
        t.raw_set(i + 1, item)?;
    }
    Ok(t)
}

fn mark(lua: &Lua, t: Option<Table>, key: &str) -> mlua::Result<Table> {
    let t = match t {
        Some(t) => t,
        None => lua.create_table()?,
    };
    t.set_metatable(Some(lua.named_registry_value(key)?))?;
    Ok(t)
}

fn decode(lua: &Lua, json: &serde_json::Value) -> mlua::Result<Value> {
    Ok(match json {
        serde_json::Value::Null => lua.named_registry_value(NULL)?,
        serde_json::Value::Array(items) => {
            let t = array(lua, Some(lua.create_table_with_capacity(items.len(), 0)?))?;
            for (i, item) in items.iter().enumerate() {
                t.raw_set(i + 1, decode(lua, item)?)?;
            }
            Value::Table(t)
        }
        serde_json::Value::Object(map) => {
            let t = mark(
                lua,
                Some(lua.create_table_with_capacity(0, map.len())?),
                OBJECT_MT,
            )?;
            for (k, v) in map {
                t.raw_set(k.as_str(), decode(lua, v)?)?;
            }
            Value::Table(t)
        }
        scalar => value::from_json(lua, scalar)?,
    })
}
