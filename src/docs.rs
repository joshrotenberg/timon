//! Declarations of every module and function.
//!
//! One source for what `help()` prints and what strict options check, so they cannot disagree.

/// One named option of a function's options table.
#[derive(Debug)]
pub struct Opt {
    pub name: &'static str,
    /// The type, in the LuaLS syntax of [`crate::types`], such as `"read" | "write"`.
    pub ty: &'static str,
    pub required: bool,
    pub doc: &'static str,
}

impl Opt {
    /// An optional option.
    pub const fn new(name: &'static str, ty: &'static str, doc: &'static str) -> Self {
        Self {
            name,
            ty,
            required: false,
            doc,
        }
    }

    /// An option that must be given.
    pub const fn required(name: &'static str, ty: &'static str, doc: &'static str) -> Self {
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
        &modules::sqlite::DOC,
        &modules::fs::FS_DOC,
        &modules::fs::PATH_DOC,
        &modules::git::DOC,
        &modules::http::DOC,
        &modules::gh::DOC,
        &modules::log::DOC,
        &modules::concurrency::DOC,
        &modules::proc::SH_DOC,
        &modules::proc::PROC_DOC,
        &modules::cli::CLI_DOC,
        &modules::flow::DOC,
        &modules::mcp::DOC,
        &modules::formats::TOML_DOC,
        &modules::formats::YAML_DOC,
        &modules::formats::CSV_DOC,
        &modules::formats::MARKDOWN_DOC,
        &modules::formats::TERM_DOC,
        &crate::tasks::TASK_DOC,
        &modules::schema::DOC,
        &modules::agent::DOC,
        &modules::sessions::DOC,
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
                "\n  {:width$}  {}{required}\n  {:width$}    {}",
                o.name, o.ty, "", o.doc
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
                        "| `{}` | `{}` | {}{required} |\n",
                        o.name,
                        o.ty.replace('|', "\\|"),
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
    fn every_option_type_parses() {
        for m in modules() {
            for f in m.functions {
                for o in f.options {
                    if let Err(e) = crate::types::parse(o.ty) {
                        panic!("{}: option {}: {e}", m.path(f), o.name);
                    }
                }
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
