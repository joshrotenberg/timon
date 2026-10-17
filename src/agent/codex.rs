//! One Codex turn through codex-wrapper.
//!
//! Codex takes its output schema as a file and writes its final message to a file; both live
//! in a temporary directory for the length of the run. A failed turn exits 1 with its events
//! on stdout, so failures are read back out of them.

use std::path::Path;
use std::time::Duration;

use codex_wrapper::{
    ApprovalPolicy, Codex, ExecCommand, ExecResumeCommand, FailureKind, JsonLineEvent, SandboxMode,
};
use serde_json::Value as Json;

use super::claude::last_lines;
use super::{Access, Answer, Outcome, Request, Status, Usage};
use crate::error::Error;

pub(super) fn no_fork() -> Error {
    Error::new(
        "unsupported",
        "codex cannot fork a session non-interactively",
    )
    .with_hint("use resume with a session timon started, or run on claude")
}

fn sandbox(access: Access) -> (SandboxMode, &'static str) {
    match access {
        Access::Read => (SandboxMode::ReadOnly, "read-only"),
        Access::Write => (SandboxMode::WorkspaceWrite, "workspace-write"),
        Access::Full => (SandboxMode::DangerFullAccess, "danger-full-access"),
    }
}

/// Runs one turn. Codex reports its items as `exec --json` events, which `events` collects
/// when `request.events` is set.
pub(super) async fn run(
    request: &Request,
    binary: Option<&Path>,
    on_spawn: Option<super::SpawnHook>,
    events: &mut Vec<Json>,
) -> Outcome {
    if request.fork.is_some() {
        return Err((Status::Failed, no_fork()).into());
    }
    let io = |what: &str, e: std::io::Error| {
        (
            Status::Failed,
            Error::new("io", format!("cannot {what} for the codex run: {e}")),
        )
    };
    let scratch = tempfile::Builder::new()
        .prefix("timon-codex-")
        .tempdir()
        .map_err(|e| io("create a temp directory", e))?;
    let last_message = scratch.path().join("last-message.txt");
    let schema_file = scratch.path().join("schema.json");
    if let Some(shape) = &request.output {
        std::fs::write(&schema_file, shape.json_schema().to_string())
            .map_err(|e| io("write the schema", e))?;
    }

    let mut builder = Codex::builder()
        .env("TIMON_CHILD", "1")
        // Linux only: the kernel stops the provider if timon dies. Elsewhere the run record
        // names the process, and the next timon command stops it.
        .die_with_parent(true)
        .termination_grace(Duration::from_millis(250))
        .output_limit(16 * 1024 * 1024);
    if let Some(binary) = binary {
        builder = builder.binary(binary);
    }
    if let Some(hook) = on_spawn {
        builder = builder.on_spawn(std::sync::Arc::new(move |info| hook(info.pid, info.pgid)));
    }
    if let Some(cwd) = &request.cwd {
        builder = builder.working_dir(cwd);
    }
    if let Some(timeout) = request.timeout {
        builder = builder.timeout(timeout);
    }
    for arg in &request.native {
        builder = builder.arg(arg.as_str());
    }
    let client = builder.build().map_err(|e| failure(&e))?;

    // Codex has no system prompt flag; the instructions lead the prompt.
    let prompt = match &request.system {
        Some(system) => format!("{system}\n\n{}", request.prompt),
        None => request.prompt.clone(),
    };
    let (mode, mode_name) = sandbox(request.access);
    let last_message_arg = last_message.display().to_string();
    let schema_arg = schema_file.display().to_string();
    let effort = request
        .effort
        .as_ref()
        .map(|e| format!("model_reasoning_effort=\"{e}\""));
    let never = std::future::pending::<()>();

    let result = match &request.resume {
        None => {
            let mut command = ExecCommand::from_stdin(&prompt)
                .skip_git_repo_check()
                .ignore_user_config()
                .disable("plugins")
                .approval_policy(ApprovalPolicy::Never)
                .sandbox(mode)
                .output_last_message(&last_message_arg);
            if let Some(model) = &request.model {
                command = command.model(model);
            }
            if let Some(effort) = &effort {
                command = command.config(effort);
            }
            if request.output.is_some() {
                command = command.output_schema(&schema_arg);
            }
            if request.ephemeral {
                command = command.ephemeral();
            }
            command.execute_json_cancellable(&client, never).await
        }
        Some(session) => {
            let mut command = ExecResumeCommand::from_stdin(&prompt)
                .session_id(session)
                .skip_git_repo_check()
                .ignore_user_config()
                .disable("plugins")
                .approval_policy(ApprovalPolicy::Never)
                .config(format!("sandbox_mode=\"{mode_name}\""))
                .output_last_message(&last_message_arg);
            if let Some(model) = &request.model {
                command = command.model(model);
            }
            if let Some(effort) = &effort {
                command = command.config(effort);
            }
            if request.output.is_some() {
                command = command.output_schema(&schema_arg);
            }
            if request.ephemeral {
                command = command.ephemeral();
            }
            command.execute_json_cancellable(&client, never).await
        }
    };
    let collect = |events: &mut Vec<Json>, all: &mut dyn Iterator<Item = JsonLineEvent>| {
        if request.events {
            for e in all {
                events.extend(super::events::from_codex(
                    &e.event_type,
                    e.extra.get("item"),
                ));
            }
        }
    };
    let result = match result {
        Ok(result) => {
            collect(events, &mut result.events.iter().cloned());
            result
        }
        Err(e) => {
            if let codex_wrapper::Error::CommandFailed { stdout, .. } = &e {
                collect(
                    events,
                    &mut stdout.lines().filter_map(|l| serde_json::from_str(l).ok()),
                );
            }
            return Err(failure(&e).into());
        }
    };

    if let Some(failed) = result.events.iter().find(|e| e.is_turn_failed()) {
        return Err((Status::Failed, turn_failed(failed)).into());
    }
    let text = std::fs::read_to_string(&last_message)
        .ok()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| result.result.clone());
    let structured = if request.output.is_some() {
        Some(serde_json::from_str::<Json>(text.trim()).map_err(|e| {
            (
                Status::Invalid,
                Error::new("invalid_answer", format!("codex's answer is not JSON: {e}")),
            )
        })?)
    } else {
        None
    };
    let usage = Usage {
        input_tokens: result
            .usage
            .as_ref()
            .and_then(|u| u.input_tokens)
            .unwrap_or(0),
        output_tokens: result
            .usage
            .as_ref()
            .and_then(|u| u.output_tokens)
            .unwrap_or(0),
        usd: None,
    };
    Ok(Answer {
        text,
        structured,
        session: result.thread_id.clone().or(result.session_id.clone()),
        usage,
    })
}

fn turn_failed(event: &JsonLineEvent) -> Error {
    let raw = event
        .turn_failure_message()
        .unwrap_or("the codex turn failed")
        .to_owned();
    // The message is often the API's error JSON; its own message is the readable part.
    let message = serde_json::from_str::<Json>(&raw)
        .ok()
        .and_then(|j| j["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or(raw);
    let schema = event
        .turn_failure_api_error()
        .and_then(|api| api.code)
        .is_some_and(|code| code == "invalid_json_schema");
    let lower = message.to_lowercase();
    if schema {
        Error::new("bad_schema", message).with_hint("codex rejected the output schema")
    } else if lower.contains("model")
        && ["not supported", "does not exist", "not found"]
            .iter()
            .any(|s| lower.contains(s))
    {
        Error::new("unknown_model", message).with_hint("use a model this Codex login can run")
    } else {
        Error::new("turn_failed", message)
    }
}

fn failure(err: &codex_wrapper::Error) -> (Status, Error) {
    use codex_wrapper::Error as E;
    match err {
        E::Timeout { .. } => (
            Status::Timeout,
            Error::new("timeout", "codex did not finish in time").with_hint("raise timeout"),
        ),
        E::Cancelled { .. } => (
            Status::Cancelled,
            Error::new("cancelled", "the run was cancelled"),
        ),
        E::NotFound => (
            Status::Failed,
            Error::new("not_installed", "codex is not installed or not on PATH")
                .with_hint("install the Codex CLI, or set TIMON_CODEX_BIN"),
        ),
        E::CommandFailed {
            stdout,
            stderr,
            exit_code,
            ..
        } => {
            let failed = stdout
                .lines()
                .filter_map(|l| serde_json::from_str::<JsonLineEvent>(l).ok())
                .find(|e| e.is_turn_failed());
            if let Some(event) = failed {
                return (Status::Failed, turn_failed(&event));
            }
            classify(err).unwrap_or_else(|| {
                (
                    Status::Failed,
                    Error::new(
                        "provider_failed",
                        format!("codex exited {exit_code}: {}", last_lines(stderr)),
                    ),
                )
            })
        }
        other => classify(other).unwrap_or_else(|| {
            (
                Status::Failed,
                Error::new("provider_failed", other.to_string()),
            )
        }),
    }
}

fn classify(err: &codex_wrapper::Error) -> Option<(Status, Error)> {
    let (kind, hint) = match err.failure_kind()? {
        FailureKind::Auth => ("not_logged_in", "run: codex login"),
        FailureKind::Config => ("provider_config", "check the codex configuration"),
        FailureKind::NotTrustedDirectory => {
            ("untrusted_directory", "codex does not trust this directory")
        }
        FailureKind::SessionNotFound => ("session_not_found", "resume a session codex has"),
        _ => return None,
    };
    Some((
        Status::Failed,
        Error::new(kind, untraced(&err.to_string())).with_hint(hint),
    ))
}

/// A message without the tracing prefixes Codex logs with, such as
/// `2026-10-16T14:50:37.166Z ERROR codex_api::endpoint::responses_websocket: `.
fn untraced(message: &str) -> String {
    static PREFIX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"\d{4}-\d\d-\d\dT[\d:.]+Z\s+(?:ERROR|WARN|INFO|DEBUG|TRACE)\s+[\w:]+:\s*",
        )
        .expect("a valid pattern")
    });
    PREFIX.replace_all(message, "").into_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn tracing_prefixes_are_dropped() {
        assert_eq!(
            super::untraced(
                "codex authentication failed: 2026-10-16T14:50:37.166759Z ERROR codex_api::endpoint::responses_websocket: failed to connect to websocket: HTTP error: 401 Unauthorized"
            ),
            "codex authentication failed: failed to connect to websocket: HTTP error: 401 Unauthorized"
        );
    }
}
