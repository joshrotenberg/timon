//! Command-line options from type declarations: the parser behind task options and
//! `cli.parse`.
//!
//! An option is declared with a type in the syntax of [`crate::types`], alone or in a table:
//!
//! ```lua
//! {
//!   base = "string?",
//!   port = "integer",
//!   verbose = { type = "boolean", short = "v" },
//!   tags = "string[]",
//!   mode = { type = '"fast" | "full"', default = "fast", doc = "how hard to try" },
//! }
//! ```
//!
//! Flags follow babashka.cli: `--key value`, `--key=value`, `--flag` and `--no-flag` for
//! booleans, a repeated flag for a list, `-k` for a declared short name, and `--` to stop.
//! `dry_run` is given as `--dry-run`. Other words are kept, in order.
//!
//! A boolean is false and a list empty unless given. An option whose type is not optional and
//! has no default must be given.

use std::rc::Rc;

use mlua::{Table, Value};
use serde_json::{Map, Value as Json};

use crate::error::Error;
use crate::modules::schema::{Node, Problem, Shape};

/// One declared option.
#[derive(Debug, Clone)]
pub struct Opt {
    pub name: String,
    /// The type as written.
    pub ty: String,
    shape: Rc<Shape>,
    pub doc: String,
    pub default: Option<Json>,
    pub short: Option<char>,
}

impl Opt {
    /// The type without its `?`.
    fn node(&self) -> &Node {
        match &self.shape.node {
            Node::Optional(inner) => &inner.node,
            node => node,
        }
    }

    fn is_flag(&self) -> bool {
        matches!(self.node(), Node::Boolean)
    }

    fn is_list(&self) -> bool {
        matches!(self.node(), Node::Array(_))
    }

    /// Whether the option must be given.
    pub fn required(&self) -> bool {
        !matches!(self.shape.node, Node::Optional(_))
            && !self.is_flag()
            && !self.is_list()
            && self.default.is_none()
    }

    /// `--dry-run` for `dry_run`.
    pub fn flag(&self) -> String {
        format!("--{}", self.name.replace('_', "-"))
    }

    /// Whether the flag takes a value: every option but a boolean.
    pub fn takes_value(&self) -> bool {
        !self.is_flag()
    }

    /// The strings a choice option takes, for completion.
    pub fn choices(&self) -> Vec<String> {
        choices(self.node())
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

/// Declared options, in name order.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub options: Vec<Opt>,
}

/// What [`Spec::parse`] found.
#[derive(Debug, Default)]
pub struct Parsed {
    /// Every option: given, defaulted, false or empty; an optional one not given is absent.
    pub values: Map<String, Json>,
    /// The other words, in order, and every word after `--`.
    pub rest: Vec<String>,
    /// `--help` or `-h` came before `--`.
    pub help: bool,
}

/// Names reserved for the words and for help.
const RESERVED: &[&str] = &["args", "help"];

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A type the command line can give: a string, number, boolean or one of several strings,
/// either of those, or a list of them.
fn scalar(node: &Node) -> bool {
    match node {
        Node::String | Node::Integer | Node::Number | Node::Boolean | Node::Enum(_) => true,
        Node::OneOf(alts) => alts
            .iter()
            .all(|a| scalar(&a.node) && !matches!(a.node, Node::Boolean)),
        _ => false,
    }
}

fn expressible(shape: &Shape) -> bool {
    let node = match &shape.node {
        Node::Optional(inner) => &inner.node,
        node => node,
    };
    match node {
        Node::Array(inner) => scalar(&inner.node) && !matches!(inner.node, Node::Boolean),
        node => scalar(node),
    }
}

/// A word as a value of `node`, or what was expected.
fn coerce(node: &Node, word: &str) -> Result<Json, String> {
    match node {
        Node::String => Ok(Json::String(word.to_owned())),
        Node::Integer => word
            .parse::<i64>()
            .map(Json::from)
            .map_err(|_| "an integer".to_owned()),
        Node::Number => word
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .and_then(serde_json::Number::from_f64)
            .map(Json::Number)
            .ok_or_else(|| "a number".to_owned()),
        Node::Boolean => match word {
            "true" => Ok(Json::Bool(true)),
            "false" => Ok(Json::Bool(false)),
            _ => Err("true or false".to_owned()),
        },
        Node::Enum(values) if values.iter().any(|v| v == word) => Ok(Json::String(word.to_owned())),
        Node::Enum(values) => {
            let list: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
            Err(format!("one of {}", list.join(", ")))
        }
        Node::OneOf(alts) => {
            let mut expected = Vec::new();
            for alt in alts {
                match coerce(&alt.node, word) {
                    Ok(v) => return Ok(v),
                    Err(e) => expected.push(e),
                }
            }
            Err(expected.join(" or "))
        }
        _ => Err("a value the command line cannot give".to_owned()),
    }
}

/// The strings an enum option takes, for did-you-mean.
fn choices(node: &Node) -> Vec<&str> {
    match node {
        Node::Enum(values) => values.iter().map(String::as_str).collect(),
        Node::OneOf(alts) => alts.iter().flat_map(|a| choices(&a.node)).collect(),
        Node::Array(inner) => choices(&inner.node),
        _ => Vec::new(),
    }
}

fn usage(message: String) -> Error {
    Error::new("usage", message)
}

impl Spec {
    /// Reads declarations from a Lua table, or says what is wrong with one.
    pub fn from_lua(table: &Table) -> Result<Self, String> {
        let mut options = Vec::new();
        for pair in table.pairs::<Value, Value>() {
            let (key, value) = pair.map_err(|e| e.to_string())?;
            let Value::String(key) = key else {
                return Err(format!(
                    "option names are strings, got a key of type {}",
                    key.type_name()
                ));
            };
            let name = key.to_string_lossy();
            if !valid_name(&name) {
                return Err(format!(
                    "\"{name}\" is not an option name: use lowercase letters, digits and _"
                ));
            }
            if RESERVED.contains(&name.as_str()) {
                return Err(format!(
                    "\"{name}\" is reserved: args holds the other words, and help is --help"
                ));
            }
            let (ty, doc, default, short) = match value {
                Value::String(s) => (s.to_string_lossy(), String::new(), Value::Nil, None),
                Value::Table(t) => {
                    for pair in t.pairs::<Value, Value>() {
                        let (k, _) = pair.map_err(|e| e.to_string())?;
                        let k = match &k {
                            Value::String(s) => s.to_string_lossy(),
                            other => other.type_name().to_owned(),
                        };
                        if !["type", "doc", "default", "short"].contains(&k.as_str()) {
                            let close =
                                crate::suggest::closest(&k, ["type", "doc", "default", "short"]);
                            return Err(match close {
                                Some(c) => format!(
                                    "option \"{name}\": unknown field \"{k}\"; did you mean \"{c}\"?"
                                ),
                                None => format!(
                                    "option \"{name}\": unknown field \"{k}\"; an option takes type, doc, default and short"
                                ),
                            });
                        }
                    }
                    let ty: Option<String> = t.raw_get("type").map_err(|e| e.to_string())?;
                    let Some(ty) = ty else {
                        return Err(format!("option \"{name}\" needs a type"));
                    };
                    let doc: Option<String> = t.raw_get("doc").map_err(|e| e.to_string())?;
                    let short: Option<String> = t.raw_get("short").map_err(|e| e.to_string())?;
                    let short = match short {
                        None => None,
                        Some(s) => {
                            let mut chars = s.chars();
                            match (chars.next(), chars.next()) {
                                (Some(c), None) if c.is_ascii_alphabetic() && c != 'h' => Some(c),
                                _ => {
                                    return Err(format!(
                                        "option \"{name}\": short must be one letter other than h, got \"{s}\""
                                    ));
                                }
                            }
                        }
                    };
                    let default: Value = t.raw_get("default").map_err(|e| e.to_string())?;
                    (ty, doc.unwrap_or_default(), default, short)
                }
                other => {
                    return Err(format!(
                        "option \"{name}\" must be a type string or a table, got {}",
                        other.type_name()
                    ));
                }
            };
            let shape = crate::types::parse(&ty).map_err(|e| format!("option \"{name}\": {e}"))?;
            if !expressible(&shape) {
                return Err(format!(
                    "option \"{name}\": a {ty} cannot be given on the command line; use string, integer, number, boolean, string choices, or a list of them"
                ));
            }
            let default = if default.is_nil() {
                None
            } else {
                if let Some(e) = shape.check_value(&default).into_iter().next() {
                    return Err(format!(
                        "option \"{name}\": the default {}",
                        problem_text(&e.problem)
                    ));
                }
                Some(
                    crate::value::to_json(&default, crate::value::Mode::Strict)
                        .map_err(|e| format!("option \"{name}\": {}", e.message))?,
                )
            };
            options.push(Opt {
                name,
                ty,
                shape,
                doc,
                default,
                short,
            });
        }
        options.sort_by(|a, b| a.name.cmp(&b.name));
        for (i, o) in options.iter().enumerate() {
            if let Some(c) = o.short
                && let Some(other) = options[..i].iter().find(|p| p.short == Some(c))
            {
                return Err(format!(
                    "options \"{}\" and \"{}\" both use -{c}",
                    other.name, o.name
                ));
            }
        }
        Ok(Self { options })
    }

    /// The option a word names: `--name`, `--name=...` or `-s`.
    pub fn lookup(&self, word: &str) -> Option<&Opt> {
        let word = word.split('=').next().unwrap_or(word);
        if let Some(long) = word.strip_prefix("--") {
            return self.find_flag(long);
        }
        let short = word.strip_prefix('-')?;
        let mut chars = short.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => self.options.iter().find(|o| o.short == Some(c)),
            _ => None,
        }
    }

    fn find_flag(&self, flag: &str) -> Option<&Opt> {
        let key = flag.replace('-', "_");
        self.options.iter().find(|o| o.name == key)
    }

    fn unknown(&self, word: &str) -> Error {
        let flags: Vec<String> = self.options.iter().map(Opt::flag).collect();
        let err = usage(format!("unknown option {word}"));
        let bare = word.split('=').next().unwrap_or(word);
        match crate::suggest::closest(bare, flags.iter().map(String::as_str)) {
            Some(close) => err.with_hint(format!("did you mean {close}?")),
            None if flags.is_empty() => {
                err.with_hint("it takes no options; put words that start with - after --")
            }
            None => err.with_hint(format!("it takes {}", flags.join(", "))),
        }
    }

    fn set(&self, values: &mut Map<String, Json>, opt: &Opt, word: &str) -> Result<(), Error> {
        let (node, list) = match opt.node() {
            Node::Array(inner) => (&inner.node, true),
            node => (node, false),
        };
        let value = coerce(node, word).map_err(|expected| {
            let err = usage(format!("{} must be {expected}, got \"{word}\"", opt.flag()));
            match crate::suggest::closest(word, choices(node)) {
                Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
                None => err,
            }
        })?;
        if list {
            match values.get_mut(&opt.name) {
                Some(Json::Array(items)) => items.push(value),
                _ => {
                    values.insert(opt.name.clone(), Json::Array(vec![value]));
                }
            }
        } else if values.insert(opt.name.clone(), value).is_some() {
            return Err(usage(format!("{} is given twice", opt.flag()))
                .with_hint("only a list option can be repeated"));
        }
        Ok(())
    }

    /// Parses `words`. Unknown options, bad values and missing required options are `usage`
    /// errors. After `--help`, nothing is checked.
    pub fn parse(&self, words: &[String]) -> Result<Parsed, Error> {
        let mut parsed = Parsed::default();
        let mut words = words.iter();
        while let Some(word) = words.next() {
            if word == "--" {
                parsed.rest.extend(words.cloned());
                break;
            }
            if word == "--help" || word == "-h" {
                parsed.help = true;
                return Ok(parsed);
            }
            let (opt, inline, negated) = if let Some(body) = word.strip_prefix("--") {
                let (name, inline) = match body.split_once('=') {
                    Some((n, v)) => (n, Some(v)),
                    None => (body, None),
                };
                match self.find_flag(name) {
                    Some(opt) => (opt, inline, false),
                    None => match name.strip_prefix("no-").and_then(|n| self.find_flag(n)) {
                        Some(opt) if opt.is_flag() && inline.is_none() => (opt, None, true),
                        _ => return Err(self.unknown(word)),
                    },
                }
            } else if word.len() > 1
                && word.starts_with('-')
                && !word[1..].starts_with(|c: char| c.is_ascii_digit() || c == '.')
            {
                let body = &word[1..];
                let (short, inline) = match body.split_once('=') {
                    Some((s, v)) => (s, Some(v)),
                    None => (body, None),
                };
                let mut chars = short.chars();
                let opt = match (chars.next(), chars.next()) {
                    (Some(c), None) => self.options.iter().find(|o| o.short == Some(c)),
                    _ => None,
                };
                match opt {
                    Some(opt) => (opt, inline, false),
                    None => return Err(self.unknown(word)),
                }
            } else {
                parsed.rest.push(word.clone());
                continue;
            };
            if opt.is_flag() {
                let value = match inline {
                    None => !negated,
                    Some("true") => true,
                    Some("false") => false,
                    Some(other) => {
                        return Err(usage(format!(
                            "{} must be true or false, got \"{other}\"",
                            opt.flag()
                        )));
                    }
                };
                if parsed
                    .values
                    .insert(opt.name.clone(), Json::Bool(value))
                    .is_some()
                {
                    return Err(usage(format!("{} is given twice", opt.flag())));
                }
                continue;
            }
            let value = match inline {
                Some(v) => v.to_owned(),
                None => words.next().cloned().ok_or_else(|| {
                    usage(format!("{} needs a value", opt.flag())).with_hint(format!(
                        "{} <{}>",
                        opt.flag(),
                        opt.ty
                    ))
                })?,
            };
            self.set(&mut parsed.values, opt, &value)?;
        }
        for opt in &self.options {
            if parsed.values.contains_key(&opt.name) {
                continue;
            }
            if opt.required() {
                let err = usage(format!("missing {}", opt.flag()));
                return Err(if opt.doc.is_empty() {
                    err.with_hint(format!("{} <{}>", opt.flag(), opt.ty))
                } else {
                    err.with_hint(format!("{} <{}>: {}", opt.flag(), opt.ty, opt.doc))
                });
            }
            if let Some(value) = self.fill(opt) {
                parsed.values.insert(opt.name.clone(), value);
            }
        }
        Ok(parsed)
    }

    /// An option's value when it is not given: its default, false, an empty list, or none.
    fn fill(&self, opt: &Opt) -> Option<Json> {
        match &opt.default {
            Some(d) => Some(d.clone()),
            None if opt.is_flag() => Some(Json::Bool(false)),
            None if opt.is_list() => Some(Json::Array(Vec::new())),
            None => None,
        }
    }

    /// The values when nothing is given, or the first required option that is missing.
    pub fn defaults(&self) -> Result<Map<String, Json>, Error> {
        self.parse(&[]).map(|p| p.values)
    }

    /// Checks options given as a Lua table, as `task.run` takes them, and fills the rest.
    /// `args`, if given, is a list of strings.
    pub fn check(&self, given: &Table, path: &str) -> Result<Map<String, Json>, Error> {
        let bad = |m: String| Error::new("bad_option", m);
        let mut values = Map::new();
        for pair in given.pairs::<Value, Value>() {
            let (k, v) = pair?;
            let Value::String(k) = k else {
                return Err(bad(format!("options to {path} must be named")));
            };
            let k = k.to_string_lossy();
            if k == "args" {
                let shape =
                    crate::types::parse("string[]").map_err(|e| Error::new("internal", e))?;
                if let Some(e) = shape.check_value(&v).into_iter().next() {
                    return Err(bad(format!(
                        "option \"args\" to {path}: {}",
                        problem_text(&e.problem)
                    )));
                }
                values.insert(k, crate::value::to_json(&v, crate::value::Mode::Strict)?);
                continue;
            }
            let Some(opt) = self.options.iter().find(|o| o.name == k) else {
                let names: Vec<&str> = self.options.iter().map(|o| o.name.as_str()).collect();
                let err = bad(format!("unknown option \"{k}\" to {path}"));
                return Err(match crate::suggest::closest(&k, names.iter().copied()) {
                    Some(c) => err.with_hint(format!("did you mean \"{c}\"?")),
                    None if names.is_empty() => err.with_hint(format!("{path} takes no options")),
                    None => err.with_hint(format!("{path} takes {}", names.join(", "))),
                });
            };
            if let Some(e) = opt.shape.check_value(&v).into_iter().next() {
                let at = if e.path.is_empty() {
                    k.clone()
                } else {
                    format!("{k}{}", e.path)
                };
                return Err(bad(format!(
                    "option \"{at}\" to {path}: {}",
                    problem_text(&e.problem)
                )));
            }
            values.insert(k, crate::value::to_json(&v, crate::value::Mode::Strict)?);
        }
        for opt in &self.options {
            if values.contains_key(&opt.name) {
                continue;
            }
            if opt.required() {
                return Err(bad(format!("missing option \"{}\" to {path}", opt.name)));
            }
            if let Some(value) = self.fill(opt) {
                values.insert(opt.name.clone(), value);
            }
        }
        Ok(values)
    }

    /// The options section of a help text.
    pub fn help(&self) -> String {
        let rows: Vec<(String, String)> = self
            .options
            .iter()
            .map(|o| {
                let short = match o.short {
                    Some(c) => format!("-{c}, "),
                    None => "    ".to_owned(),
                };
                let value = if o.is_flag() {
                    String::new()
                } else {
                    let ty = match o.node() {
                        Node::Array(_) => o.ty.trim_end_matches('?').trim_end_matches("[]"),
                        _ => o.ty.trim_end_matches('?'),
                    };
                    let ty = ty.trim_start_matches('(').trim_end_matches(')').to_owned();
                    format!(" <{ty}>{}", if o.is_list() { "..." } else { "" })
                };
                let mut doc = o.doc.clone();
                if let Some(d) = &o.default {
                    doc = format!("{doc} [default: {d}]").trim().to_owned();
                } else if o.required() {
                    doc = format!("{doc} [required]").trim().to_owned();
                }
                (format!("  {short}{}{value}", o.flag()), doc)
            })
            .chain(std::iter::once((
                "  -h, --help".to_owned(),
                "print this help".to_owned(),
            )))
            .collect();
        let width = rows
            .iter()
            .filter(|(_, doc)| !doc.is_empty())
            .map(|(l, _)| l.len())
            .max()
            .unwrap_or(0);
        let mut out = String::from("Options:\n");
        for (left, doc) in rows {
            if doc.is_empty() {
                out.push_str(&left);
            } else {
                out.push_str(&format!("{left:width$}  {doc}"));
            }
            out.push('\n');
        }
        out
    }

    /// The options as a JSON Schema object, for an MCP tool's input. `extra` adds properties,
    /// such as `args`, and `required_extra` names ones that must be given.
    pub fn json_schema(&self, extra: &[(&str, Json)], required_extra: &[&str]) -> Json {
        let mut properties = Map::new();
        let mut required: Vec<Json> = Vec::new();
        for o in &self.options {
            let mut schema = match &o.shape.node {
                Node::Optional(inner) => inner.json_schema(),
                _ => o.shape.json_schema(),
            };
            if !o.doc.is_empty() {
                schema["description"] = Json::String(o.doc.clone());
            }
            if let Some(d) = &o.default {
                schema["default"] = d.clone();
            }
            properties.insert(o.name.clone(), schema);
            if o.required() {
                required.push(Json::String(o.name.clone()));
            }
        }
        for (name, schema) in extra {
            properties.insert((*name).to_owned(), schema.clone());
        }
        for name in required_extra {
            required.push(Json::String((*name).to_owned()));
        }
        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }

    /// The options as JSON, for listings.
    pub fn to_json(&self) -> Json {
        Json::Array(
            self.options
                .iter()
                .map(|o| {
                    serde_json::json!({
                        "name": o.name,
                        "flag": o.flag(),
                        "type": o.ty,
                        "doc": o.doc,
                        "default": o.default,
                        "short": o.short.map(|c| format!("-{c}")),
                        "required": o.required(),
                    })
                })
                .collect(),
        )
    }
}

/// A problem with a value, as a phrase after its name.
fn problem_text(problem: &Problem) -> String {
    match problem {
        Problem::Wrong { expected, got } => format!("must be {expected}, got {got}"),
        Problem::NotOneOf { values, got, .. } => {
            let list: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
            format!("must be one of {}, got {got}", list.join(", "))
        }
        Problem::Unknown { key, .. } => format!("has an unknown field \"{key}\""),
        Problem::Missing { name } => format!("needs \"{name}\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(lua: &mlua::Lua, source: &str) -> Spec {
        let t: Table = lua.load(source).eval().unwrap();
        Spec::from_lua(&t).unwrap()
    }

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn every_flag_form() {
        let lua = mlua::Lua::new();
        let s = spec(
            &lua,
            r#"{ port = "integer?", verbose = { type = "boolean", short = "v" }, tags = "string[]",
                mode = { type = '"fast" | "full"', default = "fast" }, dry_run = "boolean",
                ratio = "(number | string)?" }"#,
        );
        let p = s
            .parse(&words(&[
                "a",
                "--port",
                "9000",
                "-v",
                "--tags=x",
                "--tags",
                "y",
                "--dry-run",
                "b",
                "--ratio",
                "0.5",
                "--",
                "--port",
            ]))
            .unwrap();
        assert_eq!(
            Json::Object(p.values),
            serde_json::json!({ "port": 9000, "verbose": true, "tags": ["x", "y"],
                                "mode": "fast", "dry_run": true, "ratio": 0.5 })
        );
        assert_eq!(p.rest, ["a", "b", "--port"]);
        let p = s
            .parse(&words(&["--no-verbose", "--mode=full", "-3"]))
            .unwrap();
        assert_eq!(p.values["verbose"], false);
        assert_eq!(p.values["mode"], "full");
        assert_eq!(p.values["tags"], serde_json::json!([]));
        assert!(!p.values.contains_key("port"));
        assert_eq!(p.rest, ["-3"]);
        assert!(s.parse(&words(&["--port", "x", "--help"])).is_err());
        assert!(s.parse(&words(&["--help", "--port", "x"])).unwrap().help);
    }

    #[test]
    fn mistakes_say_what_was_meant() {
        let lua = mlua::Lua::new();
        let s = spec(
            &lua,
            r#"{ base = { type = "string", doc = "the branch" }, mode = '"fast" | "full"' }"#,
        );
        let e = s.parse(&words(&["--bsae", "x"])).unwrap_err();
        assert_eq!(
            (e.kind.as_str(), e.message.as_str()),
            ("usage", "unknown option --bsae")
        );
        assert_eq!(e.hint.as_deref(), Some("did you mean --base?"));
        let e = s
            .parse(&words(&["--base", "x", "--mode", "fsat"]))
            .unwrap_err();
        assert_eq!(
            e.message,
            "--mode must be one of \"fast\", \"full\", got \"fsat\""
        );
        assert_eq!(e.hint.as_deref(), Some("did you mean \"fast\"?"));
        let e = s.parse(&words(&["--mode", "fast"])).unwrap_err();
        assert_eq!(e.message, "missing --base");
        assert_eq!(e.hint.as_deref(), Some("--base <string>: the branch"));
        let e = s
            .parse(&words(&["--base", "a", "--base", "b"]))
            .unwrap_err();
        assert_eq!(e.message, "--base is given twice");
        assert!(
            s.parse(&words(&["--base"]))
                .unwrap_err()
                .message
                .contains("needs a value")
        );
    }

    #[test]
    fn declarations_are_checked() {
        let lua = mlua::Lua::new();
        for (source, expected) in [
            (r#"{ args = "string" }"#, "\"args\" is reserved"),
            (r#"{ Base = "string" }"#, "is not an option name"),
            (
                r#"{ cfg = "table<string, string>" }"#,
                "cannot be given on the command line",
            ),
            (
                r#"{ n = { type = "integer", default = "x" } }"#,
                "the default must be",
            ),
            (
                r#"{ n = { type = "integer", defalt = 1 } }"#,
                "did you mean \"default\"?",
            ),
            (
                r#"{ a = { type = "boolean", short = "x" }, b = { type = "boolean", short = "x" } }"#,
                "both use -x",
            ),
            (
                r#"{ a = { type = "boolean", short = "h" } }"#,
                "one letter other than h",
            ),
        ] {
            let t: Table = lua.load(source).eval().unwrap();
            let e = Spec::from_lua(&t).unwrap_err();
            assert!(e.contains(expected), "{source}: {e}");
        }
    }

    #[test]
    fn help_lists_flags_types_and_defaults() {
        let lua = mlua::Lua::new();
        let s = spec(
            &lua,
            r#"{ base = { type = "string", default = "main", short = "b", doc = "the branch" },
                tags = "string[]", verbose = "boolean", model = '("sonnet" | "opus")?' }"#,
        );
        assert_eq!(
            s.help(),
            "Options:\n\
             \x20 -b, --base <string>  the branch [default: \"main\"]\n\
             \x20     --model <\"sonnet\" | \"opus\">\n\
             \x20     --tags <string>...\n\
             \x20     --verbose\n\
             \x20 -h, --help           print this help\n"
        );
    }
}
