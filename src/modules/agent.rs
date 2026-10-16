//! `agent`: Claude Code and Codex runs from Lua.

use std::path::PathBuf;
use std::rc::Rc;

use mlua::{Function, Lua, Table, Value};
use tokio::sync::Semaphore;

use crate::agent::{self, Access, Budget, Provider, Record, Request, Status};
use crate::docs::{Func, Module, Opt, Ty};
use crate::error::{self, Error};
use crate::layers::Kind;
use crate::modules::{Installer, concurrency, fail_function, json as json_module, schema};
use crate::options;
use crate::runtime::{INTERNAL_CHUNK, SETTINGS};
use crate::value;

const DEFAULT_CONCURRENCY: usize = 4;

pub const DOC: Module = Module {
    name: "agent",
    summary: "Claude Code and Codex runs. agent.run returns a record and never raises for an outcome; agent.ask returns the answer or raises. Runs spend tokens.",
    functions: &[
        Func::new(
            "run",
            "agent.run{ prompt, agent, provider, model, effort, access, ... } -> record",
            "Runs one non-interactive turn. The record has id, status (ok, failed, invalid, timeout, cancelled), answer (with output), text, error, provider, model, session, usage { input_tokens, output_tokens, usd } and ms. Fields merge: defaults < init.lua defaults < the agent definition < this call.",
            "local r = agent.run{ prompt = \"Review the diff on this branch\", output = \"review\" }\nif r.status == \"ok\" then pp(r.answer.findings) else log.error(r.error.message) end",
        )
        .with_options(&[
            Opt::required("prompt", Ty::String, "what to do"),
            Opt::new("agent", Ty::String, "a definition from agents/NAME.lua to start from"),
            Opt::new("provider", Ty::String, "\"claude\" (default) or \"codex\""),
            Opt::new("model", Ty::String, "the provider's model name"),
            Opt::new("effort", Ty::String, "claude: low, medium, high, xhigh, max; codex: passed through"),
            Opt::new("access", Ty::String, "\"read\" (default), \"write\", or \"full\" (needs limits.allow_full)"),
            Opt::new("tools", Ty::Table, "claude's tool list, replacing the one access picks"),
            Opt::new("cwd", Ty::String, "the directory to run in"),
            Opt::new("system", Ty::String, "instructions added to the system prompt"),
            Opt::new("output", Ty::Any, "a contract name or a schema; the answer is validated"),
            Opt::new("timeout", Ty::Any, "seconds, or a duration such as \"10m\""),
            Opt::new("budget", Ty::Table, "{ usd = number, turns = integer } (claude)"),
            Opt::new("resume", Ty::String, "a session id timon started, to continue"),
            Opt::new("fork", Ty::String, "a session id to branch from (claude)"),
            Opt::new("native", Ty::Table, "extra command-line arguments for the provider"),
        ]),
        Func::new(
            "ask",
            "agent.ask(prompt | spec) -> answer | text",
            "Runs like agent.run and returns the answer (with output) or the text. Raises agent_failed, with the record as error.data, when the run is not ok.",
            "local ok = agent.ask{ prompt = \"Is this function pure? \" .. src, output = \"yes_no\" }\nif ok.answer then print(ok.reason) end",
        ),
        Func::new(
            "spawn",
            "agent.spawn(spec) -> task",
            "Starts agent.run(spec) as a task on the innermost scope; task:await() returns the record.",
            "local a = agent.spawn{ prompt = \"Summarize README.md\" }\nlocal b = agent.spawn{ prompt = \"Summarize CHANGELOG.md\", provider = \"codex\" }\nlocal ra, rb = a:await(), b:await()",
        ),
        Func::new(
            "continue",
            "agent.continue(record, prompt, spec) -> record",
            "Resumes record's session with a new prompt, on the same provider. spec adds or overrides fields.",
            "local r = agent.run{ prompt = \"List the failing tests\" }\nlocal fix = agent.continue(r, \"Fix the first one\", { access = \"write\" })",
        ),
    ],
};

/// Registry key of the agent definition directories, project first.
pub(crate) const AGENT_DIRS: &str = "timon.agent_dirs";

const GLUE: &str = r#"
local run, spawn, fail = ...
local type, error, pairs = type, error, pairs
local agent = {}

function agent.spawn(spec)
  return spawn(run, spec)
end

function agent.continue(record, prompt, spec)
  if type(record) ~= "table" or record.provider == nil then
    error(fail("bad_argument", "agent.continue takes the record of an earlier run"), 2)
  end
  if record.session == nil then
    error(fail("no_session", "that run has no session to continue"), 2)
  end
  local s = {}
  for k, v in pairs(spec or {}) do s[k] = v end
  s.provider = s.provider or record.provider
  s.resume = record.session
  s.prompt = prompt
  return run(s)
end

return agent
"#;

struct State {
    permits: Rc<Semaphore>,
}

fn setting<T: mlua::FromLua>(lua: &Lua, section: &str, key: &str) -> mlua::Result<Option<T>> {
    let Some(settings) = lua.named_registry_value::<Option<Table>>(SETTINGS)? else {
        return Ok(None);
    };
    let Some(section) = settings.raw_get::<Option<Table>>(section)? else {
        return Ok(None);
    };
    section.raw_get::<Option<T>>(key)
}

fn permits(lua: &Lua) -> mlua::Result<Rc<Semaphore>> {
    if let Some(state) = lua.app_data_ref::<State>() {
        return Ok(state.permits.clone());
    }
    let n = setting::<i64>(lua, "limits", "concurrency")?
        .map(|n| n.max(1) as usize)
        .unwrap_or(DEFAULT_CONCURRENCY);
    let permits = Rc::new(Semaphore::new(n));
    lua.set_app_data(State {
        permits: permits.clone(),
    });
    Ok(permits)
}

/// Copies `from`'s entries over `into`.
fn overlay(into: &Table, from: &Table) -> mlua::Result<()> {
    for pair in from.pairs::<Value, Value>() {
        let (k, v) = pair?;
        into.raw_set(k, v)?;
    }
    Ok(())
}

/// The fields of a run after merging defaults, settings, the definition and the call.
fn merged(lua: &Lua, spec: &Table) -> Result<Table, Error> {
    let fields = lua.create_table()?;
    fields.raw_set("provider", "claude")?;
    fields.raw_set("access", "read")?;
    if let Some(settings) = lua.named_registry_value::<Option<Table>>(SETTINGS)?
        && let Some(defaults) = settings.raw_get::<Option<Table>>("defaults")?
    {
        overlay(&fields, &defaults)?;
    }
    if let Some(name) = spec.raw_get::<Option<String>>("agent")? {
        let dirs: Vec<(Kind, PathBuf)> = lua
            .named_registry_value::<Option<Vec<String>>>(AGENT_DIRS)?
            .unwrap_or_default()
            .into_iter()
            .map(|d| (Kind::User, PathBuf::from(d)))
            .collect();
        let definition = crate::agents::resolve(lua, &dirs, &name)?;
        definition.raw_set("description", Value::Nil)?;
        overlay(&fields, &definition)?;
    }
    overlay(&fields, spec)?;
    fields.raw_set("agent", Value::Nil)?;
    Ok(fields)
}

fn string_list(t: &Table, what: &str) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    for v in t.sequence_values::<Value>() {
        match v? {
            Value::String(s) => out.push(s.to_string_lossy()),
            other => {
                return Err(Error::new(
                    "bad_option",
                    format!(
                        "{what} must be a list of strings, not contain a {}",
                        other.type_name()
                    ),
                ));
            }
        }
    }
    Ok(out)
}

/// Builds the request from merged fields.
fn request(lua: &Lua, fields: &Table) -> Result<Request, Error> {
    let get = |k: &str| fields.raw_get::<Option<String>>(k);
    let provider = Provider::parse(&get("provider")?.unwrap_or_default())?;
    let prompt = get("prompt")?.unwrap_or_default();
    let mut req = Request::new(provider, prompt);
    req.access = Access::parse(&get("access")?.unwrap_or_default())?;
    if req.access == Access::Full && setting::<bool>(lua, "limits", "allow_full")? != Some(true) {
        return Err(Error::new(
            "full_access_not_allowed",
            "access = \"full\" lets the agent run anything without asking",
        )
        .with_hint("allow it with limits = { allow_full = true } in init.lua"));
    }
    if let Some(model) = get("model")? {
        if model.trim().is_empty() {
            return Err(Error::new("bad_option", "model is empty"));
        }
        req.model = Some(model);
    }
    req.effort = get("effort")?;
    req.system = get("system")?;
    req.resume = get("resume")?;
    req.fork = get("fork")?;
    req.cwd = get("cwd")?.map(PathBuf::from);
    if let Some(tools) = fields.raw_get::<Option<Table>>("tools")? {
        req.tools = Some(string_list(&tools, "tools")?);
    }
    if let Some(native) = fields.raw_get::<Option<Table>>("native")? {
        req.native = string_list(&native, "native")?;
    }
    let timeout: Value = fields.raw_get("timeout")?;
    if !timeout.is_nil() {
        req.timeout = Some(concurrency::duration(&timeout)?);
    }
    if let Some(budget) = fields.raw_get::<Option<Table>>("budget")? {
        let mut b = Budget::default();
        for pair in budget.pairs::<String, Value>() {
            let (k, v) = pair?;
            match (k.as_str(), &v) {
                ("usd", Value::Integer(i)) => b.usd = Some(*i as f64),
                ("usd", Value::Number(n)) => b.usd = Some(*n),
                ("turns", Value::Integer(i)) if *i > 0 => b.turns = Some(*i as u32),
                _ => {
                    return Err(Error::new(
                        "bad_option",
                        format!("budget.{k} is not usd (a number) or turns (a positive integer)"),
                    ));
                }
            }
        }
        req.budget = b;
    }
    let output: Value = fields.raw_get("output")?;
    if !output.is_nil() {
        req.output = Some(schema::resolve(lua, &output)?);
    }
    Ok(req)
}

/// The record as a Lua table: JSON nulls become nil, and the answer keeps its JSON shape.
fn record_table(lua: &Lua, record: &Record) -> mlua::Result<Table> {
    let Value::Table(t) = value::from_json(lua, &record.to_json())? else {
        return Err(Error::new("internal", "a record did not convert to a table").into());
    };
    if let Some(answer) = &record.answer {
        t.raw_set("answer", json_module::decode(lua, answer)?)?;
    }
    Ok(t)
}

async fn run(lua: Lua, spec: Table) -> mlua::Result<Table> {
    options::check("agent.run", &spec)?;
    let request = request(&lua, &merged(&lua, &spec)?)?;
    agent::check(&request)?;
    let permits = permits(&lua)?;
    let _permit = permits
        .acquire()
        .await
        .map_err(|_| Error::new("internal", "the run queue closed"))?;
    let record = agent::run(&request).await;
    record_table(&lua, &record)
}

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let module = installer.module(&DOC)?;
    let run_fn = installer.function(
        &DOC,
        Some(&module),
        "run",
        lua.create_async_function(|lua, spec: Table| run(lua, spec))?,
    )?;
    installer.function(
        &DOC,
        Some(&module),
        "ask",
        lua.create_async_function(|lua, arg: Value| async move {
            let spec = match arg {
                Value::String(prompt) => {
                    let t = lua.create_table()?;
                    t.raw_set("prompt", prompt)?;
                    t
                }
                Value::Table(t) => t,
                other => {
                    return Err(Error::new(
                        "bad_argument",
                        format!(
                            "agent.ask takes a prompt or a spec, not a {}",
                            other.type_name()
                        ),
                    )
                    .into());
                }
            };
            let wants_answer = !spec.raw_get::<Value>("output")?.is_nil();
            let record = run(lua.clone(), spec).await?;
            let status: String = record.raw_get("status")?;
            if status == Status::Ok.name() {
                return record.raw_get::<Value>(if wants_answer { "answer" } else { "text" });
            }
            let cause: Option<Table> = record.raw_get("error")?;
            let message = cause
                .as_ref()
                .and_then(|e| e.raw_get::<String>("message").ok())
                .unwrap_or_default();
            let hint = cause
                .as_ref()
                .and_then(|e| e.raw_get::<Option<String>>("hint").ok().flatten());
            let provider: String = record.raw_get("provider")?;
            let mut err = Error::new(
                "agent_failed",
                format!("{provider} run {status}: {message}"),
            );
            if let Some(hint) = hint {
                err = err.with_hint(hint);
            }
            let table = error::to_table(&lua, &err)?;
            table.raw_set("data", record)?;
            Err(error::raise(&lua, Value::Table(table)))
        })?,
    )?;
    let spawn: Function = lua.globals().raw_get("spawn")?;
    let helpers: Table =
        lua.load(GLUE)
            .set_name(INTERNAL_CHUNK)
            .call((run_fn, spawn, fail_function(lua)?))?;
    for name in ["spawn", "continue"] {
        let f: Function = helpers.raw_get(name)?;
        installer.lua_function(&DOC, Some(&module), name, f)?;
    }
    Ok(())
}
