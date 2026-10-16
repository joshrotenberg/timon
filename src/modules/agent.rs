//! `agent`: Claude Code and Codex runs from Lua.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use mlua::{Function, Lua, Table, Value};
use tokio::sync::Semaphore;

use crate::agent::{self, Access, Budget, Provider, Record, Request, Status};
use crate::docs::{Func, Module, Opt};
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
            Opt::required("prompt", "string", "what to do"),
            Opt::new("agent", "string", "a definition from agents/NAME.lua to start from"),
            Opt::new("provider", "\"claude\" | \"codex\"", "default claude"),
            Opt::new("model", "string", "the provider's model name"),
            Opt::new("effort", "string", "claude: low, medium, high, xhigh, max; codex: passed through"),
            Opt::new("access", "\"read\" | \"write\" | \"full\"", "default read; full needs limits.allow_full"),
            Opt::new("tools", "string[]", "claude's tool list, replacing the one access picks; {} allows none"),
            Opt::new("cwd", "string", "the directory to run in"),
            Opt::new("system", "string", "instructions added to the system prompt"),
            Opt::new("output", "string | schema", "a contract name or a schema; the answer is validated"),
            Opt::new("timeout", "number | string", "seconds, or a duration such as \"10m\""),
            Opt::new("budget", "{ usd: number?, turns: integer? }", "spending and turn limits for one run (claude)"),
            Opt::new("resume", "string", "a session id timon started, to continue"),
            Opt::new("fork", "string", "a session id to branch from (claude)"),
            Opt::new("native", "string[]", "extra command-line arguments for the provider"),
            Opt::new("worktree", "boolean", "run in a new git worktree; kept only if the run changed it"),
            Opt::new("ephemeral", "boolean", "leave no session behind for the provider to resume"),
            Opt::new("events", "boolean", "add record.events: the agent's text, reasoning, tool calls and tool results"),
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
            "runs",
            "agent.runs{ since, limit } -> array",
            "Recorded runs, newest first: id, started_at, provider, model, agent, status, ms, usd, output_tokens and the start of the prompt. timon runs show ID has the full request and result.",
            "for _, r in ipairs(agent.runs{ since = \"1d\" }) do print(r.id, r.status, r.prompt) end",
        )
        .with_options(&[
            Opt::new("since", "string | number", "a duration back from now, such as \"1d\" or \"2h\""),
            Opt::new("limit", "integer", "most entries to return (default 20)"),
        ]),
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

/// Registry key of the state directory, where run records go.
pub(crate) const STATE_DIR: &str = "timon.state_dir";

/// Limits for one invocation, and what it has used.
struct Spending {
    max_runs: Option<u64>,
    max_usd: Option<f64>,
    runs: Cell<u64>,
    usd: Cell<f64>,
}

/// Limits given on the command line; they win over init.lua.
pub struct Overrides {
    pub max_runs: Option<u64>,
    pub max_usd: Option<f64>,
}

fn spending(lua: &Lua) -> mlua::Result<()> {
    if lua.app_data_ref::<Spending>().is_some() {
        return Ok(());
    }
    let (cli_runs, cli_usd) = match lua.app_data_ref::<Overrides>() {
        Some(o) => (o.max_runs, o.max_usd),
        None => (None, None),
    };
    let spending = Spending {
        max_runs: cli_runs.or(setting::<i64>(lua, "limits", "max_runs")?.map(|n| n.max(0) as u64)),
        max_usd: cli_usd.or(setting::<f64>(lua, "limits", "max_usd")?),
        runs: Cell::new(0),
        usd: Cell::new(0.0),
    };
    lua.set_app_data(spending);
    Ok(())
}

/// Raises `budget_exceeded` if starting another run would pass a limit, else counts it.
fn start_run(lua: &Lua) -> mlua::Result<()> {
    spending(lua)?;
    let Some(s) = lua.app_data_ref::<Spending>() else {
        return Ok(());
    };
    if let Some(max) = s.max_runs
        && s.runs.get() >= max
    {
        return Err(Error::new(
            "budget_exceeded",
            format!(
                "this invocation has started {} runs; the limit is {max}",
                s.runs.get()
            ),
        )
        .with_hint("raise --max-runs or limits.max_runs")
        .into());
    }
    if let Some(max) = s.max_usd
        && s.usd.get() >= max
    {
        return Err(Error::new(
            "budget_exceeded",
            format!(
                "this invocation has spent ${:.4}; the limit is ${max}",
                s.usd.get()
            ),
        )
        .with_hint(
            "raise --max-usd or limits.max_usd; Codex runs report no cost and are not counted",
        )
        .into());
    }
    s.runs.set(s.runs.get() + 1);
    Ok(())
}

fn spent(lua: &Lua, usd: Option<f64>) {
    if let (Some(s), Some(usd)) = (lua.app_data_ref::<Spending>(), usd) {
        s.usd.set(s.usd.get() + usd);
    }
}

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
    req.ephemeral = fields
        .raw_get::<Option<bool>>("ephemeral")?
        .unwrap_or(false);
    req.events = fields.raw_get::<Option<bool>>("events")?.unwrap_or(false);
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
    // Decoded as JSON, so an empty list stays an array and tool inputs keep their shapes.
    if let Some(events) = &record.events {
        t.raw_set(
            "events",
            json_module::decode(lua, &serde_json::Value::Array(events.clone()))?,
        )?;
    }
    Ok(t)
}

/// Settles a run's record exactly once: with its result, or as `cancelled` when the run's
/// future is dropped first (a cancelled scope, a `par.race` loser, Ctrl-C). Writing happens
/// synchronously, so it also works from `Drop`.
struct Settle {
    store: Option<agent::records::Store>,
    request: Request,
    id: String,
    started_at: String,
    agent: Option<String>,
    worktree: Option<agent::worktree::Worktree>,
    started: std::time::Instant,
    done: bool,
}

impl Settle {
    fn finish(&mut self, record: &mut Record) {
        self.done = true;
        record.started_at = self.started_at.clone();
        if let Some(wt) = &self.worktree {
            let kept = wt.finish().unwrap_or(true);
            record.worktree = Some(wt.to_json(kept));
        }
        if let Some(store) = &self.store
            && let Err(err) = store.save(&self.request, record, self.agent.as_deref())
        {
            eprintln!("timon: the run record was not saved: {err}");
        }
    }
}

impl Drop for Settle {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut record = Record::cancelled(
            &self.request,
            self.id.clone(),
            self.started_at.clone(),
            self.started.elapsed().as_millis() as u64,
        );
        self.finish(&mut record);
    }
}

async fn run(lua: Lua, spec: Table) -> mlua::Result<Table> {
    options::check("agent.run", &spec)?;
    let fields = merged(&lua, &spec)?;
    let request = request(&lua, &fields)?;
    let wants_worktree = fields.raw_get::<Option<bool>>("worktree")?.unwrap_or(false);
    agent::check(&request)?;
    let permits = permits(&lua)?;
    let _permit = permits
        .acquire()
        .await
        .map_err(|_| Error::new("internal", "the run queue closed"))?;
    start_run(&lua)?;
    let id = agent::new_id();
    let state = lua.named_registry_value::<Option<String>>(STATE_DIR)?;
    let mut request = request;
    let worktree = if wants_worktree {
        let cwd = match &request.cwd {
            Some(cwd) => cwd.clone(),
            None => std::env::current_dir()
                .map_err(|e| Error::new("io", format!("cannot read the working directory: {e}")))?,
        };
        let state_dir = state
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let wt = agent::worktree::Worktree::create(&cwd, &state_dir, &id)?;
        request.cwd = Some(wt.path.clone());
        Some(wt)
    } else {
        None
    };
    let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let agent_name = spec.raw_get::<Option<String>>("agent")?;
    let store = state
        .as_ref()
        .map(|s| agent::records::Store::new(std::path::Path::new(s)));
    if let Some(store) = &store
        && let Err(err) = store.begin(
            &request,
            &id,
            &started_at,
            agent_name.as_deref(),
            worktree.as_ref().map(|w| w.to_json(true)).as_ref(),
        )
    {
        eprintln!("timon: the run record was not started: {err}");
    }
    let mut settle = Settle {
        store,
        request: request.clone(),
        id: id.clone(),
        started_at,
        agent: agent_name,
        worktree,
        started: std::time::Instant::now(),
        done: false,
    };
    let mut record = agent::run_as(&request, id).await;
    settle.finish(&mut record);
    spent(&lua, record.usage.usd);
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
    installer.function(
        &DOC,
        Some(&module),
        "runs",
        lua.create_function(|lua, opts: Option<Table>| {
            options::check_opt("agent.runs", opts.as_ref())?;
            let Some(state) = lua.named_registry_value::<Option<String>>(STATE_DIR)? else {
                return json_module::array(lua, None).map(Value::Table);
            };
            let mut since = None;
            let mut limit = 20usize;
            if let Some(opts) = &opts {
                let s: Value = opts.raw_get("since")?;
                if !s.is_nil() {
                    let back = chrono::Duration::from_std(concurrency::duration(&s)?)
                        .map_err(|e| Error::new("bad_option", e.to_string()))?;
                    since = Some(chrono::Utc::now() - back);
                }
                if let Some(n) = opts.raw_get::<Option<i64>>("limit")? {
                    limit = n.max(0) as usize;
                }
            }
            let entries =
                agent::records::Store::new(std::path::Path::new(&state)).list(since, limit)?;
            json_module::decode(lua, &serde_json::Value::Array(entries))
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
