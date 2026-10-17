//! `timon serve`: an MCP server over stdio, through tower-mcp, with tasks as tools.
//!
//! Every public task in reach is a tool whose input schema comes from its declared options;
//! `eval` runs Lua or Fennel; the skills are resources. tower-mcp handlers are `Send` and the
//! Lua runtime is not, so each call crosses a channel to a task on the `LocalSet`, which runs it
//! in a fresh runtime: calls are isolated, can overlap, and see the current tasks and settings.
//! Cancelling a request drops its run, which stops its processes.
//!
//! stdout carries the protocol. The transport writes to a copy of fd 1, and fd 1 is pointed at
//! stderr, so what tasks print and the programs they run cannot corrupt the stream.

use std::rc::Rc;

use serde_json::{Value as Json, json};
use tokio::sync::{mpsc, oneshot};
use tower_mcp::extract::{Context, RawArgs, State};
use tower_mcp::{
    CallToolResult, McpRouter, RequestContext, ResourceBuilder, StdioTransport, Tool, ToolBuilder,
};

use crate::error::{Error, Failure};
use crate::layers::Layers;
use crate::runtime::Runtime;
use crate::tasks::{self, Catalog, How, Source};

/// What a tool runs.
#[derive(Clone, Debug)]
enum Kind {
    Task(String),
    Eval,
}

/// One call, sent from a tower-mcp handler to the local task.
struct Job {
    kind: Kind,
    args: Json,
    ctx: RequestContext,
    reply: oneshot::Sender<CallToolResult>,
}

fn bridged(
    name: &str,
    description: &str,
    input: Json,
    kind: Kind,
    tx: mpsc::Sender<Job>,
) -> Result<Tool, Error> {
    let tool = ToolBuilder::try_new(name)
        .map_err(|e| {
            Error::new(
                "bad_tool",
                format!("\"{name}\" cannot be an MCP tool name: {e}"),
            )
        })?
        .description(description)
        .input_schema(input)
        .extractor_handler(
            tx,
            move |State(tx): State<mpsc::Sender<Job>>, ctx: Context, RawArgs(args): RawArgs| {
                let kind = kind.clone();
                async move {
                    let (reply, rx) = oneshot::channel();
                    let job = Job {
                        kind,
                        args,
                        ctx: ctx.into_inner(),
                        reply,
                    };
                    if tx.send(job).await.is_err() {
                        return Ok(CallToolResult::error("timon has stopped"));
                    }
                    Ok(rx
                        .await
                        .unwrap_or_else(|_| CallToolResult::error("timon dropped the call")))
                }
            },
        )
        .build();
    Ok(tool)
}

/// A task's tool name: MCP tool names take letters, digits, `_`, `-` and `.`.
fn tool_name(task: &str) -> String {
    task.replace(':', ".")
}

fn task_tool(catalog: &Catalog, task: &tasks::Task, tx: &mpsc::Sender<Job>) -> Result<Tool, Error> {
    let questions = tasks::questions(catalog, &task.name);
    let args = json!({
        "type": "array",
        "items": { "type": "string" },
        "description": "words for the task, as on the command line after --",
    });
    let mut extra = vec![("args", args)];
    let mut required = Vec::new();
    if !questions.is_empty() {
        extra.push((
            "confirmed",
            json!({ "type": "boolean", "description": "true once the user has answered yes to the questions in the description" }),
        ));
        required.push("confirmed");
    }
    let input = match &task.source {
        Source::File(_) => task.options.json_schema(&extra, &required),
        Source::Script(_) => crate::flags::Spec::default().json_schema(&extra, &required),
    };
    let mut description = if task.description.is_empty() {
        format!("Runs the task {}.", task.name)
    } else {
        task.description.trim().to_owned()
    };
    if !description.ends_with(['.', '?', '!']) {
        description.push('.');
    }
    if !task.depends.is_empty() {
        description.push_str(&format!(" Runs after: {}.", task.depends.join(", ")));
    }
    if !questions.is_empty() {
        description.push_str(&format!(
            " Asks first: \"{}\" Ask the user, and pass confirmed = true only after they answer yes.",
            questions.join("\" \"")
        ));
    }
    bridged(
        &tool_name(&task.name),
        &description,
        input,
        Kind::Task(task.name.clone()),
        tx.clone(),
    )
}

fn failure_result(failure: &Failure) -> CallToolResult {
    let e = &failure.error;
    let mut result = CallToolResult::error(failure.report());
    result.structured_content = Some(json!({
        "error": { "kind": e.kind, "message": e.message, "hint": e.hint, "data": e.data },
    }));
    result
}

/// The values a run returned, as a result: text, and `{ value }` as structured content.
fn values_result(values: &mlua::MultiValue) -> Result<CallToolResult, Error> {
    let mode = crate::value::Mode::Display;
    let json: Vec<Json> = values
        .iter()
        .map(|v| crate::value::to_json(v, mode))
        .collect::<Result<_, _>>()?;
    let value = match json.len() {
        0 => Json::Null,
        1 => json.into_iter().next().unwrap_or(Json::Null),
        _ => Json::Array(json),
    };
    let text = match &value {
        Json::String(s) => s.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    };
    let mut result = CallToolResult::text(text);
    result.structured_content = Some(json!({ "value": value }));
    Ok(result)
}

fn runtime(layers: &Layers) -> Result<Runtime, Failure> {
    let rt = Runtime::new()?;
    rt.configure(layers)?;
    rt.set_progress(crate::modules::agent::Progress::Off);
    Ok(rt)
}

async fn handle(layers: &Layers, builtins: &[String], kind: Kind, args: Json) -> CallToolResult {
    let rt = match runtime(layers) {
        Ok(rt) => rt,
        Err(f) => return failure_result(&f),
    };
    let result = match kind {
        Kind::Eval => {
            let code = args["code"].as_str().unwrap_or_default().to_owned();
            if args["fennel"] == true {
                rt.eval_fennel(&code, "=eval").await
            } else {
                rt.eval(&code, "=eval").await
            }
        }
        Kind::Task(name) => run_task(&rt, layers, builtins, &name, &args).await,
    };
    match result {
        Ok(values) => values_result(&values).unwrap_or_else(|e| failure_result(&e.into())),
        Err(f) => failure_result(&f),
    }
}

async fn run_task(
    rt: &Runtime,
    layers: &Layers,
    builtins: &[String],
    name: &str,
    args: &Json,
) -> Result<mlua::MultiValue, Failure> {
    let catalog = Rc::new(Catalog::load(rt, layers, builtins)?);
    let task = catalog.find(name)?;
    let questions = tasks::questions(&catalog, &task.name);
    let confirmed = args["confirmed"] == true;
    if !questions.is_empty() && !confirmed {
        return Err(Error::new(
            "confirm",
            format!("task \"{}\" asks: {}", task.name, questions.join(" ")),
        )
        .with_hint("ask the user; call again with confirmed = true only if they answer yes")
        .into());
    }
    let mut args = args.clone();
    if let Some(o) = args.as_object_mut().filter(|_| !questions.is_empty()) {
        o.remove("confirmed");
    }
    let given = tasks::given_from_json(rt, task, &args)?;
    let how = How {
        yes: confirmed,
        jobs: 1,
        dry_run: false,
    };
    let name = task.name.clone();
    tasks::run_given(rt, catalog, &name, given, how).await
}

/// The protocol's stream: a copy of stdout, after which fd 1 points at stderr. `timon hook`
/// uses it too, so what its tasks print cannot corrupt the JSON the host reads.
#[cfg(unix)]
pub(crate) fn protocol_stdout() -> Result<tokio::fs::File, Error> {
    use std::os::fd::FromRawFd;
    let failed = || Error::new("io", "cannot set up stdout for the protocol");
    // SAFETY: duplicates and replaces this process's own standard descriptors. The copy is
    // close-on-exec, so the programs tasks start cannot write into the protocol stream.
    unsafe {
        let copy = libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 0);
        if copy < 0 {
            return Err(failed());
        }
        let copy = std::fs::File::from_raw_fd(copy);
        if libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) < 0 {
            return Err(failed());
        }
        Ok(tokio::fs::File::from_std(copy))
    }
}

#[cfg(not(unix))]
pub(crate) fn protocol_stdout() -> Result<tokio::io::Stdout, Error> {
    Ok(tokio::io::stdout())
}

/// Serves the tasks in reach of `layers` over stdio until the client closes the stream.
pub async fn serve(layers: Layers, builtins: Vec<String>) -> Result<(), Failure> {
    // Before any Lua runs, so what a task file prints at load cannot reach the client.
    let writer = protocol_stdout()?;
    let (tx, mut rx) = mpsc::channel::<Job>(64);
    let catalog = {
        let rt = runtime(&layers)?;
        Catalog::load(&rt, &layers, &builtins)?
    };
    let mut router = McpRouter::new()
        .server_info("timon", env!("CARGO_PKG_VERSION"))
        .instructions(
            "timon runs this project's tasks. Each tool runs a task after its dependencies and returns { value }; a failure is an error result with kind, message and hint. A task that asks first takes confirmed = true, which only the user can give. eval runs Lua (or Fennel with fennel = true) over timon's modules; the timon skill resources explain them.",
        );
    let eval_input = json!({
        "type": "object",
        "properties": {
            "code": { "type": "string", "description": "Lua; an expression returns its value" },
            "fennel": { "type": "boolean", "description": "the code is Fennel" },
        },
        "required": ["code"],
        "additionalProperties": false,
    });
    router = router.tool(bridged(
        "eval",
        "Evaluates Lua (or Fennel) with timon's modules and returns the result as { value }. help() lists the modules.",
        eval_input,
        Kind::Eval,
        tx.clone(),
    )?);
    for task in catalog.tasks.iter().filter(|t| !t.private && t.available()) {
        if tool_name(&task.name) == "eval" {
            eprintln!("timon: task \"eval\" is not served; the eval tool has the name");
            continue;
        }
        match task_tool(&catalog, task, &tx) {
            Ok(tool) => router = router.tool(tool),
            Err(e) => eprintln!("timon: {}: {}", e.kind, e.message),
        }
    }
    for skill in crate::skills::list(&layers) {
        let text = match skill.text() {
            Ok(text) => text,
            Err(_) => continue,
        };
        router = router.resource(
            ResourceBuilder::new(format!("timon://skills/{}", skill.name))
                .name(skill.name.clone())
                .description(skill.description.clone())
                .mime_type("text/markdown")
                .text(text),
        );
    }
    drop(catalog);

    let local = {
        let layers = layers.clone();
        let builtins = builtins.clone();
        tokio::task::spawn_local(async move {
            while let Some(job) = rx.recv().await {
                let layers = layers.clone();
                let builtins = builtins.clone();
                tokio::task::spawn_local(async move {
                    let Job {
                        kind,
                        args,
                        ctx,
                        reply,
                    } = job;
                    let result = tokio::select! {
                        result = handle(&layers, &builtins, kind, args) => result,
                        () = ctx.cancelled() => CallToolResult::error("cancelled"),
                    };
                    let _ = reply.send(result);
                });
            }
        })
    };
    drop(tx);
    let served = StdioTransport::new(router)
        .run_with_streams(tokio::io::stdin(), writer)
        .await;
    local.abort();
    served.map_err(|e| {
        Failure::from(Error::new(
            "mcp_failed",
            format!("the MCP server stopped: {e}"),
        ))
    })
}
