//! Every Claude Code and Codex session on the machine, read only.
//!
//! The archive reading is ported from trovador (bottega d89c451) and adapted: it honors the
//! CLIs' own `CLAUDE_CONFIG_DIR` and `CODEX_HOME`, and reads zstd-compressed Codex rollouts.

pub mod archive;
pub(crate) mod codex;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::error::Error;
use archive::{ArchiveEntry, SessionMeta};

/// Which CLI wrote a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Claude,
    Codex,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Claude => "claude",
            Backend::Codex => "codex",
        }
    }

    pub fn parse(s: &str) -> Result<Self, Error> {
        match s {
            "claude" => Ok(Backend::Claude),
            "codex" => Ok(Backend::Codex),
            other => Err(
                Error::new("bad_option", format!("unknown backend \"{other}\""))
                    .with_hint("backend is \"claude\" or \"codex\""),
            ),
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where each CLI keeps its sessions.
#[derive(Clone, Debug)]
pub struct Roots {
    /// `<claude config>/projects`.
    pub claude: PathBuf,
    /// `<codex home>/sessions`.
    pub codex: PathBuf,
    /// `<codex home>/archived_sessions`.
    pub codex_archived: PathBuf,
}

impl Roots {
    /// From the CLIs' own variables, `CLAUDE_CONFIG_DIR` (default `~/.claude`) and
    /// `CODEX_HOME` (default `~/.codex`).
    pub fn from_env(home: Option<&Path>) -> Result<Self, Error> {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let home_dir = |dot: &str| -> Result<PathBuf, Error> {
            home.map(|h| h.join(dot))
                .ok_or_else(|| Error::new("not_found", "no home directory is set"))
        };
        let claude = match var("CLAUDE_CONFIG_DIR") {
            Some(dir) => dir,
            None => home_dir(".claude")?,
        };
        let codex = match var("CODEX_HOME") {
            Some(dir) => dir,
            None => home_dir(".codex")?,
        };
        Ok(Self {
            claude: claude.join("projects"),
            codex: codex.join("sessions"),
            codex_archived: codex.join("archived_sessions"),
        })
    }
}

/// Origins of sessions a program started rather than a person.
const AUTOMATED_ORIGINS: &[&str] = &["sdk-cli", "sdk-ts", "exec"];

/// Filters for [`list`].
#[derive(Debug, Default, Clone)]
pub struct ListOptions {
    pub backend: Option<Backend>,
    /// A substring of the session's directory, case-insensitive.
    pub project: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub limit: usize,
    pub subagents: bool,
    pub automated: bool,
}

/// A session row, the shape Lua and the CLI see.
pub fn row(e: &ArchiveEntry, meta: &SessionMeta) -> Json {
    json!({
        "id": e.session_id,
        "backend": e.backend.as_str(),
        "title": meta.title,
        "snippet": meta.snippet,
        "project": meta.project(),
        "cwd": meta.cwd,
        "modified": e.modified(),
        "size_bytes": e.size_bytes,
        "origin": meta.origin,
        "agent_name": meta.agent_name,
        "pull_requests": meta.pull_requests,
        "subagent": meta.subagent,
        "parent": meta.parent_session_id,
        "archived": e.archived,
        "resumable": archive::resumability(e).resumable,
    })
}

/// A session row with what `show` adds: the file, the resume command and why a session
/// cannot be resumed.
pub fn detail(e: &ArchiveEntry, meta: &SessionMeta) -> Json {
    let mut out = row(e, meta);
    let resumability = archive::resumability(e);
    out["file"] = json!(e.file);
    out["compressed"] = json!(archive::is_compressed(&e.file));
    out["title_source"] = json!(meta.title_source);
    out["originator"] = json!(meta.originator);
    out["artifacts"] = json!(meta.artifacts);
    out["open_with"] = json!(archive::open_with(e.backend, meta.project(), &e.session_id));
    out["resume_note"] = json!(resumability.reason);
    out
}

fn automated(meta: &SessionMeta, id: &str, started_by_timon: &HashSet<String>) -> bool {
    started_by_timon.contains(id)
        || meta
            .origin
            .as_deref()
            .is_some_and(|o| AUTOMATED_ORIGINS.contains(&o))
}

/// Sessions newest first. Subagents and automated sessions (`claude -p`, `codex exec`, and any
/// session timon started) are left out unless asked for, and counted.
pub fn list(roots: &Roots, started_by_timon: &HashSet<String>, opts: &ListOptions) -> Json {
    let project = opts.project.as_ref().map(|p| p.to_ascii_lowercase());
    let (mut hidden_subagents, mut hidden_automated) = (0u64, 0u64);
    let mut rows = Vec::new();
    for e in archive::scan_all(roots) {
        if rows.len() >= opts.limit {
            break;
        }
        if opts.backend.is_some_and(|b| b != e.backend) {
            continue;
        }
        if let Some(since) = opts.since
            && DateTime::<Utc>::from(e.mtime) < since
        {
            break;
        }
        let meta = archive::read_meta(&e);
        if let Some(project) = &project {
            let paths = [meta.cwd.as_deref(), meta.relocated_cwd.as_deref()];
            if !paths
                .iter()
                .flatten()
                .any(|p| p.to_ascii_lowercase().contains(project))
            {
                continue;
            }
        }
        if meta.subagent && !opts.subagents {
            hidden_subagents += 1;
            continue;
        }
        if !opts.automated && automated(&meta, &e.session_id, started_by_timon) {
            hidden_automated += 1;
            continue;
        }
        rows.push(row(&e, &meta));
    }
    json!({
        "sessions": rows,
        "hidden": { "subagents": hidden_subagents, "automated": hidden_automated },
    })
}

/// One session by id or unambiguous prefix.
pub fn show(roots: &Roots, id: &str, backend: Option<Backend>) -> Result<Json, Error> {
    let e = archive::find_session(roots, id, backend)?;
    let meta = archive::read_meta(&e);
    Ok(detail(&e, &meta))
}

/// The session timon is running inside, from the variables the CLIs export to tools:
/// `CLAUDE_CODE_SESSION_ID`, and Codex's `CODEX_THREAD_ID` or `CODEX_SESSION_ID`. Inside a
/// Claude subagent the id is the parent session's.
pub fn current(roots: &Roots) -> Option<Json> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let (id, backend) = match var("CLAUDE_CODE_SESSION_ID") {
        Some(id) => (id, Backend::Claude),
        None => (
            var("CODEX_THREAD_ID").or_else(|| var("CODEX_SESSION_ID"))?,
            Backend::Codex,
        ),
    };
    // CLAUDE_CODE_CHILD_SESSION is set in top-level desktop sessions too, so it cannot mark
    // a subagent; it is not reported.
    Some(match show(roots, &id, Some(backend)) {
        Ok(found) => found,
        Err(_) => json!({ "id": id, "backend": backend.as_str() }),
    })
}

/// Parses `since`: a duration back from now (`90m`, `36h`, `2d`, `1w`), an RFC 3339 time, or
/// a date (`2026-10-15`, midnight UTC).
pub fn parse_since(text: &str) -> Result<DateTime<Utc>, Error> {
    let t = text.trim();
    if let Ok(time) = DateTime::parse_from_rfc3339(t) {
        return Ok(time.with_timezone(&Utc));
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        return Ok(date.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc());
    }
    let back = match t.strip_suffix('w').and_then(|n| n.parse::<f64>().ok()) {
        Some(weeks) => std::time::Duration::from_secs_f64(weeks * 7.0 * 86400.0),
        None => crate::modules::concurrency::parse_duration(t).map_err(|_| {
            Error::new("bad_option", format!("cannot read since \"{text}\"")).with_hint(
                "use a duration such as 2d or 36h, a date such as 2026-10-15, or an RFC 3339 time",
            )
        })?,
    };
    let back =
        chrono::Duration::from_std(back).map_err(|e| Error::new("bad_option", e.to_string()))?;
    Ok(Utc::now() - back)
}
