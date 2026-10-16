//! Transcript projection: raw provider JSONL into bounded, allowlisted records.
//!
//! Anything not recognised becomes an `omitted` record with a category rather than being
//! dropped, so a reader can tell "nothing happened" from "something happened that this
//! projection does not show". Consecutive omitted records of one category collapse into one
//! with a count: a Claude session carries hundreds of `total_tokens_reminder` attachments,
//! and one record each would fill a page with noise.
//!
//! Files are streamed line by line. Codex sessions over 370 MB exist, so nothing here reads
//! a whole file into memory, and a page stops reading once it is full.
//!
//! Ported from trovador's `src/transcript.rs` (bottega d89c451); reads through
//! `archive::open_at`, so compressed Codex rollouts work, and drops the JSON Schema derives.

use std::io::{BufRead, BufReader, Read};

/// Bumped whenever the projection changes what it emits, which invalidates every index row.
pub const PROJECTION_VERSION: i64 = 9;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::archive::{ArchiveEntry, block_text};
use super::{Backend, Limits};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    User,
    Assistant,
    ToolCall,
    ToolResult,
    Attachment,
    System,
    /// What the model said about its own reasoning: Claude thinking text, Codex reasoning
    /// summaries. Only present when the provider kept readable text.
    Reasoning,
    Omitted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Position in the projection, stable for a given file and projection version.
    pub index: usize,
    pub kind: RecordKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Tool name, attachment file name, or omitted category.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// How many consecutive source records an `omitted` record stands for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}

/// A record before it has a position.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Draft {
    pub kind: RecordKind,
    pub text: Option<String>,
    pub name: Option<String>,
}

impl Draft {
    fn new(kind: RecordKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: Some(text.into()),
            name: None,
        }
    }
    fn named(kind: RecordKind, name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: Some(text.into()),
            name: Some(name.into()),
        }
    }
    fn omitted(category: impl Into<String>) -> Self {
        Self {
            kind: RecordKind::Omitted,
            text: None,
            name: Some(category.into()),
        }
    }
}

fn cap(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push_str(" ...[truncated]");
        out
    }
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

// --- claude ----------------------------------------------------------------

fn project_claude(r: &Value, limits: &Limits, out: &mut Vec<Draft>) {
    let (rc, tc) = (limits.record_chars, limits.tool_chars);
    match str_at(r, "type") {
        // Issue #38: client-generated context is not a user turn. Keep compact
        // summaries readable even when Claude also marks them as meta.
        Some("user") if r.get("isCompactSummary").and_then(Value::as_bool) == Some(true) => {
            let text = block_text(r.pointer("/message/content").unwrap_or(&Value::Null));
            out.push(if text.trim().is_empty() {
                Draft::omitted("compact_summary")
            } else {
                Draft::named(RecordKind::System, "compact_summary", cap(&text, rc))
            });
        }
        Some("user") if r.get("isMeta").and_then(Value::as_bool) == Some(true) => {
            out.push(Draft::omitted("meta"));
        }
        Some("user") => match r.pointer("/message/content") {
            Some(Value::String(s)) => out.push(Draft::new(RecordKind::User, cap(s, rc))),
            Some(Value::Array(blocks)) => {
                for b in blocks {
                    match str_at(b, "type") {
                        Some("text") => {
                            if let Some(t) = str_at(b, "text").filter(|t| !t.trim().is_empty()) {
                                out.push(Draft::new(RecordKind::User, cap(t, rc)));
                            }
                        }
                        Some("tool_result") => out.push(Draft::new(
                            RecordKind::ToolResult,
                            cap(&block_text(b.get("content").unwrap_or(&Value::Null)), tc),
                        )),
                        _ => {}
                    }
                }
            }
            _ => {}
        },
        Some("assistant") => {
            for b in r
                .pointer("/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match str_at(b, "type") {
                    Some("text") => {
                        if let Some(t) = str_at(b, "text").filter(|t| !t.trim().is_empty()) {
                            out.push(Draft::new(RecordKind::Assistant, cap(t, rc)));
                        }
                    }
                    Some("tool_use") => out.push(Draft::named(
                        RecordKind::ToolCall,
                        str_at(b, "name").unwrap_or("tool"),
                        cap(&b.get("input").unwrap_or(&Value::Null).to_string(), tc),
                    )),
                    // About three quarters of thinking blocks carry only a signature.
                    Some("thinking") => {
                        if let Some(t) = str_at(b, "thinking").filter(|t| !t.trim().is_empty()) {
                            out.push(Draft::new(RecordKind::Reasoning, cap(t, rc)));
                        }
                    }
                    _ => {}
                }
            }
        }
        // Most attachment subtypes are UI bookkeeping. Two carry content that is otherwise
        // unrecoverable: a file the user pasted or referenced, and a subagent's final
        // structured result.
        Some("attachment") => {
            let a = r.get("attachment").unwrap_or(&Value::Null);
            match str_at(a, "type") {
                Some("file") => match a.pointer("/content/file/content").and_then(Value::as_str) {
                    Some(content) => out.push(Draft::named(
                        RecordKind::Attachment,
                        str_at(a, "filename").unwrap_or("file"),
                        cap(content, rc),
                    )),
                    None => out.push(Draft::omitted("attachment")),
                },
                Some("structured_output") if a.get("data").is_some() => out.push(Draft::named(
                    RecordKind::Attachment,
                    "structured_output",
                    cap(&a["data"].to_string(), rc),
                )),
                _ => out.push(Draft::omitted("attachment")),
            }
        }
        // An away summary is a recap sent to the user; other system subtypes are noise.
        Some("system") => match (str_at(r, "subtype"), str_at(r, "content")) {
            (Some("away_summary"), Some(c)) => out.push(Draft::new(RecordKind::System, cap(c, rc))),
            _ => out.push(Draft::omitted("system")),
        },
        Some(other) => out.push(Draft::omitted(other)),
        None => {}
    }
}

// --- codex -----------------------------------------------------------------

/// Which of Codex's parallel streams carries the conversation.
///
/// Codex writes each turn more than once: a model-facing `response_item` (which includes
/// injected context) and a UI-facing event. Projecting both duplicates every message. The
/// UI-facing stream is what the user actually saw, so it wins whenever it exists:
///
/// - `Items`: current versions, `event_msg/item_completed` with typed items.
/// - `Events`: older versions, `event_msg/user_message` and `event_msg/agent_message`.
/// - `Responses`: the oldest files, where `response_item` (or a bare top-level record) is
///   all there is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum CodexMode {
    #[default]
    Responses,
    Events,
    Items,
}

impl CodexMode {
    pub fn as_i64(self) -> i64 {
        self as i64
    }
    pub fn from_i64(v: i64) -> Self {
        match v {
            2 => CodexMode::Items,
            1 => CodexMode::Events,
            _ => CodexMode::Responses,
        }
    }
}

/// The mode a single record proves the file is in, if any.
pub(crate) fn codex_mode_signal(r: &Value) -> Option<CodexMode> {
    if str_at(r, "type") != Some("event_msg") {
        return None;
    }
    match r.pointer("/payload/type").and_then(Value::as_str) {
        Some("item_completed") => Some(CodexMode::Items),
        Some("user_message" | "agent_message") => Some(CodexMode::Events),
        _ => None,
    }
}

pub(crate) fn project_codex(r: &Value, mode: CodexMode, limits: &Limits, out: &mut Vec<Draft>) {
    let (rc, tc) = (limits.record_chars, limits.tool_chars);
    let outer = str_at(r, "type");
    // The oldest files have no envelope: the record is its own payload.
    let p = match r.get("payload") {
        Some(p) => p,
        None if matches!(
            outer,
            Some("message" | "function_call" | "function_call_output")
        ) =>
        {
            r
        }
        None => return,
    };
    let inner = str_at(p, "type");
    match (outer, inner) {
        (_, Some("item_completed")) => {
            if mode == CodexMode::Items {
                project_codex_item(p.get("item").unwrap_or(&Value::Null), limits, out);
            }
        }
        (Some("event_msg"), Some("user_message")) => {
            if mode == CodexMode::Events
                && let Some(m) = str_at(p, "message")
            {
                match user_text(m) {
                    Some(text) => out.push(Draft::new(RecordKind::User, cap(&text, rc))),
                    None => out.push(Draft::omitted("injected_context")),
                }
            }
        }
        (Some("event_msg"), Some("agent_message")) => {
            if mode == CodexMode::Events
                && let Some(m) = str_at(p, "message")
            {
                out.push(Draft::new(RecordKind::Assistant, cap(m, rc)));
            }
        }
        (_, Some("message")) => {
            if mode != CodexMode::Responses
                && !(mode == CodexMode::Items && str_at(p, "role") == Some("user"))
            {
                return;
            }
            let content = p.get("content").unwrap_or(&Value::Null);
            if str_at(p, "role") == Some("user") {
                // Codex sends injected context (environment, AGENTS.md, plugin lists) as
                // user-role messages of their own. Without the pre-scan they reached
                // transcripts as user turns (over 1,200 on the development machine); they
                // are context, so they are counted but not shown.
                let typed = typed_text(content);
                if typed.trim().is_empty() {
                    if !block_text(content).trim().is_empty() {
                        out.push(Draft::omitted("injected_context"));
                    } else {
                        out.push(Draft::omitted("message"));
                    }
                } else {
                    out.push(Draft::new(RecordKind::User, cap(&typed, rc)));
                }
                return;
            }
            let text = block_text(content);
            if text.trim().is_empty() {
                return;
            }
            match str_at(p, "role") {
                Some("assistant") => out.push(Draft::new(RecordKind::Assistant, cap(&text, rc))),
                Some(role) => out.push(Draft::omitted(role)),
                None => out.push(Draft::omitted("message")),
            }
        }
        // In item mode the same calls arrive as CommandExecution and McpToolCall items.
        (_, Some("function_call" | "custom_tool_call")) => {
            if mode != CodexMode::Items {
                let args = str_at(p, "arguments")
                    .or_else(|| str_at(p, "input"))
                    .unwrap_or("");
                out.push(Draft::named(
                    RecordKind::ToolCall,
                    str_at(p, "name").unwrap_or("tool"),
                    cap(args, tc),
                ));
            }
        }
        (_, Some("function_call_output" | "custom_tool_call_output")) => {
            if mode != CodexMode::Items {
                let output = match p.get("output") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                out.push(Draft::new(RecordKind::ToolResult, cap(&output, tc)));
            }
        }
        // Reasoning, as summary headings. The full reasoning is encrypted; about four in
        // five records have no summary at all and project to nothing.
        (Some("response_item"), Some("reasoning")) => {
            if mode != CodexMode::Items {
                let text = p
                    .get("summary")
                    .and_then(Value::as_array)
                    .map(|s| {
                        s.iter()
                            .filter_map(|x| x.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                if !text.trim().is_empty() {
                    out.push(Draft::new(RecordKind::Reasoning, cap(&text, rc)));
                    return;
                }
            }
            out.push(Draft::omitted("reasoning"));
        }
        // The item stream carries the same search as a WebSearch item, with results.
        (Some("response_item"), Some("web_search_call")) => {
            if mode != CodexMode::Items {
                out.push(Draft::named(
                    RecordKind::ToolCall,
                    "web_search",
                    cap(
                        &search_queries(p.get("action").unwrap_or(&Value::Null), p),
                        tc,
                    ),
                ));
            } else {
                out.push(Draft::omitted("web_search_call"));
            }
        }
        // Tool discovery has no item form, so it is projected in every stream mode.
        (Some("response_item"), Some("tool_search_call")) => out.push(Draft::named(
            RecordKind::ToolCall,
            "tool_search",
            cap(
                p.pointer("/arguments/query")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                tc,
            ),
        )),
        (Some("response_item"), Some("tool_search_output")) => {
            let found = p
                .get("tools")
                .and_then(Value::as_array)
                .map(|ts| {
                    ts.iter()
                        .filter_map(|t| t.get("name").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            out.push(Draft::named(
                RecordKind::ToolResult,
                "tool_search",
                cap(&found, tc),
            ));
        }
        // A compaction replaces earlier history with a copy of it; only a written summary
        // is new. None of 3,234 local records had one, but a non-empty one is kept.
        (Some("compacted"), None) => match str_at(p, "message").filter(|m| !m.trim().is_empty()) {
            Some(m) => out.push(Draft::named(RecordKind::System, "compacted", cap(m, rc))),
            None => out.push(Draft::omitted("compacted")),
        },
        // Messages between Codex agents in a multi-agent run.
        (Some("response_item"), Some("agent_message")) => {
            let from = str_at(p, "author").unwrap_or("agent");
            let to = str_at(p, "recipient").unwrap_or("agent");
            out.push(Draft::named(
                RecordKind::System,
                format!("{from} -> {to}"),
                cap(&block_text(p.get("content").unwrap_or(&Value::Null)), rc),
            ));
        }
        (_, Some(kind)) => out.push(Draft::omitted(kind)),
        (Some(kind), None) => out.push(Draft::omitted(kind)),
        (None, None) => {}
    }
}

fn project_codex_item(item: &Value, limits: &Limits, out: &mut Vec<Draft>) {
    let (rc, tc) = (limits.record_chars, limits.tool_chars);
    let content = || block_text(item.get("content").unwrap_or(&Value::Null));
    match str_at(item, "type") {
        // The same rule as the response stream, so both copies of a turn project to the
        // same text and the one at a stream switch is recognised as a copy.
        Some("UserMessage") => {
            let content = item.get("content").unwrap_or(&Value::Null);
            let typed = typed_text(content);
            if !typed.trim().is_empty() {
                out.push(Draft::new(RecordKind::User, cap(&typed, rc)));
            } else if !block_text(content).trim().is_empty() {
                out.push(Draft::omitted("injected_context"));
            }
        }
        Some("AgentMessage") => {
            let t = content();
            if !t.trim().is_empty() {
                out.push(Draft::new(RecordKind::Assistant, cap(&t, rc)));
            }
        }
        Some("CommandExecution") => {
            let cmd = item
                .get("parsed_cmd")
                .and_then(Value::as_array)
                .map(|cmds| {
                    cmds.iter()
                        .filter_map(|c| str_at(c, "cmd"))
                        .collect::<Vec<_>>()
                        .join(" && ")
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| block_text(item.get("command").unwrap_or(&Value::Null)));
            out.push(Draft::named(RecordKind::ToolCall, "exec", cap(&cmd, tc)));
            let stdout = str_at(item, "stdout").unwrap_or("");
            let stderr = str_at(item, "stderr").unwrap_or("");
            let result = if stderr.is_empty() {
                stdout.to_string()
            } else {
                format!("{stdout}\n[stderr] {stderr}")
            };
            out.push(Draft::new(RecordKind::ToolResult, cap(&result, tc)));
        }
        Some("McpToolCall") => {
            let name = format!(
                "{}/{}",
                str_at(item, "server").unwrap_or("mcp"),
                str_at(item, "tool").unwrap_or("tool")
            );
            let args = item
                .get("arguments")
                .map(Value::to_string)
                .unwrap_or_default();
            out.push(Draft::named(RecordKind::ToolCall, name, cap(&args, tc)));
            if let Some(err) = item.pointer("/error/message").and_then(Value::as_str) {
                out.push(Draft::new(
                    RecordKind::ToolResult,
                    cap(&format!("error: {err}"), tc),
                ));
            } else if let Some(res) = item.get("result") {
                out.push(Draft::new(
                    RecordKind::ToolResult,
                    cap(&res.to_string(), tc),
                ));
            }
        }
        Some("FileChange") => {
            let changes = item
                .get("changes")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .map(|(path, c)| {
                            format!("{} {path}", str_at(c, "type").unwrap_or("change"))
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            out.push(Draft::named(
                RecordKind::ToolCall,
                "file_change",
                cap(&changes, tc),
            ));
        }
        Some("Extension") => {
            let what = str_at(item, "query")
                .map(str::to_string)
                .unwrap_or_else(|| item.get("action").map(Value::to_string).unwrap_or_default());
            out.push(Draft::named(
                RecordKind::ToolCall,
                str_at(item, "kind").unwrap_or("extension"),
                cap(&what, tc),
            ));
        }
        Some("SubAgentActivity") => out.push(Draft::new(
            RecordKind::System,
            format!(
                "subagent {} {}",
                str_at(item, "agent_path").unwrap_or("?"),
                str_at(item, "kind").unwrap_or("activity")
            ),
        )),
        Some("ContextCompaction") => out.push(Draft::new(RecordKind::System, "context compacted")),
        Some("Reasoning") => {
            let text = item
                .get("summary_text")
                .and_then(Value::as_array)
                .map(|s| {
                    s.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if text.trim().is_empty() {
                out.push(Draft::omitted("Reasoning"));
            } else {
                out.push(Draft::new(RecordKind::Reasoning, cap(&text, rc)));
            }
        }
        Some("WebSearch") => {
            out.push(Draft::named(
                RecordKind::ToolCall,
                "web_search",
                cap(
                    &search_queries(item.get("action").unwrap_or(&Value::Null), item),
                    tc,
                ),
            ));
            let results = item
                .get("results")
                .and_then(Value::as_array)
                .map(|rs| {
                    rs.iter()
                        .map(|r| {
                            format!(
                                "{} {}",
                                str_at(r, "title").unwrap_or(""),
                                str_at(r, "url").unwrap_or("")
                            )
                            .trim()
                            .to_string()
                        })
                        .filter(|l| !l.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if !results.is_empty() {
                out.push(Draft::named(
                    RecordKind::ToolResult,
                    "web_search",
                    cap(&results, tc),
                ));
            }
        }
        Some("FunctionCallOutput") => {
            let name = match (str_at(item, "namespace"), str_at(item, "name")) {
                (Some(ns), Some(n)) => format!("{ns}/{n}"),
                (None, Some(n)) => n.to_string(),
                _ => "function".to_string(),
            };
            let output = match item.get("output") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            out.push(Draft::named(RecordKind::ToolResult, name, cap(&output, tc)));
        }
        Some("ImageView") => out.push(Draft::named(
            RecordKind::ToolCall,
            "view_image",
            cap(str_at(item, "path").unwrap_or(""), tc),
        )),
        // Every one seen was the parent waiting on its subagents with none named; one that
        // names agents says who it was waiting for.
        Some("CollabAgentToolCall") => {
            let agents: Vec<String> = item
                .get("receiver_agents")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|x| match x {
                            Value::String(s) => s.clone(),
                            other => str_at(other, "agent_path")
                                .or_else(|| str_at(other, "name"))
                                .map(str::to_string)
                                .unwrap_or_else(|| other.to_string()),
                        })
                        .collect()
                })
                .unwrap_or_default();
            if agents.is_empty() {
                out.push(Draft::omitted("CollabAgentToolCall"));
            } else {
                out.push(Draft::named(
                    RecordKind::System,
                    str_at(item, "tool").unwrap_or("collab"),
                    cap(&agents.join(", "), tc),
                ));
            }
        }
        Some(other) => out.push(Draft::omitted(other)),
        None => out.push(Draft::omitted("item")),
    }
}

/// A Codex `compacted` record, read without building it into a JSON tree.
///
/// Each one carries a copy of the history it replaced: 562 such lines made up 3.3 GB of the
/// 10 GB local Codex archive, up to 15.7 MB each. Only `payload.message` is projected, and
/// it was empty in all of them, but parsing them whole set the peak memory of a full index
/// build (637 MB for Codex alone, against 77 MB for Claude). The type sits in the first
/// few hundred bytes, so the rest is skipped without allocating it.
fn compacted_without_parsing(line: &[u8], limits: &Limits) -> Option<Draft> {
    let head = &line[..line.len().min(512)];
    if !head.windows(18).any(|w| w == br#""type":"compacted""#) {
        return None;
    }
    #[derive(serde::Deserialize)]
    struct Compacted {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        payload: Option<CompactedPayload>,
    }
    #[derive(serde::Deserialize)]
    struct CompactedPayload {
        #[serde(default)]
        message: Option<String>,
    }
    let c: Compacted = serde_json::from_slice(line).ok()?;
    if c.kind != "compacted" {
        return None;
    }
    Some(
        match c
            .payload
            .and_then(|p| p.message)
            .filter(|m| !m.trim().is_empty())
        {
            Some(m) => Draft::named(
                RecordKind::System,
                "compacted",
                cap(&m, limits.record_chars),
            ),
            None => Draft::omitted("compacted"),
        },
    )
}

/// Whether a block of a user-role message is context a client injected rather than text a
/// person typed: a leading tag such as `<environment_context>`, or the project's AGENTS.md.
fn is_injected(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('<') || t.starts_with("# AGENTS.md instructions")
}

fn user_text(text: &str) -> Option<String> {
    // Issue #37: question replies carry user decisions inside a client wrapper. Only
    // decode this exact envelope so other injected context remains omitted.
    if let Some(payload) = text
        .trim()
        .strip_prefix("<send_user_message_question_reply>")
    {
        let payload = payload.strip_suffix("</send_user_message_question_reply>")?;
        let replies: Vec<Value> = serde_json::from_str(payload).ok()?;
        let answers: Vec<String> = replies
            .iter()
            .filter_map(|reply| {
                let answer = reply.get("answer")?.as_str()?;
                Some(match reply.get("question").and_then(Value::as_str) {
                    Some(question) => format!("{question} → {answer}"),
                    None => answer.to_owned(),
                })
            })
            .collect();
        return (!answers.is_empty()).then(|| answers.join("\n"));
    }
    (!is_injected(text)).then(|| text.to_owned())
}

/// The blocks of a message a person typed, joined.
fn typed_text(content: &Value) -> String {
    match content {
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .filter(|t| !t.trim().is_empty())
            .filter_map(user_text)
            .collect::<Vec<_>>()
            .concat(),
        Value::String(s) => user_text(s).unwrap_or_default(),
        _ => String::new(),
    }
}

/// A web search's queries: the list when there is one, else the single query.
fn search_queries(action: &Value, outer: &Value) -> String {
    action
        .get("queries")
        .and_then(Value::as_array)
        .map(|q| {
            q.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|s| !s.is_empty())
        .or_else(|| str_at(action, "query").map(str::to_string))
        .or_else(|| str_at(outer, "query").map(str::to_string))
        .unwrap_or_default()
}

// --- reading ---------------------------------------------------------------

/// Where a projection stopped, so the index can continue an append-only file from there
/// and assign exactly the record indexes a full read would.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectionState {
    /// Bytes consumed, always at a line boundary.
    pub offset: u64,
    /// Issue #35: a known byte boundary survives index resumes without rescanning history.
    pub codex_inherited_end: Option<u64>,
    pub next_index: usize,
    /// Issue #36: retain typed titles across resumes without rereading the old transcript.
    pub custom_title: Option<String>,
    pub mode: CodexMode,
    /// An omitted run still open at `offset`. A full read would merge records appended
    /// after it into the same run, so it is carried rather than flushed.
    pub run: Option<(String, usize)>,
    /// The last turn eligible for a different-stream copy, with a hash of concatenated source
    /// text for users and projected text for assistants. Persisted across index resumes.
    pub last_turn: Option<(RecordKind, u64)>,
    /// Clients may add injected blocks to just one copy, so retain its filtered hash too.
    pub last_user_filtered_hash: Option<u64>,
    /// Each stream can contribute one copy, including across an index resume.
    pub last_user_streams: u8,
}

/// FNV-1a, 64-bit. Stable across Rust versions and platforms, unlike the std hasher,
/// because the value is stored in the index and compared on a later run.
pub fn text_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn codex_user_hash(r: &Value, filtered: bool) -> Option<u64> {
    let p = r.get("payload").unwrap_or(r);
    let content = match str_at(p, "type") {
        Some("message") if str_at(p, "role") == Some("user") => p.get("content")?,
        Some("item_completed")
            if p.pointer("/item/type").and_then(Value::as_str) == Some("UserMessage") =>
        {
            p.pointer("/item/content")?
        }
        Some("user_message") => {
            let text = str_at(p, "message")?;
            return Some(text_hash(&if filtered {
                user_text(text).unwrap_or_default()
            } else {
                text.to_owned()
            }));
        }
        _ => return None,
    };
    let text = match content {
        Value::String(s) => {
            if filtered {
                user_text(s).unwrap_or_default()
            } else {
                s.clone()
            }
        }
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| str_at(b, "text"))
            .filter_map(|text| {
                if filtered {
                    user_text(text)
                } else {
                    Some(text.to_owned())
                }
            })
            .collect::<String>(),
        _ => return None,
    };
    Some(text_hash(&text))
}

fn is_turn(k: RecordKind) -> bool {
    matches!(k, RecordKind::User | RecordKind::Assistant)
}

/// Streams a session's projected records, with omitted runs collapsed and indexes assigned.
pub struct Projection {
    reader: BufReader<Box<dyn Read>>,
    backend: Backend,
    inherited_end: u64,
    scan_limit: Option<u64>,
    limits: Limits,
    state: ProjectionState,
    pending: std::collections::VecDeque<Draft>,
    /// Indexing reads only whole lines and leaves an open run open at the end; a transcript
    /// page reads whatever is there and closes the run.
    for_index: bool,
    done: bool,
    line: Vec<u8>,
}

impl Projection {
    /// A full read from the start, as a transcript page wants it.
    pub fn open(e: &ArchiveEntry, limits: &Limits) -> std::io::Result<Self> {
        Self::start(e, limits, None, false)
    }

    /// A read for the index: from `resume` when given, whole lines only, with the final
    /// state available from [`Projection::state`] once the iterator is exhausted.
    pub fn for_index(
        e: &ArchiveEntry,
        limits: &Limits,
        resume: Option<ProjectionState>,
    ) -> std::io::Result<Self> {
        Self::start(e, limits, resume, true)
    }

    fn start(
        e: &ArchiveEntry,
        limits: &Limits,
        resume: Option<ProjectionState>,
        for_index: bool,
    ) -> std::io::Result<Self> {
        // Without a resume point every file starts in the oldest stream and moves forward
        // only as newer stream records appear, in order. A pre-scan that started in the
        // newest stream it could find dropped the older-format turns of a session continued
        // by a newer client (trovador#31); the copy written at each switch is dropped by
        // comparing it with the last emitted turn instead.
        let mut state = resume.unwrap_or_default();
        let mut scan_limit = None;
        let inherited_end = if e.backend == Backend::Codex {
            if let Some(end) = state.codex_inherited_end {
                end
            } else {
                let boundary = super::codex::fork_boundary(
                    &mut super::archive::open_buffered(&e.file)?,
                    e.archived,
                    true,
                )?;
                // Issue #35: terminal events before the start ordinal belong to
                // copied history. Display may fall back, but indexing must wait
                // for the real boundary and never persist the parent's completion.
                state.codex_inherited_end = boundary.end.filter(|_| !boundary.display_fallback);
                let end = boundary
                    .end
                    .filter(|_| !for_index || !boundary.display_fallback);
                if end.is_none() {
                    scan_limit = Some(boundary.scanned);
                }
                end.unwrap_or(boundary.scanned)
            }
        } else {
            0
        };
        Ok(Self {
            reader: super::archive::open_at(&e.file, state.offset)?,
            backend: e.backend,
            inherited_end,
            scan_limit,
            limits: limits.clone(),
            state,
            pending: Default::default(),
            for_index,
            done: false,
            line: Vec::new(),
        })
    }

    /// Where the read stopped. Meaningful once the iterator has returned `None`.
    pub fn state(&self) -> &ProjectionState {
        &self.state
    }

    fn fill(&mut self) {
        while self.pending.is_empty() && !self.done {
            // Issue #35: bytes appended after an incomplete boundary scan belong to
            // the next pass, which can determine whether they are child-owned.
            if self
                .scan_limit
                .is_some_and(|limit| self.state.offset >= limit)
            {
                self.done = true;
                break;
            }
            self.line.clear();
            let n = match self.reader.read_until(b'\n', &mut self.line) {
                Ok(0) | Err(_) => {
                    self.done = true;
                    break;
                }
                Ok(n) => n,
            };
            // A torn final line is normal in a session still being written. The index
            // leaves it for the next pass; a transcript page tries it and skips it.
            if !self.line.ends_with(b"\n") && self.for_index {
                self.done = true;
                break;
            }
            let line_start = self.state.offset;
            self.state.offset += n as u64;
            if line_start > 0 && line_start < self.inherited_end {
                self.pending.push_back(Draft::omitted("forked_history"));
                continue;
            }
            if self.line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            if self.backend == Backend::Codex
                && let Some(d) = compacted_without_parsing(&self.line, &self.limits)
            {
                self.pending.push_back(d);
                continue;
            }
            // Parse the bytes directly; copy through a lossy conversion only for the rare
            // line that is not valid UTF-8.
            let parsed = serde_json::from_slice::<Value>(&self.line)
                .or_else(|_| serde_json::from_str::<Value>(&String::from_utf8_lossy(&self.line)));
            let Ok(r) = parsed else {
                continue;
            };
            if self.backend == Backend::Claude
                && let Some(title) = super::archive::custom_title(&r)
            {
                self.state.custom_title = Some(title.to_owned());
            }
            let mut drafts = Vec::new();
            match self.backend {
                Backend::Claude => project_claude(&r, &self.limits, &mut drafts),
                Backend::Codex => {
                    // A stream seen later than the pre-scan reached still takes over from
                    // that point on.
                    // A new task cannot be a copy of the preceding task's identical user.
                    if matches!(
                        r.pointer("/payload/type").and_then(Value::as_str),
                        Some("task_started" | "task_complete")
                    ) && self
                        .state
                        .last_turn
                        .is_some_and(|(kind, _)| kind == RecordKind::User)
                    {
                        self.state.last_turn = None;
                        self.state.last_user_filtered_hash = None;
                        self.state.last_user_streams = 0;
                    }
                    let before = self.state.mode;
                    if let Some(m) = codex_mode_signal(&r) {
                        self.state.mode = self.state.mode.max(m);
                    }
                    project_codex(&r, self.state.mode, &self.limits, &mut drafts);
                    // Issues #19 and #34: copies may span all three streams. Remember
                    // which streams matched instead of consuming eligibility at the first
                    // switch. A repeated stream starts a new turn, so consecutive identical
                    // user messages survive even without a task boundary.
                    // Exact concatenation preserves the real flattened item copy. The
                    // filtered hash also allows injected blocks added to only one copy;
                    // neither hash ignores whitespace or compares a truncated prefix.
                    let source_hash = codex_user_hash(&r, false);
                    let filtered_hash = codex_user_hash(&r, true);
                    let stream = if r.pointer("/payload/item/type").and_then(Value::as_str)
                        == Some("UserMessage")
                    {
                        4
                    } else if r.pointer("/payload/type").and_then(Value::as_str)
                        == Some("user_message")
                    {
                        2
                    } else if source_hash.is_some() {
                        1
                    } else {
                        0
                    };
                    let user_copy = stream != 0
                        && self.state.last_user_streams & stream == 0
                        && self.state.last_turn.is_some_and(|(kind, hash)| {
                            kind == RecordKind::User
                                && (source_hash == Some(hash)
                                    || (filtered_hash.is_some()
                                        && filtered_hash == self.state.last_user_filtered_hash))
                        });
                    if user_copy {
                        drafts.retain(|d| d.kind != RecordKind::User);
                        self.state.last_user_streams |= stream;
                    } else if self.state.mode > before
                        && let Some(last) = self.state.last_turn
                        && let Some(i) = drafts.iter().position(|d| {
                            d.kind == RecordKind::Assistant
                                && (d.kind, text_hash(d.text.as_deref().unwrap_or(""))) == last
                        })
                    {
                        drafts.remove(i);
                    } else if let Some(d) = drafts.iter().rev().find(|d| is_turn(d.kind)) {
                        self.state.last_user_filtered_hash = filtered_hash;
                        self.state.last_user_streams = stream;
                        self.state.last_turn = Some((
                            d.kind,
                            source_hash
                                .unwrap_or_else(|| text_hash(d.text.as_deref().unwrap_or(""))),
                        ));
                    }
                }
            }
            self.pending.extend(drafts);
        }
    }

    fn assign(&mut self, d: Draft, count: Option<usize>) -> Record {
        let index = self.state.next_index;
        self.state.next_index += 1;
        Record {
            index,
            kind: d.kind,
            text: d.text,
            name: d.name,
            count,
        }
    }
}

impl Iterator for Projection {
    type Item = Record;

    fn next(&mut self) -> Option<Record> {
        loop {
            self.fill();
            let Some(d) = self.pending.pop_front() else {
                if self.for_index {
                    return None;
                }
                // End of file: close the run in progress.
                let (cat, n) = self.state.run.take()?;
                return Some(self.assign(Draft::omitted(cat), Some(n)));
            };
            if d.kind == RecordKind::Omitted {
                let cat = d.name.clone().unwrap_or_default();
                match &mut self.state.run {
                    Some((c, n)) if *c == cat => *n += 1,
                    Some(_) => {
                        let (prev, n) = self.state.run.replace((cat, 1)).expect("run is some");
                        return Some(self.assign(Draft::omitted(prev), Some(n)));
                    }
                    None => self.state.run = Some((cat, 1)),
                }
                continue;
            }
            if let Some((cat, n)) = self.state.run.take() {
                self.pending.push_front(d);
                return Some(self.assign(Draft::omitted(cat), Some(n)));
            }
            return Some(self.assign(d, None));
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptPage {
    pub offset: usize,
    pub records: Vec<Record>,
    /// More records exist after this page.
    pub has_more: bool,
    /// The next offset to ask for, when `has_more`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    /// Total record count, known only when the page reached the end of the file. Reading a
    /// 370 MB session to the end just to count it is not worth doing for a single page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    /// Set when the page stopped early because it reached the response size cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated_by: Option<String>,
}

/// One page of a session's records from index `offset`. With `kinds`, only records of those
/// kinds are returned; indexes and `total` still count every record.
pub fn page(
    e: &ArchiveEntry,
    offset: usize,
    limit: Option<usize>,
    kinds: Option<&[RecordKind]>,
    limits: &Limits,
) -> std::io::Result<TranscriptPage> {
    let limit = limit.unwrap_or(limits.page_size).clamp(1, limits.page_size);
    let mut records = Vec::new();
    let mut chars = 0usize;
    let mut seen = 0usize;
    for r in Projection::open(e, limits)? {
        seen += 1;
        if r.index < offset || kinds.is_some_and(|k| !k.contains(&r.kind)) {
            continue;
        }
        let size = r.text.as_deref().map(str::len).unwrap_or(0) + 64;
        let full = records.len() == limit;
        let over = !records.is_empty() && chars + size > limits.response_max_chars;
        if full || over {
            return Ok(TranscriptPage {
                offset,
                records,
                has_more: true,
                next_offset: Some(r.index),
                total: None,
                truncated_by: (over && !full).then(|| {
                    format!(
                        "response_max_chars ({}); continue from next_offset",
                        limits.response_max_chars
                    )
                }),
            });
        }
        chars += size;
        records.push(r);
    }
    Ok(TranscriptPage {
        offset,
        records,
        has_more: false,
        next_offset: None,
        total: Some(seen),
        truncated_by: None,
    })
}

/// Plain text, one record per line, for summaries and search excerpts.
pub fn render(r: &Record) -> String {
    let text = r.text.as_deref().unwrap_or("");
    match r.kind {
        RecordKind::Omitted => format!(
            "[omitted: {}{}]",
            r.name.as_deref().unwrap_or("?"),
            r.count
                .filter(|n| *n > 1)
                .map(|n| format!(" x{n}"))
                .unwrap_or_default()
        ),
        RecordKind::ToolCall => {
            format!("TOOL CALL: {}({text})", r.name.as_deref().unwrap_or("tool"))
        }
        RecordKind::ToolResult => format!("TOOL RESULT: {text}"),
        RecordKind::Attachment => match &r.name {
            Some(n) => format!("ATTACHMENT ({n}): {text}"),
            None => format!("ATTACHMENT: {text}"),
        },
        RecordKind::User => format!("USER: {text}"),
        RecordKind::Assistant => format!("ASSISTANT: {text}"),
        RecordKind::System => match &r.name {
            Some(n) => format!("SYSTEM ({n}): {text}"),
            None => format!("SYSTEM: {text}"),
        },
        RecordKind::Reasoning => format!("REASONING: {text}"),
    }
}
