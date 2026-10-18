//! Search over the session index, and bringing the index up to date.
//!
//! Ported from the search half of trovador's `src/sessions.rs` (bottega d89c451), with
//! timon's options: a `phrase` mode, `since` and `until` as times, and the sessions timon
//! started counted as automated, as `sessions.list` counts them.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use super::archive::{self, ArchiveEntry};
use super::index::{self, Index, IndexError, RefreshOpts, SearchOpts, Visibility};
use super::{Backend, Limits, Paths};
use crate::error::Error;

/// How a query is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Words and "quoted phrases", all required.
    Terms,
    /// The whole query as one phrase.
    Phrase,
    /// An FTS5 expression: AND, OR, NOT, NEAR, prefix*.
    Fts,
    /// A case-insensitive regular expression.
    Regex,
}

impl Mode {
    pub fn parse(name: &str) -> Result<Self, Error> {
        Ok(match name {
            "terms" => Mode::Terms,
            "phrase" => Mode::Phrase,
            "fts" => Mode::Fts,
            "regex" => Mode::Regex,
            other => {
                return Err(
                    Error::new("bad_option", format!("unknown mode \"{other}\""))
                        .with_hint("mode is terms, phrase, fts or regex"),
                );
            }
        })
    }

    fn name(self) -> &'static str {
        match self {
            Mode::Terms => "terms",
            Mode::Phrase => "phrase",
            Mode::Fts => "fts",
            Mode::Regex => "regex",
        }
    }
}

/// What to search for, and which sessions count.
#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub query: String,
    pub mode: Mode,
    pub backend: Option<Backend>,
    pub project: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
    pub per_session: Option<usize>,
    pub recent: bool,
    pub subagents: bool,
    pub active: bool,
    pub automated: bool,
    pub exclude: Option<String>,
}

impl SearchOptions {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            mode: Mode::Terms,
            backend: None,
            project: None,
            since: None,
            until: None,
            limit: None,
            per_session: None,
            recent: false,
            subagents: false,
            active: false,
            automated: false,
            exclude: None,
        }
    }
}

fn query_error(e: IndexError) -> Error {
    Error::from(e)
}

fn ns(t: DateTime<Utc>) -> i64 {
    t.timestamp_nanos_opt().unwrap_or(i64::MAX)
}

/// Brings the index up to date within the search budget, then queries it.
pub fn search(paths: &Paths, limits: &Limits, opts: &SearchOptions) -> Result<Json, Error> {
    if opts.query.trim().is_empty() {
        return Err(Error::new("bad_query", "the query is empty"));
    }
    if opts.mode == Mode::Regex {
        index::search_regex(&opts.query)
            .map_err(|e| Error::new("bad_query", format!("invalid regular expression: {e}")))?;
    }
    // Words that make no tokens can never match; fail before spending the refresh budget.
    match opts.mode {
        Mode::Terms => index::validate_terms(&opts.query).map_err(query_error)?,
        Mode::Phrase => index::validate_phrase(&opts.query).map_err(query_error)?,
        Mode::Fts | Mode::Regex => {}
    }
    let (query, fts) = match opts.mode {
        Mode::Phrase => (format!("\"{}\"", opts.query.replace('"', "\"\"")), true),
        Mode::Fts => (opts.query.clone(), true),
        Mode::Terms | Mode::Regex => (opts.query.clone(), false),
    };
    let mut idx = Index::open(paths, limits).map_err(query_error)?;
    let refresh = idx
        .refresh(
            paths,
            limits,
            &RefreshOpts {
                budget: Some(Duration::from_millis(limits.index_budget_ms as u64)),
            },
        )
        .map_err(query_error)?;
    let limit = opts.limit.unwrap_or(limits.search_default).max(1);
    let per_session = opts
        .per_session
        .unwrap_or(limits.per_session_default)
        .max(1);
    let index_opts = SearchOpts {
        query,
        fts,
        regex: opts.mode == Mode::Regex,
        backend: opts.backend,
        project: opts.project.clone(),
        origin: None,
        include_subagents: opts.subagents,
        include_active: opts.active,
        include_automated: opts.automated,
        exclude_session: opts.exclude.clone(),
        since: None,
        until: None,
        limit: Some(limit),
        per_session: Some(per_session),
        order: if opts.recent {
            index::Order::Recent
        } else {
            index::Order::Relevance
        },
    };
    let vis = Visibility {
        archive_roots: Some(
            paths
                .archive_roots()
                .into_iter()
                .map(|root| root.display().to_string())
                .collect(),
        ),
        since_ns: opts.since.map(ns),
        until_ns: opts.until.map(ns),
        active_since_ns: (!opts.active && limits.active_window_s > 0).then(|| {
            let cutoff =
                std::time::SystemTime::now() - Duration::from_secs(limits.active_window_s as u64);
            index::mtime_ns(cutoff)
        }),
        exclude_session: opts.exclude.clone().filter(|s| !s.is_empty()),
        // Sessions timon started are automated too, as in sessions.list: no exceptions.
        automated_except: (!opts.automated).then(Vec::new),
    };
    let raw = idx
        .search(&index_opts, &vis, limit, per_session, true)
        .map_err(|e| match e {
            IndexError::Sql(err) if fts => {
                Error::new("bad_query", format!("invalid FTS5 query: {err}"))
                    .with_hint("quote words with punctuation, or use mode = \"terms\"")
            }
            other => query_error(other),
        })?;

    let entries: HashMap<(Backend, String), ArchiveEntry> = archive::scan_all(&paths.roots())
        .into_iter()
        .map(|e| ((e.backend, e.session_id.clone()), e))
        .collect();
    let found = raw.hits.len();
    let hits: Vec<Json> = raw
        .hits
        .into_iter()
        .filter_map(|h| {
            let e = entries.get(&(h.backend, h.session_id.clone()))?;
            let meta = idx.read_meta_entry(e, limits).unwrap_or(h.meta);
            let mut hit = super::row(e, &meta);
            hit["match_count"] = json!(h.match_count);
            hit["matches"] = json!(h.matches);
            Some(hit)
        })
        .collect();

    let mut hidden = raw.hidden.clone();
    hidden.unavailable = found - hits.len();
    let text_excluded = idx.text_excluded().map_err(query_error)?;
    let exhaustive =
        refresh.pending == 0 && refresh.failed == 0 && refresh.unavailable_roots.is_empty();
    let excluded_note = (opts.subagents && !text_excluded.is_empty()).then(|| {
        let mut parts: Vec<String> = text_excluded
            .iter()
            .map(|(o, n)| format!("{n} {}", if o.is_empty() { "unknown" } else { o }))
            .collect();
        parts.sort();
        format!("text of {} sessions is not indexed", parts.join(", "))
    });
    let stale_note = (refresh.pending > 0 || refresh.failed > 0).then(|| {
        format!(
            "{} session files were not indexed within the {} ms budget and {} could not be read; run `timon sessions index` to finish",
            refresh.pending, limits.index_budget_ms, refresh.failed
        )
    });
    let unavailable_note = (!refresh.unavailable_roots.is_empty()).then(|| {
        format!(
            "archive roots could not be read: {}; their sessions were not checked, and {} matching sessions in them could not be returned",
            refresh.unavailable_roots.join(", "),
            hidden.unavailable
        )
    });
    let h = &hidden;
    let hidden_note = (h.active + h.automated + h.subagents + h.excluded > 0).then(|| {
        let mut parts = Vec::new();
        if h.active > 0 {
            parts.push(format!(
                "{} written in the last {} s (active = true)",
                h.active, limits.active_window_s
            ));
        }
        if h.automated > 0 {
            parts.push(format!("{} automated (automated = true)", h.automated));
        }
        if h.subagents > 0 {
            parts.push(format!("{} subagents (subagents = true)", h.subagents));
        }
        if h.excluded > 0 {
            parts.push("the excluded session".to_string());
        }
        format!(
            "matching sessions left out by default: {}",
            parts.join(", ")
        )
    });
    let query_note = if opts.mode == Mode::Fts && hits.is_empty() {
        index::fts_absence_note(&opts.query).map_err(query_error)?
    } else {
        None
    };
    let query_is_proof = query_note.is_none();
    // Issue #122: a record is indexed up to its cap, so nothing past the cut was searched.
    let t = &raw.truncated;
    let truncated_note = (hits.is_empty() && t.sessions > 0).then(|| {
        format!(
            "absence is not proof: {} records in {} sessions were truncated before indexing, at {} characters ({} for tool calls and results), and the text past that was not searched",
            t.records, t.sessions, limits.record_chars, limits.tool_chars
        )
    });
    let note: Vec<String> = [
        stale_note,
        unavailable_note,
        excluded_note,
        hidden_note,
        query_note,
        truncated_note,
    ]
    .into_iter()
    .flatten()
    .collect();
    let returned = hits.len();
    let absence_is_proof = hits
        .is_empty()
        .then_some(exhaustive && !hidden.any() && query_is_proof && t.sessions == 0);
    Ok(json!({
        "query": opts.query,
        "mode": opts.mode.name(),
        "expression": raw.expression,
        "sessions_matched": raw.sessions_matched,
        "returned": returned,
        "hits": hits,
        "hidden": hidden,
        "exhaustive": exhaustive,
        "absence_is_proof": absence_is_proof,
        "note": (!note.is_empty()).then(|| note.join("; ")),
        "coverage": {
            "refresh": refresh,
            "text_excluded": text_excluded,
            "truncated": raw.truncated,
        },
    }))
}

/// Brings the index fully up to date with no time budget; with `rebuild`, starts from no
/// index at all.
pub fn reindex(paths: &Paths, limits: &Limits, rebuild: bool) -> Result<Json, Error> {
    if rebuild {
        // Deleting every row leaves the file at its old size, so a rebuild removes it.
        let db = index::db_path(paths);
        for suffix in ["", "-wal", "-shm"] {
            let mut p = db.clone().into_os_string();
            p.push(suffix);
            match std::fs::remove_file(std::path::PathBuf::from(p)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::new("io", format!("cannot remove the index: {e}"))),
            }
        }
    }
    let mut idx = Index::open(paths, limits).map_err(query_error)?;
    let report = idx
        .refresh(paths, limits, &RefreshOpts { budget: None })
        .map_err(query_error)?;
    Ok(json!(report))
}
