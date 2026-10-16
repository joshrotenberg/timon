//! Agent definitions: `agents/NAME.lua` (or `.fnl`) in the project and user layers, each
//! returning the fields of an `agent.run` call to use by default.
//!
//! A definition replaces one of the same name in a lower layer; `extends = "NAME"` merges onto
//! the next definition of that name further down instead.

use std::path::{Path, PathBuf};

use mlua::{Lua, Table, Value};

use crate::error::Error;
use crate::layers::{Kind, Layers};
use crate::suggest;

/// Fields a definition may set: the `agent.run` options that describe how to run, plus
/// `extends` and `description`.
pub const FIELDS: &[&str] = &[
    "description",
    "extends",
    "provider",
    "model",
    "effort",
    "access",
    "tools",
    "system",
    "output",
    "timeout",
    "budget",
    "worktree",
    "native",
    "ephemeral",
    "events",
];

/// One definition file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub name: String,
    pub layer: Kind,
    pub path: PathBuf,
}

/// The agent directories of the trusted layers, project first.
pub fn dirs(layers: &Layers) -> Vec<(Kind, PathBuf)> {
    layers
        .active()
        .into_iter()
        .map(|l| (l.kind, l.root.join("agents")))
        .filter(|(_, p)| p.is_dir())
        .collect()
}

/// Every definition in reach; a name a higher layer has hides lower ones.
pub fn list(dirs: &[(Kind, PathBuf)]) -> Vec<Definition> {
    let mut found: Vec<Definition> = Vec::new();
    for (kind, dir) in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut here: Vec<Definition> = entries
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let ext = path.extension()?.to_str()?;
                if !matches!(ext, "lua" | "fnl") {
                    return None;
                }
                Some(Definition {
                    name: path.file_stem()?.to_str()?.to_owned(),
                    layer: *kind,
                    path,
                })
            })
            .collect();
        here.sort_by(|a, b| a.name.cmp(&b.name));
        for d in here {
            if !found.iter().any(|f| f.name == d.name) {
                found.push(d);
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn load_file(lua: &Lua, path: &Path) -> Result<Table, Error> {
    let bad = |message: String| Error::new("bad_config", format!("{}: {message}", path.display()));
    let text = std::fs::read_to_string(path).map_err(|e| bad(e.to_string()))?;
    let name = format!("@{}", path.display());
    let source = if path.extension().is_some_and(|e| e == "fnl") {
        crate::fennel::compile(lua, &text, &name)?
    } else {
        text
    };
    let value: Value = lua
        .load(source)
        .set_name(name)
        .set_environment(crate::settings::definition_env(lua)?)
        .call(())
        .map_err(|e| bad(Error::from_lua(&e).message))?;
    let Value::Table(t) = value else {
        return Err(bad(format!(
            "must return a table, not a {}",
            value.type_name()
        )));
    };
    for pair in t.pairs::<Value, Value>() {
        let (k, _) = pair?;
        let key = match k {
            Value::String(s) => s.to_string_lossy(),
            other => return Err(bad(format!("a {} key is not a field", other.type_name()))),
        };
        if !FIELDS.contains(&key.as_str()) {
            let err = bad(format!("unknown field \"{key}\""));
            return Err(match suggest::closest(&key, FIELDS.iter().copied()) {
                Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
                None => err.with_hint(format!("fields are {}", FIELDS.join(", "))),
            });
        }
    }
    check_values(&t).map_err(|e| bad(e.message.clone()).with_hint_opt(e.hint))?;
    Ok(t)
}

/// Checks each field's value against the `agent.run` option of the same name.
fn check_values(t: &Table) -> Result<(), Error> {
    use crate::modules::schema::{Node, Problem};
    let run = crate::options::shape("agent.run")?;
    let Node::Object(options) = &run.node else {
        return Ok(());
    };
    for (key, value) in t.pairs::<String, Value>().flatten() {
        let shape = match key.as_str() {
            "description" | "extends" => {
                crate::types::parse("string").map_err(|e| Error::new("internal", e))?
            }
            _ => match options.iter().find(|(n, _)| *n == key) {
                Some((_, shape)) => shape.clone(),
                None => continue,
            },
        };
        let Some(e) = shape.check_value(&value).into_iter().next() else {
            continue;
        };
        let at = match e.path.as_str() {
            "" => key.clone(),
            p if p.starts_with('[') => format!("{key}{p}"),
            p => format!("{key}.{p}"),
        };
        let close_hint = |close: Option<String>| close.map(|c| format!("did you mean \"{c}\"?"));
        let (message, hint) = match e.problem {
            Problem::Wrong { expected, got } => (
                format!("field \"{at}\" must be {expected}, got {got}"),
                None,
            ),
            Problem::NotOneOf { values, got, close } => {
                let list: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
                (
                    format!(
                        "field \"{at}\" must be one of {}, got {got}",
                        list.join(", ")
                    ),
                    close_hint(close),
                )
            }
            Problem::Unknown { key, close } => (
                format!("unknown field \"{key}\" in \"{at}\""),
                close_hint(close),
            ),
            Problem::Missing { name } => (format!("missing field \"{name}\" in \"{at}\""), None),
        };
        return Err(Error::new("bad_config", message).with_hint_opt(hint));
    }
    Ok(())
}

/// A definition's description, after following `extends`; empty when it has none.
pub fn describe(lua: &Lua, dirs: &[(Kind, PathBuf)], name: &str) -> Result<String, Error> {
    Ok(resolve(lua, dirs, name)?
        .raw_get::<Option<String>>("description")?
        .unwrap_or_default())
}

/// Resolves definition `name` into one table of fields, following `extends` down the layers.
pub(crate) fn resolve(lua: &Lua, dirs: &[(Kind, PathBuf)], name: &str) -> Result<Table, Error> {
    resolve_from(lua, dirs, name, 0)
}

fn resolve_from(
    lua: &Lua,
    dirs: &[(Kind, PathBuf)],
    name: &str,
    start: usize,
) -> Result<Table, Error> {
    for (i, (_, dir)) in dirs.iter().enumerate().skip(start) {
        for ext in ["lua", "fnl"] {
            let path = dir.join(format!("{name}.{ext}"));
            if !path.is_file() {
                continue;
            }
            let own = load_file(lua, &path)?;
            let merged = match own.raw_get::<Option<String>>("extends")? {
                Some(base) => {
                    let below = if base == name { i + 1 } else { 0 };
                    resolve_from(lua, dirs, &base, below)?
                }
                None => lua.create_table()?,
            };
            for pair in own.pairs::<Value, Value>() {
                let (k, v) = pair?;
                merged.raw_set(k, v)?;
            }
            merged.raw_set("extends", Value::Nil)?;
            return Ok(merged);
        }
    }
    let names: Vec<String> = list(dirs).into_iter().map(|d| d.name).collect();
    let err = Error::new("not_found", format!("no agent named \"{name}\""));
    Err(
        match suggest::closest(name, names.iter().map(String::as_str)) {
            Some(close) => err.with_hint(format!("did you mean \"{close}\"?")),
            None if names.is_empty() => {
                err.with_hint("define one in ~/.config/timon/agents/NAME.lua")
            }
            None => err.with_hint(format!("agents: {}", names.join(", "))),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: PathBuf, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn definitions_layer_and_extend() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user/agents");
        let project = tmp.path().join("project/agents");
        write(
            user.join("reviewer.lua"),
            "return { provider = 'claude', model = 'opus', access = 'read', output = 'review' }",
        );
        write(user.join("fast.fnl"), "{:extends :reviewer :model :haiku}");
        write(
            project.join("reviewer.lua"),
            "return { extends = 'reviewer', system = 'Check the migrations too.' }",
        );
        let dirs = vec![(Kind::Project, project), (Kind::User, user)];
        let runtime = crate::Runtime::new().unwrap();
        let lua = runtime.lua();

        let reviewer = resolve(lua, &dirs, "reviewer").unwrap();
        let get = |t: &Table, k: &str| t.raw_get::<Option<String>>(k).unwrap();
        assert_eq!(get(&reviewer, "model").as_deref(), Some("opus"));
        assert_eq!(
            get(&reviewer, "system").as_deref(),
            Some("Check the migrations too.")
        );
        assert_eq!(get(&reviewer, "extends"), None);

        let fast = resolve(lua, &dirs, "fast").unwrap();
        assert_eq!(get(&fast, "model").as_deref(), Some("haiku"));
        assert_eq!(
            get(&fast, "system").as_deref(),
            Some("Check the migrations too.")
        );

        let names: Vec<(String, Kind)> =
            list(&dirs).into_iter().map(|d| (d.name, d.layer)).collect();
        assert_eq!(
            names,
            [
                ("fast".to_owned(), Kind::User),
                ("reviewer".to_owned(), Kind::Project)
            ]
        );
        assert_eq!(
            resolve(lua, &dirs, "reviwer").unwrap_err().hint.as_deref(),
            Some("did you mean \"reviewer\"?")
        );
    }

    #[test]
    fn unknown_fields_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        write(dir.join("x.lua"), "return { modle = 'opus' }");
        let runtime = crate::Runtime::new().unwrap();
        let err = resolve(runtime.lua(), &[(Kind::User, dir)], "x").unwrap_err();
        assert_eq!(err.kind, "bad_config");
        assert_eq!(err.hint.as_deref(), Some("did you mean \"model\"?"));
    }
}
