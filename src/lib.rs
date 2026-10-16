//! timon: a self-contained agent harness.
//!
//! One binary embedding Lua 5.4 over agent, session and tool APIs. This crate is at phase 0:
//! it evaluates plain Lua. See `docs/design.md` for where it is going.

use mlua::{Lua, LuaSerdeExt, MultiValue, Value};

/// Chunk name used in error messages for code passed to `eval`.
const EVAL_CHUNK: &str = "=eval";

/// Evaluates Lua source and returns every value it produces.
///
/// A chunk that parses as an expression is evaluated as `return <chunk>`, as the Lua REPL does,
/// so `1 + 1` returns 2. Anything else runs as a block and returns what its `return` gives.
pub fn eval(lua: &Lua, source: &str) -> mlua::Result<MultiValue> {
    match lua
        .load(format!("return {source}"))
        .set_name(EVAL_CHUNK)
        .into_function()
    {
        Ok(expression) => expression.call(()),
        Err(_) => lua.load(source).set_name(EVAL_CHUNK).eval(),
    }
}

/// Renders a value for stdout: strings raw, tables as JSON, scalars as Lua prints them.
///
/// `pretty` indents JSON, for a terminal.
pub fn render(lua: &Lua, value: &Value, pretty: bool) -> mlua::Result<String> {
    match value {
        Value::String(s) => Ok(s.to_string_lossy()),
        Value::Table(_) => {
            let json: serde_json::Value = lua.from_value(value.clone())?;
            let text = if pretty {
                serde_json::to_string_pretty(&json)
            } else {
                serde_json::to_string(&json)
            };
            text.map_err(mlua::Error::external)
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval_one(source: &str) -> String {
        let lua = Lua::new();
        let values = eval(&lua, source).expect("eval");
        let value = values.into_iter().next().unwrap_or(Value::Nil);
        render(&lua, &value, false).expect("render")
    }

    #[test]
    fn an_expression_returns_its_value() {
        assert_eq!(eval_one("1 + 1"), "2");
        assert_eq!(eval_one("7 / 2"), "3.5");
        assert_eq!(eval_one("'hi' .. ' there'"), "hi there");
    }

    #[test]
    fn a_block_returns_what_it_returns() {
        assert_eq!(eval_one("local x = 6; return x * 7"), "42");
        assert_eq!(eval_one("local x = 1"), "nil");
    }

    #[test]
    fn tables_render_as_json() {
        assert_eq!(eval_one("{ 1, 2, 3 }"), "[1,2,3]");
        assert_eq!(eval_one("{ name = 'timon' }"), r#"{"name":"timon"}"#);
    }

    #[test]
    fn a_syntax_error_names_the_chunk() {
        let lua = Lua::new();
        let err = eval(&lua, "local = 1").expect_err("syntax error");
        assert!(err.to_string().contains("eval:"), "{err}");
    }
}
