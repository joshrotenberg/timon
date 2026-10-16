//! Strict options: every options table is checked against its function's declaration before
//! the function does anything.
//!
//! Each declared option type parses into a [`Shape`]; a function's options are the object of
//! those shapes, required ones as declared and the rest optional. The same shape gives the
//! options' JSON Schema.

use std::collections::HashMap;
use std::rc::Rc;

use mlua::{Table, Value};

use crate::docs::{self, Func};
use crate::error::Error;
use crate::modules::schema::{Node, Problem, Shape};

thread_local! {
    static SHAPES: std::cell::RefCell<HashMap<String, Rc<Shape>>> = Default::default();
}

/// The options of `path` as one object shape, built once per thread.
pub fn shape(path: &str) -> Result<Rc<Shape>, Error> {
    if let Some(shape) = SHAPES.with(|s| s.borrow().get(path).cloned()) {
        return Ok(shape);
    }
    let (_, func) = docs::function(path)
        .ok_or_else(|| Error::new("internal", format!("{path} has no declaration")))?;
    let mut fields = Vec::new();
    for o in func.options {
        let mut ty = crate::types::parse(o.ty)
            .map_err(|e| Error::new("internal", format!("{path}: option {}: {e}", o.name)))?;
        if !o.required && !matches!(ty.node, Node::Optional(_)) {
            ty = Shape::new(Node::Optional(ty));
        }
        fields.push((o.name.to_owned(), ty));
    }
    fields.sort_by(|a, b| a.0.cmp(&b.0));
    let shape = Shape::new(Node::Object(fields));
    SHAPES.with(|s| s.borrow_mut().insert(path.to_owned(), shape.clone()));
    Ok(shape)
}

/// Checks `opts` against the options declared for `path`.
///
/// Unknown keys, positional entries, wrong types, values outside an enum and missing required
/// options raise `bad_option`, with the closest declared name or value as the hint.
pub fn check(path: &str, opts: &Table) -> Result<(), Error> {
    let (_, func) = docs::function(path)
        .ok_or_else(|| Error::new("internal", format!("{path} has no declaration")))?;
    let errors = shape(path)?.check_value(&Value::Table(opts.clone()));
    match errors.into_iter().next() {
        None => Ok(()),
        Some(e) => Err(describe(path, func, &e.path, e.problem)),
    }
}

/// Checks an optional options table.
pub fn check_opt(path: &str, opts: Option<&Table>) -> Result<(), Error> {
    match opts {
        Some(opts) => check(path, opts),
        None => Ok(()),
    }
}

fn describe(path: &str, func: &Func, at: &str, problem: Problem) -> Error {
    let bad = |message: String| Error::new("bad_option", message);
    let did_you_mean = |err: Error, close: Option<String>, otherwise: String| match close {
        Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
        None if otherwise.is_empty() => err,
        None => err.with_hint(otherwise),
    };
    match problem {
        Problem::Wrong { expected, got } if at.is_empty() && expected == "named fields" => {
            bad(format!("options to {path} must be named, got {got}")).with_hint(takes(path, func))
        }
        Problem::Unknown { key, close } if at.is_empty() => did_you_mean(
            bad(format!("unknown option \"{key}\" to {path}")),
            close,
            takes(path, func),
        ),
        Problem::Unknown { key, close } => did_you_mean(
            bad(format!(
                "unknown field \"{key}\" in option \"{at}\" to {path}"
            )),
            close,
            format!("see help(\"{path}\")"),
        ),
        Problem::Missing { name } if at.is_empty() => {
            let doc = func.options.iter().find(|o| o.name == name).map(|o| o.doc);
            let err = bad(format!("missing option \"{name}\" to {path}"));
            match doc {
                Some(doc) => err.with_hint(doc),
                None => err,
            }
        }
        Problem::Missing { name } => bad(format!(
            "missing field \"{name}\" in option \"{at}\" to {path}"
        )),
        Problem::Wrong { expected, got } => bad(format!(
            "option \"{at}\" to {path} must be {expected}, got {got}"
        )),
        Problem::NotOneOf { values, got, close } => {
            let list: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
            did_you_mean(
                bad(format!(
                    "option \"{at}\" to {path} must be one of {}, got {got}",
                    list.join(", ")
                )),
                close,
                String::new(),
            )
        }
    }
}

fn takes(path: &str, func: &Func) -> String {
    if func.options.is_empty() {
        return format!("{path} takes no options");
    }
    let names: Vec<&str> = func.options.iter().map(|o| o.name).collect();
    format!("{path} takes {}", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_have_a_json_schema() {
        let schema = shape("agent.run").unwrap().json_schema();
        assert_eq!(
            schema["properties"]["access"]["anyOf"][0]["enum"],
            serde_json::json!(["read", "write", "full"])
        );
        assert_eq!(
            schema["properties"]["prompt"],
            serde_json::json!({ "type": "string" })
        );
    }
}
