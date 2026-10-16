//! `log` and `pp`: output for people, on stderr, so stdout stays data.

use mlua::{MultiValue, Value};

use crate::docs::{Func, Module};
use crate::modules::{GLOBALS_DOC, Installer};
use crate::value;

pub const DOC: Module = Module {
    name: "log",
    summary: "Messages on stderr, so stdout stays parseable.",
    functions: &[
        Func::new(
            "info",
            "log.info(...)",
            "Writes \"info: \" and the values, separated by spaces. Tables print as JSON.",
            "log.info(\"reviewing\", #prs, \"PRs\")",
        ),
        Func::new(
            "warn",
            "log.warn(...)",
            "Writes \"warn: \" and the values.",
            "log.warn(\"no tests found in\", dir)",
        ),
        Func::new(
            "error",
            "log.error(...)",
            "Writes \"error: \" and the values. It does not raise; use error() to stop.",
            "log.error(\"review failed:\", run.error.message)",
        ),
    ],
};

fn line(values: &MultiValue) -> String {
    values
        .iter()
        .map(|v| value::render(v, false).unwrap_or_else(|e| e.to_string()))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let log = installer.module(&DOC)?;
    for level in ["info", "warn", "error"] {
        let f = lua.create_function(move |_, values: MultiValue| {
            eprintln!("{level}: {}", line(&values));
            Ok(())
        })?;
        installer.function(&DOC, Some(&log), level, f)?;
    }
    installer.function(
        &GLOBALS_DOC,
        None,
        "pp",
        lua.create_function(|_, values: MultiValue| {
            for v in values.iter() {
                let shown = match v {
                    Value::String(s) => format!("{:?}", s.to_string_lossy()),
                    other => value::render(other, true).unwrap_or_else(|e| e.to_string()),
                };
                eprintln!("{shown}");
            }
            Ok(values)
        })?,
    )?;
    Ok(())
}
