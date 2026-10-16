//! The runtime: one Lua state, with user code run as coroutines on the current tokio runtime.

use std::path::Path;
use std::rc::Rc;

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::error::{self, Error, Failure};
use crate::scripts::ScriptRun;
use crate::strict;

/// Chunk name of timon's own Lua glue, which tracebacks and strict globals skip.
pub(crate) const INTERNAL_CHUNK: &str = "=[timon]";

/// Registry key of the top-level runner, which tasks run under to get error tables with a
/// traceback.
pub(crate) const RUN: &str = "timon.run";

/// Registry key of the strict environment user chunks run in.
pub(crate) const ENV: &str = "timon.env";

/// Registry key of the merged `init.lua` settings.
pub(crate) const SETTINGS: &str = "timon.settings";

/// Lua glue: the host-function wrapper, `try`, and the top-level runner.
///
/// `wrap` calls a host function under `pcall` and re-raises its error as an error table at the
/// caller's level. `run` uses `xpcall` so the handler sees the stack before it unwinds. Both
/// `pcall` and `xpcall` can yield in Lua 5.4, so async host functions work through them.
const GLUE: &str = r#"
local normalize, handler = ...
local pack, unpack, pcall, xpcall, error = table.pack, table.unpack, pcall, xpcall, error

local function wrap(f)
  return function(...)
    local r = pack(pcall(f, ...))
    if r[1] then return unpack(r, 2, r.n) end
    error(normalize(r[2]), 2)
  end
end

local function try(f, ...)
  local r = pack(pcall(f, ...))
  if r[1] then return true, unpack(r, 2, r.n) end
  return false, normalize(r[2])
end

local function run(f, ...)
  return xpcall(f, handler, ...)
end

return wrap, try, run
"#;

/// One Lua state with timon's conventions installed.
pub struct Runtime {
    lua: Lua,
    env: Table,
    wrap: Function,
}

impl Runtime {
    /// Creates a runtime with the standard library, timon's modules, and strict globals for user
    /// code.
    pub fn new() -> Result<Self, Error> {
        let runtime = Self::build()?;
        crate::modules::install(&runtime)?;
        Ok(runtime)
    }

    fn build() -> mlua::Result<Self> {
        let lua = Lua::new();
        error::install(&lua)?;
        let normalize = lua.create_function(|lua, value: Value| error::normalize(lua, value))?;
        let handler = lua.create_function(|lua, value: Value| {
            let err = error::normalize(lua, value)?;
            if err.raw_get::<Value>("traceback")?.is_nil() {
                err.raw_set("traceback", lua.create_sequence_from(traceback(lua))?)?;
            }
            Ok(err)
        })?;
        let (wrap, try_fn, run): (Function, Function, Function) = lua
            .load(GLUE)
            .set_name(INTERNAL_CHUNK)
            .call((normalize, handler))?;
        lua.globals().set("try", try_fn)?;
        lua.set_named_registry_value(RUN, &run)?;
        let env = strict::environment(&lua)?;
        lua.set_named_registry_value(ENV, &env)?;
        crate::fennel::install_searcher(&lua)?;
        Ok(Self { lua, env, wrap })
    }

    /// Applies the definition layers: their `lib/` directories go first on the module path,
    /// and their `init.lua` settings are loaded and merged.
    pub fn configure(&self, layers: &crate::layers::Layers) -> Result<(), Error> {
        let package: Table = self.lua.globals().get("package")?;
        let path: String = package.get("path")?;
        let mut prefix = String::new();
        for dir in layers.lib_dirs() {
            prefix.push_str(&format!("{0}/?.lua;{0}/?/init.lua;", dir.display()));
        }
        package.set("path", format!("{prefix}{path}"))?;
        let contract_dirs: Vec<String> = layers
            .contract_dirs()
            .iter()
            .map(|d| d.display().to_string())
            .collect();
        self.lua.set_named_registry_value(
            crate::modules::schema::CONTRACT_DIRS,
            self.lua.create_sequence_from(contract_dirs)?,
        )?;
        let agent_dirs: Vec<String> = crate::agents::dirs(layers)
            .iter()
            .map(|(_, d)| d.display().to_string())
            .collect();
        self.lua.set_named_registry_value(
            crate::modules::agent::AGENT_DIRS,
            self.lua.create_sequence_from(agent_dirs)?,
        )?;
        self.lua.set_named_registry_value(
            crate::modules::agent::STATE_DIR,
            layers.dirs.state.display().to_string(),
        )?;
        self.lua.set_named_registry_value(
            crate::modules::sessions::CACHE,
            layers.dirs.cache.display().to_string(),
        )?;
        if let Some(home) = &layers.dirs.home {
            self.lua.set_named_registry_value(
                crate::modules::sessions::HOME,
                home.display().to_string(),
            )?;
        }
        self.lua.set_app_data(crate::modules::mcp::Dirs(
            layers
                .active()
                .into_iter()
                .map(|l| (l.kind.name(), l.root.join("mcp")))
                .filter(|(_, d)| d.is_dir())
                .collect(),
        ));
        let settings = crate::settings::load(&self.lua, &layers.init_files())?;
        self.lua.set_named_registry_value(SETTINGS, settings)?;
        Ok(())
    }

    /// Limits from the command line, which win over `init.lua`'s `limits`.
    /// How agent runs report progress on stderr.
    pub fn set_progress(&self, progress: crate::modules::agent::Progress) {
        self.lua.set_app_data(progress);
    }

    pub fn set_limits(&self, max_runs: Option<u64>, max_usd: Option<f64>) {
        self.lua
            .set_app_data(crate::modules::agent::Overrides { max_runs, max_usd });
    }

    /// The merged settings, as JSON.
    pub fn settings(&self) -> Result<serde_json::Value, Error> {
        match self.lua.named_registry_value::<Option<Table>>(SETTINGS)? {
            Some(t) => crate::value::to_json(&Value::Table(t), crate::value::Mode::Display),
            None => Ok(serde_json::json!({})),
        }
    }

    /// The Lua state, for installing modules.
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Wraps a host function so its errors reach Lua code as error tables.
    pub fn host_function(&self, f: Function) -> mlua::Result<Function> {
        self.wrap.call(f)
    }

    /// Compiles user code under strict globals.
    ///
    /// With `expression`, a chunk that parses as an expression compiles as `return <chunk>`, as
    /// the Lua REPL does.
    pub fn compile(&self, source: &str, name: &str, expression: bool) -> Result<Function, Failure> {
        if expression
            && let Ok(f) = self
                .lua
                .load(format!("return {source}"))
                .set_name(name)
                .set_environment(self.env.clone())
                .into_function()
        {
            return Ok(f);
        }
        self.lua
            .load(source)
            .set_name(name)
            .set_environment(self.env.clone())
            .into_function()
            .map_err(|e| Failure::from(Error::from_lua(&e)))
    }

    /// Calls `f` as the root scope of a run and returns its values, or the error that stopped
    /// it with a traceback. The run ends when `f` and every task it spawned have finished.
    ///
    /// Must be awaited inside a [`tokio::task::LocalSet`], because tasks are `spawn_local`.
    pub async fn call(&self, f: Function, args: MultiValue) -> Result<MultiValue, Failure> {
        crate::modules::concurrency::run_root(&self.lua, f, args)
            .await
            .map_err(|err| failure(Some(err)))
    }

    /// Calls `f` outside any scope, as a definition file's top level runs, and returns its
    /// first value, or the error that stopped it with a traceback.
    pub(crate) fn call_sync(&self, f: Function) -> Result<Value, Failure> {
        let lua_err = |e: mlua::Error| Failure::from(Error::from_lua(&e));
        let run: Function = self.lua.named_registry_value(RUN).map_err(lua_err)?;
        let mut out = run.call::<MultiValue>(f).map_err(lua_err)?.into_iter();
        match out.next() {
            Some(Value::Boolean(true)) => Ok(out.next().unwrap_or(Value::Nil)),
            _ => Err(failure(out.next())),
        }
    }

    /// Compiles code the way `timon eval` does: an expression returns its value, and with
    /// `fennel` the source is Fennel.
    pub fn compile_eval(
        &self,
        source: &str,
        name: &str,
        fennel: bool,
    ) -> Result<Function, Failure> {
        if fennel {
            let lua_source = crate::fennel::compile(&self.lua, source, name)?;
            return self.compile(&lua_source, name, false);
        }
        self.compile(source, name, true)
    }

    /// Sets a global from JSON, with `json`'s array and object markers.
    pub fn set_global(&self, name: &str, value: &serde_json::Value) -> Result<(), Failure> {
        let lua_err = |e: mlua::Error| Failure::from(Error::from_lua(&e));
        let value = crate::modules::json::decode(&self.lua, value).map_err(lua_err)?;
        self.lua.globals().raw_set(name, value).map_err(lua_err)
    }

    /// Evaluates `source` the way `timon eval` does.
    pub async fn eval(&self, source: &str, name: &str) -> Result<MultiValue, Failure> {
        let f = self.compile(source, name, true)?;
        self.call(f, MultiValue::new()).await
    }

    /// Evaluates Fennel source the way `timon eval --fnl` does.
    pub async fn eval_fennel(&self, source: &str, name: &str) -> Result<MultiValue, Failure> {
        let lua_source = crate::fennel::compile(&self.lua, source, name)?;
        let f = self.compile(&lua_source, name, false)?;
        self.call(f, MultiValue::new()).await
    }

    /// Runs a script file with `args` as the `arg` table and as `...`. A `.fnl` file is
    /// compiled as Fennel first.
    pub async fn run_script(&self, path: &Path, args: &[String]) -> Result<MultiValue, Failure> {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.run_named_script(path, args, &name, None).await
    }

    /// What started the script runs this runtime records (default `manual`), such as
    /// `hook:SessionStart`.
    pub fn set_trigger(&self, trigger: &str) {
        self.lua.set_app_data(Trigger(trigger.to_owned()));
    }

    /// Resumes the next script run from an earlier one: see [`crate::scripts`].
    pub fn set_resume(&self, id: Option<String>) {
        if let Some(id) = id {
            self.lua.set_app_data(Resume(id));
        }
    }

    /// Runs a script and records the run under `state/scripts/` (see [`crate::scripts`]), with
    /// `name` and the `layer` it came from.
    pub async fn run_named_script(
        &self,
        path: &Path,
        args: &[String],
        name: &str,
        layer: Option<&str>,
    ) -> Result<MultiValue, Failure> {
        let source = std::fs::read_to_string(path).map_err(|e| {
            Failure::from(Error::new(
                "io",
                format!("cannot read {}: {e}", path.display()),
            ))
        })?;
        let record = self.begin_script(path, args, name, layer, &source, None)?;
        let driver = self.script_function(path, &source, true)?;
        let script_args = || -> mlua::Result<MultiValue> {
            let arg = self.lua.create_table()?;
            arg.raw_set(0, path.display().to_string())?;
            for (i, a) in args.iter().enumerate() {
                arg.raw_set(i + 1, a.as_str())?;
            }
            self.lua.globals().raw_set("arg", arg)?;
            args.iter()
                .map(|a| self.lua.create_string(a).map(Value::String))
                .collect()
        };
        let args = script_args().map_err(|e| Failure::from(Error::from_lua(&e)))?;
        let result = self.call(driver, args).await;
        self.finish_script(record, &result);
        result
    }

    /// Compiles a script file into a function that runs it: a chunk that returns
    /// `{ run = function(...) end, ... }` is checked and its `run` called (see
    /// `SCRIPT_DRIVER`). With `record`, its metadata goes into the script run's record.
    pub(crate) fn script_function(
        &self,
        path: &Path,
        source: &str,
        record: bool,
    ) -> Result<Function, Failure> {
        script_function_in(&self.lua, path, source, record)
    }

    /// Settles a script run's record with how the run ended.
    pub(crate) fn finish_script(
        &self,
        record: Option<Rc<ScriptRun>>,
        result: &Result<MultiValue, Failure>,
    ) {
        let Some(record) = record else {
            return;
        };
        match result {
            Ok(_) => record.finish("ok", None),
            Err(failure) => record.finish(
                "failed",
                Some(serde_json::json!({
                    "kind": failure.error.kind,
                    "message": failure.error.message,
                    "hint": failure.error.hint,
                })),
            ),
        }
        self.lua.remove_app_data::<Rc<ScriptRun>>();
    }

    /// Starts the record of a script run, when there is a state directory to keep it in.
    /// `extra` fields are added to the request.
    pub(crate) fn begin_script(
        &self,
        path: &Path,
        args: &[String],
        name: &str,
        layer: Option<&str>,
        source: &str,
        extra: Option<serde_json::Value>,
    ) -> Result<Option<Rc<ScriptRun>>, Failure> {
        let Some(state) = self
            .lua
            .named_registry_value::<Option<String>>(crate::modules::agent::STATE_DIR)
            .map_err(|e| Failure::from(Error::from_lua(&e)))?
        else {
            return Ok(None);
        };
        let mut request = serde_json::json!({
            "name": name,
            "layer": layer,
            "path": path,
            "source_sha256": crate::scripts::source_hash(source),
            "args": args,
            "trigger": self
                .lua
                .app_data_ref::<Trigger>()
                .map_or_else(|| "manual".to_owned(), |t| t.0.clone()),
            "cwd": std::env::current_dir().ok(),
        });
        if let Some(serde_json::Value::Object(extra)) = extra {
            for (k, v) in extra {
                request[k] = v;
            }
        }
        let resume = self.lua.app_data_ref::<Resume>().map(|r| r.0.clone());
        let run = Rc::new(ScriptRun::begin(
            Path::new(&state),
            request,
            resume.as_deref(),
        )?);
        self.lua.set_app_data(run.clone());
        Ok(Some(run))
    }
}

/// [`Runtime::script_function`], with only the Lua state at hand.
pub(crate) fn script_function_in(
    lua: &Lua,
    path: &Path,
    source: &str,
    record: bool,
) -> Result<Function, Failure> {
    let lua_err = |e: mlua::Error| Failure::from(Error::from_lua(&e));
    let name = format!("@{}", path.display());
    let lua_source = if path.extension().is_some_and(|ext| ext == "fnl") {
        crate::fennel::compile(lua, &crate::fennel::strip_shebang(source), &name)?
    } else {
        strip_shebang(source).into_owned()
    };
    let env: Table = lua.named_registry_value(ENV).map_err(lua_err)?;
    let f = lua
        .load(lua_source)
        .set_name(&name)
        .set_environment(env)
        .into_function()
        .map_err(lua_err)?;
    let describe = if record {
        lua.create_function(describe_script)
    } else {
        lua.create_function(|_, spec: Table| Ok(check_script(&spec)?))
    }
    .map_err(lua_err)?;
    lua.load(SCRIPT_DRIVER)
        .set_name(INTERNAL_CHUNK)
        .call((f, describe))
        .map_err(lua_err)
}

/// The script run to resume from, given on the command line.
struct Resume(String);

/// What started a script run, for its record.
struct Trigger(String);

/// Runs a script's chunk. A chunk that returns `{ run = function(...) end, ... }` is a script
/// with metadata: its `description`, `phases` and `budget` go into the run's record, and
/// `run` is called with the arguments, inside a scope with `budget` when there is one.
const SCRIPT_DRIVER: &str = r#"
local chunk, describe = ...
local pack, unpack, type = table.pack, table.unpack, type
return function(...)
  local args = pack(...)
  local out = pack(chunk(...))
  local spec = out[1]
  if out.n == 1 and type(spec) == "table" and type(spec.run) == "function" then
    describe(spec)
    local function run() return spec.run(unpack(args, 1, args.n)) end
    if spec.budget ~= nil then
      return scope(run, { budget = spec.budget })
    end
    return run()
  end
  return unpack(out, 1, out.n)
end
"#;

/// The fields a script with metadata may return, as a type expression.
const SCRIPT_SHAPE: &str = "{ description: string?, phases: string[]?, budget: { runs: integer?, usd: number? }?, run: function }";

/// Checks a script's metadata and adds it to the script run's record.
fn describe_script(lua: &Lua, spec: Table) -> mlua::Result<()> {
    check_script(&spec)?;
    if let Some(run) = lua.app_data_ref::<Rc<ScriptRun>>() {
        let mut metadata = serde_json::Map::new();
        for key in ["description", "phases", "budget"] {
            let value: Value = spec.raw_get(key)?;
            if !value.is_nil() {
                metadata.insert(
                    key.to_owned(),
                    crate::value::to_json(&value, crate::value::Mode::Display)?,
                );
            }
        }
        run.describe(serde_json::Value::Object(metadata))?;
    }
    Ok(())
}

/// Checks a script's metadata against `SCRIPT_SHAPE`.
fn check_script(spec: &Table) -> Result<(), Error> {
    use crate::modules::schema::Problem;
    let shape = crate::types::parse(SCRIPT_SHAPE).map_err(|e| Error::new("internal", e))?;
    if let Some(problem) = shape
        .check_value(&Value::Table(spec.clone()))
        .into_iter()
        .next()
    {
        let at = if problem.path.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", problem.path)
        };
        let err = match problem.problem {
            Problem::Unknown { key, .. } if key == "params" => Error::new(
                "unsupported",
                "a script's params are not built yet; read arguments from arg",
            ),
            Problem::Unknown { key, close } => {
                let err = Error::new("bad_script", format!("unknown script field \"{key}\""));
                match close {
                    Some(c) => err.with_hint(format!("did you mean \"{c}\"?")),
                    None => {
                        err.with_hint("a script may return description, phases, budget and run")
                    }
                }
            }
            Problem::Wrong { expected, got } => Error::new(
                "bad_script",
                format!("script field{at} must be {expected}, got {got}"),
            ),
            Problem::NotOneOf { got, .. } => Error::new(
                "bad_script",
                format!("script field{at} has an unknown value {got}"),
            ),
            Problem::Missing { name } => Error::new(
                "bad_script",
                format!("a script with metadata needs \"{name}\""),
            ),
        };
        return Err(err);
    }
    Ok(())
}

/// Comments out a leading `#!` line, keeping line numbers.
fn strip_shebang(source: &str) -> std::borrow::Cow<'_, str> {
    if source.starts_with("#!") {
        format!("--{source}").into()
    } else {
        source.into()
    }
}

fn failure(value: Option<Value>) -> Failure {
    match value {
        Some(Value::Table(t)) => {
            let traceback = t
                .raw_get::<Option<Vec<String>>>("traceback")
                .ok()
                .flatten()
                .unwrap_or_default();
            Failure {
                error: error::from_table(&t),
                traceback,
            }
        }
        Some(Value::String(s)) => Error::new("lua", s.to_string_lossy()).into(),
        _ => Error::new("lua", "the run failed without an error value").into(),
    }
}

/// Lua frames of the current stack, innermost first, skipping C functions and timon's glue.
fn traceback(lua: &Lua) -> Vec<String> {
    let mut frames = Vec::new();
    for level in 1..64 {
        let Some(frame) = lua.inspect_stack(level, |d| {
            let source = d.source();
            // mlua runs async functions through a chunk of its own, named __mlua_async_poll.
            let internal = source.source.as_deref() == Some(INTERNAL_CHUNK)
                || source
                    .short_src
                    .as_deref()
                    .is_some_and(|s| s.starts_with("__mlua"));
            if source.what == "C" || internal {
                return None;
            }
            let place = match d.current_line() {
                Some(line) => format!("{}:{line}", source.short_src.as_deref().unwrap_or("?")),
                None => source.short_src.as_deref().unwrap_or("?").to_owned(),
            };
            Some(match (source.what, d.names().name) {
                ("main", _) => place,
                (_, Some(name)) => format!("{place} in function '{name}'"),
                _ => format!("{place} in a function"),
            })
        }) else {
            break;
        };
        frames.extend(frame);
    }
    frames
}
