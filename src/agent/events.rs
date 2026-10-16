//! `events = true`: what the agent did, as one list of events in the same shape for both
//! providers.
//!
//! ```text
//! { kind = "text",        text }
//! { kind = "reasoning",   text }
//! { kind = "tool_call",   id, name, input }
//! { kind = "tool_result", id, error, text }
//! { kind = "omitted",     count }            -- past MAX_EVENTS
//! ```
//!
//! Claude's come from its `stream-json` output, Codex's from the `exec --json` items. Text and
//! string inputs are capped so one large file read cannot dominate a record.

use serde_json::{Value as Json, json};

/// Characters kept of any one string.
const MAX_CHARS: usize = 2_000;
/// Events kept per run; the rest are counted in one `omitted` event.
const MAX_EVENTS: usize = 500;

fn text(kind: &str, text: &str) -> Json {
    json!({ "kind": kind, "text": text })
}

fn call(id: &Json, name: &str, input: Json) -> Json {
    json!({ "kind": "tool_call", "id": id, "name": name, "input": input })
}

fn result(id: &Json, error: bool, text: &str) -> Json {
    json!({ "kind": "tool_result", "id": id, "error": error, "text": text })
}

/// The events in one line of Claude's `stream-json` output.
pub fn from_claude(line: &Json) -> Vec<Json> {
    let kind = line["type"].as_str().unwrap_or("");
    if kind != "assistant" && kind != "user" {
        return Vec::new();
    }
    let Some(blocks) = line["message"]["content"].as_array() else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|b| match b["type"].as_str()? {
            "text" if kind == "assistant" => Some(text("text", b["text"].as_str()?)),
            "thinking" => Some(text("reasoning", b["thinking"].as_str()?)),
            "tool_use" => Some(call(&b["id"], b["name"].as_str()?, b["input"].clone())),
            "tool_result" => {
                let body = match &b["content"] {
                    Json::String(s) => s.clone(),
                    Json::Array(parts) => parts
                        .iter()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
                let error = b["is_error"].as_bool().unwrap_or(false);
                Some(result(&b["tool_use_id"], error, &body))
            }
            _ => None,
        })
        .collect()
}

/// The events in one Codex `exec --json` event and its `item`; only completed items carry any.
pub fn from_codex(event_type: &str, item: Option<&Json>) -> Vec<Json> {
    let (true, Some(item)) = (event_type == "item.completed", item) else {
        return Vec::new();
    };
    let id = &item["id"];
    let s = |key: &str| item[key].as_str().unwrap_or("").to_owned();
    match item["type"].as_str().unwrap_or("") {
        "agent_message" => vec![text("text", &s("text"))],
        "reasoning" => vec![text("reasoning", &s("text"))],
        "command_execution" => {
            let failed = item["exit_code"].as_i64().is_some_and(|c| c != 0)
                || item["status"].as_str() == Some("failed");
            vec![
                call(id, "shell", json!({ "command": item["command"] })),
                result(id, failed, &s("aggregated_output")),
            ]
        }
        "file_change" => vec![
            call(id, "file_change", json!({ "changes": item["changes"] })),
            result(id, item["status"].as_str() == Some("failed"), &s("status")),
        ],
        "mcp_tool_call" => {
            let name = format!("{}.{}", s("server"), s("tool"));
            let error = !item["error"].is_null();
            let body = if error {
                item["error"]["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| item["error"].to_string())
            } else {
                item["result"].to_string()
            };
            vec![
                call(id, &name, item["arguments"].clone()),
                result(id, error, &body),
            ]
        }
        "web_search" => vec![call(id, "web_search", json!({ "query": item["query"] }))],
        _ => Vec::new(),
    }
}

fn cap(value: &mut Json) {
    match value {
        Json::String(s) if s.chars().count() > MAX_CHARS => {
            let total = s.chars().count();
            let kept: String = s.chars().take(MAX_CHARS).collect();
            *s = format!("{kept}... [{} more characters]", total - MAX_CHARS);
        }
        Json::Array(items) => items.iter_mut().for_each(cap),
        Json::Object(map) => map.values_mut().for_each(cap),
        _ => {}
    }
}

/// Caps every string and the number of events.
pub fn finish(mut events: Vec<Json>) -> Vec<Json> {
    events.iter_mut().for_each(cap);
    if events.len() > MAX_EVENTS {
        let count = events.len() - MAX_EVENTS;
        events.truncate(MAX_EVENTS);
        events.push(json!({ "kind": "omitted", "count": count }));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_blocks_become_events() {
        let line = json!({ "type": "assistant", "message": { "content": [
            { "type": "thinking", "thinking": "look first" },
            { "type": "tool_use", "id": "t1", "name": "Read", "input": { "file_path": "a.rs" } },
        ] } });
        assert_eq!(
            from_claude(&line),
            [
                json!({ "kind": "reasoning", "text": "look first" }),
                json!({ "kind": "tool_call", "id": "t1", "name": "Read", "input": { "file_path": "a.rs" } }),
            ]
        );
        let line = json!({ "type": "user", "message": { "content": [
            { "type": "tool_result", "tool_use_id": "t1", "is_error": true,
              "content": [{ "type": "text", "text": "no such file" }] } ] } });
        assert_eq!(
            from_claude(&line),
            [json!({ "kind": "tool_result", "id": "t1", "error": true, "text": "no such file" })]
        );
    }

    #[test]
    fn codex_items_become_events() {
        let item = json!({ "id": "i2", "type": "command_execution", "command": "ls",
            "aggregated_output": "a.rs\n", "exit_code": 0, "status": "completed" });
        let events = from_codex("item.completed", Some(&item));
        assert_eq!(events[0]["name"], "shell");
        assert_eq!(
            events[1],
            json!({ "kind": "tool_result", "id": "i2", "error": false, "text": "a.rs\n" })
        );
        assert!(from_codex("turn.started", Some(&item)).is_empty());
    }

    #[test]
    fn long_text_and_long_runs_are_capped() {
        let long = "x".repeat(MAX_CHARS + 5);
        let events = finish(vec![text("text", &long); MAX_EVENTS + 2]);
        assert_eq!(events.len(), MAX_EVENTS + 1);
        assert!(
            events[0]["text"]
                .as_str()
                .unwrap()
                .ends_with("... [5 more characters]")
        );
        assert_eq!(events[MAX_EVENTS], json!({ "kind": "omitted", "count": 2 }));
    }
}
