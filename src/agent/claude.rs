//! One Claude Code turn through claude-wrapper.

use std::path::Path;
use std::time::Duration;

use claude_wrapper::auth::AuthErrorKind;
use claude_wrapper::{Claude, Effort, PermissionMode, QueryCommand};
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

pub(super) async fn run(request: &Request, binary: Option<&Path>) -> Outcome {
    let mut builder = Claude::builder()
        .env("TIMON_CHILD", "1")
        .kill_grace(Duration::from_millis(250))
        .output_limit(16 * 1024 * 1024);
    if let Some(binary) = binary {
        builder = builder.binary(binary);
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

    let mut result = command
        .execute_json_cancellable(&client, std::future::pending::<()>())
        .await
        .map_err(|e| failure(&e))?;

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
        E::Auth { kind, message, .. } => {
            let (kind, hint) = match kind {
                AuthErrorKind::NotAuthenticated => ("not_logged_in", "run claude, then /login"),
                AuthErrorKind::Expired => ("auth_expired", "run claude, then /login"),
                AuthErrorKind::RateLimit => ("rate_limited", "wait, then try again"),
                _ => ("auth", "check claude's login with: claude auth status"),
            };
            (
                Status::Failed,
                Error::new(kind, message.clone()).with_hint(hint),
            )
        }
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
        } => (
            Status::Failed,
            Error::new(
                "provider_failed",
                format!("claude exited {exit_code}: {}", last_lines(stderr)),
            ),
        ),
        other => (
            Status::Failed,
            Error::new("provider_failed", other.to_string()),
        ),
    }
}

/// The last few lines of a stream, for an error message.
pub(super) fn last_lines(text: &str) -> String {
    let lines: Vec<&str> = text.trim().lines().collect();
    let start = lines.len().saturating_sub(5);
    lines[start..].join(" | ")
}
