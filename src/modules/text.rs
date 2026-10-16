//! `str`: string helpers, also installed as string methods so `s:trim()` works.

use mlua::{Lua, Table, Value};

use crate::docs::{Func, Module};
use crate::error::Error;
use crate::modules::{Installer, json};
use crate::value;

pub const DOC: Module = Module {
    name: "str",
    summary: "String helpers. Each is also a string method: s:split(\",\"), s:trim().",
    functions: &[
        Func::new(
            "split",
            "str.split(s, sep) -> array",
            "Splits on a plain separator (no patterns). Without one, splits on runs of whitespace and drops empty pieces.",
            "str.split(\"a,b,,c\", \",\")  --> { \"a\", \"b\", \"\", \"c\" }\n(\"  one two \"):split()     --> { \"one\", \"two\" }",
        ),
        Func::new(
            "trim",
            "str.trim(s) -> string",
            "Removes leading and trailing whitespace.",
            "(\"  hi \\n\"):trim()  --> \"hi\"",
        ),
        Func::new(
            "lines",
            "str.lines(s) -> array",
            "Splits into lines, accepting \\n and \\r\\n. A trailing newline does not add an empty line.",
            "for i, line in ipairs(str.lines(fs.read(\"notes.txt\"))) do print(i, line) end",
        ),
        Func::new(
            "starts",
            "str.starts(s, prefix) -> boolean",
            "Whether s begins with prefix.",
            "(\"feat: x\"):starts(\"feat:\")  --> true",
        ),
        Func::new(
            "ends",
            "str.ends(s, suffix) -> boolean",
            "Whether s ends with suffix.",
            "(\"review.lua\"):ends(\".lua\")  --> true",
        ),
        Func::new(
            "fmt",
            "str.fmt(template, vars) -> string",
            "Fills ${name} placeholders from vars. Strings are inserted as they are, tables as JSON, other values with tostring. $${ writes a literal ${. A missing name is an error.",
            "str.fmt(\"Review PR ${n}: ${files}\", { n = 12, files = { \"a.rs\" } })\n--> Review PR 12: [\"a.rs\"]",
        ),
    ],
};

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let module = installer.module(&DOC)?;
    let string: Table = lua.globals().raw_get("string")?;

    let functions = [
        (
            "split",
            lua.create_function(
                |lua, (s, sep): (mlua::LuaString, Option<mlua::LuaString>)| {
                    let s = s.to_string_lossy();
                    let pieces: Vec<String> = match sep {
                        None => s.split_whitespace().map(str::to_owned).collect(),
                        Some(sep) => {
                            let sep = sep.to_string_lossy();
                            if sep.is_empty() {
                                return Err(Error::new(
                                    "bad_argument",
                                    "str.split needs a non-empty separator",
                                )
                                .with_hint("leave sep out to split on whitespace")
                                .into());
                            }
                            s.split(sep.as_str()).map(str::to_owned).collect()
                        }
                    };
                    json::array_from(lua, pieces)
                },
            )?,
        ),
        (
            "trim",
            lua.create_function(|_, s: mlua::LuaString| Ok(s.to_string_lossy().trim().to_owned()))?,
        ),
        (
            "lines",
            lua.create_function(|lua, s: mlua::LuaString| {
                let s = s.to_string_lossy();
                json::array_from(lua, s.lines().map(str::to_owned).collect::<Vec<_>>())
            })?,
        ),
        (
            "starts",
            lua.create_function(|_, (s, prefix): (mlua::LuaString, mlua::LuaString)| {
                Ok(s.as_bytes().starts_with(&prefix.as_bytes()))
            })?,
        ),
        (
            "ends",
            lua.create_function(|_, (s, suffix): (mlua::LuaString, mlua::LuaString)| {
                Ok(s.as_bytes().ends_with(&suffix.as_bytes()))
            })?,
        ),
        (
            "fmt",
            lua.create_function(|lua, (template, vars): (String, Table)| {
                Ok(fmt(lua, &template, &vars)?)
            })?,
        ),
    ];
    for (name, f) in functions {
        let installed = installer.function(&DOC, Some(&module), name, f)?;
        string.raw_set(name, installed)?;
    }
    Ok(())
}

fn fmt(_lua: &Lua, template: &str, vars: &Table) -> Result<String, Error> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        if let Some(literal) = after.strip_prefix("$${") {
            out.push_str("${");
            rest = literal;
        } else if let Some(open) = after.strip_prefix("${") {
            let Some(end) = open.find('}') else {
                return Err(Error::new(
                    "bad_argument",
                    "str.fmt: a ${ placeholder is not closed",
                ));
            };
            let name = &open[..end];
            let value: Value = match name.parse::<i64>() {
                Ok(i) => vars.raw_get(i)?,
                Err(_) => vars.raw_get(name)?,
            };
            if value.is_nil() {
                return Err(Error::new(
                    "bad_argument",
                    format!("str.fmt: no value for ${{{name}}}"),
                ));
            }
            out.push_str(&value::render(&value, false)?);
            rest = &open[end + 1..];
        } else {
            out.push('$');
            rest = &after[1..];
        }
    }
    out.push_str(rest);
    Ok(out)
}
