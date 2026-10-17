//! `agent`: Claude Code and Codex runs from Lua.

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
use crate::scripts::ScriptRun;
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
            Opt::new("budget", "{ usd: number?, turns: integer? }", "spending and turn limits for one run (claude); under a USD limit, usd is also what the run holds while it runs, so other runs are not given that money"),
            Opt::new("resume", "string", "a session id timon started, to continue"),
            Opt::new("fork", "string", "a session id to branch from (claude)"),
            Opt::new("native", "string[]", "extra command-line arguments for the provider; codex takes them before exec, so only its top-level options work"),
            Opt::new("worktree", "boolean", "run in a new git worktree; kept only if the run changed it"),
            Opt::new("ephemeral", "boolean", "leave no session behind for the provider to resume"),
            Opt::new("events", "boolean", "add record.events: the agent's text, reasoning, tool calls and tool results"),
            Opt::new("label", "string", "a short name for progress lines and the record"),
            Opt::new("phase", "string", "the workflow phase; default: the calling task's phase()"),
            Opt::new("idempotent", "boolean", "safe to reuse on resume even with write access"),
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
            "agent.runs{ since, limit, parent } -> array",
            "Recorded runs, newest first: id, started_at, provider, model, agent, status, ms, usd, output_tokens and the start of the prompt. timon runs show ID has the full request and result.",
            "for _, r in ipairs(agent.runs{ since = \"1d\" }) do print(r.id, r.status, r.prompt) end",
        )
        .with_options(&[
            Opt::new("since", "string | number", "a duration back from now, such as \"1d\" or \"2h\""),
            Opt::new("limit", "integer", "most entries to return (default 20)"),
            Opt::new("parent", "string", "only the runs of this script run"),
        ]),
        Func::new(
            "continue",
            "agent.continue(record, prompt, spec) -> record",
            "Resumes record's session with a new prompt. It starts from the call that made record (provider, model, effort, access, output, tools, system, cwd, and the kept worktree as cwd), and spec overrides any of them. A record not made in this process carries only its provider.",
            "local r = agent.run{ prompt = \"List the failing tests\" }\nlocal fix = agent.continue(r, \"Fix the first one\", { access = \"write\" })",
        ),
    ],
};

/// Registry key of the agent definition directories, project first.
pub(crate) const AGENT_DIRS: &str = "timon.agent_dirs";

/// Registry key of the state directory, where run records go.
pub(crate) const STATE_DIR: &str = "timon.state_dir";

/// How agent runs report progress on stderr, from `--progress`: a line when each run starts
/// and when it ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Progress {
    Text,
    Json,
    #[default]
    Off,
}

fn progress(lua: &Lua) -> Progress {
    lua.app_data_ref::<Progress>()
        .map(|p| *p)
        .unwrap_or_default()
}

/// ` [phase] label`, as much of it as the run has.
fn tag(request: &Request) -> String {
    match (&request.phase, &request.label) {
        (Some(p), Some(l)) => format!(" [{p}] {l}"),
        (Some(p), None) => format!(" [{p}]"),
        (None, Some(l)) => format!(" {l}"),
        (None, None) => String::new(),
    }
}

fn report_start(mode: Progress, id: &str, request: &Request) {
    match mode {
        Progress::Text => eprintln!(
            "timon: {id}{} started: {}{}",
            tag(request),
            request.provider.name(),
            request
                .model
                .as_deref()
                .map(|m| format!(" {m}"))
                .unwrap_or_default()
        ),
        Progress::Json => eprintln!(
            "{}",
            serde_json::json!({
                "event": "run_started", "id": id, "label": request.label, "phase": request.phase,
                "provider": request.provider.name(), "model": request.model,
            })
        ),
        Progress::Off => {}
    }
}

fn report_end(mode: Progress, record: &Record, request: &Request) {
    match mode {
        Progress::Text => eprintln!(
            "timon: {}{} {} in {:.1} s{}",
            record.id,
            tag(request),
            record.status.name(),
            record.ms as f64 / 1000.0,
            record
                .usage
                .usd
                .map(|u| format!(", ${u:.4}"))
                .unwrap_or_default()
        ),
        Progress::Json => eprintln!(
            "{}",
            serde_json::json!({
                "event": "run_finished", "id": record.id, "label": request.label,
                "phase": request.phase, "status": record.status.name(), "ms": record.ms,
                "usd": record.usage.usd,
            })
        ),
        Progress::Off => {}
    }
}

/// Limits given on the command line; they win over init.lua.
pub struct Overrides {
    pub max_runs: Option<u64>,
    pub max_usd: Option<f64>,
}

/// Registry key of a table from each record `agent.run` returned to the spec it was called
/// with, weak in its keys, so `agent.continue` can start from the same request.
const RUN_SPECS: &str = "timon.run_specs";

fn run_specs(lua: &Lua) -> mlua::Result<Table> {
    if let Some(t) = lua.named_registry_value::<Option<Table>>(RUN_SPECS)? {
        return Ok(t);
    }
    let t = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.raw_set("__mode", "k")?;
    t.set_metatable(Some(mt))?;
    lua.set_named_registry_value(RUN_SPECS, &t)?;
    Ok(t)
}

/// Remembers a copy of `spec` as the call that produced `record`.
fn remember_spec(lua: &Lua, record: &Table, spec: &Table) -> mlua::Result<()> {
    let copy = lua.create_table()?;
    for pair in spec.pairs::<Value, Value>() {
        let (k, v) = pair?;
        copy.raw_set(k, v)?;
    }
    run_specs(lua)?.raw_set(record, copy)
}

const GLUE: &str = r#"
local run_rust, ask_rust, spawn, fail, spec_of, phase_of = ...
local type, error, pairs, running = type, error, pairs, coroutine.running
local agent = {}

-- A run without a phase takes the calling task's.
local function with_phase(spec)
  if type(spec) ~= "table" or spec.phase ~= nil then return spec end
  local p = phase_of()
  if p == nil then return spec end
  local s = {}
  for k, v in pairs(spec) do s[k] = v end
  s.phase = p
  return s
end

-- The caller's coroutine names its task, whose budgets the run counts against.
local function run(spec)
  return run_rust(with_phase(spec), (running()))
end

agent.run = run

function agent.ask(arg)
  if type(arg) == "string" then arg = { prompt = arg } end
  return ask_rust(with_phase(arg), (running()))
end

-- Fields of the earlier call that do not carry over to a continuation.
local fresh = { prompt = true, resume = true, fork = true, worktree = true }

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
  -- Start from the earlier call, in its worktree when it kept one; spec overrides.
  local before = spec_of(record)
  if before then
    if before.ephemeral then
      error(fail("no_session", "that run was ephemeral, so it left no session to continue",
        "run it without ephemeral = true to continue it"), 2)
    end
    for k, v in pairs(before) do
      if not fresh[k] then s[k] = v end
    end
    if type(record.worktree) == "table" and record.worktree.kept then
      s.cwd = record.worktree.path
    end
  end
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
    req.label = get("label")?;
    req.phase = get("phase")?;
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
    req.idempotent = fields
        .raw_get::<Option<bool>>("idempotent")?
        .unwrap_or(false);
    req.worktree = fields.raw_get::<Option<bool>>("worktree")?.unwrap_or(false);
    let output: Value = fields.raw_get("output")?;
    if let Value::String(name) = &output {
        req.contract = Some(name.to_string_lossy());
    }
    if !output.is_nil() {
        req.output = Some(schema::resolve(lua, &output)?);
    }
    Ok(req)
}

/// The record as a Lua table: JSON nulls become nil, and the answer keeps its JSON shape.
/// An earlier run's stored record, marked `cached`, for a resumed script. Its text is the
/// redacted text the record kept.
fn cached_record(lua: &Lua, runs: &std::path::Path, id: &str) -> mlua::Result<Option<Table>> {
    let Ok(text) = std::fs::read_to_string(runs.join(id).join("result.json")) else {
        return Ok(None);
    };
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(None);
    };
    json["cached"] = serde_json::Value::Bool(true);
    let Value::Table(t) = value::from_json(lua, &json)? else {
        return Ok(None);
    };
    for key in ["answer", "events"] {
        if !json[key].is_null() {
            t.raw_set(key, json_module::decode(lua, &json[key])?)?;
        }
    }
    Ok(Some(t))
}

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
    progress: Progress,
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
        report_end(self.progress, record, &self.request);
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

/// Refuses to resume a session no recorded run started, so a script never continues a
/// person's own sessions.
fn check_started(lua: &Lua, session: &str, provider: agent::Provider) -> Result<(), Error> {
    let started = lua
        .named_registry_value::<Option<String>>(STATE_DIR)?
        .is_some_and(|state| {
            agent::records::Store::new(std::path::Path::new(&state))
                .sessions()
                .contains(session)
        });
    if started {
        return Ok(());
    }
    let hint = match provider {
        agent::Provider::Claude => "fork = ID branches from any session and leaves it untouched",
        agent::Provider::Codex => "start a new run: codex cannot fork a session",
    };
    Err(Error::new(
        "unknown_session",
        format!("timon did not start session \"{session}\", so it cannot resume it"),
    )
    .with_hint(hint))
}

/// Runs one agent turn for the task on `thread`, charging that task's budgets.
async fn run(lua: Lua, spec: Table, thread: Option<mlua::Thread>) -> mlua::Result<Table> {
    Ok(run_with(lua, spec, thread).await?.0)
}

/// [`run`], and whether the merged request has an output, so the record has an answer.
async fn run_with(
    lua: Lua,
    spec: Table,
    thread: Option<mlua::Thread>,
) -> mlua::Result<(Table, bool)> {
    crate::tasks::refuse_while_loading(&lua, "an agent run")?;
    options::check("agent.run", &spec)?;
    let fields = merged(&lua, &spec)?;
    let request = request(&lua, &fields)?;
    let wants_answer = request.output.is_some();
    agent::check(&request)?;
    if let Some(session) = &request.resume {
        check_started(&lua, session, request.provider)?;
    }
    // In a script run, the run gets a key, and a resumed script reuses a matching earlier run.
    let script = lua.app_data_ref::<Rc<ScriptRun>>().map(|s| s.clone());
    let mut request = request;
    if let Some(script) = &script {
        request.parent = Some(script.id.clone());
        let key = script.key(&request.hash());
        if let Some(earlier) = script.cached(&key).map(str::to_owned)
            && let Some(table) = cached_record(&lua, script.runs_dir(), &earlier)?
        {
            script.add(&serde_json::json!({
                "id": earlier, "key": key, "status": "ok", "cached": true,
            }));
            remember_spec(&lua, &table, &spec)?;
            return Ok((table, wants_answer));
        }
        request.key = Some(key);
    }
    let permits = permits(&lua)?;
    let _permit = permits
        .acquire()
        .await
        .map_err(|_| Error::new("internal", "the run queue closed"))?;
    let budget = concurrency::budget_of(&lua, thread.as_ref())?;
    // A Claude run holds what it may spend, so concurrent runs never share the money left.
    let claude = request.provider == Provider::Claude;
    let hold = budget.admit(claude, request.budget.usd).await?;
    let id = agent::new_id();
    let state = lua.named_registry_value::<Option<String>>(STATE_DIR)?;
    if claude && let Some(usd) = hold.usd() {
        request.budget.usd = Some(usd);
    }
    let worktree = if request.worktree {
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
    let mut on_spawn: Option<agent::SpawnHook> = None;
    if let Some(store) = &store {
        match store.begin(
            &request,
            &id,
            &started_at,
            agent_name.as_deref(),
            worktree.as_ref().map(|w| w.to_json(true)).as_ref(),
        ) {
            Ok(line) => {
                // Name the provider process in the record, so it can be stopped if timon dies.
                let records =
                    agent::records::Store::new(store.dir().parent().unwrap_or(store.dir()));
                on_spawn = Some(std::sync::Arc::new(move |pid, pgid| {
                    if let Err(err) = records.spawned(&line, pid, pgid) {
                        eprintln!("timon: the provider's pid was not recorded: {err}");
                    }
                }));
            }
            Err(err) => eprintln!("timon: the run record was not started: {err}"),
        }
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
        progress: progress(&lua),
    };
    report_start(settle.progress, &id, &request);
    let mut record = agent::run_as(&request, id, on_spawn).await;
    settle.finish(&mut record);
    hold.settle(record.usage.usd.unwrap_or(0.0));
    if let Some(script) = &script {
        script.add(&serde_json::json!({
            "id": record.id, "key": request.key, "status": record.status.name(),
            "usd": record.usage.usd, "input_tokens": record.usage.input_tokens,
            "output_tokens": record.usage.output_tokens, "cached": false,
        }));
    }
    let table = record_table(&lua, &record)?;
    remember_spec(&lua, &table, &spec)?;
    Ok((table, wants_answer))
}

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let module = installer.module(&DOC)?;
    let run_fn = installer.wrap(lua.create_async_function(
        |lua, (spec, thread): (Table, Option<mlua::Thread>)| run(lua, spec, thread),
    )?)?;
    let ask_fn = installer.wrap(lua.create_async_function(
        |lua, (arg, thread): (Value, Option<mlua::Thread>)| async move {
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
            let (record, wants_answer) = run_with(lua.clone(), spec, thread).await?;
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
        },
    )?)?;
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
            let mut parent: Option<String> = None;
            if let Some(opts) = &opts {
                parent = opts.raw_get("parent")?;
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
            let entries = agent::records::Store::new(std::path::Path::new(&state)).list_in(
                since,
                parent.as_deref(),
                limit,
            )?;
            json_module::decode(lua, &serde_json::Value::Array(entries))
        })?,
    )?;
    let spawn: Function = lua.globals().raw_get("spawn")?;
    let phase_of: Function = lua.named_registry_value(concurrency::PHASE_OF)?;
    let helpers: Table = lua.load(GLUE).set_name(INTERNAL_CHUNK).call((
        run_fn,
        ask_fn,
        spawn,
        fail_function(lua)?,
        lua.create_function(|lua, record: Value| match record {
            Value::Table(t) => run_specs(lua)?.raw_get::<Value>(t),
            _ => Ok(Value::Nil),
        })?,
        phase_of,
    ))?;
    for name in ["run", "ask", "spawn", "continue"] {
        let f: Function = helpers.raw_get(name)?;
        installer.lua_function(&DOC, Some(&module), name, f)?;
    }
    Ok(())
}
