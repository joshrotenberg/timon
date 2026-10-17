//! Structured concurrency: `scope`, `spawn`, `sleep` and `par`.
//!
//! Every task runs in a scope, and a scope does not end until its tasks have. A task is a Lua
//! thread driven as a `spawn_local` future, so tasks interleave at await points on one thread.
//! A task that raises fails its scope: the scope cancels the rest, waits for them to stop, and
//! raises the first error. The main chunk of every run is itself a scope.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

use mlua::{
    Function, Lua, MetaMethod, MultiValue, Table, UserData, UserDataMethods, Value, WeakLua,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::budget::Budget;
use crate::docs::{Func, Module, Opt};
use crate::error::{self, Error};
use crate::modules::{GLOBALS_DOC, Installer, fail_function};
use crate::options;
use crate::runtime::{INTERNAL_CHUNK, RUN};

const SOURCE: &str = include_str!("par.lua");

pub const DOC: Module = Module {
    name: "par",
    summary: "Run functions concurrently. Each call is a scope: an error cancels the rest and is raised.",
    functions: &[
        Func::new(
            "map",
            "par.map(items, f, { limit, settle }) -> array",
            "Calls f(item, i) for every item, at most limit at a time, and returns the results in input order. With settle, failures do not cancel the rest: each result is { ok = true, value = v } or { ok = false, error = err }.",
            "local reviews = par.map(prs, function(pr)\n  return agent.run{ agent = \"reviewer\", prompt = \"Review PR \" .. pr }\nend, { limit = 3 })",
        )
        .with_options(&[
            Opt::new("limit", "integer", "most calls running at once (default: all)"),
            Opt::new("settle", "boolean", "collect failures as results instead of raising"),
        ]),
        Func::new(
            "all",
            "par.all(f1, f2, ...) -> r1, r2, ...",
            "Runs the functions concurrently and returns the first result of each, in order.",
            "local log, status = par.all(\n  function() return sh(\"git log -5\") end,\n  function() return sh(\"git status\") end\n)",
        ),
        Func::new(
            "race",
            "par.race(f1, f2, ...) -> result, i",
            "Runs the functions concurrently, returns the first result to finish and its position, and cancels the others.",
            "local answer, i = par.race(\n  function() return agent.ask{ provider = \"claude\", prompt = q } end,\n  function() return agent.ask{ provider = \"codex\", prompt = q } end\n)",
        ),
        Func::new(
            "first",
            "par.first(tasks) -> i",
            "Waits until one of tasks has finished and returns its position. Unlike par.race it cancels nothing, so the rest keep running: remove the finished task and call it again to take results as they arrive. A task that raises fails its scope as usual.",
            "local tasks = tbl.map(prs, function(pr) return agent.spawn{ prompt = \"Review PR \" .. pr } end)\nwhile #tasks > 0 do\n  local r = table.remove(tasks, par.first(tasks)):await()\n  print(r.status, r.text)\nend",
        ),
    ],
};

/// A spawned task before and while it runs.
struct Pending {
    scope: Weak<ScopeInner>,
    /// Its Lua thread, once the task has entered.
    thread: Option<usize>,
    /// The phase it starts with, inherited from whoever spawned it.
    phase: Option<String>,
    /// Whether it runs the scope's function, rather than a task spawned on the scope.
    body: bool,
}

/// Per-runtime state, kept in Lua app data.
struct State {
    /// The scope each running task's Lua thread belongs to, for the global `spawn`.
    threads: RefCell<HashMap<usize, Weak<ScopeInner>>>,
    /// The phase of each running task's Lua thread, set by `phase(name)` or inherited.
    phases: RefCell<HashMap<usize, String>>,
    /// Tasks that have been spawned and not yet ended.
    tokens: RefCell<HashMap<u64, Pending>>,
    next_token: Cell<u64>,
    /// Notified whenever any task finishes.
    changed: Rc<Notify>,
}

/// Registry key of the internal function every task starts in.
const TASK_MAIN: &str = "timon.task_main";

/// The first thing a task runs: it records its own thread for the global `spawn`, then calls
/// the task's function. Tasks run through `Function::call_async`, so mlua recycles their
/// threads and, when a task is cancelled, closes its pending to-be-closed variables.
///
/// The thread comes from `coroutine.running()`: inside a callback, `Lua::current_thread`
/// does not name the coroutine that called it, so every task looked like the same thread.
const TASK_MAIN_SOURCE: &str = r#"
local enter, running = ...
return function(token, f, ...)
  enter(token, (running()))
  return f(...)
end
"#;

/// The global `spawn`: it passes its caller's coroutine, which picks the scope.
const SPAWN_SOURCE: &str = r#"
local spawn_from, running, fail = ...
local type = type
return function(f, ...)
  if type(f) ~= "function" then
    error(fail("bad_argument",
      "spawn: argument 1 must be a function, not " .. type(f)), 2)
  end
  return spawn_from((running()), f, ...)
end
"#;

fn thread_key(thread: &mlua::Thread) -> usize {
    Value::Thread(thread.clone()).to_pointer() as usize
}

fn phase_of_key(lua: &Lua, key: usize) -> Option<String> {
    lua.app_data_ref::<State>()
        .and_then(|state| state.phases.borrow().get(&key).cloned())
}

/// Registry key of a function returning the calling task's phase, for modules written in Lua.
pub(crate) const PHASE_OF: &str = "timon.phase_of";

/// `scope`, `phase` and the phase reader pass their caller's coroutine, as `spawn` does.
const PHASE_SOURCE: &str = r##"
local scope_from, phase_set, phase_get, running = ...
local function scope(f, opts)
  return scope_from((running()), f, opts)
end
local function phase(...)
  if select("#", ...) == 0 then
    return phase_get((running()))
  end
  return phase_set((running()), ...)
end
local function phase_of()
  return phase_get((running()))
end
return scope, phase, phase_of
"##;

/// Records `thread` as belonging to the scope a task was spawned on.
fn enter(lua: &Lua, (token, thread): (u64, mlua::Thread)) -> mlua::Result<()> {
    let key = thread_key(&thread);
    if let Some(state) = lua.app_data_ref::<State>() {
        let mut tokens = state.tokens.borrow_mut();
        if let Some(pending) = tokens.get_mut(&token) {
            pending.thread = Some(key);
            state
                .threads
                .borrow_mut()
                .insert(key, pending.scope.clone());
            if let Some(phase) = &pending.phase {
                state.phases.borrow_mut().insert(key, phase.clone());
            }
            if pending.body
                && let Some(scope) = pending.scope.upgrade()
            {
                scope.body_thread.set(Some(key));
            }
        }
    }
    Ok(())
}

fn changed(lua: &Lua) -> Rc<Notify> {
    match lua.app_data_ref::<State>() {
        Some(state) => state.changed.clone(),
        None => Rc::new(Notify::new()),
    }
}

enum TaskState {
    Running,
    Done(Vec<Value>),
    Failed(Value),
    Cancelled,
}

struct TaskInner {
    state: RefCell<TaskState>,
    join: RefCell<Option<JoinHandle<()>>>,
    /// The runtime's change signal, notified when the task finishes or is cancelled.
    changed: Rc<Notify>,
}

impl TaskInner {
    fn finished(&self) -> bool {
        !matches!(*self.state.borrow(), TaskState::Running)
    }

    /// Marks the task cancelled and aborts its future. An aborted future never reaches its own
    /// notification, so this wakes whoever is already awaiting the task.
    fn cancel(&self) {
        let running = !self.finished();
        if running {
            *self.state.borrow_mut() = TaskState::Cancelled;
        }
        if let Some(join) = self.join.borrow().as_ref() {
            join.abort();
        }
        if running {
            self.changed.notify_waiters();
        }
    }

    /// Waits until the task's future has completed or been dropped.
    async fn stopped(&self) {
        let join = self.join.borrow_mut().take();
        if let Some(join) = join {
            let _ = join.await;
        }
    }
}

#[derive(Default)]
struct ScopeInner {
    children: RefCell<Vec<Rc<TaskInner>>>,
    first_error: RefCell<Option<Value>>,
    ended: Cell<bool>,
    /// The phase of the task that made the scope.
    phase: RefCell<Option<String>>,
    /// The thread running the scope's function, once it has entered.
    body_thread: Cell<Option<usize>>,
    /// The budget runs inside the scope count against: its own, or the one it was made in.
    budget: RefCell<Option<Rc<Budget>>>,
    /// Whether `budget` is the scope's own, to give back unused reserved runs when it ends.
    own_budget: Cell<bool>,
    /// Run when the scope ends, however it ends: stopping processes it started, and the like.
    cleanups: RefCell<Vec<Box<dyn FnOnce()>>>,
}

impl ScopeInner {
    /// Runs the scope's cleanups, newest first, then gives back its reserved runs.
    fn end(&self) {
        let cleanups = std::mem::take(&mut *self.cleanups.borrow_mut());
        for cleanup in cleanups.into_iter().rev() {
            cleanup();
        }
        self.release_budget();
    }

    fn release_budget(&self) {
        if self.own_budget.get()
            && let Some(b) = self.budget.borrow().as_ref()
        {
            b.release();
        }
    }
}

/// Ends a scope when its run ends, however it ends: cleanups, then reserved runs.
struct EndScope(Rc<ScopeInner>);

impl Drop for EndScope {
    fn drop(&mut self) {
        self.0.end();
    }
}

/// Registers `cleanup` to run when the scope of the task on `thread` ends. Returns false, and
/// drops nothing, when the thread belongs to no scope.
pub(crate) fn on_scope_end(lua: &Lua, thread: &mlua::Thread, cleanup: Box<dyn FnOnce()>) -> bool {
    let key = thread_key(thread);
    let scope = lua
        .app_data_ref::<State>()
        .and_then(|state| state.threads.borrow().get(&key).and_then(Weak::upgrade));
    match scope {
        Some(scope) => {
            scope.cleanups.borrow_mut().push(cleanup);
            true
        }
        None => false,
    }
}

/// The budget an agent run started from `thread` counts against: its scope's, or the
/// invocation's when the thread belongs to no scope.
pub(crate) fn budget_of(lua: &Lua, thread: Option<&mlua::Thread>) -> mlua::Result<Rc<Budget>> {
    let from_scope = thread.and_then(|t| {
        let key = thread_key(t);
        let state = lua.app_data_ref::<State>()?;
        let scope = state.threads.borrow().get(&key).and_then(Weak::upgrade)?;
        scope.budget.borrow().clone()
    });
    match from_scope {
        Some(b) => Ok(b),
        None => crate::budget::invocation(lua),
    }
}

impl ScopeInner {
    /// The phase a task spawned on this scope with `s:spawn` starts with: the current phase of
    /// the scope's function, or the phase the scope was made in.
    fn spawn_phase(&self, lua: &Lua) -> Option<String> {
        self.body_thread
            .get()
            .and_then(|key| phase_of_key(lua, key))
            .or_else(|| self.phase.borrow().clone())
    }

    fn failure(&self) -> Option<Value> {
        self.first_error.borrow().clone()
    }

    fn all_finished(&self) -> bool {
        self.children.borrow().iter().all(|t| t.finished())
    }

    fn cancel_children(&self) {
        for task in self.children.borrow().iter() {
            task.cancel();
        }
    }

    /// Cancels every running child and waits for all of them to stop.
    async fn stop_children(&self) {
        self.cancel_children();
        self.join_children().await;
    }

    async fn join_children(&self) {
        let children: Vec<Rc<TaskInner>> = self.children.borrow().clone();
        for task in children {
            task.stopped().await;
        }
    }

    /// Waits for every child, or for the first failure, which cancels the rest.
    async fn wait(&self, changed: &Notify) -> Result<(), Value> {
        loop {
            if let Some(err) = self.failure() {
                self.stop_children().await;
                return Err(err);
            }
            if self.all_finished() {
                self.join_children().await;
                return Ok(());
            }
            changed.notified().await;
        }
    }
}

/// Forgets a task's thread when the task's future ends or is dropped.
struct Registration {
    lua: WeakLua,
    token: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(lua) = self.lua.try_upgrade()
            && let Some(state) = lua.app_data_ref::<State>()
            && let Some(Pending {
                thread: Some(key), ..
            }) = state.tokens.borrow_mut().remove(&self.token)
        {
            state.threads.borrow_mut().remove(&key);
            state.phases.borrow_mut().remove(&key);
        }
    }
}

/// Starts `f(args)` as a task. Children belong to the scope and fail it when they raise; the
/// body of a scope is not a child.
fn spawn_task(
    lua: &Lua,
    scope: &Rc<ScopeInner>,
    f: Function,
    args: MultiValue,
    child: bool,
    phase: Option<String>,
) -> mlua::Result<Rc<TaskInner>> {
    if child && scope.ended.get() {
        return Err(
            Error::new("scope_ended", "cannot spawn on a scope that has ended")
                .with_hint("spawn inside the scope's function, before it returns")
                .into(),
        );
    }
    let run: Function = lua.named_registry_value(RUN)?;
    let task_main: Function = lua.named_registry_value(TASK_MAIN)?;
    let token = match lua.app_data_ref::<State>() {
        Some(state) => {
            let token = state.next_token.get();
            state.next_token.set(token + 1);
            state.tokens.borrow_mut().insert(
                token,
                Pending {
                    scope: Rc::downgrade(scope),
                    thread: None,
                    phase,
                    body: !child,
                },
            );
            token
        }
        None => 0,
    };
    let call: MultiValue = [
        Value::Function(task_main),
        Value::Integer(token as i64),
        Value::Function(f),
    ]
    .into_iter()
    .chain(args)
    .collect();
    let future = run.call_async::<MultiValue>(call);

    let notify = changed(lua);
    let task = Rc::new(TaskInner {
        state: RefCell::new(TaskState::Running),
        join: RefCell::new(None),
        changed: notify.clone(),
    });
    let (this, owner, lua_handle) = (task.clone(), scope.clone(), lua.clone());
    let registration = Registration {
        lua: lua.weak(),
        token,
    };
    let join = tokio::task::spawn_local(async move {
        let _registration = registration;
        let next = match future.await {
            Ok(mut values) => match values.pop_front() {
                Some(Value::Boolean(true)) => TaskState::Done(values.into_iter().collect()),
                _ => TaskState::Failed(values.pop_front().unwrap_or(Value::Nil)),
            },
            Err(err) => TaskState::Failed(
                error::normalize(&lua_handle, Value::Error(Box::new(err)))
                    .map(Value::Table)
                    .unwrap_or(Value::Nil),
            ),
        };
        if child && let TaskState::Failed(err) = &next {
            owner
                .first_error
                .borrow_mut()
                .get_or_insert_with(|| err.clone());
        }
        {
            let mut state = this.state.borrow_mut();
            if matches!(*state, TaskState::Running) {
                *state = next;
            }
        }
        notify.notify_waiters();
    });
    *task.join.borrow_mut() = Some(join);
    if child {
        scope.children.borrow_mut().push(task.clone());
    }
    Ok(task)
}

/// Aborts a scope's tasks if the future running it is dropped, as on Ctrl-C.
struct ScopeGuard {
    scope: Rc<ScopeInner>,
    body: Option<Rc<TaskInner>>,
    done: bool,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        if !self.done {
            if let Some(body) = &self.body {
                body.cancel();
            }
            self.scope.cancel_children();
            self.scope.ended.set(true);
        }
    }
}

fn cancelled(lua: &Lua, what: &str) -> Value {
    error::to_table(
        lua,
        &Error::new("cancelled", format!("the {what} was cancelled")),
    )
    .map(Value::Table)
    .unwrap_or(Value::Nil)
}

/// Runs `f(args)` as the body of `scope` and returns its values once every child has finished,
/// or the first error from the body or a child, after cancelling the rest.
async fn scope_run(
    lua: &Lua,
    scope: Rc<ScopeInner>,
    f: Function,
    args: MultiValue,
) -> Result<MultiValue, Value> {
    let notify = changed(lua);
    let _end = EndScope(scope.clone());
    let mut guard = ScopeGuard {
        scope: scope.clone(),
        body: None,
        done: false,
    };
    let phase = scope.phase.borrow().clone();
    let body = spawn_task(lua, &scope, f, args, false, phase).map_err(|e| {
        error::normalize(lua, Value::Error(Box::new(e)))
            .map(Value::Table)
            .unwrap_or(Value::Nil)
    })?;
    guard.body = Some(body.clone());

    while !body.finished() {
        if let Some(err) = scope.failure() {
            body.cancel();
            body.stopped().await;
            scope.stop_children().await;
            scope.ended.set(true);
            guard.done = true;
            return Err(err);
        }
        notify.notified().await;
    }
    body.stopped().await;
    let result = std::mem::replace(&mut *body.state.borrow_mut(), TaskState::Cancelled);
    let outcome = match result {
        TaskState::Done(values) => scope
            .wait(&notify)
            .await
            .map(|()| values.into_iter().collect()),
        TaskState::Failed(err) => {
            scope.stop_children().await;
            Err(err)
        }
        TaskState::Cancelled | TaskState::Running => {
            scope.stop_children().await;
            Err(cancelled(lua, "scope"))
        }
    };
    scope.ended.set(true);
    guard.done = true;
    outcome
}

/// Runs a chunk's function as the root scope of a run.
pub(crate) async fn run_root(
    lua: &Lua,
    f: Function,
    args: MultiValue,
) -> Result<MultiValue, Value> {
    let scope = Rc::new(ScopeInner::default());
    let budget = crate::budget::invocation(lua).map_err(|e| {
        error::normalize(lua, Value::Error(Box::new(e)))
            .map(Value::Table)
            .unwrap_or(Value::Nil)
    })?;
    *scope.budget.borrow_mut() = Some(budget);
    scope_run(lua, scope, f, args).await
}

/// A scope, as Lua sees it: `s:spawn(f, ...)`, `s:cancel()`, and `__close` for the
/// `local s <close> = scope()` form.
struct ScopeHandle(Rc<ScopeInner>);

impl UserData for ScopeHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("spawn", |lua, this, (f, args): (Function, MultiValue)| {
            let phase = this.0.spawn_phase(lua);
            Ok(TaskHandle(spawn_task(lua, &this.0, f, args, true, phase)?))
        });
        methods.add_method("cancel", |_, this, ()| {
            this.0.cancel_children();
            Ok(())
        });
        methods.add_async_meta_method(MetaMethod::Close, |lua, this, err: Value| {
            let scope = this.0.clone();
            async move {
                let notify = changed(&lua);
                let result = if err.is_nil() {
                    scope.wait(&notify).await
                } else {
                    scope.stop_children().await;
                    Ok(())
                };
                scope.ended.set(true);
                scope.end();
                result.map_err(|e| error::raise(&lua, e))
            }
        });
    }
}

/// A task, as Lua sees it: `h:await()`, `h:cancel()`, `h:done()`.
struct TaskHandle(Rc<TaskInner>);

impl UserData for TaskHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method("await", |lua, this, ()| {
            let task = this.0.clone();
            async move { await_task(&lua, &task).await }
        });
        methods.add_method("cancel", |_, this, ()| {
            this.0.cancel();
            Ok(())
        });
        methods.add_method("done", |_, this, ()| Ok(this.0.finished()));
    }
}

async fn await_task(lua: &Lua, task: &TaskInner) -> mlua::Result<MultiValue> {
    let notify = changed(lua);
    loop {
        match &*task.state.borrow() {
            TaskState::Done(values) => return Ok(values.iter().cloned().collect()),
            TaskState::Failed(err) => return Err(error::raise(lua, err.clone())),
            TaskState::Cancelled => return Err(error::raise(lua, cancelled(lua, "task"))),
            TaskState::Running => {}
        }
        notify.notified().await;
    }
}

/// Parses a duration: a number of seconds, or a string such as "250ms", "2s", "1.5m", "1h".
pub(crate) fn duration(value: &Value) -> Result<Duration, Error> {
    let seconds = match value {
        Value::Integer(i) => *i as f64,
        Value::Number(n) => *n,
        Value::String(s) => return parse_duration(&s.to_string_lossy()),
        _ => return Err(bad_duration()),
    };
    // A negative, infinite or too large number is an error, not a panic (#131).
    Duration::try_from_secs_f64(seconds).map_err(|_| bad_duration())
}

fn bad_duration() -> Error {
    Error::new(
        "bad_argument",
        "a duration is a number of seconds or a string like \"250ms\", \"2s\" or \"1d\"",
    )
}

/// Parses "250ms", "2s", "1.5m", "1h", "1d", or a bare number of seconds.
pub fn parse_duration(text: &str) -> Result<Duration, Error> {
    let s = text.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (number, unit) = s.split_at(split);
    let n: f64 = number.parse().map_err(|_| bad_duration())?;
    let scale = match unit.trim() {
        "ms" => 0.001,
        "" | "s" => 1.0,
        "m" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        _ => return Err(bad_duration()),
    };
    Duration::try_from_secs_f64(n * scale).map_err(|_| bad_duration())
}

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    lua.set_app_data(State {
        threads: RefCell::new(HashMap::new()),
        phases: RefCell::new(HashMap::new()),
        tokens: RefCell::new(HashMap::new()),
        next_token: Cell::new(1),
        changed: Rc::new(Notify::new()),
    });
    let coroutine: Table = lua.globals().raw_get("coroutine")?;
    let running: Function = coroutine.raw_get("running")?;
    let task_main: Function = lua
        .load(TASK_MAIN_SOURCE)
        .set_name(INTERNAL_CHUNK)
        .call((lua.create_function(enter)?, running.clone()))?;
    lua.set_named_registry_value(TASK_MAIN, task_main)?;

    let scope_from = installer.wrap(lua.create_async_function(
        |lua, (thread, f, opts): (mlua::Thread, Option<Function>, Option<Table>)| async move {
            options::check_opt("scope", opts.as_ref())?;
            let inner = Rc::new(ScopeInner::default());
            *inner.phase.borrow_mut() = phase_of_key(&lua, thread_key(&thread));
            let outer = budget_of(&lua, Some(&thread))?;
            let own = match &opts {
                Some(o) => match o.raw_get::<Option<Table>>("budget")? {
                    Some(b) => Some(Budget::child(
                        &outer,
                        b.raw_get::<Option<i64>>("runs")?.map(|n| n.max(0) as u64),
                        b.raw_get::<Option<f64>>("usd")?,
                        o.raw_get::<Option<bool>>("reserve")?.unwrap_or(false),
                    )?),
                    None if o.raw_get::<Option<bool>>("reserve")? == Some(true) => {
                        return Err(Error::new("bad_option", "reserve = true needs budget.runs")
                            .with_hint("scope(fn, { budget = { runs = N }, reserve = true })")
                            .into());
                    }
                    None => None,
                },
                None => None,
            };
            inner.own_budget.set(own.is_some());
            *inner.budget.borrow_mut() = Some(own.unwrap_or(outer));
            let handle = lua.create_userdata(ScopeHandle(inner.clone()))?;
            let Some(f) = f else {
                return Ok(MultiValue::from_iter([Value::UserData(handle)]));
            };
            scope_run(
                &lua,
                inner,
                f,
                MultiValue::from_iter([Value::UserData(handle)]),
            )
            .await
            .map_err(|err| error::raise(&lua, err))
        },
    )?)?;
    let spawn_from = installer.wrap(lua.create_function(
        |lua, (thread, f, args): (mlua::Thread, Function, MultiValue)| {
            let key = thread_key(&thread);
            let scope = lua
                .app_data_ref::<State>()
                .and_then(|state| state.threads.borrow().get(&key).and_then(Weak::upgrade));
            let Some(scope) = scope else {
                return Err(Error::new("no_scope", "spawn was called outside a scope")
                    .with_hint("call s:spawn on a scope from scope(function(s) ... end)")
                    .into());
            };
            let phase = phase_of_key(lua, key);
            Ok(TaskHandle(spawn_task(lua, &scope, f, args, true, phase)?))
        },
    )?)?;
    let spawn: Function = lua.load(SPAWN_SOURCE).set_name(INTERNAL_CHUNK).call((
        spawn_from,
        running.clone(),
        fail_function(lua)?,
    ))?;
    installer.lua_function(&GLOBALS_DOC, None, "spawn", spawn)?;
    let phase_set = installer.wrap(lua.create_function(
        |lua, (thread, name): (mlua::Thread, Value)| {
            let name = match name {
                Value::Nil => None,
                Value::String(s) => Some(s.to_string_lossy()),
                other => {
                    return Err(Error::new(
                        "bad_argument",
                        format!("phase takes a name or nil, not a {}", other.type_name()),
                    )
                    .into());
                }
            };
            let key = thread_key(&thread);
            let Some(state) = lua.app_data_ref::<State>() else {
                return Ok(None);
            };
            let mut phases = state.phases.borrow_mut();
            Ok(match name {
                Some(name) => phases.insert(key, name),
                None => phases.remove(&key),
            })
        },
    )?)?;
    let phase_get = lua
        .create_function(|lua, thread: mlua::Thread| Ok(phase_of_key(lua, thread_key(&thread))))?;
    let (scope, phase, phase_of): (Function, Function, Function) = lua
        .load(PHASE_SOURCE)
        .set_name(INTERNAL_CHUNK)
        .call((scope_from, phase_set, phase_get, running))?;
    installer.lua_function(&GLOBALS_DOC, None, "scope", scope.clone())?;
    installer.lua_function(&GLOBALS_DOC, None, "phase", phase)?;
    lua.set_named_registry_value(PHASE_OF, phase_of)?;
    installer.function(
        &GLOBALS_DOC,
        None,
        "sleep",
        lua.create_async_function(|_, d: Value| async move {
            tokio::time::sleep(duration(&d)?).await;
            Ok(())
        })?,
    )?;

    let check = lua.create_function(|_, (path, opts): (String, Option<Table>)| {
        Ok(options::check_opt(&path, opts.as_ref())?)
    })?;
    let first = lua.create_async_function(|lua, handles: Table| async move {
        let notify = changed(&lua);
        let not_a_task = || {
            Error::new(
                "bad_argument",
                "par.first takes a list of tasks, from spawn, s:spawn or agent.spawn",
            )
        };
        let tasks: Vec<Rc<TaskInner>> = handles
            .sequence_values::<Value>()
            .map(|h| match h? {
                Value::UserData(ud) => Ok(ud
                    .borrow::<TaskHandle>()
                    .map_err(|_| not_a_task())?
                    .0
                    .clone()),
                _ => Err(not_a_task().into()),
            })
            .collect::<mlua::Result<_>>()?;
        if tasks.is_empty() {
            return Err(Error::new("bad_argument", "par.first needs at least one task").into());
        }
        loop {
            if let Some(i) = tasks.iter().position(|t| t.finished()) {
                return Ok(i + 1);
            }
            notify.notified().await;
        }
    })?;
    let json: Table = lua.globals().raw_get("json")?;
    let array: Function = json.raw_get("array")?;
    let try_fn: Function = lua.globals().raw_get("try")?;
    let functions: Table = lua.load(SOURCE).set_name(INTERNAL_CHUNK).call((
        scope,
        try_fn,
        array,
        installer.wrap(check)?,
        installer.wrap(first)?,
        fail_function(lua)?,
    ))?;
    let par = installer.module(&DOC)?;
    for func in DOC.functions {
        let f: Function = functions.raw_get(func.name)?;
        installer.lua_function(&DOC, Some(&par), func.name, f)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        let lua = Lua::new();
        let s = |text: &str| Value::String(lua.create_string(text).unwrap());
        assert_eq!(duration(&s("250ms")).unwrap(), Duration::from_millis(250));
        assert_eq!(duration(&s("2s")).unwrap(), Duration::from_secs(2));
        assert_eq!(duration(&s("1.5m")).unwrap(), Duration::from_secs(90));
        assert_eq!(
            duration(&Value::Number(0.5)).unwrap(),
            Duration::from_millis(500)
        );
        assert_eq!(duration(&s("soon")).unwrap_err().kind, "bad_argument");
        assert_eq!(
            duration(&Value::Integer(-1)).unwrap_err().kind,
            "bad_argument"
        );
    }
}
