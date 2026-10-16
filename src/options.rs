//! Strict options: every options table is checked against its function's declaration before
//! the function does anything.

use mlua::{Table, Value};

use crate::docs::{self, Func};
use crate::error::Error;
use crate::suggest;

/// Checks `opts` against the options declared for `path`.
///
/// Unknown keys, positional entries, wrong types and missing required options raise
/// `bad_option`, with the closest declared name as the hint when there is one.
pub fn check(path: &str, opts: &Table) -> Result<(), Error> {
    let (_, func) = docs::function(path)
        .ok_or_else(|| Error::new("internal", format!("{path} has no declaration")))?;
    for pair in opts.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let Value::String(key) = key else {
            return Err(Error::new(
                "bad_option",
                format!(
                    "options to {path} must be named, got a {} key",
                    key.type_name()
                ),
            )
            .with_hint(takes(path, func)));
        };
        let key = key.to_string_lossy();
        let Some(opt) = func.options.iter().find(|o| o.name == key) else {
            let err = Error::new("bad_option", format!("unknown option \"{key}\" to {path}"));
            return Err(
                match suggest::closest(&key, func.options.iter().map(|o| o.name)) {
                    Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
                    None => err.with_hint(takes(path, func)),
                },
            );
        };
        if !opt.ty.accepts(&value) {
            return Err(Error::new(
                "bad_option",
                format!(
                    "option \"{key}\" to {path} must be a {}, got a {}",
                    opt.ty.name(),
                    value.type_name()
                ),
            ));
        }
    }
    for opt in func.options.iter().filter(|o| o.required) {
        if opts.raw_get::<Value>(opt.name)?.is_nil() {
            return Err(Error::new(
                "bad_option",
                format!("missing option \"{}\" to {path}", opt.name),
            )
            .with_hint(opt.doc));
        }
    }
    Ok(())
}

/// Checks an optional options table.
pub fn check_opt(path: &str, opts: Option<&Table>) -> Result<(), Error> {
    match opts {
        Some(opts) => check(path, opts),
        None => Ok(()),
    }
}

fn takes(path: &str, func: &Func) -> String {
    if func.options.is_empty() {
        return format!("{path} takes no options");
    }
    let names: Vec<&str> = func.options.iter().map(|o| o.name).collect();
    format!("{path} takes {}", names.join(", "))
}
