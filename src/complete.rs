//! timon's own commands described the way tasks are, and shell completion from the same
//! declarations.
//!
//! The commands are declared once, with clap. [`builtins`] reads clap's command model into the
//! shape `timon tasks --json` gives a task: name, description and options with a type in the
//! syntax of [`crate::types`]. [`complete`] answers a shell's completion request from the same
//! model and the task catalog, and [`script`] is the shell side, which calls
//! `timon __complete -- WORDS`.

use std::any::TypeId;

use clap::{Arg, ArgAction, Command};
use serde_json::{Value as Json, json};

use crate::tasks::{Catalog, Task};

/// The type of a clap argument, in the type syntax: a boolean for a flag, the choices for
/// possible values, an integer or a number from the value parser, else a string; `[]` when it
/// takes several values.
fn arg_type(arg: &Arg) -> String {
    if matches!(
        arg.get_action(),
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
    ) {
        return "boolean".into();
    }
    let choices: Vec<String> = arg
        .get_possible_values()
        .iter()
        .filter(|v| !v.is_hide_set())
        .map(|v| format!("\"{}\"", v.get_name()))
        .collect();
    let id = arg.get_value_parser().type_id();
    let base = if !choices.is_empty() {
        choices.join(" | ")
    } else if [
        TypeId::of::<u8>(),
        TypeId::of::<u16>(),
        TypeId::of::<u32>(),
        TypeId::of::<u64>(),
        TypeId::of::<usize>(),
        TypeId::of::<i32>(),
        TypeId::of::<i64>(),
    ]
    .iter()
    .any(|t| id == *t)
    {
        "integer".into()
    } else if id == TypeId::of::<f64>() || id == TypeId::of::<f32>() {
        "number".into()
    } else {
        "string".into()
    };
    let many = matches!(arg.get_action(), ArgAction::Append)
        || arg.get_num_args().is_some_and(|n| n.max_values() > 1);
    match (many, choices.len() > 1) {
        (true, true) => format!("({base})[]"),
        (true, false) => format!("{base}[]"),
        (false, _) => base,
    }
}

fn hidden(arg: &Arg) -> bool {
    arg.is_hide_set() || matches!(arg.get_id().as_str(), "help" | "version")
}

fn doc(arg: &Arg) -> String {
    arg.get_help().map(|h| h.to_string()).unwrap_or_default()
}

fn default(arg: &Arg, ty: &str) -> Json {
    match arg.get_default_values().first() {
        None => Json::Null,
        Some(v) => {
            let text = v.to_string_lossy();
            match ty {
                "integer" => text.parse::<i64>().map(Json::from).unwrap_or(Json::Null),
                "number" => text
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Json::Number)
                    .unwrap_or(Json::Null),
                "boolean" => Json::Null,
                _ => Json::String(text.into_owned()),
            }
        }
    }
}

/// A command's options, in the shape of a task's (see [`crate::flags::Spec::to_json`]).
fn options(cmd: &Command) -> Json {
    let mut out: Vec<Json> = cmd
        .get_arguments()
        .filter(|a| !a.is_positional() && !hidden(a) && !a.is_global_set())
        .map(|a| {
            let ty = arg_type(a);
            json!({
                "name": a.get_id().as_str(),
                "flag": a.get_long().map(|l| format!("--{l}")),
                "type": ty,
                "doc": doc(a),
                "default": default(a, &ty),
                "short": a.get_short().map(|c| format!("-{c}")),
                "required": a.is_required_set(),
            })
        })
        .collect();
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Json::Array(out)
}

fn positionals(cmd: &Command) -> Json {
    Json::Array(
        cmd.get_positionals()
            .filter(|a| !hidden(a))
            .map(|a| {
                json!({
                    "name": a.get_id().as_str(),
                    "type": arg_type(a),
                    "doc": doc(a),
                    "required": a.is_required_set(),
                })
            })
            .collect(),
    )
}

/// timon's own commands, as `timon tasks --json` describes tasks: every visible command and
/// subcommand, named by its path (`sessions search`), and the global flags as `timon`.
pub fn builtins(root: &Command) -> Vec<Json> {
    fn walk(cmd: &Command, path: &str, out: &mut Vec<Json>) {
        for sub in cmd.get_subcommands().filter(|c| !c.is_hide_set()) {
            let name = if path.is_empty() {
                sub.get_name().to_owned()
            } else {
                format!("{path} {}", sub.get_name())
            };
            let has_subcommands = sub.get_subcommands().any(|c| !c.is_hide_set());
            if !has_subcommands || !sub.is_subcommand_required_set() {
                out.push(json!({
                    "name": name,
                    "layer": "builtin",
                    "kind": "builtin",
                    "description": sub.get_about().map(|a| a.to_string()).unwrap_or_default(),
                    "aliases": sub.get_visible_aliases().collect::<Vec<_>>(),
                    "options": options(sub),
                    "positionals": positionals(sub),
                }));
            }
            walk(sub, &name, out);
        }
    }
    let globals: Vec<Json> = root
        .get_arguments()
        .filter(|a| a.is_global_set() && !hidden(a))
        .map(|a| {
            let ty = arg_type(a);
            json!({
                "name": a.get_id().as_str(),
                "flag": a.get_long().map(|l| format!("--{l}")),
                "type": ty,
                "doc": doc(a),
                "default": default(a, &ty),
                "short": a.get_short().map(|c| format!("-{c}")),
                "required": false,
            })
        })
        .collect();
    let mut out = vec![json!({
        "name": "timon",
        "layer": "builtin",
        "kind": "builtin",
        "description": "timon's own flags, given before a command or task name",
        "aliases": [],
        "options": globals,
        "positionals": [],
    })];
    walk(root, "", &mut out);
    out
}

/// The top-level commands, for the text listing: name and description.
pub fn top_level(root: &Command) -> Vec<(String, String)> {
    root.get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| {
            (
                c.get_name().to_owned(),
                c.get_about().map(|a| a.to_string()).unwrap_or_default(),
            )
        })
        .collect()
}

/// Where the words typed so far lead.
enum At<'a> {
    Command(&'a Command),
    Task(&'a Task),
    /// After `--`, or after something unknown: nothing to offer.
    Nothing,
}

fn long_flags(cmd: &Command, globals: bool) -> Vec<String> {
    cmd.get_arguments()
        .filter(|a| !a.is_positional() && !a.is_hide_set() && (globals || !a.is_global_set()))
        .filter_map(|a| a.get_long().map(|l| format!("--{l}")))
        .collect()
}

/// The clap argument a flag word names, in `cmd` or the root's globals.
fn clap_flag<'a>(cmd: &'a Command, root: &'a Command, word: &str) -> Option<&'a Arg> {
    let word = word.split('=').next().unwrap_or(word);
    let find = |c: &'a Command| {
        c.get_arguments().find(|a| {
            word.strip_prefix("--")
                .is_some_and(|l| a.get_long() == Some(l))
                || (word.len() == 2
                    && word.starts_with('-')
                    && a.get_short().map(|s| format!("-{s}")).as_deref() == Some(word))
        })
    };
    find(cmd).or_else(|| find(root))
}

fn takes_value(arg: &Arg) -> bool {
    !matches!(
        arg.get_action(),
        ArgAction::SetTrue
            | ArgAction::SetFalse
            | ArgAction::Count
            | ArgAction::Help
            | ArgAction::Version
    )
}

fn subcommand<'a>(cmd: &'a Command, word: &str) -> Option<&'a Command> {
    cmd.get_subcommands()
        .find(|c| c.get_name() == word || c.get_all_aliases().any(|a| a == word))
}

fn task_names(catalog: Option<&Catalog>) -> Vec<String> {
    catalog
        .map(|c| {
            c.tasks
                .iter()
                .filter(|t| !t.private)
                .flat_map(|t| std::iter::once(t.name.clone()).chain(t.aliases.iter().cloned()))
                .collect()
        })
        .unwrap_or_default()
}

/// Candidates for the last of `words`, the words after `timon`, the last possibly empty.
pub fn complete(root: &Command, catalog: Option<&Catalog>, words: &[String]) -> Vec<String> {
    let (current, done) = match words.split_last() {
        Some((c, d)) => (c.as_str(), d),
        None => ("", &[][..]),
    };
    let mut at = At::Command(root);
    let mut wants_task = false;
    let mut words_only = false;
    let mut i = 0;
    while i < done.len() {
        let word = done[i].as_str();
        i += 1;
        if words_only {
            continue;
        }
        if word == "--" {
            words_only = true;
            continue;
        }
        match &at {
            At::Command(cmd) => {
                if word.starts_with('-') {
                    if !word.contains('=') && clap_flag(cmd, root, word).is_some_and(takes_value) {
                        i += 1;
                    }
                } else if wants_task {
                    match catalog.and_then(|c| c.resolve(word)) {
                        Some(task) => at = At::Task(task),
                        None => at = At::Nothing,
                    }
                    wants_task = false;
                } else if let Some(sub) = subcommand(cmd, word) {
                    wants_task = matches!(sub.get_name(), "task");
                    at = At::Command(sub);
                } else if std::ptr::eq(*cmd, root) {
                    at = match catalog.and_then(|c| c.resolve(word)) {
                        Some(task) => At::Task(task),
                        None => At::Nothing,
                    };
                }
            }
            At::Task(task) => {
                if word.starts_with('-')
                    && !word.contains('=')
                    && task.options.lookup(word).is_some_and(|o| o.takes_value())
                {
                    i += 1;
                }
            }
            At::Nothing => {}
        }
    }
    if words_only {
        return Vec::new();
    }
    // A value for the flag just before the current word.
    if let Some(previous) = done
        .last()
        .filter(|w| w.starts_with('-') && !w.contains('='))
    {
        let choices: Option<Vec<String>> = match &at {
            At::Command(cmd) => clap_flag(cmd, root, previous)
                .filter(|a| takes_value(a))
                .map(|a| {
                    a.get_possible_values()
                        .iter()
                        .filter(|v| !v.is_hide_set())
                        .map(|v| v.get_name().to_owned())
                        .collect()
                }),
            At::Task(task) => task
                .options
                .lookup(previous)
                .filter(|o| o.takes_value())
                .map(|o| o.choices()),
            At::Nothing => None,
        };
        if let Some(choices) = choices {
            return filter(choices, current);
        }
    }
    let candidates = match &at {
        At::Nothing => Vec::new(),
        At::Task(task) if current.starts_with('-') => task
            .options
            .options
            .iter()
            .map(|o| o.flag())
            .chain(["--help".to_owned()])
            .collect(),
        At::Task(_) => Vec::new(),
        At::Command(cmd) if current.starts_with('-') => {
            let mut flags = long_flags(cmd, false);
            flags.extend(long_flags(root, true).into_iter().filter(|f| {
                root.get_arguments().any(|a| {
                    a.is_global_set()
                        && a.get_long().map(|l| format!("--{l}")).as_deref() == Some(f)
                })
            }));
            flags.push("--help".into());
            flags
        }
        At::Command(_) if wants_task => task_names(catalog),
        At::Command(cmd) => {
            let mut names: Vec<String> = cmd
                .get_subcommands()
                .filter(|c| !c.is_hide_set())
                .map(|c| c.get_name().to_owned())
                .collect();
            if std::ptr::eq(*cmd, root) {
                names.extend(task_names(catalog));
            }
            names
        }
    };
    filter(candidates, current)
}

fn filter(mut candidates: Vec<String>, prefix: &str) -> Vec<String> {
    candidates.retain(|c| c.starts_with(prefix));
    candidates.sort();
    candidates.dedup();
    candidates
}

/// The completion script for a shell.
pub fn script(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" => Some(
            r#"# timon completion for bash: eval "$(timon completions bash)"
_timon() {
  local words=("${COMP_WORDS[@]:1:COMP_CWORD}")
  local IFS=$'\n'
  COMPREPLY=($(timon __complete -- "${words[@]}" 2>/dev/null))
}
complete -o default -F _timon timon
"#,
        ),
        "zsh" => Some(
            r#"# timon completion for zsh: eval "$(timon completions zsh)"
_timon() {
  local -a items
  items=("${(@f)$(timon __complete -- "${(@)words[2,CURRENT]}" 2>/dev/null)}")
  if (( ${#items[@]} )) && [[ -n "${items[1]}" ]]; then
    compadd -a items
  else
    _files
  fi
}
compdef _timon timon
"#,
        ),
        "fish" => Some(
            r#"# timon completion for fish: timon completions fish | source
complete -c timon -f -a '(timon __complete -- (commandline -opc)[2..-1] (commandline -ct) 2>/dev/null)'
"#,
        ),
        _ => None,
    }
}
