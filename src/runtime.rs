//! The runtime: one Lua state, with user code run as coroutines on the current tokio runtime.

use std::path::Path;

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::error::{self, Error, Failure};
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
        let settings = crate::settings::load(&self.lua, &layers.init_files())?;
        self.lua.set_named_registry_value(SETTINGS, settings)?;
        Ok(())
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
        let source = std::fs::read_to_string(path).map_err(|e| {
            Failure::from(Error::new(
                "io",
                format!("cannot read {}: {e}", path.display()),
            ))
        })?;
        let name = format!("@{}", path.display());
        let lua_source = if path.extension().is_some_and(|ext| ext == "fnl") {
            crate::fennel::compile(&self.lua, &crate::fennel::strip_shebang(&source), &name)?
        } else {
            strip_shebang(&source).into_owned()
        };
        let f = self.compile(&lua_source, &name, false)?;
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
        self.call(f, args).await
    }
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
            if source.what == "C" || source.source.as_deref() == Some(INTERNAL_CHUNK) {
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
