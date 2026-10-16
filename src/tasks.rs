//! Tasks: `tasks.lua` (or `tasks.fnl`) in the user and project layers, and the scripts in
//! their `scripts/` directories, as one catalog.
//!
//! ```lua
//! return {
//!   test = "cargo test",                                   -- a command for sh
//!   check = { description = "The gates", depends = { "test" } },
//!   review = {
//!     description = "Review the diff against a base",
//!     depends = { "check" },
//!     options = { base = { type = "string", default = "main", doc = "the branch" } },
//!     run = function(opts, deps)
//!       return agent.ask{ prompt = "Review the diff against " .. opts.base }
//!     end,
//!   },
//! }
//! ```
//!
//! `timon tasks` lists the catalog and `timon NAME` runs a task: its dependencies first, once
//! each and depth first, then the task, as one script run (see [`crate::scripts`]). With
//! `--jobs N`, independent dependencies run at the same time. A project's tasks replace the
//! user's by name. A script on its own runs as it always has.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use mlua::{Lua, MultiValue, Table, Value};

use mlua::Function;
use serde_json::{Map, Value as Json};

use crate::docs::{Func, Module, Opt};
use crate::error::{Error, Failure};
use crate::flags::Spec;
use crate::layers::{self, Kind, Layer, Layers};
use crate::modules::agent::Progress;
use crate::modules::schema::Problem;
use crate::runtime::{INTERNAL_CHUNK, Runtime};
use crate::scripts::ScriptRun;

/// The fields of a task table.
pub const FIELDS: &[Opt] = &[
    Opt::new("description", "string", "one line for timon tasks"),
    Opt::new(
        "run",
        "function | string",
        "run(opts, deps), or a command for sh; opts.args holds the words after the task name",
    ),
    Opt::new(
        "options",
        "table<string, string | { type: string, default: any?, short: string?, doc: string? }>",
        "typed options, given as --name flags and passed in opts; a type string, or { type, default, short, doc }",
    ),
    Opt::new(
        "depends",
        "string[]",
        "tasks to run first, once each; deps.NAME is each one's result",
    ),
    Opt::new(
        "budget",
        "{ runs: integer?, usd: number? }",
        "limits on the agent runs inside the task",
    ),
    Opt::new(
        "aliases",
        "string[]",
        "other names for the task on the command line",
    ),
    Opt::new("group", "string", "a heading in timon tasks"),
    Opt::new(
        "private",
        "boolean",
        "left out of timon tasks; still runnable",
    ),
    Opt::new(
        "confirm",
        "string",
        "a question to answer before anything runs; --yes answers it",
    ),
    Opt::new(
        "platforms",
        "(\"android\" | \"dragonfly\" | \"freebsd\" | \"illumos\" | \"ios\" | \"linux\" | \"macos\" | \"netbsd\" | \"openbsd\" | \"solaris\" | \"windows\")[]",
        "the operating systems the task runs on; default: all",
    ),
];

/// Set while tasks files load. Their top level runs whenever tasks are listed, so programs and
/// agents are refused there.
pub(crate) struct Loading;

/// Raises while tasks files load, naming what was refused.
pub(crate) fn refuse_while_loading(lua: &Lua, what: &str) -> Result<(), Error> {
    if lua.app_data_ref::<Loading>().is_some() {
        return Err(
            Error::new("bad_tasks", format!("{what} cannot run while tasks load")).with_hint(
                "the top level of tasks.lua runs whenever tasks are listed; do this inside a task's run",
            ),
        );
    }
    Ok(())
}

/// Where a task is defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// An entry of a layer's `tasks.lua` or `tasks.fnl`.
    File(PathBuf),
    /// A script in a layer's `scripts/`.
    Script(PathBuf),
}

impl Source {
    pub fn path(&self) -> &Path {
        match self {
            Source::File(p) | Source::Script(p) => p,
        }
    }
}

/// One task of the catalog.
#[derive(Debug, Clone)]
pub struct Task {
    pub name: String,
    pub layer: Kind,
    pub source: Source,
    pub description: String,
    pub depends: Vec<String>,
    pub aliases: Vec<String>,
    pub group: Option<String>,
    pub private: bool,
    pub confirm: Option<String>,
    pub platforms: Vec<String>,
    /// Typed options, for a task in a tasks file.
    pub options: Spec,
    /// The layer whose task of the same name this one replaces.
    pub replaces: Option<Kind>,
}

impl Task {
    fn script(script: layers::Script) -> Self {
        Self {
            name: script.name,
            layer: script.layer,
            source: Source::Script(script.path),
            description: script.description,
            depends: Vec::new(),
            aliases: Vec::new(),
            group: None,
            private: false,
            confirm: None,
            platforms: Vec::new(),
            options: Spec::default(),
            replaces: None,
        }
    }

    /// Whether the task runs on this operating system.
    pub fn available(&self) -> bool {
        self.platforms.is_empty() || self.platforms.iter().any(|p| p == std::env::consts::OS)
    }
}

/// What a tasks-file task runs, kept in the Lua state.
struct Body {
    /// A function, or nothing for a task that only has dependencies.
    run: Value,
    /// A command's words, for a task whose `run` is a string.
    words: Option<Vec<String>>,
    budget: Value,
}

/// A command string's words. Shell syntax is refused: a command task runs without a shell,
/// so the words given to it are passed as words.
fn command_words(path: &Path, task: &str, command: &str) -> Result<Vec<String>, Error> {
    if let Some(syntax) = crate::modules::proc::shell_syntax(command) {
        return Err(bad(
            path,
            format!("the command of task \"{task}\" uses shell syntax ({syntax})"),
        )
        .with_hint("run it from a function: run = function() sh(\"...\", { shell = true }) end"));
    }
    match shell_words::split(command) {
        Ok(words) if !words.is_empty() => Ok(words),
        Ok(_) => Err(bad(
            path,
            format!("the command of task \"{task}\" is empty"),
        )),
        Err(e) => Err(bad(
            path,
            format!("the command of task \"{task}\" cannot be split into words: {e}"),
        )),
    }
}

/// Every task in reach.
pub struct Catalog {
    /// Sorted by name.
    pub tasks: Vec<Task>,
    bodies: HashMap<String, Body>,
    /// timon's own commands, which take precedence over a task's name.
    builtins: Vec<String>,
    /// An untrusted project layer, whose tasks and scripts are left out.
    untrusted: Option<Layer>,
}

fn bad(path: &Path, message: impl std::fmt::Display) -> Error {
    Error::new("bad_tasks", format!("{}: {message}", path.display()))
}

/// Task names and aliases: a letter or digit, then letters, digits, `-`, `_` or `:`.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
}

/// The layer's tasks file, if it has one.
fn tasks_file(layer: &Layer) -> Result<Option<PathBuf>, Error> {
    let lua = layer.root.join("tasks.lua");
    let fnl = layer.root.join("tasks.fnl");
    match (lua.is_file(), fnl.is_file()) {
        (true, true) => Err(Error::new(
            "bad_tasks",
            format!("{} has both tasks.lua and tasks.fnl", layer.root.display()),
        )
        .with_hint("keep one of them")),
        (true, false) => Ok(Some(lua)),
        (false, true) => Ok(Some(fnl)),
        (false, false) => Ok(None),
    }
}

impl Catalog {
    /// Loads the tasks of the trusted layers. `builtins` are timon's own command names.
    pub fn load(rt: &Runtime, layers: &Layers, builtins: &[String]) -> Result<Self, Failure> {
        let mut tasks: Vec<Task> = Vec::new();
        let mut bodies = HashMap::new();
        let mut order = layers.active();
        order.reverse();
        for layer in order {
            let mut here: Vec<(Task, Option<Body>)> = Vec::new();
            if let Some(file) = tasks_file(layer)? {
                here.extend(load_file(rt, layer, &file)?);
            }
            for script in layers::scripts_in(layer) {
                if let Some((task, _)) = here.iter().find(|(t, _)| t.name == script.name) {
                    return Err(bad(
                        task.source.path(),
                        format!("task \"{}\" is also {}", task.name, script.path.display()),
                    )
                    .with_hint("rename one of them")
                    .into());
                }
                here.push((Task::script(script), None));
            }
            for (mut task, body) in here {
                if let Some(i) = tasks.iter().position(|t| t.name == task.name) {
                    task.replaces = Some(tasks.remove(i).layer);
                    bodies.remove(&task.name);
                }
                if let Some(body) = body {
                    bodies.insert(task.name.clone(), body);
                }
                tasks.push(task);
            }
        }
        tasks.sort_by(|a, b| a.name.cmp(&b.name));
        let untrusted = layers.project.clone().filter(|p| !p.trusted);
        let catalog = Self {
            tasks,
            bodies,
            builtins: builtins.to_vec(),
            untrusted,
        };
        catalog.check()?;
        Ok(catalog)
    }

    /// The task with this name or alias.
    pub fn resolve(&self, name: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.name == name).or_else(|| {
            self.tasks
                .iter()
                .find(|t| t.aliases.iter().any(|a| a == name))
        })
    }

    /// The task with this name or alias, or an error that says what is close or untrusted.
    pub fn find(&self, name: &str) -> Result<&Task, Error> {
        if let Some(task) = self.resolve(name) {
            return Ok(task);
        }
        if let Some(project) = &self.untrusted {
            let root = project.root.parent().unwrap_or(&project.root);
            let script = layers::scripts_in(project)
                .into_iter()
                .any(|s| s.name == name);
            if script || matches!(tasks_file(project), Ok(Some(_)) | Err(_)) {
                let message = if script {
                    format!(
                        "{name} is in {}, which is not trusted",
                        project.root.display()
                    )
                } else {
                    format!(
                        "no task named \"{name}\" is loaded, and {} is not trusted",
                        project.root.display()
                    )
                };
                return Err(Error::new("untrusted", message).with_hint(format!(
                    "review it, then run: timon trust {}",
                    root.display()
                )));
            }
        }
        let names: Vec<&str> = self
            .tasks
            .iter()
            .flat_map(|t| {
                std::iter::once(t.name.as_str()).chain(t.aliases.iter().map(String::as_str))
            })
            .collect();
        let err = Error::new("not_found", format!("no task named \"{name}\""));
        Err(match crate::suggest::closest(name, names) {
            Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
            None => err.with_hint("timon tasks lists them"),
        })
    }

    /// The tasks a run of `name` runs, in order: dependencies once each, depth first in
    /// declared order, then the task.
    pub fn plan(&self, name: &str) -> Vec<&Task> {
        fn visit<'a>(
            c: &'a Catalog,
            name: &str,
            seen: &mut HashSet<String>,
            out: &mut Vec<&'a Task>,
        ) {
            if !seen.insert(name.to_owned()) {
                return;
            }
            let Some(task) = c.tasks.iter().find(|t| t.name == name) else {
                return;
            };
            for dep in &task.depends {
                visit(c, dep, seen, out);
            }
            out.push(task);
        }
        let mut out = Vec::new();
        visit(self, name, &mut HashSet::new(), &mut out);
        out
    }

    /// Whether a built-in command takes the task's name, so only `timon task NAME` runs it.
    pub fn hidden_by_builtin(&self, task: &Task) -> bool {
        self.builtins.contains(&task.name)
    }

    /// Unknown dependencies, cycles and conflicting aliases.
    fn check(&self) -> Result<(), Error> {
        let names: HashSet<&str> = self.tasks.iter().map(|t| t.name.as_str()).collect();
        let mut aliases: HashMap<&str, &str> = HashMap::new();
        for task in &self.tasks {
            let path = task.source.path();
            for dep in &task.depends {
                if names.contains(dep.as_str()) {
                    continue;
                }
                let err = bad(
                    path,
                    format!(
                        "task \"{}\" depends on \"{dep}\", which is not a task",
                        task.name
                    ),
                );
                let alias_of = self
                    .tasks
                    .iter()
                    .find(|t| t.aliases.iter().any(|a| a == dep));
                return Err(match alias_of {
                    Some(t) => err.with_hint(format!(
                        "depends takes task names; \"{dep}\" is an alias of \"{}\"",
                        t.name
                    )),
                    None => match crate::suggest::closest(dep, names.iter().copied()) {
                        Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
                        None => err,
                    },
                });
            }
            for alias in &task.aliases {
                let conflict = if !valid_name(alias) {
                    Some("is not a valid name: use letters, digits, -, _ and :".to_owned())
                } else if names.contains(alias.as_str()) {
                    Some("is also a task's name".to_owned())
                } else if self.builtins.contains(alias) {
                    Some("is a timon command".to_owned())
                } else {
                    aliases
                        .insert(alias, &task.name)
                        .map(|other| format!("is also an alias of \"{other}\""))
                };
                if let Some(why) = conflict {
                    return Err(bad(
                        path,
                        format!("alias \"{alias}\" of task \"{}\" {why}", task.name),
                    ));
                }
            }
        }
        // Depth-first search for a cycle, reporting its path.
        fn walk<'a>(
            c: &'a Catalog,
            name: &'a str,
            done: &mut HashSet<&'a str>,
            stack: &mut Vec<&'a str>,
        ) -> Option<Vec<&'a str>> {
            if let Some(at) = stack.iter().position(|n| *n == name) {
                let mut cycle = stack[at..].to_vec();
                cycle.push(name);
                return Some(cycle);
            }
            if !done.insert(name) {
                return None;
            }
            stack.push(name);
            let task = c.tasks.iter().find(|t| t.name == name)?;
            for dep in &task.depends {
                if let Some(cycle) = walk(c, dep, done, stack) {
                    return Some(cycle);
                }
            }
            stack.pop();
            None
        }
        let mut done = HashSet::new();
        for task in &self.tasks {
            if let Some(cycle) = walk(self, &task.name, &mut done, &mut Vec::new()) {
                return Err(bad(
                    task.source.path(),
                    format!("dependency cycle: {}", cycle.join(" -> ")),
                ));
            }
        }
        Ok(())
    }

    /// The catalog as JSON, leaving out private tasks unless `all`.
    pub fn to_json(&self, all: bool) -> serde_json::Value {
        let list: Vec<serde_json::Value> = self
            .tasks
            .iter()
            .filter(|t| all || !t.private)
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "layer": t.layer.name(),
                    "kind": match t.source { Source::File(_) => "task", Source::Script(_) => "script" },
                    "path": t.source.path(),
                    "description": t.description,
                    "depends": t.depends,
                    "aliases": t.aliases,
                    "group": t.group,
                    "private": t.private,
                    "confirm": t.confirm,
                    "platforms": t.platforms,
                    "options": t.options.to_json(),
                    "available": t.available(),
                    "hidden_by_builtin": self.hidden_by_builtin(t),
                    "replaces": t.replaces.map(Kind::name),
                })
            })
            .collect();
        serde_json::Value::Array(list)
    }

    /// The catalog as text: ungrouped tasks, then each group under its name.
    /// With `all`, private tasks too, and `builtins` (name, description) under `timon`.
    pub fn listing(&self, all: bool, builtins: &[(String, String)]) -> String {
        let shown: Vec<&Task> = self.tasks.iter().filter(|t| all || !t.private).collect();
        let builtins = if all { builtins } else { &[] };
        let width = shown
            .iter()
            .map(|t| t.name.len())
            .chain(builtins.iter().map(|(n, _)| n.len()))
            .max()
            .unwrap_or(0);
        let layer_width = shown
            .iter()
            .map(|t| t.layer.name().len())
            .chain(builtins.iter().map(|_| Kind::Builtin.name().len()))
            .max()
            .unwrap_or(0);
        let line = |t: &Task, indent: &str| {
            let mut notes = Vec::new();
            if !t.aliases.is_empty() {
                notes.push(format!("(also {})", t.aliases.join(", ")));
            }
            if self.hidden_by_builtin(t) {
                notes.push(format!(
                    "[a timon command has this name: timon task {}]",
                    t.name
                ));
            }
            if let Some(layer) = t.replaces {
                notes.push(format!("[replaces the {} task]", layer.name()));
            }
            if !t.available() {
                notes.push(format!("[unavailable: {} only]", t.platforms.join(", ")));
            }
            if t.private {
                notes.push("[private]".to_owned());
            }
            let mut text = format!(
                "{indent}{:width$}  {:layer_width$}  {}",
                t.name,
                t.layer.name(),
                t.description
            );
            for note in notes {
                text.push(' ');
                text.push_str(&note);
            }
            format!("{}\n", text.trim_end())
        };
        let mut out = String::new();
        for t in shown.iter().filter(|t| t.group.is_none()) {
            out.push_str(&line(t, ""));
        }
        let mut groups: Vec<&str> = shown.iter().filter_map(|t| t.group.as_deref()).collect();
        groups.sort();
        groups.dedup();
        for group in groups {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(group);
            out.push('\n');
            for t in shown.iter().filter(|t| t.group.as_deref() == Some(group)) {
                out.push_str(&line(t, "  "));
            }
        }
        if !builtins.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("timon\n");
            for (name, description) in builtins {
                let text = format!(
                    "  {name:width$}  {:layer_width$}  {description}",
                    Kind::Builtin.name()
                );
                out.push_str(text.trim_end());
                out.push('\n');
            }
        }
        out
    }

    /// A note for stderr when an untrusted project layer's tasks are left out.
    pub fn untrusted_note(&self) -> Option<String> {
        let project = self.untrusted.as_ref()?;
        let has_tasks = !layers::scripts_in(project).is_empty()
            || matches!(tasks_file(project), Ok(Some(_)) | Err(_));
        has_tasks.then(|| {
            format!(
                "timon: {} is not trusted, so its tasks are not listed",
                project.root.display()
            )
        })
    }
}

/// Loads one tasks file. Its top level runs with programs and agents refused.
fn load_file(
    rt: &Runtime,
    layer: &Layer,
    path: &Path,
) -> Result<Vec<(Task, Option<Body>)>, Failure> {
    let lua = rt.lua();
    let source = std::fs::read_to_string(path).map_err(|e| bad(path, e))?;
    let chunk_name = format!("@{}", path.display());
    let lua_source = if path.extension().is_some_and(|e| e == "fnl") {
        crate::fennel::compile(lua, &crate::fennel::strip_shebang(&source), &chunk_name)?
    } else {
        source
    };
    let chunk = rt.compile(&lua_source, &chunk_name, false)?;
    lua.set_app_data(Loading);
    let returned = rt.call_sync(chunk);
    lua.remove_app_data::<Loading>();
    let returned = returned?;
    let Value::Table(table) = returned else {
        return Err(bad(
            path,
            format!("must return a table of tasks, got {}", returned.type_name()),
        )
        .with_hint("return { NAME = { description = ..., run = function(opts, deps) ... end } }")
        .into());
    };
    let shape = crate::options::object(FIELDS).map_err(|e| Error::new("internal", e))?;
    let mut found = Vec::new();
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair.map_err(|e| Failure::from(Error::from_lua(&e)))?;
        let Value::String(key) = key else {
            return Err(bad(
                path,
                format!(
                    "task names are strings, got a key of type {}",
                    key.type_name()
                ),
            )
            .with_hint("return a table of named tasks: { NAME = ... }")
            .into());
        };
        let name = key.to_string_lossy();
        if !valid_name(&name) {
            return Err(bad(
                path,
                format!("\"{name}\" is not a task name: use letters, digits, -, _ and :"),
            )
            .into());
        }
        let mut task = Task {
            name: name.clone(),
            layer: layer.kind,
            source: Source::File(path.to_path_buf()),
            description: String::new(),
            depends: Vec::new(),
            aliases: Vec::new(),
            group: None,
            private: false,
            confirm: None,
            platforms: Vec::new(),
            options: Spec::default(),
            replaces: None,
        };
        let body = match value {
            Value::String(s) => {
                task.description = s.to_string_lossy();
                Body {
                    run: Value::Nil,
                    words: Some(command_words(path, &name, &task.description)?),
                    budget: Value::Nil,
                }
            }
            Value::Function(f) => Body {
                run: Value::Function(f),
                words: None,
                budget: Value::Nil,
            },
            Value::Table(t) => {
                if let Some(e) = shape
                    .check_value(&Value::Table(t.clone()))
                    .into_iter()
                    .next()
                {
                    return Err(field_error(path, &name, &e.path, e.problem).into());
                }
                read_fields(&mut task, &t).map_err(|e| Failure::from(Error::from_lua(&e)))?;
                if let Some(options) = t
                    .raw_get::<Option<Table>>("options")
                    .map_err(|e| Failure::from(Error::from_lua(&e)))?
                {
                    task.options = Spec::from_lua(&options)
                        .map_err(|m| bad(path, format!("task \"{name}\": {m}")))?;
                }
                let run: Value = t
                    .raw_get("run")
                    .map_err(|e| Failure::from(Error::from_lua(&e)))?;
                if run.is_nil() && task.depends.is_empty() {
                    return Err(bad(path, format!("task \"{name}\" has nothing to run"))
                        .with_hint("give it run, depends, or both")
                        .into());
                }
                let words = match &run {
                    Value::String(s) => {
                        let command = s.to_string_lossy();
                        if task.description.is_empty() {
                            task.description = command.clone();
                        }
                        Some(command_words(path, &name, &command)?)
                    }
                    _ => None,
                };
                Body {
                    run: if words.is_some() { Value::Nil } else { run },
                    words,
                    budget: t
                        .raw_get("budget")
                        .map_err(|e| Failure::from(Error::from_lua(&e)))?,
                }
            }
            other => {
                return Err(bad(
                    path,
                    format!(
                        "task \"{name}\" must be a table, a command string or a function, got {}",
                        other.type_name()
                    ),
                )
                .into());
            }
        };
        found.push((task, Some(body)));
    }
    found.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    Ok(found)
}

fn read_fields(task: &mut Task, t: &Table) -> mlua::Result<()> {
    task.description = t
        .raw_get::<Option<String>>("description")?
        .unwrap_or_default();
    task.depends = t
        .raw_get::<Option<Vec<String>>>("depends")?
        .unwrap_or_default();
    task.aliases = t
        .raw_get::<Option<Vec<String>>>("aliases")?
        .unwrap_or_default();
    task.group = t.raw_get("group")?;
    task.private = t.raw_get::<Option<bool>>("private")?.unwrap_or(false);
    task.confirm = t.raw_get("confirm")?;
    task.platforms = t
        .raw_get::<Option<Vec<String>>>("platforms")?
        .unwrap_or_default();
    Ok(())
}

fn field_error(path: &Path, task: &str, at: &str, problem: Problem) -> Error {
    let fields: Vec<&str> = FIELDS.iter().map(|o| o.name).collect();
    match problem {
        Problem::Unknown { key, close } => {
            let err = bad(path, format!("unknown field \"{key}\" in task \"{task}\""));
            match close {
                Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
                None => err.with_hint(format!("a task takes {}", fields.join(", "))),
            }
        }
        Problem::Wrong { expected, got } => bad(
            path,
            format!("field \"{at}\" of task \"{task}\" must be {expected}, got {got}"),
        ),
        Problem::NotOneOf { values, got, close } => {
            let list: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
            let close = close.or_else(|| {
                let bare = got.trim_matches('"');
                values
                    .iter()
                    .find(|v| v.eq_ignore_ascii_case(bare))
                    .cloned()
            });
            let err = bad(
                path,
                format!(
                    "field \"{at}\" of task \"{task}\" must be one of {}, got {got}",
                    list.join(", ")
                ),
            );
            match close {
                Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
                None => err,
            }
        }
        Problem::Missing { name } => bad(path, format!("task \"{task}\" needs \"{name}\"")),
    }
}

/// Asks each `confirm` question of the plan on stderr, unless `yes`.
fn confirm(plan: &[&Task], yes: bool) -> Result<(), Error> {
    for task in plan {
        let Some(question) = &task.confirm else {
            continue;
        };
        if yes {
            continue;
        }
        if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
            return Err(Error::new(
                "confirm",
                format!("task \"{}\" asks: {question}", task.name),
            )
            .with_hint(format!(
                "answer it with timon --yes {}",
                plan.last().map_or("", |t| t.name.as_str())
            )));
        }
        eprint!("{question} [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            return Err(Error::new("confirm", format!("not confirmed: {question}")));
        }
    }
    Ok(())
}

/// Runs steps: each level in order, the steps of a level at most `jobs` at a time, each with
/// its dependencies' results. Returns the last step's values.
const TASK_DRIVER: &str = r#"
local started, finished, failed, xrun, jobs = ...
local pack, unpack, rawset, ipairs, error = table.pack, table.unpack, rawset, ipairs, error
local results = {}
local function run_step(step)
  started(step.name)
  local deps = {}
  for _, d in ipairs(step.depends) do deps[d] = results[d] end
  local function body()
    if step.script then
      rawset(_G, "arg", { [0] = step.script, unpack(step.opts.args) })
      return step.run(unpack(step.opts.args))
    elseif step.words then
      local cmd = {}
      for _, w in ipairs(step.words) do cmd[#cmd + 1] = w end
      for _, w in ipairs(step.opts.args) do cmd[#cmd + 1] = w end
      cmd.prefix = step.prefix
      sh(cmd)
    elseif step.run ~= nil then
      return step.run(step.opts, deps)
    end
  end
  local function bounded()
    if step.budget ~= nil then return scope(body, { budget = step.budget }) end
    return body()
  end
  local r = pack(xrun(bounded))
  if not r[1] then
    failed(step.name, r[2])
    error(r[2], 0)
  end
  results[step.name] = r[2]
  finished(step.name)
  return pack(unpack(r, 2, r.n))
end
return function(steps)
  local levels, out = {}, pack()
  for _, step in ipairs(steps) do
    local level = levels[step.level]
    if not level then
      level = {}
      levels[step.level] = level
    end
    level[#level + 1] = step
  end
  for i = 1, #levels do
    local level = levels[i]
    if jobs > 1 and #level > 1 then
      par.map(level, run_step, { limit = jobs })
    else
      for _, step in ipairs(level) do out = run_step(step) end
    end
  end
  return unpack(out, 1, out.n)
end
"#;

/// How a task runs, from timon's own flags.
#[derive(Debug, Clone, Copy)]
pub struct How {
    /// Answer every `confirm` question.
    pub yes: bool,
    /// Most tasks of one level running at once.
    pub jobs: usize,
    /// Return the plan instead of running it.
    pub dry_run: bool,
}

impl Default for How {
    fn default() -> Self {
        Self {
            yes: false,
            jobs: 1,
            dry_run: false,
        }
    }
}

/// The plan for `name` without the tasks already `done` (the task itself always stays), each
/// with its level: 1 for a task whose dependencies are all done, else one more than its
/// highest dependency's.
fn levels<'a>(catalog: &'a Catalog, name: &str, done: &HashSet<String>) -> Vec<(&'a Task, usize)> {
    let mut level: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::new();
    for task in catalog.plan(name) {
        if task.name != name && done.contains(&task.name) {
            continue;
        }
        let l = 1 + task
            .depends
            .iter()
            .filter_map(|d| level.get(d.as_str()))
            .max()
            .copied()
            .unwrap_or(0);
        level.insert(&task.name, l);
        out.push((task, l));
    }
    out
}

/// What a step runs, for a dry run.
fn runs(catalog: &Catalog, task: &Task) -> Json {
    match (&task.source, catalog.bodies.get(&task.name)) {
        (Source::Script(path), _) => Json::String(format!("script {}", path.display())),
        (Source::File(_), Some(Body { words: Some(w), .. })) => Json::String(shell_words::join(w)),
        (
            Source::File(_),
            Some(Body {
                run: Value::Function(_),
                ..
            }),
        ) => Json::String("function".into()),
        _ => Json::Null,
    }
}

/// The options of a step: the target's, or its own defaults for a dependency.
fn step_opts(
    task: &Task,
    target: &str,
    given: &Map<String, Json>,
) -> Result<Map<String, Json>, Error> {
    let mut opts = if task.name == target {
        given.clone()
    } else {
        task.options.defaults().map_err(|e| {
            Error::new(
                "bad_tasks",
                format!(
                    "task \"{}\" is a dependency, so it runs with its defaults, but {}",
                    task.name, e.message
                ),
            )
            .with_hint("give the option a default, or call the task with task.run(name, opts)")
        })?
    };
    opts.entry("args")
        .or_insert_with(|| Json::Array(Vec::new()));
    Ok(opts)
}

/// The steps of a plan, as the driver takes them.
fn steps(
    lua: &Lua,
    catalog: &Catalog,
    planned: &[(&Task, usize)],
    target: &str,
    given: &Map<String, Json>,
    jobs: usize,
) -> Result<Table, Failure> {
    let lua_err = |e: mlua::Error| Failure::from(Error::from_lua(&e));
    let steps = lua.create_table().map_err(lua_err)?;
    for (i, (task, level)) in planned.iter().enumerate() {
        let step = lua.create_table().map_err(lua_err)?;
        step.raw_set("name", task.name.as_str()).map_err(lua_err)?;
        step.raw_set("depends", task.depends.clone())
            .map_err(lua_err)?;
        step.raw_set("level", *level).map_err(lua_err)?;
        let opts = step_opts(task, target, given)?;
        step.raw_set(
            "opts",
            crate::modules::json::decode(lua, &Json::Object(opts)).map_err(lua_err)?,
        )
        .map_err(lua_err)?;
        let shared = planned.iter().filter(|(_, l)| l == level).count() > 1;
        if jobs > 1 && shared {
            step.raw_set("prefix", format!("[{}] ", task.name))
                .map_err(lua_err)?;
        }
        match &task.source {
            Source::File(_) => {
                if let Some(body) = catalog.bodies.get(&task.name) {
                    step.raw_set("run", body.run.clone()).map_err(lua_err)?;
                    step.raw_set("budget", body.budget.clone())
                        .map_err(lua_err)?;
                    if let Some(words) = &body.words {
                        step.raw_set("words", words.clone()).map_err(lua_err)?;
                    }
                }
            }
            Source::Script(path) => {
                let source = std::fs::read_to_string(path).map_err(|e| bad(path, e))?;
                let f = crate::runtime::script_function_in(lua, path, &source, false)?;
                step.raw_set("run", f).map_err(lua_err)?;
                step.raw_set("script", path.display().to_string())
                    .map_err(lua_err)?;
            }
        }
        steps.raw_set(i + 1, step).map_err(lua_err)?;
    }
    Ok(steps)
}

/// A task run in progress: what has run, what is running, and the driver, for `task.run`.
struct Invocation {
    catalog: Rc<Catalog>,
    record: Option<Rc<ScriptRun>>,
    running: RefCell<HashMap<String, Instant>>,
    done: RefCell<HashSet<String>>,
    announce: bool,
    jobs: usize,
    run_steps: RefCell<Option<Function>>,
}

impl Invocation {
    fn settle(&self, name: &str, status: &str) {
        let started = self.running.borrow_mut().remove(name);
        if let (Some(record), Some(at)) = (&self.record, started) {
            record.task(name, status, at.elapsed().as_millis() as u64);
        }
    }
}

/// The help text of a task.
pub fn help(catalog: &Catalog, task: &Task) -> String {
    let mut out = String::new();
    if !task.description.is_empty() {
        out.push_str(&task.description);
        out.push_str("\n\n");
    }
    match &task.source {
        Source::Script(_) => out.push_str(&format!(
            "Usage: timon {} [ARGS]...\n\nA script: its words are in arg and ...\n",
            task.name
        )),
        Source::File(_) => {
            out.push_str(&format!(
                "Usage: timon {} [OPTIONS] [WORDS]...\n\n",
                task.name
            ));
            out.push_str(&task.options.help());
            out.push_str("\nThe other words, and every word after --, are in opts.args.\n");
        }
    }
    let mut notes = Vec::new();
    if !task.depends.is_empty() {
        notes.push(format!("Runs after: {}", task.depends.join(", ")));
    }
    if !task.aliases.is_empty() {
        notes.push(format!("Also: {}", task.aliases.join(", ")));
    }
    if let Some(q) = &task.confirm {
        notes.push(format!("Asks: {q}"));
    }
    if !task.platforms.is_empty() {
        notes.push(format!("Runs on: {}", task.platforms.join(", ")));
    }
    if catalog.hidden_by_builtin(task) {
        notes.push(format!(
            "A timon command has this name: run it as timon task {}",
            task.name
        ));
    }
    notes.push(format!(
        "From: {} ({})",
        task.layer.name(),
        task.source.path().display()
    ));
    out.push('\n');
    for n in notes {
        out.push_str(&n);
        out.push('\n');
    }
    out
}

/// Runs the task `name` after its dependencies, with `words` for the task itself. A script
/// on its own runs as `timon run` always has.
pub async fn run(
    rt: &Runtime,
    catalog: Rc<Catalog>,
    name: &str,
    words: &[String],
    how: How,
) -> Result<MultiValue, Failure> {
    let lua = rt.lua();
    let lua_err = |e: mlua::Error| Failure::from(Error::from_lua(&e));
    let task = catalog.find(name)?;
    let given = match &task.source {
        Source::Script(path) if !how.dry_run => {
            return rt
                .run_named_script(path, words, &task.name, Some(task.layer.name()))
                .await;
        }
        Source::Script(_) => {
            let mut m = Map::new();
            m.insert("args".into(), Json::from(words.to_vec()));
            m
        }
        Source::File(_) => {
            let parsed = task.options.parse(words).map_err(|e| {
                Error::new(e.kind, format!("task \"{}\": {}", task.name, e.message)).with_hint_opt(
                    e.hint
                        .or_else(|| Some(format!("timon {} --help lists its options", task.name))),
                )
            })?;
            if parsed.help {
                let text = help(&catalog, task);
                return Ok(MultiValue::from_iter([Value::String(
                    lua.create_string(text.trim_end()).map_err(lua_err)?,
                )]));
            }
            let mut values = parsed.values;
            values.insert("args".into(), Json::from(parsed.rest));
            values
        }
    };
    let name = task.name.clone();
    run_given(rt, catalog, &name, given, how).await
}

/// The `confirm` questions of the tasks a run of `name` runs.
pub fn questions(catalog: &Catalog, name: &str) -> Vec<String> {
    catalog
        .plan(name)
        .iter()
        .filter_map(|t| t.confirm.clone())
        .collect()
}

/// Checks options given as JSON, as an MCP call gives them, against the task's declarations,
/// and fills in the defaults. `args`, if given, is a list of words.
pub fn given_from_json(rt: &Runtime, task: &Task, args: &Json) -> Result<Map<String, Json>, Error> {
    let lua = rt.lua();
    let table = match crate::modules::json::decode(lua, args).map_err(|e| Error::from_lua(&e))? {
        Value::Table(t) => t,
        _ => lua.create_table().map_err(|e| Error::from_lua(&e))?,
    };
    let mut given = match &task.source {
        Source::Script(_) => {
            let mut m = Map::new();
            if let Some(args) = args.get("args") {
                m.insert("args".into(), args.clone());
            }
            m
        }
        Source::File(_) => task.options.check(&table, &format!("task {}", task.name))?,
    };
    given
        .entry("args")
        .or_insert_with(|| Json::Array(Vec::new()));
    Ok(given)
}

/// Runs the task `name` with options already parsed: `given` holds them and `args`.
pub async fn run_given(
    rt: &Runtime,
    catalog: Rc<Catalog>,
    name: &str,
    given: Map<String, Json>,
    how: How,
) -> Result<MultiValue, Failure> {
    let lua = rt.lua();
    let lua_err = |e: mlua::Error| Failure::from(Error::from_lua(&e));
    let task = catalog.find(name)?;
    if let (Source::Script(path), false) = (&task.source, how.dry_run) {
        let words: Vec<String> = match given.get("args") {
            Some(Json::Array(a)) => a
                .iter()
                .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        return rt
            .run_named_script(path, &words, &task.name, Some(task.layer.name()))
            .await;
    }
    let planned = levels(&catalog, &task.name, &HashSet::new());
    if how.dry_run {
        let plan: Vec<Json> = planned
            .iter()
            .map(|(t, level)| {
                serde_json::json!({
                    "name": t.name,
                    "level": level,
                    "runs": runs(&catalog, t),
                    "opts": step_opts(t, &task.name, &given).ok(),
                    "confirm": t.confirm,
                    "available": t.available(),
                })
            })
            .collect();
        let out = serde_json::json!({ "task": task.name, "jobs": how.jobs, "plan": plan });
        return Ok(MultiValue::from_iter([crate::modules::json::decode(
            lua, &out,
        )
        .map_err(lua_err)?]));
    }
    if let Some((t, _)) = planned.iter().find(|(t, _)| !t.available()) {
        return Err(Error::new(
            "platform",
            format!(
                "task \"{}\" runs only on {}",
                t.name,
                t.platforms.join(", ")
            ),
        )
        .with_hint(format!("this system is {}", std::env::consts::OS))
        .into());
    }
    let tasks: Vec<&Task> = planned.iter().map(|(t, _)| *t).collect();
    confirm(&tasks, how.yes)?;
    let built = steps(lua, &catalog, &planned, &task.name, &given, how.jobs)?;
    let file = task.source.path();
    let source = std::fs::read_to_string(file).map_err(|e| bad(file, e))?;
    let names: Vec<&str> = tasks.iter().map(|t| t.name.as_str()).collect();
    let args: Vec<String> = match given.get("args") {
        Some(Json::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    let record = rt.begin_script(
        file,
        &args,
        &task.name,
        Some(task.layer.name()),
        &source,
        Some(serde_json::json!({ "kind": "task", "plan": names, "jobs": how.jobs })),
    )?;
    let progress = lua
        .app_data_ref::<Progress>()
        .map(|p| *p)
        .unwrap_or_default();
    let invocation = Rc::new(Invocation {
        catalog: catalog.clone(),
        record: record.clone(),
        running: RefCell::default(),
        done: RefCell::default(),
        announce: planned.len() > 1 && matches!(progress, Progress::Text),
        jobs: how.jobs,
        run_steps: RefCell::new(None),
    });
    let started = {
        let inv = invocation.clone();
        lua.create_function(move |_, name: String| {
            if inv.announce {
                eprintln!("timon: task {name}");
            }
            inv.running.borrow_mut().insert(name, Instant::now());
            Ok(())
        })
        .map_err(lua_err)?
    };
    let finished = {
        let inv = invocation.clone();
        lua.create_function(move |_, name: String| {
            inv.settle(&name, "ok");
            inv.done.borrow_mut().insert(name);
            Ok(())
        })
        .map_err(lua_err)?
    };
    let failed = {
        let inv = invocation.clone();
        lua.create_function(move |_, (name, err): (String, Value)| {
            let kind = match &err {
                Value::Table(t) => t.raw_get::<Option<String>>("kind")?,
                _ => None,
            };
            let status = if kind.as_deref() == Some("cancelled") {
                "cancelled"
            } else {
                "failed"
            };
            inv.settle(&name, status);
            Ok(())
        })
        .map_err(lua_err)?
    };
    let xrun: Function = lua
        .named_registry_value(crate::runtime::RUN)
        .map_err(lua_err)?;
    let run_steps: Function = lua
        .load(TASK_DRIVER)
        .set_name(INTERNAL_CHUNK)
        .call((started, finished, failed, xrun, how.jobs))
        .map_err(lua_err)?;
    *invocation.run_steps.borrow_mut() = Some(run_steps.clone());
    lua.set_app_data(invocation.clone());
    let result = rt
        .call(run_steps, MultiValue::from_iter([Value::Table(built)]))
        .await;
    lua.remove_app_data::<Rc<Invocation>>();
    let left: Vec<String> = invocation.running.borrow().keys().cloned().collect();
    for name in left {
        invocation.settle(&name, "cancelled");
    }
    rt.finish_script(record, &result);
    result
}

pub const TASK_DOC: Module = Module {
    name: "task",
    summary: "Tasks from inside a task: run another one with its options.",
    functions: &[Func::new(
        "run",
        "task.run(name, opts) -> ...",
        "Runs a task from inside a task's run: its dependencies that have not run in this invocation, then the task, with opts checked against its options and their defaults filled in (opts.args for its words). Returns what the task returns. It does not ask confirm questions.",
        "-- in tasks.lua\nrelease = {\n  run = function(opts, deps)\n    task.run(\"check\")\n    return task.run(\"publish\", { channel = \"stable\" })\n  end,\n}",
    )],
};

/// Builds the steps of `task.run(name, opts)` and returns them with the driver.
fn build(lua: &Lua, (name, opts): (String, Option<Table>)) -> mlua::Result<(Table, Function)> {
    let inv = lua
        .app_data_ref::<Rc<Invocation>>()
        .map(|i| i.clone())
        .ok_or_else(|| {
            Error::new("bad_call", "task.run works inside a task's run")
                .with_hint("run a task from the shell with timon NAME")
        })?;
    let task = inv.catalog.find(&name)?;
    let path = format!("task.run(\"{}\")", task.name);
    let given = match opts {
        Some(t) => task.options.check(&t, &path)?,
        None => task.options.check(&lua.create_table()?, &path)?,
    };
    let planned = levels(&inv.catalog, &task.name, &inv.done.borrow());
    if let Some((t, _)) = planned.iter().find(|(t, _)| !t.available()) {
        return Err(Error::new(
            "platform",
            format!(
                "task \"{}\" runs only on {}",
                t.name,
                t.platforms.join(", ")
            ),
        )
        .into());
    }
    let built = steps(lua, &inv.catalog, &planned, &task.name, &given, inv.jobs)
        .map_err(|f| mlua::Error::from(f.error))?;
    let run_steps = inv
        .run_steps
        .borrow()
        .clone()
        .ok_or_else(|| Error::new("internal", "the task driver is gone"))?;
    Ok((built, run_steps))
}

const TASK_RUN: &str = r#"
local build = ...
return function(name, opts)
  local steps, run_steps = build(name, opts)
  return run_steps(steps)
end
"#;

pub(crate) fn install(installer: &crate::modules::Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let table = installer.module(&TASK_DOC)?;
    let build = installer.wrap(lua.create_function(build)?)?;
    let f: Function = lua.load(TASK_RUN).set_name(INTERNAL_CHUNK).call(build)?;
    installer.lua_function(&TASK_DOC, Some(&table), "run", f)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_words_that_cannot_be_flags_or_paths() {
        for ok in ["check", "lint:fix", "build_2", "9lives"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "-x", "a b", "x.lua", "dir/x", "_x"] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn the_task_fields_parse() {
        crate::options::object(FIELDS).unwrap();
    }
}
