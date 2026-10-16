//! `mcp`: any MCP server's tools as Lua calls, through tower-mcp's stdio client.
//!
//! MCP servers are timon's pods: programs whose functions a script calls. A server runs in its
//! own process group, and a connection belongs to the scope that opened it: when that scope
//! ends, the server stops. Named servers are definition files in a layer's `mcp/` directory.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use mlua::{Function, Lua, Table, UserData, UserDataMethods, Value};
use serde_json::{Value as Json, json};
use tower_mcp::client::{McpClient, StdioClientTransport};

use crate::docs::{Func, Module, Opt};
use crate::error::Error;
use crate::modules::Installer;
use crate::modules::json::decode_result as to_lua;
use crate::modules::proc::Stopper;
use crate::runtime::INTERNAL_CHUNK;

pub const DOC: Module = Module {
    name: "mcp",
    summary: "MCP servers' tools as Lua calls. mcp.connect returns a connection: c:tools() lists { name, description, input_schema }; c:call(tool, args) returns the structured content, or the text, and raises mcp_tool_failed when the tool reports an error; c:resources() and c:read(uri); c:close(). A connection closes, and its server stops, when the scope that opened it ends. Errors: mcp_failed, mcp_tool_failed, not_found.",
    functions: &[
        Func::new(
            "connect",
            "mcp.connect(name | { command, env, cwd }, { timeout }) -> connection",
            "Starts an MCP server over stdio and initializes it: a name from mcp/NAME.lua in the layers, or a command list. c.server holds the server's name and version.",
            "local fs = mcp.connect{ command = { \"npx\", \"-y\", \"@modelcontextprotocol/server-filesystem\", \".\" } }\nfor _, t in ipairs(fs:tools()) do print(t.name) end\nlocal text = fs:call(\"read_text_file\", { path = \"README.md\" })",
        )
        .with_options(&[Opt::new(
            "timeout",
            "number | string",
            "how long starting and initializing may take (default 30s)",
        )]),
        Func::new(
            "servers",
            "mcp.servers() -> { { name, layer, description, command } }",
            "The named servers in the layers' mcp/ directories; a project's entry hides a user's of the same name.",
            "for _, s in ipairs(mcp.servers()) do print(s.name, s.description) end",
        ),
    ],
};

/// Where named servers are defined, highest layer first.
pub(crate) struct Dirs(pub Vec<(&'static str, PathBuf)>);

/// A server to start.
struct Spec {
    name: String,
    command: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<String>,
    description: String,
}

const SPEC_SHAPE: &str =
    "{ description: string?, command: string[], env: table<string, string>?, cwd: string? }";

fn spec_from(name: &str, t: &Table, place: &str) -> Result<Spec, Error> {
    let shape = crate::types::parse(SPEC_SHAPE).map_err(|e| Error::new("internal", e))?;
    if let Some(e) = shape
        .check_value(&Value::Table(t.clone()))
        .into_iter()
        .next()
    {
        use crate::modules::schema::Problem;
        let message = match e.problem {
            Problem::Unknown { key, close } => {
                let m = format!("{place}: unknown field \"{key}\"");
                return Err(match close {
                    Some(c) => {
                        Error::new("bad_mcp_server", m).with_hint(format!("did you mean \"{c}\"?"))
                    }
                    None => Error::new("bad_mcp_server", m)
                        .with_hint("a server takes description, command, env and cwd"),
                });
            }
            Problem::Missing { name } => format!("{place}: needs \"{name}\""),
            Problem::Wrong { expected, got } => {
                format!("{place}: \"{}\" must be {expected}, got {got}", e.path)
            }
            Problem::NotOneOf { got, .. } => format!("{place}: \"{}\" cannot be {got}", e.path),
        };
        return Err(Error::new("bad_mcp_server", message));
    }
    let command: Vec<String> = t.raw_get("command").map_err(|e| Error::from_lua(&e))?;
    if command.is_empty() {
        return Err(Error::new(
            "bad_mcp_server",
            format!("{place}: the command is empty"),
        ));
    }
    let mut env = Vec::new();
    if let Some(e) = t
        .raw_get::<Option<Table>>("env")
        .map_err(|e| Error::from_lua(&e))?
    {
        for pair in e.pairs::<String, String>() {
            env.push(pair.map_err(|e| Error::from_lua(&e))?);
        }
        env.sort();
    }
    Ok(Spec {
        name: name.to_owned(),
        command,
        env,
        cwd: t.raw_get("cwd").map_err(|e| Error::from_lua(&e))?,
        description: t
            .raw_get::<Option<String>>("description")
            .map_err(|e| Error::from_lua(&e))?
            .unwrap_or_default(),
    })
}

/// Loads `mcp/NAME.lua` without effects.
fn load(lua: &Lua, path: &Path, name: &str) -> Result<Spec, Error> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| Error::new("io", format!("cannot read {}: {e}", path.display())))?;
    let env = crate::settings::definition_env(lua).map_err(|e| Error::from_lua(&e))?;
    let value: Value = lua
        .load(source)
        .set_name(format!("@{}", path.display()))
        .set_environment(env)
        .call(())
        .map_err(|e| {
            Error::new(
                "bad_mcp_server",
                format!("{}: {}", path.display(), Error::from_lua(&e).message),
            )
        })?;
    let Value::Table(t) = value else {
        return Err(Error::new(
            "bad_mcp_server",
            format!(
                "{} must return a table, got {}",
                path.display(),
                value.type_name()
            ),
        ));
    };
    spec_from(name, &t, &path.display().to_string())
}

/// Every named server, highest layer first.
fn named(lua: &Lua) -> Vec<(&'static str, String, PathBuf)> {
    let Some(dirs) = lua.app_data_ref::<Dirs>() else {
        return Vec::new();
    };
    let mut found: Vec<(&'static str, String, PathBuf)> = Vec::new();
    for (layer, dir) in &dirs.0 {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut here: Vec<(String, PathBuf)> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "lua"))
            .filter_map(|p| Some((p.file_stem()?.to_str()?.to_owned(), p)))
            .collect();
        here.sort();
        for (name, path) in here {
            if !found.iter().any(|(_, n, _)| *n == name) {
                found.push((layer, name, path));
            }
        }
    }
    found.sort_by(|a, b| a.1.cmp(&b.1));
    found
}

fn find(lua: &Lua, name: &str) -> Result<Spec, Error> {
    let all = named(lua);
    match all.iter().find(|(_, n, _)| n == name) {
        Some((_, n, path)) => load(lua, path, n),
        None => {
            let err = Error::new("not_found", format!("no MCP server named \"{name}\""));
            Err(
                match crate::suggest::closest(name, all.iter().map(|(_, n, _)| n.as_str())) {
                    Some(c) => err.with_hint(format!("did you mean \"{c}\"?")),
                    None => err.with_hint(
                        "define it in mcp/NAME.lua in ~/.config/timon or .timon, or pass { command = { ... } }",
                    ),
                },
            )
        }
    }
}

fn failed(what: &str, e: impl std::fmt::Display) -> Error {
    Error::new("mcp_failed", format!("{what}: {e}"))
}

struct Inner {
    name: String,
    server: Json,
    client: RefCell<Option<Rc<McpClient>>>,
    stopper: RefCell<Option<Stopper>>,
}

impl Inner {
    fn client(&self) -> mlua::Result<Rc<McpClient>> {
        self.client.borrow().clone().ok_or_else(|| {
            Error::new(
                "closed",
                format!("the connection to {} is closed", self.name),
            )
            .into()
        })
    }

    /// Drops the client and stops the server's process group.
    fn drop_now(&self) {
        self.client.borrow_mut().take();
        self.stopper.borrow_mut().take();
    }
}

struct Connection(Rc<Inner>);

/// Text from content blocks, joined by newlines.
fn texts(content: &Json) -> Option<String> {
    let items = content.as_array()?;
    let parts: Vec<&str> = items
        .iter()
        .filter(|c| c["type"] == "text")
        .filter_map(|c| c["text"].as_str())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

impl UserData for Connection {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("name", |_, this| Ok(this.0.name.clone()));
        fields.add_field_method_get("server", |lua, this| to_lua(lua, &this.0.server));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method("tools", |lua, this, ()| async move {
            let client = this.0.client()?;
            let tools = client
                .list_all_tools()
                .await
                .map_err(|e| failed(&format!("{}: tools/list", this.0.name), e))?;
            let list: Vec<Json> = tools
                .iter()
                .map(|t| {
                    let v = serde_json::to_value(t).unwrap_or(Json::Null);
                    json!({
                        "name": v["name"], "title": v["title"], "description": v["description"],
                        "input_schema": v["inputSchema"], "annotations": v["annotations"],
                    })
                })
                .collect();
            to_lua(&lua, &Json::Array(list))
        });
        methods.add_async_method(
            "call",
            |lua, this, (tool, args): (String, Option<Value>)| async move {
                let client = this.0.client()?;
                let args = match args {
                    None | Some(Value::Nil) => json!({}),
                    Some(v) => crate::value::to_json(&v, crate::value::Mode::Strict)?,
                };
                let args = if args.as_array().is_some_and(Vec::is_empty) {
                    json!({})
                } else {
                    args
                };
                let result = client
                    .call_tool(&tool, args)
                    .await
                    .map_err(|e| failed(&format!("{}: {tool}", this.0.name), e))?;
                let result =
                    serde_json::to_value(&result).map_err(|e| failed("a tool's result", e))?;
                let text = texts(&result["content"]);
                if result["isError"] == true {
                    return Err(Error::new(
                        "mcp_tool_failed",
                        format!(
                            "{}: {tool}: {}",
                            this.0.name,
                            text.as_deref().unwrap_or("the tool reported an error")
                        ),
                    )
                    .with_data(json!({ "content": result["content"] }))
                    .into());
                }
                match (&result["structuredContent"], text) {
                    (s, _) if !s.is_null() => to_lua(&lua, s),
                    (_, Some(text)) => Ok(Value::String(lua.create_string(text)?)),
                    _ => to_lua(&lua, &result["content"]),
                }
            },
        );
        methods.add_async_method("resources", |lua, this, ()| async move {
            let client = this.0.client()?;
            let list = client
                .list_all_resources()
                .await
                .map_err(|e| failed(&format!("{}: resources/list", this.0.name), e))?;
            let list: Vec<Json> = list
                .iter()
                .map(|r| {
                    let v = serde_json::to_value(r).unwrap_or(Json::Null);
                    json!({ "uri": v["uri"], "name": v["name"], "description": v["description"], "mime_type": v["mimeType"] })
                })
                .collect();
            to_lua(&lua, &Json::Array(list))
        });
        methods.add_async_method("read", |lua, this, uri: String| async move {
            let client = this.0.client()?;
            let result = client
                .read_resource(&uri)
                .await
                .map_err(|e| failed(&format!("{}: {uri}", this.0.name), e))?;
            let v = serde_json::to_value(&result).map_err(|e| failed("a resource", e))?;
            let contents = v["contents"].as_array().cloned().unwrap_or_default();
            if let [one] = contents.as_slice()
                && let Some(text) = one["text"].as_str()
            {
                return Ok(Value::String(lua.create_string(text)?));
            }
            let list: Vec<Json> = contents
                .iter()
                .map(|c| json!({ "uri": c["uri"], "mime_type": c["mimeType"], "text": c["text"], "blob": c["blob"] }))
                .collect();
            to_lua(&lua, &Json::Array(list))
        });
        methods.add_async_method("close", |_, this, ()| async move {
            let client = this.0.client.borrow_mut().take();
            if let Some(client) = client
                && let Ok(client) = Rc::try_unwrap(client)
            {
                let _ = tokio::time::timeout(Duration::from_secs(5), client.shutdown()).await;
            }
            this.0.stopper.borrow_mut().take();
            Ok(())
        });
        methods.add_meta_method("__tostring", |_, this, ()| {
            Ok(format!("mcp connection {}", this.0.name))
        });
    }
}

async fn connect(
    lua: Lua,
    thread: mlua::Thread,
    target: Value,
    opts: Option<Table>,
) -> mlua::Result<Connection> {
    crate::options::check_opt("mcp.connect", opts.as_ref())?;
    crate::tasks::refuse_while_loading(&lua, "mcp.connect")?;
    let spec = match &target {
        Value::String(s) => find(&lua, &s.to_string_lossy())?,
        Value::Table(t) => {
            let name = t
                .raw_get::<Option<Vec<String>>>("command")?
                .and_then(|c| c.first().cloned())
                .map(|p| {
                    Path::new(&p)
                        .file_name()
                        .map_or(p.clone(), |f| f.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            spec_from(&name, t, "mcp.connect")?
        }
        other => {
            return Err(Error::new(
                "bad_argument",
                format!(
                    "mcp.connect takes a server name or {{ command = {{ ... }} }}, not a {}",
                    other.type_name()
                ),
            )
            .into());
        }
    };
    let timeout = match opts
        .as_ref()
        .map(|o| o.raw_get::<Value>("timeout"))
        .transpose()?
    {
        None | Some(Value::Nil) => Duration::from_secs(30),
        Some(v) => crate::modules::concurrency::duration(&v)?,
    };
    let mut cmd = tokio::process::Command::new(&spec.command[0]);
    cmd.args(&spec.command[1..]);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    let shown = shell_words::join(&spec.command);
    let start = async {
        let transport = StdioClientTransport::spawn_command(&mut cmd)
            .await
            .map_err(|e| {
                let message = e.to_string();
                if message.contains("No such file") || message.contains("not found") {
                    Error::new(
                        "not_found",
                        format!(
                            "cannot start `{shown}`: no program named {}",
                            spec.command[0]
                        ),
                    )
                    .with_hint("check its name and PATH")
                } else {
                    failed(&format!("cannot start `{shown}`"), message)
                }
            })?;
        let pid = transport.child_id();
        let transport = transport.process_group_shutdown(true);
        let stopper = Stopper::for_group(pid);
        let client = McpClient::connect(transport)
            .await
            .map_err(|e| failed(&format!("cannot connect to `{shown}`"), e))?;
        let init = client
            .initialize("timon", env!("CARGO_PKG_VERSION"))
            .await
            .map_err(|e| failed(&format!("`{shown}` did not initialize"), e))?;
        let server = serde_json::to_value(&init.server_info).unwrap_or(Json::Null);
        Ok::<_, Error>((client, stopper, server))
    };
    let (client, stopper, server) =
        tokio::time::timeout(timeout, start).await.map_err(|_| {
            Error::new(
                "timeout",
                format!("`{shown}` did not start and initialize within {timeout:?}"),
            )
            .with_hint("raise timeout, or check that the program is an MCP server on stdio")
        })??;
    let inner = Rc::new(Inner {
        name: spec.name,
        server,
        client: RefCell::new(Some(Rc::new(client))),
        stopper: RefCell::new(Some(stopper)),
    });
    let weak = Rc::downgrade(&inner);
    crate::modules::concurrency::on_scope_end(
        &lua,
        &thread,
        Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.drop_now();
            }
        }),
    );
    let _ = &spec.description;
    Ok(Connection(inner))
}

const GLUE: &str = r#"
local connect_from, running = ...
return function(target, opts) return connect_from((running()), target, opts) end
"#;

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let module = installer.module(&DOC)?;
    let connect_from = installer.wrap(lua.create_async_function(
        |lua, (thread, target, opts): (mlua::Thread, Value, Option<Table>)| async move {
            connect(lua, thread, target, opts).await
        },
    )?)?;
    let coroutine: Table = lua.globals().raw_get("coroutine")?;
    let running: Function = coroutine.raw_get("running")?;
    let f: Function = lua
        .load(GLUE)
        .set_name(INTERNAL_CHUNK)
        .call((connect_from, running))?;
    installer.lua_function(&DOC, Some(&module), "connect", f)?;
    installer.function(
        &DOC,
        Some(&module),
        "servers",
        lua.create_function(|lua, ()| {
            let mut list = Vec::new();
            for (layer, name, path) in named(lua) {
                let entry = match load(lua, &path, &name) {
                    Ok(spec) => json!({
                        "name": name, "layer": layer, "description": spec.description, "command": spec.command,
                    }),
                    Err(e) => json!({ "name": name, "layer": layer, "error": e.message }),
                };
                list.push(entry);
            }
            to_lua(lua, &Json::Array(list))
        })?,
    )?;
    Ok(())
}
