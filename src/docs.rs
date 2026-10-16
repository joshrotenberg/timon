//! Declarations of every module and function.
//!
//! One source for what `help()` prints and what strict options check, so they cannot disagree.

use mlua::Value;

/// The type an option accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    String,
    Integer,
    Number,
    Boolean,
    Table,
    Function,
    Any,
}

impl Ty {
    /// The name used in signatures and errors.
    pub fn name(self) -> &'static str {
        match self {
            Ty::String => "string",
            Ty::Integer => "integer",
            Ty::Number => "number",
            Ty::Boolean => "boolean",
            Ty::Table => "table",
            Ty::Function => "function",
            Ty::Any => "any",
        }
    }

    /// Whether `value` has this type. An integral float counts as an integer.
    pub fn accepts(self, value: &Value) -> bool {
        match (self, value) {
            (Ty::Any, _) => true,
            (Ty::String, Value::String(_)) => true,
            (Ty::Integer, Value::Integer(_)) => true,
            (Ty::Integer, Value::Number(n)) => n.fract() == 0.0,
            (Ty::Number, Value::Integer(_) | Value::Number(_)) => true,
            (Ty::Boolean, Value::Boolean(_)) => true,
            (Ty::Table, Value::Table(_)) => true,
            (Ty::Function, Value::Function(_)) => true,
            _ => false,
        }
    }
}

/// One named option of a function's options table.
#[derive(Debug)]
pub struct Opt {
    pub name: &'static str,
    pub ty: Ty,
    pub required: bool,
    pub doc: &'static str,
}

impl Opt {
    /// An optional option.
    pub const fn new(name: &'static str, ty: Ty, doc: &'static str) -> Self {
        Self {
            name,
            ty,
            required: false,
            doc,
        }
    }

    /// An option that must be given.
    pub const fn required(name: &'static str, ty: Ty, doc: &'static str) -> Self {
        Self {
            name,
            ty,
            required: true,
            doc,
        }
    }
}

/// One function.
#[derive(Debug)]
pub struct Func {
    pub name: &'static str,
    /// The call shape and result, such as `json.encode(value, { pretty }) -> string`.
    pub signature: &'static str,
    pub summary: &'static str,
    pub options: &'static [Opt],
    pub example: &'static str,
}

impl Func {
    pub const fn new(
        name: &'static str,
        signature: &'static str,
        summary: &'static str,
        example: &'static str,
    ) -> Self {
        Self {
            name,
            signature,
            summary,
            options: &[],
            example,
        }
    }

    pub const fn with_options(mut self, options: &'static [Opt]) -> Self {
        self.options = options;
        self
    }
}

/// One module. Functions of the `globals` module are called without a prefix.
#[derive(Debug)]
pub struct Module {
    pub name: &'static str,
    pub summary: &'static str,
    pub functions: &'static [Func],
}

impl Module {
    /// The full name of one of this module's functions, such as `json.encode` or `try`.
    pub fn path(&self, func: &Func) -> String {
        if self.name == GLOBALS {
            func.name.to_owned()
        } else {
            format!("{}.{}", self.name, func.name)
        }
    }
}

/// Name of the pseudo-module holding global functions.
pub const GLOBALS: &str = "globals";

/// Every documented module, in the order `help()` lists them.
pub fn modules() -> &'static [&'static Module] {
    use crate::modules;
    &[
        &modules::GLOBALS_DOC,
        &modules::json::DOC,
        &modules::text::DOC,
        &modules::tbl::DOC,
        &modules::fs::FS_DOC,
        &modules::fs::PATH_DOC,
        &modules::log::DOC,
        &modules::concurrency::DOC,
        &modules::schema::DOC,
    ]
}

/// Finds a module by name.
pub fn module(name: &str) -> Option<&'static Module> {
    modules().iter().copied().find(|m| m.name == name)
}

/// Finds a function by its full name, such as `json.encode` or `try`.
pub fn function(path: &str) -> Option<(&'static Module, &'static Func)> {
    let (module_name, func_name) = path.rsplit_once('.').unwrap_or((GLOBALS, path));
    let module = module(module_name)?;
    let func = module.functions.iter().find(|f| f.name == func_name)?;
    Some((module, func))
}

/// The overview `help()` prints with no argument.
pub fn overview() -> String {
    let mut out = String::from("Modules (help(name) for one, help(name.fn) for a function):\n\n");
    let width = modules().iter().map(|m| m.name.len()).max().unwrap_or(0);
    for m in modules() {
        out.push_str(&format!("  {:width$}  {}\n", m.name, m.summary));
    }
    out.trim_end().to_owned()
}

/// What `help(module)` prints.
pub fn describe_module(module: &Module) -> String {
    let mut out = format!("{}: {}\n\n", module.name, module.summary);
    for f in module.functions {
        out.push_str(&format!("  {}\n      {}\n", f.signature, f.summary));
    }
    out.trim_end().to_owned()
}

/// What `help(fn)` prints.
pub fn describe_function(func: &Func) -> String {
    let mut out = format!("{}\n\n{}", func.signature, func.summary);
    if !func.options.is_empty() {
        out.push_str("\n\nOptions:");
        let width = func.options.iter().map(|o| o.name.len()).max().unwrap_or(0);
        for o in func.options {
            let required = if o.required { " (required)" } else { "" };
            out.push_str(&format!(
                "\n  {:width$}  {:8} {}{required}",
                o.name,
                o.ty.name(),
                o.doc
            ));
        }
    }
    if !func.example.is_empty() {
        out.push_str("\n\nExample:\n");
        for line in func.example.lines() {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out.trim_end().to_owned()
}

/// The full API reference, in Markdown: every module and function with its example. Shipped as
/// the `api` reference of the `timon` skill.
pub fn reference() -> String {
    let mut out =
        String::from("# timon API\n\nGenerated from the same declarations as `help()`.\n");
    for m in modules() {
        let title = if m.name == GLOBALS { "Globals" } else { m.name };
        out.push_str(&format!("\n## {title}\n\n{}\n", m.summary));
        for f in m.functions {
            out.push_str(&format!("\n### `{}`\n\n{}\n", f.signature, f.summary));
            if !f.options.is_empty() {
                out.push_str("\n| Option | Type | |\n|---|---|---|\n");
                for o in f.options {
                    let required = if o.required { " (required)" } else { "" };
                    out.push_str(&format!(
                        "| `{}` | {} | {}{required} |\n",
                        o.name,
                        o.ty.name(),
                        o.doc
                    ));
                }
            }
            out.push_str(&format!("\n```lua\n{}\n```\n", f.example));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_function_has_a_signature_summary_and_example() {
        for m in modules() {
            for f in m.functions {
                let path = m.path(f);
                assert!(
                    f.signature.starts_with(&path),
                    "{path}: signature {:?}",
                    f.signature
                );
                assert!(!f.summary.is_empty(), "{path}: no summary");
                assert!(!f.example.is_empty(), "{path}: no example");
            }
        }
    }

    #[test]
    fn functions_are_found_by_path() {
        assert_eq!(function("json.encode").unwrap().1.name, "encode");
        assert_eq!(function("try").unwrap().0.name, GLOBALS);
        assert!(function("json.nope").is_none());
    }
}
