//! One Claude Code turn through claude-wrapper.

use std::path::Path;
use std::time::Duration;

use claude_wrapper::auth::AuthErrorKind;
use claude_wrapper::streaming::{StreamEvent, stream_query_cancellable};
use claude_wrapper::{Claude, Effort, OutputFormat, PermissionMode, QueryCommand, QueryResult};
use serde_json::Value as Json;

use super::{Access, Outcome, Request, Status, Usage};
use crate::error::Error;

/// Read-only tools: no Bash, no subagents, no skills, no MCP.
const READ_TOOLS: &[&str] = &["Read", "Glob", "Grep"];
const WRITE_TOOLS: &[&str] = &["Read", "Glob", "Grep", "Edit", "Write", "NotebookEdit"];
const DEFAULT_MAX_TURNS: u32 = 30;

pub(super) fn effort(name: &str) -> Result<Effort, Error> {
    Ok(match name {
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "high" => Effort::High,
        "xhigh" => Effort::Xhigh,
        "max" => Effort::Max,
        other => {
            return Err(
                Error::new("bad_option", format!("claude has no effort \"{other}\""))
                    .with_hint("effort for claude is low, medium, high, xhigh or max"),
            );
        }
    })
}

/// Runs one turn. With `request.events`, Claude streams its events and `events` collects them;
/// the final result event is classified the same way either way.
pub(super) async fn run(
    request: &Request,
    binary: Option<&Path>,
    on_spawn: Option<super::SpawnHook>,
    events: &mut Vec<Json>,
) -> Outcome {
    let mut builder = Claude::builder()
        .env("TIMON_CHILD", "1")
        // Linux only: the kernel stops the provider if timon dies. Elsewhere the run record
        // names the process, and the next timon command stops it.
        .die_with_parent(true)
        .kill_grace(Duration::from_millis(250))
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

    let mut command = QueryCommand::new(&request.prompt)
        .prompt_via_stdin(true)
        .max_turns(request.budget.turns.unwrap_or(DEFAULT_MAX_TURNS))
        .safe_mode()
        .strict_mcp_config()
        .mcp_config(r#"{"mcpServers":{}}"#)
        .disable_slash_commands();
    command = match request.access {
        Access::Full => command.dangerously_skip_permissions(),
        access => {
            let (mode, defaults) = if access == Access::Read {
                (PermissionMode::DontAsk, READ_TOOLS)
            } else {
                (PermissionMode::AcceptEdits, WRITE_TOOLS)
            };
            let tools: Vec<String> = match &request.tools {
                Some(tools) => tools.clone(),
                None => defaults.iter().map(|t| (*t).to_owned()).collect(),
            };
            if tools.is_empty() {
                // An empty list would emit no flag and leave Claude's defaults on; `--tools ""`
                // turns every tool off.
                command.permission_mode(mode).tools([""])
            } else {
                command
                    .permission_mode(mode)
                    .tools(tools.clone())
                    .allowed_tools(tools)
            }
        }
    };
    if let Some(model) = &request.model {
        command = command.model(model);
    }
    if let Some(name) = &request.effort {
        command = command.effort(effort(name).map_err(|e| (Status::Failed, e))?);
    }
    if let Some(system) = &request.system {
        command = command.append_system_prompt(system);
    }
    if let Some(shape) = &request.output {
        command = command.json_schema(shape.json_schema().to_string());
    }
    if request.ephemeral {
        command = command.no_session_persistence();
    }
    if let Some(usd) = request.budget.usd {
        command = command.max_budget_usd(usd);
    }
    match (&request.resume, &request.fork) {
        (Some(session), _) => command = command.resume(session),
        (None, Some(session)) => command = command.resume(session).fork_session(),
        (None, None) => {}
    }

    let never = std::future::pending::<()>();
    let result = if request.events {
        let command = command.output_format(OutputFormat::StreamJson);
        let mut last: Option<Json> = None;
        let streamed = stream_query_cancellable(&client, &command, never, |event: StreamEvent| {
            if event.is_result() {
                last = Some(event.data.clone());
            }
            events.extend(super::events::from_claude(&event.data));
        })
        .await;
        match (streamed, last) {
            // A failed turn exits non-zero after its result event; classify the event.
            (Ok(_) | Err(claude_wrapper::Error::CommandFailed { .. }), Some(last)) => {
                serde_json::from_value::<QueryResult>(last).map_err(|e| {
                    (
                        Status::Failed,
                        Error::new(
                            "provider_failed",
                            format!("claude's result is not readable: {e}"),
                        ),
                    )
                })?
            }
            (Ok(_), None) => {
                return Err((
                    Status::Failed,
                    Error::new("provider_failed", "claude ended without a result"),
                ));
            }
            (Err(e), _) => return Err(failure(&e)),
        }
    } else {
        command
            .execute_json_cancellable(&client, never)
            .await
            .map_err(|e| failure(&e))?
    };
    answer(result)
}

/// Classifies Claude's final result.
fn answer(mut result: QueryResult) -> Outcome {
    let subtype = result
        .extra
        .get("subtype")
        .and_then(Json::as_str)
        .unwrap_or("success")
        .to_owned();
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
        usd: result.cost_usd,
    };
    let session = Some(result.session_id.clone()).filter(|s| !s.is_empty());
    if result.is_error || subtype != "success" {
        // A login problem ends with an error result too (on stdout, exit 1); in stream mode
        // timon sees that result first, so it is classified here as well as in `failure`.
        if let Some(kind) = claude_wrapper::auth::classify_failure(1, &result.result, "") {
            return Err(auth(kind, result.result.clone()));
        }
        let (kind, hint) = match subtype.as_str() {
            "error_max_turns" => ("max_turns", "raise budget.turns, or split the task"),
            "error_max_budget_usd" => ("max_budget", "raise budget.usd"),
            _ => ("provider_failed", "see the message; the run did not finish"),
        };
        let message = if result.result.is_empty() {
            format!("claude ended with {subtype}")
        } else {
            result.result.clone()
        };
        return Err((Status::Failed, Error::new(kind, message).with_hint(hint)));
    }
    Ok(super::Answer {
        text: result.result.clone(),
        structured: result.extra.remove("structured_output"),
        session,
        usage,
    })
}

fn failure(err: &claude_wrapper::Error) -> (Status, Error) {
    use claude_wrapper::Error as E;
    match err {
        E::Timeout { .. } => (
            Status::Timeout,
            Error::new("timeout", "claude did not finish in time").with_hint("raise timeout"),
        ),
        E::Cancelled => (
            Status::Cancelled,
            Error::new("cancelled", "the run was cancelled"),
        ),
        E::NotFound => (
            Status::Failed,
            Error::new("not_installed", "claude is not installed or not on PATH")
                .with_hint("install Claude Code, or set TIMON_CLAUDE_BIN"),
        ),
        E::Auth { kind, message, .. } => auth(*kind, readable(message)),
        E::MaxTurnsExceeded { .. } => (
            Status::Failed,
            Error::new("max_turns", "claude reached its turn limit")
                .with_hint("raise budget.turns, or split the task"),
        ),
        E::MaxBudgetExceeded { .. } => (
            Status::Failed,
            Error::new("max_budget", "claude reached its budget").with_hint("raise budget.usd"),
        ),
        E::CommandFailed {
            exit_code, stderr, ..
        } => match unknown_model(stderr) {
            Some(model) => (
                Status::Failed,
                Error::new(
                    "unknown_model",
                    format!("claude does not know the model \"{model}\""),
                )
                .with_hint("use a model Claude Code knows, such as haiku, sonnet or opus"),
            ),
            None => (
                Status::Failed,
                Error::new(
                    "provider_failed",
                    format!("claude exited {exit_code}: {}", diagnostic(stderr)),
                ),
            ),
        },
        other => (
            Status::Failed,
            Error::new("provider_failed", other.to_string()),
        ),
    }
}

fn auth(kind: AuthErrorKind, message: String) -> (Status, Error) {
    let (kind, hint) = match kind {
        AuthErrorKind::NotAuthenticated => ("not_logged_in", "run claude, then /login"),
        AuthErrorKind::Expired => ("auth_expired", "run claude, then /login"),
        AuthErrorKind::RateLimit => ("rate_limited", "wait, then try again"),
        _ => ("auth", "check claude's login with: claude auth status"),
    };
    (Status::Failed, Error::new(kind, message).with_hint(hint))
}

/// The model Claude rejected, from `[claude-code:unrecognized_model] {"model":"..."}` on stderr
/// (Claude Code 2.1.301).
fn unknown_model(stderr: &str) -> Option<String> {
    let rest = stderr.split("[claude-code:unrecognized_model]").nth(1)?;
    let detail = rest.lines().next().unwrap_or("").trim();
    Some(
        serde_json::from_str::<Json>(detail)
            .ok()
            .and_then(|j| j["model"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "?".to_owned()),
    )
}

/// The human part of a CLI message: the `result` text when the message is Claude's result
/// JSON, as it is when the CLI exits 1 after printing an error result.
fn readable(message: &str) -> String {
    serde_json::from_str::<Json>(message.trim())
        .ok()
        .and_then(|j| j["result"].as_str().map(str::to_owned))
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| message.trim().to_owned())
}

/// A stream's diagnostic for an error message: its last few non-blank lines, led by its first
/// `error` line (or first line) when a usage footer or other output pushed that out.
pub(super) fn diagnostic(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() <= 5 {
        return lines.join(" | ");
    }
    let start = lines.len() - 4;
    let primary = lines[..start]
        .iter()
        .find(|l| l.to_lowercase().starts_with("error"))
        .unwrap_or(&lines[0]);
    std::iter::once(primary)
        .chain(&lines[start..])
        .copied()
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_error_line_outlives_a_long_tail() {
        let stderr = "starting\nerror: unexpected argument '--x' found\n\n  tip: use '-- --x'\n\n\
                      Usage: codex [OPTIONS] [PROMPT]\n       codex [OPTIONS] <COMMAND> [ARGS]\n\n\
                      For more information, try '--help'.\n";
        assert_eq!(
            super::diagnostic(stderr),
            "error: unexpected argument '--x' found | tip: use '-- --x' | Usage: codex [OPTIONS] [PROMPT] \
             | codex [OPTIONS] <COMMAND> [ARGS] | For more information, try '--help'."
        );
        assert_eq!(super::diagnostic("a\n\nb\n"), "a | b");
        assert_eq!(super::diagnostic("1\n2\n3\n4\n5\n6"), "1 | 3 | 4 | 5 | 6");
    }
}
