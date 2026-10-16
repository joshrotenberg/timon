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
    ],
};

/// A spawned task's scope, and its thread once the task has entered.
type Pending = (Weak<ScopeInner>, Option<usize>);

/// Per-runtime state, kept in Lua app data.
struct State {
    /// The scope each running task's Lua thread belongs to, for the global `spawn`.
    threads: RefCell<HashMap<usize, Weak<ScopeInner>>>,
    /// Tasks that have been spawned: their scope, and their thread once the task has entered.
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
const TASK_MAIN_SOURCE: &str = r#"
local enter = ...
return function(token, f, ...)
  enter(token)
  return f(...)
end
"#;

/// Records the calling thread as belonging to the scope a task was spawned on.
fn enter(lua: &Lua, token: u64) -> mlua::Result<()> {
    let key = lua.current_thread().to_pointer() as usize;
    if let Some(state) = lua.app_data_ref::<State>() {
        let mut tokens = state.tokens.borrow_mut();
        if let Some((scope, thread)) = tokens.get_mut(&token) {
            *thread = Some(key);
            state.threads.borrow_mut().insert(key, scope.clone());
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
}

impl TaskInner {
    fn finished(&self) -> bool {
        !matches!(*self.state.borrow(), TaskState::Running)
    }

    fn cancel(&self) {
        if !self.finished() {
            *self.state.borrow_mut() = TaskState::Cancelled;
        }
        if let Some(join) = self.join.borrow().as_ref() {
            join.abort();
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
}

impl ScopeInner {
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
            && let Some((_, Some(key))) = state.tokens.borrow_mut().remove(&self.token)
        {
            state.threads.borrow_mut().remove(&key);
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
            state
                .tokens
                .borrow_mut()
                .insert(token, (Rc::downgrade(scope), None));
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

    let task = Rc::new(TaskInner {
        state: RefCell::new(TaskState::Running),
        join: RefCell::new(None),
    });
    let notify = changed(lua);
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
    let mut guard = ScopeGuard {
        scope: scope.clone(),
        body: None,
        done: false,
    };
    let body = spawn_task(lua, &scope, f, args, false).map_err(|e| {
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
    scope_run(lua, Rc::new(ScopeInner::default()), f, args).await
}

/// A scope, as Lua sees it: `s:spawn(f, ...)`, `s:cancel()`, and `__close` for the
/// `local s <close> = scope()` form.
struct ScopeHandle(Rc<ScopeInner>);

impl UserData for ScopeHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("spawn", |lua, this, (f, args): (Function, MultiValue)| {
            Ok(TaskHandle(spawn_task(lua, &this.0, f, args, true)?))
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
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(bad_duration());
    }
    Ok(Duration::from_secs_f64(seconds))
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
    let seconds = n * scale;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(bad_duration());
    }
    Ok(Duration::from_secs_f64(seconds))
}

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    lua.set_app_data(State {
        threads: RefCell::new(HashMap::new()),
        tokens: RefCell::new(HashMap::new()),
        next_token: Cell::new(1),
        changed: Rc::new(Notify::new()),
    });
    let task_main: Function = lua
        .load(TASK_MAIN_SOURCE)
        .set_name(INTERNAL_CHUNK)
        .call(lua.create_function(enter)?)?;
    lua.set_named_registry_value(TASK_MAIN, task_main)?;

    let scope = installer.function(
        &GLOBALS_DOC,
        None,
        "scope",
        lua.create_async_function(|lua, f: Option<Function>| async move {
            let inner = Rc::new(ScopeInner::default());
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
        })?,
    )?;
    installer.function(
        &GLOBALS_DOC,
        None,
        "spawn",
        lua.create_function(|lua, (f, args): (Function, MultiValue)| {
            let key = lua.current_thread().to_pointer() as usize;
            let scope = lua
                .app_data_ref::<State>()
                .and_then(|state| state.threads.borrow().get(&key).and_then(Weak::upgrade));
            let Some(scope) = scope else {
                return Err(Error::new("no_scope", "spawn was called outside a scope")
                    .with_hint("call s:spawn on a scope from scope(function(s) ... end)")
                    .into());
            };
            Ok(TaskHandle(spawn_task(lua, &scope, f, args, true)?))
        })?,
    )?;
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
        let tasks: Vec<Rc<TaskInner>> = handles
            .sequence_values::<mlua::AnyUserData>()
            .map(|h| Ok(h?.borrow::<TaskHandle>()?.0.clone()))
            .collect::<mlua::Result<_>>()?;
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
