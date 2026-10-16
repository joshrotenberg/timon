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
//!     run = function(opts, deps)
//!       return agent.ask{ prompt = "Review the diff against " .. (opts.args[1] or "main") }
//!     end,
//!   },
//! }
//! ```
//!
//! `timon tasks` lists the catalog and `timon NAME` runs a task: its dependencies first, once
//! each and depth first, then the task, as one script run (see [`crate::scripts`]). A project's
//! tasks replace the user's by name. A script on its own runs as it always has.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use mlua::{Lua, MultiValue, Table, Value};

use crate::docs::Opt;
use crate::error::{Error, Failure};
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
    run: Value,
    budget: Value,
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
                    "available": t.available(),
                    "hidden_by_builtin": self.hidden_by_builtin(t),
                    "replaces": t.replaces.map(Kind::name),
                })
            })
            .collect();
        serde_json::Value::Array(list)
    }

    /// The catalog as text: ungrouped tasks, then each group under its name.
    pub fn listing(&self, all: bool) -> String {
        let shown: Vec<&Task> = self.tasks.iter().filter(|t| all || !t.private).collect();
        let width = shown.iter().map(|t| t.name.len()).max().unwrap_or(0);
        let layer_width = shown
            .iter()
            .map(|t| t.layer.name().len())
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
            replaces: None,
        };
        let body = match value {
            Value::String(s) => {
                task.description = s.to_string_lossy();
                Body {
                    run: Value::String(s),
                    budget: Value::Nil,
                }
            }
            Value::Function(f) => Body {
                run: Value::Function(f),
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
                let run: Value = t
                    .raw_get("run")
                    .map_err(|e| Failure::from(Error::from_lua(&e)))?;
                if run.is_nil() && task.depends.is_empty() {
                    return Err(bad(path, format!("task \"{name}\" has nothing to run"))
                        .with_hint("give it run, depends, or both")
                        .into());
                }
                if task.description.is_empty()
                    && let Value::String(s) = &run
                {
                    task.description = s.to_string_lossy();
                }
                Body {
                    run,
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

/// The words for a task: words before `--` that look like options are refused until tasks
/// declare options; `--` itself is dropped and everything after it is taken as it is.
pub fn task_args(name: &str, words: &[String]) -> Result<Vec<String>, Error> {
    let mut args = Vec::new();
    let mut rest = false;
    for word in words {
        if rest {
            args.push(word.clone());
        } else if word == "--" {
            rest = true;
        } else if word.starts_with('-') && word.len() > 1 {
            return Err(Error::new(
                "usage",
                format!("task \"{name}\" takes no options, got \"{word}\""),
            )
            .with_hint(format!(
                "pass words for the task after --: timon {name} -- {word}; timon's own flags go before the task name"
            )));
        } else {
            args.push(word.clone());
        }
    }
    Ok(args)
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

/// Runs the plan: each task with its dependencies' results, the last with the words.
const TASK_DRIVER: &str = r#"
local steps, started, finished, join = ...
local pack, unpack, type, rawset = table.pack, table.unpack, type, rawset
return function(args)
  local results, out = {}, pack()
  for i, step in ipairs(steps) do
    started(step.name)
    local deps = {}
    for _, d in ipairs(step.depends) do deps[d] = results[d] end
    local opts = { args = i == #steps and args or {} }
    local run = step.run
    local function body()
      if step.script then
        rawset(_G, "arg", { [0] = step.script })
        return run()
      elseif type(run) == "string" then
        sh(#opts.args > 0 and run .. " " .. join(opts.args) or run)
      elseif run ~= nil then
        return run(opts, deps)
      end
    end
    if step.budget ~= nil then
      out = pack(scope(body, { budget = step.budget }))
    else
      out = pack(body())
    end
    results[step.name] = out[1]
    finished(step.name)
  end
  return unpack(out, 1, out.n)
end
"#;

/// Which task of a plan is running, for the record.
struct Progressing {
    record: Option<Rc<ScriptRun>>,
    current: RefCell<Option<(String, Instant)>>,
    announce: bool,
}

/// Runs the task `name` after its dependencies, with `words` for the task itself. A script
/// on its own runs as `timon run` always has.
pub async fn run(
    rt: &Runtime,
    catalog: &Catalog,
    name: &str,
    words: &[String],
    yes: bool,
) -> Result<MultiValue, Failure> {
    let task = catalog.find(name)?;
    let Source::File(file) = &task.source else {
        let path = task.source.path();
        return rt
            .run_named_script(path, words, &task.name, Some(task.layer.name()))
            .await;
    };
    let args = task_args(&task.name, words)?;
    let plan = catalog.plan(&task.name);
    if let Some(t) = plan.iter().find(|t| !t.available()) {
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
    confirm(&plan, yes)?;
    let lua = rt.lua();
    let lua_err = |e: mlua::Error| Failure::from(Error::from_lua(&e));
    let steps = lua.create_table().map_err(lua_err)?;
    for (i, t) in plan.iter().enumerate() {
        let step = lua.create_table().map_err(lua_err)?;
        step.raw_set("name", t.name.as_str()).map_err(lua_err)?;
        step.raw_set("depends", t.depends.clone())
            .map_err(lua_err)?;
        match &t.source {
            Source::File(_) => {
                if let Some(body) = catalog.bodies.get(&t.name) {
                    step.raw_set("run", body.run.clone()).map_err(lua_err)?;
                    step.raw_set("budget", body.budget.clone())
                        .map_err(lua_err)?;
                }
            }
            Source::Script(path) => {
                let source = std::fs::read_to_string(path).map_err(|e| bad(path, e))?;
                let f = rt.script_function(path, &source, false)?;
                step.raw_set("run", f).map_err(lua_err)?;
                step.raw_set("script", path.display().to_string())
                    .map_err(lua_err)?;
            }
        }
        steps.raw_set(i + 1, step).map_err(lua_err)?;
    }
    let source = std::fs::read_to_string(file).map_err(|e| bad(file, e))?;
    let names: Vec<&str> = plan.iter().map(|t| t.name.as_str()).collect();
    let record = rt.begin_script(
        file,
        &args,
        &task.name,
        Some(task.layer.name()),
        &source,
        Some(serde_json::json!({ "kind": "task", "plan": names })),
    )?;
    let progress = lua
        .app_data_ref::<Progress>()
        .map(|p| *p)
        .unwrap_or_default();
    let state = Rc::new(Progressing {
        record: record.clone(),
        current: RefCell::new(None),
        announce: plan.len() > 1 && matches!(progress, Progress::Text),
    });
    let started = {
        let state = state.clone();
        lua.create_function(move |_, name: String| {
            if state.announce {
                eprintln!("timon: task {name}");
            }
            *state.current.borrow_mut() = Some((name, Instant::now()));
            Ok(())
        })
        .map_err(lua_err)?
    };
    let finished = {
        let state = state.clone();
        lua.create_function(move |_, name: String| {
            let started = state.current.borrow_mut().take();
            if let (Some(record), Some((_, at))) = (&state.record, started) {
                record.task(&name, "ok", at.elapsed().as_millis() as u64);
            }
            Ok(())
        })
        .map_err(lua_err)?
    };
    let join = lua
        .create_function(|_, words: Vec<String>| Ok(shell_words::join(words)))
        .map_err(lua_err)?;
    let driver: mlua::Function = lua
        .load(TASK_DRIVER)
        .set_name(INTERNAL_CHUNK)
        .call((steps, started, finished, join))
        .map_err(lua_err)?;
    let words = lua.create_sequence_from(args).map_err(lua_err)?;
    let result = rt
        .call(driver, MultiValue::from_iter([Value::Table(words)]))
        .await;
    if result.is_err()
        && let (Some(record), Some((name, at))) = (&state.record, state.current.borrow_mut().take())
    {
        record.task(&name, "failed", at.elapsed().as_millis() as u64);
    }
    rt.finish_script(record, &result);
    result
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
    fn words_before_a_double_dash_cannot_look_like_options() {
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            task_args("t", &words(&["a", "-", "--", "--release", "--"])).unwrap(),
            ["a", "-", "--release", "--"]
        );
        let err = task_args("t", &words(&["--release"])).unwrap_err();
        assert_eq!(err.kind, "usage");
        assert!(err.hint.unwrap().contains("timon t -- --release"));
    }

    #[test]
    fn the_task_fields_parse() {
        crate::options::object(FIELDS).unwrap();
    }
}
