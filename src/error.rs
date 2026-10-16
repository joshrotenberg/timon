//! Errors as data.
//!
//! Every failure timon reports has a kind, a message and, when there is one, a hint. Host
//! functions return [`Error`]; Lua code sees it as a table with a `__tostring`, whether it was
//! raised by Rust or by `error{ kind = ..., message = ... }` in Lua.

use std::fmt;

use mlua::{Lua, Table, Value};

/// Registry key of the metatable every error table carries.
const ERROR_MT: &str = "timon.error_mt";

/// A failure with a machine-readable kind.
#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    /// Short `snake_case` category a script can branch on, such as `bad_option`.
    pub kind: String,
    /// What went wrong, for a person.
    pub message: String,
    /// What to do about it, when timon knows.
    pub hint: Option<String>,
    /// Structured detail, such as validation errors. Boxed to keep the error small.
    pub data: Option<Box<serde_json::Value>>,
}

impl Error {
    /// An error of `kind` with `message`.
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            hint: None,
            data: None,
        }
    }

    /// Adds what the reader should do about it.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Adds structured detail.
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(Box::new(data));
        self
    }

    /// Converts any mlua error, unwrapping callback and context layers to the cause.
    pub fn from_lua(err: &mlua::Error) -> Self {
        match err {
            mlua::Error::CallbackError { cause, .. } => Self::from_lua(cause),
            mlua::Error::WithContext { context, cause } => {
                let mut inner = Self::from_lua(cause);
                inner.message = format!("{context}: {}", inner.message);
                inner
            }
            mlua::Error::ExternalError(external) => match external.downcast_ref::<Error>() {
                Some(own) => own.clone(),
                None => Self::new("host", external.to_string()),
            },
            mlua::Error::RuntimeError(message) => Self::new("lua", message.clone()),
            mlua::Error::SyntaxError { message, .. } => Self::new("syntax", message.clone()),
            mlua::Error::MemoryError(message) => Self::new("memory", message.clone()),
            mlua::Error::BadArgument { .. } | mlua::Error::FromLuaConversionError { .. } => {
                Self::new("bad_argument", err.to_string())
            }
            other => Self::new("lua", other.to_string()),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for Error {}

impl From<Error> for mlua::Error {
    fn from(err: Error) -> Self {
        mlua::Error::external(err)
    }
}

impl From<mlua::Error> for Error {
    fn from(err: mlua::Error) -> Self {
        Self::from_lua(&err)
    }
}

/// An error that reached the top of a run, with where it happened.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    /// The error itself.
    pub error: Error,
    /// Lua frames from the innermost out, such as `eval:1` or `review.lua:12 in function 'f'`.
    pub traceback: Vec<String>,
}

impl Failure {
    /// The report printed to stderr: the error, its hint, then the frames.
    pub fn report(&self) -> String {
        let mut out = format!("timon: {}", self.error);
        if let Some(hint) = &self.error.hint {
            out.push_str(&format!("\n  hint: {hint}"));
        }
        for frame in &self.traceback {
            out.push_str(&format!("\n  at {frame}"));
        }
        out
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self {
            error,
            traceback: Vec::new(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.report())
    }
}

impl std::error::Error for Failure {}

/// Creates the error metatable. Called once per Lua state.
pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let mt = lua.create_table()?;
    mt.set("__name", "timon.error")?;
    mt.set(
        "__tostring",
        lua.create_function(|_, t: Table| {
            let kind: String = t.raw_get("kind").unwrap_or_default();
            let message: String = t.raw_get("message").unwrap_or_default();
            Ok(format!("{kind}: {message}"))
        })?,
    )?;
    lua.set_named_registry_value(ERROR_MT, mt)
}

/// Builds the Lua table for `err`.
pub(crate) fn to_table(lua: &Lua, err: &Error) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.raw_set("kind", err.kind.as_str())?;
    t.raw_set("message", err.message.as_str())?;
    if let Some(hint) = &err.hint {
        t.raw_set("hint", hint.as_str())?;
    }
    if let Some(data) = &err.data {
        t.raw_set("data", crate::value::from_json(lua, data)?)?;
    }
    t.set_metatable(Some(lua.named_registry_value(ERROR_MT)?))?;
    Ok(t)
}

/// Turns anything Lua can raise into an error table.
///
/// A table with a string `kind` is already an error and only gains the metatable. Rust errors,
/// strings and other values become `{ kind, message }`; a table without a kind is kept as `data`.
pub(crate) fn normalize(lua: &Lua, value: Value) -> mlua::Result<Table> {
    match value {
        Value::Table(t) => {
            if let Value::String(kind) = t.raw_get::<Value>("kind")? {
                if t.metatable().is_none() {
                    t.set_metatable(Some(lua.named_registry_value(ERROR_MT)?))?;
                }
                if t.raw_get::<Value>("message")?.is_nil() {
                    t.raw_set("message", kind)?;
                }
                Ok(t)
            } else {
                let err = to_table(lua, &Error::new("error", "a table was raised as an error"))?;
                err.raw_set("data", t)?;
                Ok(err)
            }
        }
        Value::Error(err) => to_table(lua, &Error::from_lua(&err)),
        Value::String(s) => to_table(lua, &Error::new("lua", s.to_string_lossy())),
        Value::Nil => to_table(lua, &Error::new("lua", "nil raised as an error")),
        other => {
            let err = to_table(
                lua,
                &Error::new("lua", format!("{} raised as an error", other.type_name())),
            )?;
            err.raw_set("data", other)?;
            Ok(err)
        }
    }
}

/// Reads an error table back into Rust.
pub(crate) fn from_table(t: &Table) -> Error {
    let kind: String = t.raw_get("kind").unwrap_or_else(|_| "error".to_owned());
    let message: String = t.raw_get("message").unwrap_or_default();
    let mut err = Error::new(kind, message);
    if let Ok(Some(hint)) = t.raw_get::<Option<String>>("hint") {
        err.hint = Some(hint);
    }
    if let Ok(data) = t.raw_get::<Value>("data")
        && !data.is_nil()
    {
        err.data = crate::value::to_json(&data, crate::value::Mode::Display)
            .ok()
            .map(Box::new);
    }
    err
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_kind_then_message() {
        let err = Error::new("bad_option", "unknown option").with_hint("did you mean x?");
        assert_eq!(err.to_string(), "bad_option: unknown option");
    }

    // mlua's CallbackError holds an Arc of its own error type, which is not Send without the
    // error-send feature; the Arc here only mirrors that shape.
    #[allow(clippy::arc_with_non_send_sync)]
    #[test]
    fn a_wrapped_timon_error_comes_back_out() {
        let original = Error::new("auth_expired", "log in again").with_hint("run claude /login");
        let wrapped = mlua::Error::CallbackError {
            traceback: String::new(),
            cause: std::sync::Arc::new(original.clone().into()),
        };
        assert_eq!(Error::from_lua(&wrapped), original);
    }

    #[test]
    fn the_report_lists_hint_and_frames() {
        let failure = Failure {
            error: Error::new("undefined_global", "undefined global \"agnet\"")
                .with_hint("did you mean \"agent\"?"),
            traceback: vec!["eval:1".to_owned()],
        };
        assert_eq!(
            failure.report(),
            "timon: undefined_global: undefined global \"agnet\"\n  hint: did you mean \"agent\"?\n  at eval:1"
        );
    }
}
