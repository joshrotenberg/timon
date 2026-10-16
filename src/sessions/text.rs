//! Plain-text forms of the `sessions` results, for `timon sessions` without `--json`. Each
//! returns what goes to standard output and an optional note for standard error.

use serde_json::Value as Json;

fn s(v: &Json) -> &str {
    v.as_str().unwrap_or("")
}

fn or_dash(v: &Json) -> &str {
    v.as_str().filter(|t| !t.is_empty()).unwrap_or("-")
}

fn short(id: &Json) -> &str {
    let id = s(id);
    id.get(..8).unwrap_or(id)
}

fn minute(t: &Json) -> &str {
    let t = s(t);
    t.get(..16).unwrap_or(t)
}

fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}...", flat.chars().take(max).collect::<String>())
    }
}

/// One session as a line: short id, backend, modified, project, title.
pub fn row(r: &Json) -> String {
    let title = r["title"]
        .as_str()
        .or(r["snippet"].as_str())
        .map(|t| one_line(t, 80))
        .unwrap_or_default();
    format!(
        "{}  {:6}  {:16}  {}  {}",
        short(&r["id"]),
        s(&r["backend"]),
        minute(&r["modified"]),
        or_dash(&r["project"]),
        title
    )
}

fn hidden_note(hidden: &Json, flags: &[(&str, &str)]) -> Option<String> {
    let parts: Vec<String> = flags
        .iter()
        .filter_map(|(key, flag)| {
            let n = hidden[key].as_u64().unwrap_or(0);
            (n > 0).then(|| format!("{n} {key} ({flag})"))
        })
        .collect();
    (!parts.is_empty()).then(|| format!("left out: {}", parts.join(", ")))
}

/// `sessions.list`.
pub fn list(r: &Json) -> (String, Option<String>) {
    let rows: Vec<String> = r["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(row)
        .collect();
    let note = hidden_note(
        &r["hidden"],
        &[("automated", "--automated"), ("subagents", "--subagents")],
    );
    (rows.join("\n"), note)
}

/// `sessions.show` and `sessions.current`.
pub fn show(r: &Json) -> String {
    let mut lines = vec![
        format!("id        {}", s(&r["id"])),
        format!("backend   {}", s(&r["backend"])),
        format!("title     {}", or_dash(&r["title"])),
        format!("project   {}", or_dash(&r["project"])),
        format!("cwd       {}", or_dash(&r["cwd"])),
        format!("modified  {}", s(&r["modified"])),
        format!("file      {}", or_dash(&r["file"])),
    ];
    if let Some(prs) = r["pull_requests"].as_array().filter(|p| !p.is_empty()) {
        let prs: Vec<&str> = prs.iter().filter_map(Json::as_str).collect();
        lines.push(format!("prs       {}", prs.join(" ")));
    }
    if let Some(parent) = r["parent"].as_str() {
        lines.push(format!("parent    {parent}"));
    }
    if let Some(open) = r["open_with"].as_str() {
        lines.push(format!("resume    {open}"));
    } else if let Some(note) = r["resume_note"].as_str() {
        lines.push(format!("resume    {note}"));
    }
    lines.join("\n")
}

fn label(kind: &str) -> String {
    kind.replace('_', " ").to_uppercase()
}

/// One transcript record, as `[index] KIND: text`.
pub fn record(r: &Json) -> String {
    let kind = s(&r["kind"]);
    let text = s(&r["text"]);
    let name = r["name"].as_str();
    match kind {
        "omitted" => {
            let count = r["count"].as_u64().filter(|n| *n > 1);
            format!(
                "[{}] [omitted: {}{}]",
                r["index"],
                name.unwrap_or("?"),
                count.map(|n| format!(" x{n}")).unwrap_or_default()
            )
        }
        "tool_call" => format!(
            "[{}] TOOL CALL: {}({text})",
            r["index"],
            name.unwrap_or("tool")
        ),
        _ => match name {
            Some(n) => format!("[{}] {} ({n}): {text}", r["index"], label(kind)),
            None => format!("[{}] {}: {text}", r["index"], label(kind)),
        },
    }
}

/// `sessions.transcript`.
pub fn transcript(r: &Json) -> (String, Option<String>) {
    let lines: Vec<String> = r["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(record)
        .collect();
    let note = (r["has_more"] == true).then(|| {
        format!(
            "{} of {} records; more with --offset {}",
            lines.len(),
            r["total"],
            r["next_offset"]
        )
    });
    (lines.join("\n"), note)
}

/// `sessions.search`.
pub fn search(r: &Json) -> (String, Option<String>) {
    let mut out = Vec::new();
    for hit in r["hits"].as_array().into_iter().flatten() {
        out.push(row(hit));
        for m in hit["matches"].as_array().into_iter().flatten() {
            out.push(format!(
                "  [{}] {}: {}",
                m["index"],
                s(&m["kind"]),
                one_line(s(&m["excerpt"]), 160)
            ));
        }
    }
    let mut notes = Vec::new();
    if r["hits"].as_array().is_some_and(Vec::is_empty) {
        notes.push(if r["absence_is_proof"] == true {
            "no matches".to_owned()
        } else {
            "no matches, which proves nothing".to_owned()
        });
    }
    if let Some(note) = r["note"].as_str() {
        notes.push(note.to_owned());
    }
    (
        out.join("\n"),
        (!notes.is_empty()).then(|| notes.join("\n")),
    )
}

/// `sessions.summarize`.
pub fn summary(r: &Json) -> (String, Option<String>) {
    let cost = if r["cached"] == true {
        "cached".to_owned()
    } else {
        format!("${:.4}", r["usd"].as_f64().unwrap_or(0.0))
    };
    let mut note = format!("{}, {cost}", s(&r["model"]));
    if r["chunks_summarized"] != r["chunks"] {
        note.push_str(&format!(
            ", {} of {} parts summarized",
            r["chunks_summarized"], r["chunks"]
        ));
    }
    (s(&r["text"]).to_owned(), Some(note))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_and_records_read_on_one_line() {
        let r = json!({ "id": "aaaaaaaa-1111", "backend": "claude", "modified": "2026-10-07T12:00:00Z",
            "project": null, "title": null, "snippet": "fix\n the build" });
        assert_eq!(
            row(&r),
            "aaaaaaaa  claude  2026-10-07T12:00  -  fix the build"
        );
        assert_eq!(
            record(&json!({ "index": 3, "kind": "tool_result", "text": "ok" })),
            "[3] TOOL RESULT: ok"
        );
        assert_eq!(
            record(&json!({ "index": 4, "kind": "omitted", "name": "progress", "count": 2 })),
            "[4] [omitted: progress x2]"
        );
    }
}
