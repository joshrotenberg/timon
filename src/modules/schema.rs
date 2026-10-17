//! `schema`: shapes for structured data, and contracts (named schemas).
//!
//! A schema compiles to JSON Schema that both providers accept for structured output, and
//! checks values with error paths such as `findings[2].line`. Every object field is required
//! unless it is wrapped in `optional`, and objects allow no extra fields.

use std::path::PathBuf;
use std::rc::Rc;

use include_dir::{Dir, include_dir};
use mlua::{AnyUserData, Lua, MetaMethod, Table, UserData, UserDataMethods, Value};
use serde_json::{Map, Value as Json, json};

use crate::docs::{Func, Module};
use crate::error::Error;
use crate::modules::{Installer, json as json_module};
use crate::suggest;
use crate::value::{self, Mode};

static BUILTIN_CONTRACTS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/contracts");

/// Registry key of the directories searched for contracts, project layer first.
pub(crate) const CONTRACT_DIRS: &str = "timon.contract_dirs";
const CACHE: &str = "timon.contract_cache";

pub const DOC: Module = Module {
    name: "schema",
    summary: "Shapes for structured data. Values: schema.string, schema.int, schema.number, schema.bool, schema.any. Every object field is required unless optional; s:describe(text) documents a field. A contract is a named schema in contracts/.",
    functions: &[
        Func::new(
            "object",
            "schema.object(fields) -> schema",
            "An object with exactly these fields. Each field is required unless wrapped in schema.optional.",
            "schema.object{\n  verdict = schema.enum{ \"approve\", \"request_changes\" },\n  summary = schema.string:describe(\"two sentences at most\"),\n  notes = schema.optional(schema.string),\n}",
        ),
        Func::new(
            "array",
            "schema.array(item) -> schema",
            "A list whose elements all match item.",
            "schema.array(schema.object{ file = schema.string, line = schema.int })",
        ),
        Func::new(
            "map",
            "schema.map(value) -> schema",
            "An object with any string keys whose values all match value. Codex's strict output mode does not accept it; use object for agent output.",
            "schema.map(schema.int)  -- { a = 1, b = 2 }",
        ),
        Func::new(
            "enum",
            "schema.enum(values) -> schema",
            "One of the given strings.",
            "schema.enum{ \"high\", \"medium\", \"low\" }",
        ),
        Func::new(
            "optional",
            "schema.optional(s) -> schema",
            "s, or missing, or null.",
            "schema.object{ name = schema.string, nickname = schema.optional(schema.string) }",
        ),
        Func::new(
            "check",
            "schema.check(s, value) -> true | false, errors",
            "Validates value. Each error is { path, message }, with paths like findings[2].line.",
            "local ok, errors = schema.check(schema.object{ n = schema.int }, { n = \"x\" })\n-- false, { { path = \"n\", message = \"expected an integer, got a string\" } }",
        ),
        Func::new(
            "json",
            "schema.json(s) -> table",
            "The JSON Schema for s, as given to providers for structured output.",
            "json.encode(schema.json(schema.contract(\"review\")), { pretty = true })",
        ),
        Func::new(
            "contract",
            "schema.contract(name) -> schema",
            "A named schema: contracts/NAME.lua in the project or user layer, or a built-in one (review, implementation, investigation, yes_no, list).",
            "local review = schema.contract(\"review\")",
        ),
    ],
};

/// A shape and its description.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub node: Node,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    String,
    Integer,
    Number,
    Boolean,
    Any,
    Enum(Vec<String>),
    Array(Rc<Shape>),
    Map(Rc<Shape>),
    /// Fields in name order.
    Object(Vec<(String, Rc<Shape>)>),
    Optional(Rc<Shape>),
    /// Any one of these.
    OneOf(Vec<Rc<Shape>>),
    /// A Lua function; only options take one.
    Function,
    /// A schema value; only options take one.
    Schema,
}

/// What is wrong with a Lua value, found by [`Shape::check_value`].
#[derive(Debug, Clone, PartialEq)]
pub enum Problem {
    /// A field the shape does not have, and the closest one it does.
    Unknown { key: String, close: Option<String> },
    /// A required field that is missing.
    Missing { name: String },
    /// A value of the wrong type.
    Wrong { expected: String, got: String },
    /// A string that is not one of the allowed values, and the closest one.
    NotOneOf {
        values: Vec<String>,
        got: String,
        close: Option<String>,
    },
}

/// A [`Problem`] at a path such as `budget.turns` or `kinds[2]`; the root is "".
#[derive(Debug, Clone, PartialEq)]
pub struct ValueError {
    pub path: String,
    pub problem: Problem,
}

impl Shape {
    pub(crate) fn new(node: Node) -> Rc<Self> {
        Rc::new(Self {
            node,
            description: None,
        })
    }

    fn kind(&self) -> &'static str {
        match self.node {
            Node::String => "string",
            Node::Integer => "int",
            Node::Number => "number",
            Node::Boolean => "bool",
            Node::Any => "any",
            Node::Enum(_) => "enum",
            Node::Array(_) => "array",
            Node::Map(_) => "map",
            Node::Object(_) => "object",
            Node::Optional(_) => "optional",
            Node::OneOf(_) => "one_of",
            Node::Function => "function",
            Node::Schema => "schema",
        }
    }

    /// What a value of this shape is, for messages: "a string", "one of \"a\", \"b\"".
    pub fn expected(&self) -> String {
        match &self.node {
            Node::String => "a string".into(),
            Node::Integer => "an integer".into(),
            Node::Number => "a number".into(),
            Node::Boolean => "a boolean".into(),
            Node::Any => "a value".into(),
            Node::Function => "a function".into(),
            Node::Schema => "a schema".into(),
            Node::Enum(values) => {
                let list: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
                format!("one of {}", list.join(", "))
            }
            Node::Array(_) => "an array".into(),
            Node::Map(_) | Node::Object(_) => "a table".into(),
            Node::Optional(inner) => inner.expected(),
            Node::OneOf(options) => {
                let names: Vec<String> = options.iter().map(|o| o.expected()).collect();
                names.join(" or ")
            }
        }
    }

    /// Checks a Lua value, such as an options table, returning every problem with its path.
    pub fn check_value(&self, value: &Value) -> Vec<ValueError> {
        let mut errors = Vec::new();
        self.check_value_at(value, "", &mut errors);
        errors
    }

    fn check_value_at(&self, value: &Value, path: &str, out: &mut Vec<ValueError>) {
        let at = |problem| ValueError {
            path: path.to_owned(),
            problem,
        };
        let wrong = || {
            at(Problem::Wrong {
                expected: self.expected(),
                got: lua_type(value).to_owned(),
            })
        };
        let ok = match (&self.node, value) {
            (Node::Any, _) => true,
            (Node::Optional(_), Value::Nil) => true,
            (Node::Optional(inner), _) => {
                inner.check_value_at(value, path, out);
                true
            }
            (Node::String, Value::String(_)) => true,
            (Node::Integer, Value::Integer(_)) => true,
            (Node::Integer, Value::Number(n)) => n.fract() == 0.0,
            (Node::Number, Value::Integer(_) | Value::Number(_)) => true,
            (Node::Boolean, Value::Boolean(_)) => true,
            (Node::Function, Value::Function(_)) => true,
            (Node::Schema, Value::UserData(ud)) => ud.borrow::<Schema>().is_ok(),
            (Node::Enum(values), Value::String(s)) => {
                let s = s.to_string_lossy();
                if !values.contains(&s) {
                    out.push(at(Problem::NotOneOf {
                        values: values.clone(),
                        close: suggest::closest(&s, values.iter().map(String::as_str))
                            .map(str::to_owned),
                        got: format!("{s:?}"),
                    }));
                }
                true
            }
            (Node::OneOf(options), _) => options.iter().any(|o| o.check_value(value).is_empty()),
            (Node::Array(item), Value::Table(t)) => {
                let len = t.raw_len();
                if t.pairs::<Value, Value>().count() != len {
                    false
                } else {
                    for i in 1..=len {
                        let v: Value = t.raw_get(i).unwrap_or(Value::Nil);
                        item.check_value_at(&v, &format!("{path}[{i}]"), out);
                    }
                    true
                }
            }
            (Node::Map(inner), Value::Table(t)) => {
                for (k, v) in t.pairs::<Value, Value>().flatten() {
                    match k {
                        Value::String(k) => {
                            inner.check_value_at(&v, &join(path, &k.to_string_lossy()), out)
                        }
                        other => out.push(at(Problem::Wrong {
                            expected: "string keys".into(),
                            got: lua_type(&other).to_owned(),
                        })),
                    }
                }
                true
            }
            (Node::Object(fields), Value::Table(t)) => {
                for (k, v) in t.pairs::<Value, Value>().flatten() {
                    let Value::String(key) = k else {
                        out.push(at(Problem::Wrong {
                            expected: "named fields".into(),
                            got: format!("{} key", lua_type(&k)),
                        }));
                        continue;
                    };
                    let key = key.to_string_lossy();
                    match fields.iter().find(|(n, _)| *n == key) {
                        Some((_, shape)) => shape.check_value_at(&v, &join(path, &key), out),
                        None => out.push(at(Problem::Unknown {
                            close: suggest::closest(&key, fields.iter().map(|(n, _)| n.as_str()))
                                .map(str::to_owned),
                            key,
                        })),
                    }
                }
                for (name, shape) in fields {
                    if !matches!(shape.node, Node::Optional(_))
                        && t.raw_get::<Value>(name.as_str()).is_ok_and(|v| v.is_nil())
                    {
                        out.push(at(Problem::Missing { name: name.clone() }));
                    }
                }
                true
            }
            _ => false,
        };
        if !ok {
            out.push(wrong());
        }
    }

    /// The JSON Schema for this shape.
    pub fn json_schema(&self) -> Json {
        let mut out = match &self.node {
            Node::String => json!({ "type": "string" }),
            Node::Integer => json!({ "type": "integer" }),
            Node::Number => json!({ "type": "number" }),
            Node::Boolean => json!({ "type": "boolean" }),
            Node::Any => json!({}),
            Node::Enum(values) => json!({ "type": "string", "enum": values }),
            Node::Array(item) => json!({ "type": "array", "items": item.json_schema() }),
            Node::Map(value) => {
                json!({ "type": "object", "additionalProperties": value.json_schema() })
            }
            Node::Object(fields) => {
                let mut properties = Map::new();
                for (name, shape) in fields {
                    properties.insert(name.clone(), shape.json_schema());
                }
                let required: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
                json!({
                    "type": "object",
                    "properties": properties,
                    "required": required,
                    "additionalProperties": false,
                })
            }
            Node::Optional(inner) => {
                json!({ "anyOf": [inner.json_schema(), { "type": "null" }] })
            }
            Node::OneOf(options) => {
                json!({ "anyOf": options.iter().map(|o| o.json_schema()).collect::<Vec<_>>() })
            }
            Node::Function => json!({ "description": "a Lua function" }),
            Node::Schema => json!({ "type": "object", "description": "a JSON Schema" }),
        };
        if let (Some(text), Json::Object(map)) = (&self.description, &mut out) {
            map.insert("description".to_owned(), Json::String(text.clone()));
        }
        out
    }

    /// Validates `value`, returning every problem as `(path, message)`.
    pub fn check(&self, value: &Json) -> Vec<(String, String)> {
        let mut errors = Vec::new();
        self.check_at(value, "", &mut errors);
        errors
    }

    /// A Lua value as JSON to check against this shape. An empty Lua table cannot say it is an
    /// array, so an unmarked one becomes `[]` where this shape expects an array. Decoded JSON
    /// and tables marked by `json.object` keep their shape.
    pub fn lua_json(&self, value: &Value) -> Result<Json, Error> {
        let mut data = value::to_json(value, Mode::Strict)?;
        self.untyped_empties(value, &mut data);
        Ok(data)
    }

    fn untyped_empties(&self, value: &Value, data: &mut Json) {
        let Value::Table(t) = value else {
            return;
        };
        let field = |key: &str| -> Value {
            match t.raw_get::<Value>(key) {
                Ok(Value::Nil) => key
                    .parse::<i64>()
                    .ok()
                    .and_then(|i| t.raw_get(i).ok())
                    .unwrap_or(Value::Nil),
                v => v.unwrap_or(Value::Nil),
            }
        };
        match &self.node {
            Node::Optional(inner) => inner.untyped_empties(value, data),
            Node::OneOf(options) => {
                for option in options {
                    let mut candidate = data.clone();
                    option.untyped_empties(value, &mut candidate);
                    if option.check(&candidate).is_empty() {
                        *data = candidate;
                        return;
                    }
                }
            }
            Node::Array(item) => match data {
                Json::Object(map) if map.is_empty() && !value::is_marked(t) => {
                    *data = Json::Array(Vec::new());
                }
                Json::Array(items) => {
                    for (i, d) in items.iter_mut().enumerate() {
                        item.untyped_empties(&t.raw_get(i + 1).unwrap_or(Value::Nil), d);
                    }
                }
                _ => {}
            },
            Node::Map(inner) => {
                if let Json::Object(map) = data {
                    for (k, d) in map.iter_mut() {
                        inner.untyped_empties(&field(k), d);
                    }
                }
            }
            Node::Object(fields) => {
                if let Json::Object(map) = data {
                    for (name, shape) in fields {
                        if let Some(d) = map.get_mut(name) {
                            shape.untyped_empties(&field(name), d);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn check_at(&self, value: &Json, path: &str, errors: &mut Vec<(String, String)>) {
        let here = || {
            if path.is_empty() {
                "(root)".to_owned()
            } else {
                path.to_owned()
            }
        };
        let mut wrong = |expected: &str| {
            errors.push((
                here(),
                format!("expected {expected}, got {}", describe(value)),
            ))
        };
        match &self.node {
            Node::Any => {}
            Node::String => {
                if !value.is_string() {
                    wrong("a string");
                }
            }
            Node::Integer => {
                let ok = value.is_i64()
                    || value.is_u64()
                    || value.as_f64().is_some_and(|f| f.fract() == 0.0);
                if !ok {
                    wrong("an integer");
                }
            }
            Node::Number => {
                if !value.is_number() {
                    wrong("a number");
                }
            }
            Node::Boolean => {
                if !value.is_boolean() {
                    wrong("a boolean");
                }
            }
            Node::Enum(values) => match value.as_str() {
                Some(s) if values.iter().any(|v| v == s) => {}
                _ => {
                    let list: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
                    wrong(&format!("one of {}", list.join(", ")));
                }
            },
            Node::Optional(inner) => {
                if !value.is_null() {
                    inner.check_at(value, path, errors);
                }
            }
            Node::OneOf(options) => {
                if !options.iter().any(|o| o.check(value).is_empty()) {
                    wrong(&self.expected());
                }
            }
            Node::Function => wrong("a function"),
            Node::Schema => {
                if !value.is_object() {
                    wrong("a JSON Schema object");
                }
            }
            Node::Array(item) => match value {
                Json::Array(items) => {
                    for (i, v) in items.iter().enumerate() {
                        item.check_at(v, &format!("{path}[{}]", i + 1), errors);
                    }
                }
                _ => wrong("an array"),
            },
            Node::Map(inner) => match value {
                Json::Object(map) => {
                    for (k, v) in map {
                        inner.check_at(v, &join(path, k), errors);
                    }
                }
                _ => wrong("an object"),
            },
            Node::Object(fields) => match value {
                Json::Object(map) => {
                    for (name, shape) in fields {
                        match map.get(name) {
                            Some(v) => shape.check_at(v, &join(path, name), errors),
                            None if matches!(shape.node, Node::Optional(_)) => {}
                            None => errors.push((here(), format!("missing field \"{name}\""))),
                        }
                    }
                    for key in map.keys() {
                        if !fields.iter().any(|(n, _)| n == key) {
                            let names = fields.iter().map(|(n, _)| n.as_str());
                            let message = match suggest::closest(key, names) {
                                Some(close) => {
                                    format!("unexpected field \"{key}\"; did you mean \"{close}\"?")
                                }
                                None => format!("unexpected field \"{key}\""),
                            };
                            errors.push((here(), message));
                        }
                    }
                }
                _ => wrong("an object"),
            },
        }
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn describe(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "a boolean",
        Json::Number(n) if n.is_f64() => "a number",
        Json::Number(_) => "an integer",
        Json::String(_) => "a string",
        Json::Array(_) => "an array",
        Json::Object(_) => "an object",
    }
}

/// The type of a Lua value, for messages.
fn lua_type(value: &Value) -> &'static str {
    match value {
        Value::Nil => "nil",
        Value::Boolean(_) => "a boolean",
        Value::Integer(_) => "an integer",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Table(_) => "a table",
        Value::Function(_) => "a function",
        Value::UserData(ud) if ud.borrow::<Schema>().is_ok() => "a schema",
        _ => "a value of another type",
    }
}

/// A schema, as Lua sees it.
#[derive(Clone)]
pub struct Schema(pub Rc<Shape>);

impl UserData for Schema {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("describe", |_, this, text: String| {
            let mut shape = (*this.0).clone();
            shape.description = Some(text);
            Ok(Schema(Rc::new(shape)))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("schema({})", this.0.kind()))
        });
    }
}

fn shape_of(value: &Value, what: &str) -> Result<Rc<Shape>, Error> {
    if let Value::UserData(ud) = value
        && let Ok(schema) = ud.borrow::<Schema>()
    {
        return Ok(schema.0.clone());
    }
    Err(Error::new(
        "bad_argument",
        format!("{what} must be a schema, not a {}", value.type_name()),
    )
    .with_hint("build one with schema.string, schema.object{...} and the rest"))
}

/// The shape named by `value`: a schema, or the name of a contract.
pub(crate) fn resolve(lua: &Lua, value: &Value) -> Result<Rc<Shape>, Error> {
    match value {
        Value::String(name) => contract(lua, &name.to_string_lossy()),
        other => shape_of(other, "output"),
    }
}

/// Loads contract `name` from the layers, then the built-in ones; cached per runtime.
pub(crate) fn contract(lua: &Lua, name: &str) -> Result<Rc<Shape>, Error> {
    let cache: Table = match lua.named_registry_value::<Option<Table>>(CACHE)? {
        Some(t) => t,
        None => {
            let t = lua.create_table()?;
            lua.set_named_registry_value(CACHE, &t)?;
            t
        }
    };
    if let Some(ud) = cache.raw_get::<Option<AnyUserData>>(name)? {
        return Ok(ud.borrow::<Schema>()?.0.clone());
    }
    let dirs: Vec<String> = lua
        .named_registry_value::<Option<Vec<String>>>(CONTRACT_DIRS)?
        .unwrap_or_default();
    let mut found: Option<(String, String)> = None;
    for dir in &dirs {
        for ext in ["lua", "fnl"] {
            let path = PathBuf::from(dir).join(format!("{name}.{ext}"));
            if let Ok(text) = std::fs::read_to_string(&path) {
                found = Some((path.display().to_string(), text));
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    if found.is_none()
        && let Some(file) = BUILTIN_CONTRACTS.get_file(format!("{name}.lua"))
    {
        found = Some((
            format!("[builtin]/contracts/{name}.lua"),
            file.contents_utf8().unwrap_or_default().to_owned(),
        ));
    }
    let Some((path, text)) = found else {
        let names = contract_names(&dirs);
        let err = Error::new("not_found", format!("no contract named \"{name}\""));
        return Err(
            match suggest::closest(name, names.iter().map(String::as_str)) {
                Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
                None => err.with_hint(format!("contracts: {}", names.join(", "))),
            },
        );
    };
    let source = if path.ends_with(".fnl") {
        crate::fennel::compile(lua, &text, &format!("@{path}"))?
    } else {
        text
    };
    let bad = |message: String| Error::new("bad_config", format!("{path}: {message}"));
    let value: Value = lua
        .load(source)
        .set_name(format!("@{path}"))
        .set_environment(crate::settings::definition_env(lua)?)
        .call(())
        .map_err(|e| bad(Error::from_lua(&e).message))?;
    let shape = shape_of(&value, "a contract").map_err(|e| bad(e.message))?;
    cache.raw_set(name, lua.create_userdata(Schema(shape.clone()))?)?;
    Ok(shape)
}

fn contract_names(dirs: &[String]) -> Vec<String> {
    let mut names: Vec<String> = BUILTIN_CONTRACTS
        .files()
        .filter_map(|f| f.path().file_stem()?.to_str().map(str::to_owned))
        .collect();
    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if matches!(p.extension().and_then(|e| e.to_str()), Some("lua" | "fnl"))
                    && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
                {
                    names.push(stem.to_owned());
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Validation errors as a Lua array of `{ path, message }`.
pub(crate) fn errors_table(lua: &Lua, errors: &[(String, String)]) -> mlua::Result<Table> {
    let list = json_module::array(lua, None)?;
    for (i, (path, message)) in errors.iter().enumerate() {
        let e = lua.create_table()?;
        e.raw_set("path", path.as_str())?;
        e.raw_set("message", message.as_str())?;
        list.raw_set(i + 1, e)?;
    }
    Ok(list)
}

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let module = installer.module(&DOC)?;
    for (name, node) in [
        ("string", Node::String),
        ("int", Node::Integer),
        ("number", Node::Number),
        ("bool", Node::Boolean),
        ("any", Node::Any),
    ] {
        module.raw_set(name, Schema(Shape::new(node)))?;
    }

    let functions = [
        (
            "object",
            lua.create_function(|_, fields: Table| {
                let mut out = Vec::new();
                for pair in fields.pairs::<Value, Value>() {
                    let (k, v) = pair?;
                    let Value::String(name) = k else {
                        return Err(Error::new(
                            "bad_argument",
                            "schema.object takes { name = schema, ... }",
                        )
                        .into());
                    };
                    let name = name.to_string_lossy();
                    let shape = shape_of(&v, &format!("field \"{name}\""))?;
                    out.push((name, shape));
                }
                out.sort_by(|a, b| a.0.cmp(&b.0));
                Ok(Schema(Shape::new(Node::Object(out))))
            })?,
        ),
        (
            "array",
            lua.create_function(|_, item: Value| {
                Ok(Schema(Shape::new(Node::Array(shape_of(
                    &item, "the item",
                )?))))
            })?,
        ),
        (
            "map",
            lua.create_function(|_, v: Value| {
                Ok(Schema(Shape::new(Node::Map(shape_of(&v, "the value")?))))
            })?,
        ),
        (
            "enum",
            lua.create_function(|_, values: Vec<String>| {
                if values.is_empty() {
                    return Err(
                        Error::new("bad_argument", "schema.enum needs at least one value").into(),
                    );
                }
                Ok(Schema(Shape::new(Node::Enum(values))))
            })?,
        ),
        (
            "optional",
            lua.create_function(|_, v: Value| {
                Ok(Schema(Shape::new(Node::Optional(shape_of(
                    &v,
                    "the value",
                )?))))
            })?,
        ),
        (
            "check",
            lua.create_function(|lua, (s, v): (Value, Value)| {
                let shape = shape_of(&s, "the first argument")?;
                let errors = shape.check(&shape.lua_json(&v)?);
                if errors.is_empty() {
                    Ok(mlua::MultiValue::from_iter([Value::Boolean(true)]))
                } else {
                    Ok(mlua::MultiValue::from_iter([
                        Value::Boolean(false),
                        Value::Table(errors_table(lua, &errors)?),
                    ]))
                }
            })?,
        ),
        (
            "json",
            lua.create_function(|lua, s: Value| {
                let shape = shape_of(&s, "the argument")?;
                json_module::decode(lua, &shape.json_schema())
            })?,
        ),
        (
            "contract",
            lua.create_function(|lua, name: String| Ok(Schema(contract(lua, &name)?)))?,
        ),
    ];
    for (name, f) in functions {
        installer.function(&DOC, Some(&module), name, f)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(fields: Vec<(&str, Rc<Shape>)>) -> Rc<Shape> {
        Shape::new(Node::Object(
            fields.into_iter().map(|(n, s)| (n.to_owned(), s)).collect(),
        ))
    }

    #[test]
    fn json_schema_is_strict_compatible() {
        let s = object(vec![
            ("name", Shape::new(Node::String)),
            ("note", Shape::new(Node::Optional(Shape::new(Node::String)))),
        ]);
        assert_eq!(
            s.json_schema(),
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "note": { "anyOf": [{ "type": "string" }, { "type": "null" }] }
                },
                "required": ["name", "note"],
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn check_reports_paths() {
        let finding = object(vec![
            ("file", Shape::new(Node::String)),
            ("line", Shape::new(Node::Integer)),
        ]);
        let s = object(vec![("findings", Shape::new(Node::Array(finding)))]);
        let errors =
            s.check(&json!({ "findings": [{ "file": "a", "line": 1 }, { "file": 2, "lien": 3 }] }));
        assert_eq!(
            errors,
            [
                (
                    "findings[2].file".to_owned(),
                    "expected a string, got an integer".to_owned()
                ),
                (
                    "findings[2]".to_owned(),
                    "missing field \"line\"".to_owned()
                ),
                (
                    "findings[2]".to_owned(),
                    "unexpected field \"lien\"; did you mean \"line\"?".to_owned()
                ),
            ]
        );
    }
}
