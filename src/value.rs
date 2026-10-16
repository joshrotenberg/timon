//! Lua values to and from JSON, and how values print.

use mlua::{Lua, Table, Value};

use crate::error::Error;

/// Deepest nesting converted before giving up, which also stops cycles.
const MAX_DEPTH: usize = 128;

/// How strictly to convert values JSON has no form for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// For printing: functions and other opaque values become descriptive strings.
    Display,
    /// For data: anything without a JSON form is an error.
    Strict,
}

/// Converts a Lua value to JSON.
///
/// A table whose keys are exactly `1..n` with `n > 0` is an array; any other table is an object
/// with its keys as strings, sorted. `nil` is `null`.
pub fn to_json(value: &Value, mode: Mode) -> Result<serde_json::Value, Error> {
    convert(value, mode, 0)
}

fn convert(value: &Value, mode: Mode, depth: usize) -> Result<serde_json::Value, Error> {
    if depth > MAX_DEPTH {
        return Err(Error::new(
            "not_json",
            format!("nested deeper than {MAX_DEPTH} levels, or a table contains itself"),
        ));
    }
    Ok(match value {
        Value::Nil => serde_json::Value::Null,
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Integer(i) => serde_json::Value::from(*i),
        Value::Number(n) => match serde_json::Number::from_f64(*n) {
            Some(n) => serde_json::Value::Number(n),
            None => opaque(value, mode, "a number that is not finite")?,
        },
        Value::String(s) => serde_json::Value::String(s.to_string_lossy()),
        Value::Table(t) => table(t, mode, depth)?,
        _ => opaque(value, mode, value.type_name())?,
    })
}

fn table(t: &Table, mode: Mode, depth: usize) -> Result<serde_json::Value, Error> {
    let len = t.raw_len();
    let mut entries = Vec::new();
    for pair in t.pairs::<Value, Value>() {
        entries.push(pair.map_err(Error::from)?);
    }
    let is_array = len > 0
        && entries.len() == len
        && entries
            .iter()
            .all(|(k, _)| matches!(k, Value::Integer(i) if *i >= 1 && (*i as usize) <= len));
    if is_array {
        let mut items = vec![serde_json::Value::Null; len];
        for (k, v) in &entries {
            if let Value::Integer(i) = k {
                items[(*i - 1) as usize] = convert(v, mode, depth + 1)?;
            }
        }
        return Ok(serde_json::Value::Array(items));
    }
    let mut map = serde_json::Map::new();
    for (k, v) in &entries {
        let key = match k {
            Value::String(s) => s.to_string_lossy(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Boolean(b) => b.to_string(),
            other => match mode {
                Mode::Display => describe(other),
                Mode::Strict => {
                    return Err(Error::new(
                        "not_json",
                        format!("a {} cannot be a JSON object key", other.type_name()),
                    ));
                }
            },
        };
        map.insert(key, convert(v, mode, depth + 1)?);
    }
    Ok(serde_json::Value::Object(map))
}

fn opaque(value: &Value, mode: Mode, what: &str) -> Result<serde_json::Value, Error> {
    match mode {
        Mode::Display => Ok(serde_json::Value::String(describe(value))),
        Mode::Strict => Err(Error::new("not_json", format!("{what} has no JSON form"))),
    }
}

fn describe(value: &Value) -> String {
    value
        .to_string()
        .unwrap_or_else(|_| format!("<{}>", value.type_name()))
}

/// Converts JSON to a Lua value: objects and arrays become tables, `null` becomes `nil`.
pub fn from_json(lua: &Lua, json: &serde_json::Value) -> mlua::Result<Value> {
    Ok(match json {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
        },
        serde_json::Value::String(s) => Value::String(lua.create_string(s)?),
        serde_json::Value::Array(items) => {
            let t = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item) in items.iter().enumerate() {
                t.raw_set(i + 1, from_json(lua, item)?)?;
            }
            Value::Table(t)
        }
        serde_json::Value::Object(map) => {
            let t = lua.create_table_with_capacity(0, map.len())?;
            for (k, v) in map {
                t.raw_set(k.as_str(), from_json(lua, v)?)?;
            }
            Value::Table(t)
        }
    })
}

/// How a value prints on stdout: strings raw, tables as JSON, everything else as Lua's
/// `tostring` would. `pretty` indents JSON, for a terminal.
pub fn render(value: &Value, pretty: bool) -> Result<String, Error> {
    match value {
        Value::String(s) => Ok(s.to_string_lossy()),
        Value::Table(_) => {
            let json = to_json(value, Mode::Display)?;
            let text = if pretty {
                serde_json::to_string_pretty(&json)
            } else {
                serde_json::to_string(&json)
            };
            text.map_err(|e| Error::new("not_json", e.to_string()))
        }
        other => Ok(describe(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(lua: &Lua, code: &str) -> Value {
        lua.load(code).eval().expect("eval")
    }

    #[test]
    fn sequences_are_arrays_and_the_rest_objects() {
        let lua = Lua::new();
        let v = eval(&lua, "return { 1, 'two', { a = true } }");
        assert_eq!(
            to_json(&v, Mode::Strict).unwrap(),
            serde_json::json!([1, "two", { "a": true }])
        );
        let v = eval(&lua, "return { [1] = 'a', [3] = 'c' }");
        assert_eq!(
            to_json(&v, Mode::Strict).unwrap(),
            serde_json::json!({ "1": "a", "3": "c" })
        );
    }

    #[test]
    fn functions_print_in_display_mode_and_fail_in_strict_mode() {
        let lua = Lua::new();
        let v = eval(&lua, "return { f = print }");
        let shown = to_json(&v, Mode::Display).unwrap();
        assert!(shown["f"].as_str().unwrap().starts_with("function"));
        let err = to_json(&v, Mode::Strict).unwrap_err();
        assert_eq!(err.kind, "not_json");
    }

    #[test]
    fn cycles_are_an_error_not_a_hang() {
        let lua = Lua::new();
        let v = eval(&lua, "local t = {}; t.self = t; return t");
        assert_eq!(to_json(&v, Mode::Display).unwrap_err().kind, "not_json");
    }

    #[test]
    fn json_round_trips_through_lua() {
        let lua = Lua::new();
        let json =
            serde_json::json!({ "n": 1, "f": 1.5, "s": "x", "a": [1, 2], "o": { "k": false } });
        let v = from_json(&lua, &json).unwrap();
        assert_eq!(to_json(&v, Mode::Strict).unwrap(), json);
    }

    #[test]
    fn rendering() {
        let lua = Lua::new();
        assert_eq!(render(&eval(&lua, "return 'hi'"), false).unwrap(), "hi");
        assert_eq!(render(&eval(&lua, "return 7 / 2"), false).unwrap(), "3.5");
        assert_eq!(render(&eval(&lua, "return 6 * 7"), false).unwrap(), "42");
        assert_eq!(render(&eval(&lua, "return nil"), false).unwrap(), "nil");
        assert_eq!(
            render(&eval(&lua, "return { b = 1, a = 2 }"), false).unwrap(),
            r#"{"a":2,"b":1}"#
        );
    }
}
