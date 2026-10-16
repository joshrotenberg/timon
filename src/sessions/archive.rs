//! The provider archives: scanning, addressing, metadata, resumability.
//!
//! Both archives are read only. Nothing here writes under `~/.claude` or `~/.codex`.
//!
//! Ported from trovador's `src/archive.rs` (bottega d89c451). Changes: timon's roots and
//! errors, chrono timestamps, constants in place of trovador's limits, and Codex rollouts
//! compressed with zstd (`.jsonl.zst`), which are read through `open`.

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Backend, Roots};
use crate::error::Error;

/// Head read steps for metadata, smallest first; a larger one is read only when a smaller
/// one left cwd or the first user message missing.
const HEAD_STEPS: [usize; 3] = [64 * 1024, 512 * 1024, 4 * 1024 * 1024];
/// Claude files above this size skip the full custom-title scan.
const TITLE_SCAN_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Bytes above which a session is reported unresumable.
const RESUME_LIMIT_BYTES: u64 = 900_000;

/// A session file's text, decompressed when it is a `.jsonl.zst` rollout.
pub(crate) fn open(path: &Path) -> std::io::Result<Box<dyn Read>> {
    let file = File::open(path)?;
    if is_compressed(path) {
        Ok(Box::new(zstd::stream::read::Decoder::new(file)?))
    } else {
        Ok(Box::new(file))
    }
}

/// [`open`], buffered.
pub(crate) fn open_buffered(path: &Path) -> std::io::Result<BufReader<Box<dyn Read>>> {
    Ok(BufReader::new(open(path)?))
}

/// The text from byte `offset` on: a seek for a plain file, a skip for a compressed one.
pub(crate) fn open_at(path: &Path, offset: u64) -> std::io::Result<BufReader<Box<dyn Read>>> {
    if is_compressed(path) {
        let mut text = open(path)?;
        std::io::copy(&mut (&mut text).take(offset), &mut std::io::sink())?;
        return Ok(BufReader::new(text));
    }
    use std::io::{Seek, SeekFrom};
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    Ok(BufReader::new(Box::new(file)))
}

pub(crate) fn is_compressed(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "zst")
}

/// One session file, as found on disk, before anything inside it is read.
#[derive(Clone, Debug)]
pub struct ArchiveEntry {
    pub backend: Backend,
    pub session_id: String,
    pub file: PathBuf,
    pub size_bytes: u64,
    pub mtime: SystemTime,
    /// Codex moved this thread to `archived_sessions`.
    pub archived: bool,
}

impl ArchiveEntry {
    pub fn modified(&self) -> String {
        chrono::DateTime::<chrono::Utc>::from(self.mtime)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }
}

// --- scanning --------------------------------------------------------------

/// Claude keeps `<root>/<project>/<uuid>.jsonl`. Subagent transcripts live one level
/// deeper under `<session>/subagents/` and are deliberately not listed as sessions: they
/// cannot be resumed on their own.
pub fn scan_claude(root: &Path) -> Vec<ArchiveEntry> {
    scan_root(root, Backend::Claude, false, &mut Vec::new())
}

/// Codex nests active sessions by date and keeps archived sessions in a flat directory.
pub fn scan_codex(root: &Path, archived: bool) -> Vec<ArchiveEntry> {
    scan_root(root, Backend::Codex, archived, &mut Vec::new())
}

pub(crate) struct ScanFailure {
    // Read by the session index when it reports unavailable roots.
    #[allow(dead_code)]
    pub backend: Backend,
    pub path: PathBuf,
    pub kind: std::io::ErrorKind,
}

fn scan_root(
    root: &Path,
    backend: Backend,
    archived: bool,
    failures: &mut Vec<ScanFailure>,
) -> Vec<ArchiveEntry> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let items = match fs::read_dir(&dir) {
            Ok(items) => items,
            Err(error) => {
                failures.push(ScanFailure {
                    backend,
                    path: dir,
                    kind: error.kind(),
                });
                continue;
            }
        };
        for item in items {
            let item = match item {
                Ok(item) => item,
                Err(error) => {
                    failures.push(ScanFailure {
                        backend,
                        path: dir.clone(),
                        kind: error.kind(),
                    });
                    continue;
                }
            };
            let path = item.path();
            let ft = match item.file_type() {
                Ok(ft) => ft,
                Err(error) => {
                    failures.push(ScanFailure {
                        backend,
                        path,
                        kind: error.kind(),
                    });
                    continue;
                }
            };
            // Issue #8: a dangling project link is not an archive directory and must
            // not prevent cleanup of sessions that were actually deleted.
            if backend == Backend::Claude && depth == 0 && ft.is_symlink() && !path.is_dir() {
                continue;
            }
            if (backend == Backend::Claude && depth == 0 && (ft.is_dir() || ft.is_symlink()))
                || (backend == Backend::Codex && ft.is_dir())
            {
                stack.push((path, depth + 1));
                continue;
            }
            if backend == Backend::Claude && depth != 1 {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let id = match backend {
                Backend::Claude => name.strip_suffix(".jsonl"),
                Backend::Codex => trailing_uuid(name),
            };
            if let Some(id) = id {
                match entry(backend, id, &path, archived, failures) {
                    Ok(Some(e)) => out.push(e),
                    Ok(None) => {}
                    Err(error) => failures.push(ScanFailure {
                        backend,
                        path,
                        kind: error.kind(),
                    }),
                }
            }
        }
    }
    out
}

fn trailing_uuid(name: &str) -> Option<&str> {
    let stem = name
        .strip_suffix(".jsonl.zst")
        .or_else(|| name.strip_suffix(".jsonl"))?;
    let id = stem.get(stem.len().checked_sub(36)?..)?;
    let shape_ok = id.char_indices().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => c == '-',
        _ => c.is_ascii_hexdigit(),
    });
    shape_ok.then_some(id)
}

fn entry(
    backend: Backend,
    id: &str,
    path: &Path,
    archived: bool,
    failures: &mut Vec<ScanFailure>,
) -> std::io::Result<Option<ArchiveEntry>> {
    let md = fs::metadata(path)?;
    if !md.is_file() {
        return Ok(None);
    }
    let mtime = md.modified().unwrap_or_else(|error| {
        failures.push(ScanFailure {
            backend,
            path: path.to_path_buf(),
            kind: error.kind(),
        });
        SystemTime::UNIX_EPOCH
    });
    Ok(Some(ArchiveEntry {
        backend,
        session_id: id.to_string(),
        file: path.to_path_buf(),
        size_bytes: md.len(),
        mtime,
        archived,
    }))
}

/// Every session in both archives, newest first, one file per session.
///
/// A Claude session moved between projects can leave its old file behind. The newest file
/// is the live one. Choosing it here, once, means listing, lookup, search, and the index
/// all describe the same file; when only the index chose, a list showed the session twice,
/// every prefix was ambiguous, and search returned a row mixing the two files (#28).
pub fn scan_all(roots: &Roots) -> Vec<ArchiveEntry> {
    scan_all_with_failures(roots).0
}

pub(crate) fn scan_all_with_failures(roots: &Roots) -> (Vec<ArchiveEntry>, Vec<ScanFailure>) {
    let mut failures = Vec::new();
    let mut all = scan_root(&roots.claude, Backend::Claude, false, &mut failures);
    all.extend(scan_root(
        &roots.codex,
        Backend::Codex,
        false,
        &mut failures,
    ));
    all.extend(scan_root(
        &roots.codex_archived,
        Backend::Codex,
        true,
        &mut failures,
    ));
    // archived_sessions is optional, but only an accessible active root tells a
    // never-created archive from unavailable Codex storage.
    let active_unavailable = failures.iter().any(|f| f.path == roots.codex);
    failures.retain(|f| {
        active_unavailable
            || f.path != roots.codex_archived
            || f.kind != std::io::ErrorKind::NotFound
    });
    all.sort_by(|a, b| b.mtime.cmp(&a.mtime).then(a.session_id.cmp(&b.session_id)));
    let mut seen = std::collections::HashSet::new();
    all.retain(|e| seen.insert((e.backend, e.session_id.clone())));
    (all, failures)
}

// --- lookup ----------------------------------------------------------------

/// Exact id wins. Short of that an unambiguous prefix resolves, because full ids are
/// unwieldy to type or say aloud and callers routinely shorten them.
pub fn find_session(
    roots: &Roots,
    id: &str,
    backend: Option<Backend>,
) -> Result<ArchiveEntry, Error> {
    let candidates: Vec<ArchiveEntry> = scan_all(roots)
        .into_iter()
        .filter(|e| backend.is_none_or(|b| e.backend == b))
        .collect();
    if let Some(exact) = candidates.iter().find(|e| e.session_id == id) {
        return Ok(exact.clone());
    }
    let mut prefixed: Vec<ArchiveEntry> = if id.is_empty() {
        Vec::new()
    } else {
        candidates
            .into_iter()
            .filter(|e| e.session_id.starts_with(id))
            .collect()
    };
    match prefixed.len() {
        0 => Err(Error::new(
            "not_found",
            format!(
                "no {} session matches \"{id}\"",
                backend.map(|b| b.as_str()).unwrap_or("archived")
            ),
        )
        .with_hint("sessions.list or sessions.search finds sessions")),
        1 => Ok(prefixed.remove(0)),
        _ => {
            let candidates: Vec<String> = prefixed
                .iter()
                .map(|e| format!("{}/{}", e.backend, e.session_id))
                .collect();
            Err(Error::new(
                "ambiguous",
                format!(
                    "\"{id}\" matches {} sessions ({})",
                    candidates.len(),
                    candidates.join(", ")
                ),
            )
            .with_hint("use a longer prefix"))
        }
    }
}

// --- metadata --------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// The directory the session started in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Where the session lives now. Claude records `relocated` when a session moves to a
    /// different project directory, and a resume has to run from there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relocated_cwd: Option<String>,
    /// The first real user message, collapsed and cut to 120 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// What a person would call it: what they typed, else what the provider generated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_source: Option<TitleSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    /// Artifacts the session published, as the provider recorded them.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub artifacts: Vec<String>,
    /// Pull requests the session opened, as the provider recorded them.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub pull_requests: Vec<String>,
    /// Codex: which client wrote the session (`codex_exec`, `Codex Desktop`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub originator: Option<String>,
    /// How the session was started, in the provider's own terms. Claude: `entrypoint`
    /// (`cli`, `claude-desktop`, `sdk-cli` for `claude -p`). Codex: `source` (`cli`,
    /// `vscode`, `exec`), or for a subagent its kind (`guardian`, `thread_spawn`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Started by another session rather than by a person. On 2026-10-07, 1,694 of 3,705
    /// local Codex sessions were `guardian` approval reviewers, which is why lists hide
    /// subagents unless asked.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub subagent: bool,
    /// The session that started this one, when the provider records it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleSource {
    Custom,
    Generated,
}

impl SessionMeta {
    /// The directory a resume should run from.
    pub fn project(&self) -> Option<&str> {
        self.relocated_cwd.as_deref().or(self.cwd.as_deref())
    }
}

/// Reads cwd and snippets from head windows, and rewritten metadata from the tail.
/// Issue #36: an unindexed file gets a title scan only within the configured byte budget.
pub fn read_meta(e: &ArchiveEntry) -> SessionMeta {
    read_meta_inner(e, true)
}

#[allow(dead_code)]
pub(crate) fn read_meta_windows(e: &ArchiveEntry) -> SessionMeta {
    read_meta_inner(e, false)
}

fn read_meta_inner(e: &ArchiveEntry, scan_titles: bool) -> SessionMeta {
    let mut meta = SessionMeta::default();
    let mut covered = 0u64;
    // A compressed file's size says nothing about its text, so its steps always run.
    let compressed = is_compressed(&e.file);
    for &step in &HEAD_STEPS {
        if !compressed && covered >= e.size_bytes {
            break;
        }
        let Ok(head) = read_range(&e.file, 0, step) else {
            break;
        };
        let complete = head.len() < step;
        meta = match e.backend {
            Backend::Claude => parse_claude_head(&head),
            Backend::Codex => parse_codex_head(&head, e.archived, complete),
        };
        covered = step as u64;
        if complete || (meta.cwd.is_some() && meta.snippet.is_some()) {
            break;
        }
    }
    if e.backend == Backend::Claude && covered < e.size_bytes {
        let tail_from = e.size_bytes.saturating_sub(TAIL_BYTES as u64).max(covered);
        if let Ok(tail) = read_range(&e.file, tail_from, TAIL_BYTES) {
            apply_claude_latest(&mut meta, lines_after_first(&tail));
        }
        if scan_titles && e.size_bytes <= TITLE_SCAN_MAX_BYTES {
            let mut custom_title = None;
            if scan_custom_titles(&e.file, 0, e.size_bytes, &mut custom_title).is_ok() {
                apply_custom_title(&mut meta, custom_title);
            }
        }
    }
    meta
}

const TAIL_BYTES: usize = 256 * 1024;

pub(crate) fn apply_custom_title(meta: &mut SessionMeta, title: Option<String>) {
    if let Some(title) = title {
        meta.title = Some(title);
        meta.title_source = Some(TitleSource::Custom);
    }
}

pub(crate) fn custom_title(record: &Value) -> Option<&str> {
    (str_at(record, "type") == Some("custom-title"))
        .then(|| str_at(record, "customTitle"))
        .flatten()
}

pub(crate) fn scan_custom_titles(
    path: &Path,
    from: u64,
    until: u64,
    title: &mut Option<String>,
) -> std::io::Result<()> {
    // Only candidate records need JSON parsing. Large byte chunks avoid validating every
    // transcript line as UTF-8.
    let mut text = open(path)?;
    std::io::copy(&mut (&mut text).take(from), &mut std::io::sink())?;
    let mut reader = text.take(until.saturating_sub(from));
    let mut chunk = [0u8; 64 * 1024];
    let mut pending = Vec::new();
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        pending.extend_from_slice(&chunk[..n]);
        if let Some(end) = memchr::memrchr(b'\n', &chunk[..n]) {
            let end = pending.len() - n + end;
            let complete = &pending[..=end];
            let mut parsed_until = 0;
            for at in memchr::memmem::find_iter(complete, b"custom-title") {
                if at < parsed_until {
                    continue;
                }
                let start = memchr::memrchr(b'\n', &complete[..at]).map_or(0, |i| i + 1);
                let stop = at + memchr::memchr(b'\n', &complete[at..]).unwrap();
                parsed_until = stop + 1;
                if let Ok(record) = serde_json::from_slice::<Value>(&complete[start..stop])
                    && let Some(t) = custom_title(&record)
                {
                    *title = Some(t.to_owned());
                }
            }
            pending.drain(..=end);
        }
    }
    // Issue #36: metadata windows accept complete final JSON without a newline too.
    if memchr::memmem::find(&pending, b"custom-title").is_some()
        && let Ok(record) = serde_json::from_slice::<Value>(&pending)
        && let Some(t) = custom_title(&record)
    {
        *title = Some(t.to_owned());
    }
    Ok(())
}

/// `len` bytes of a session's text from `from`, decompressed when needed.
fn read_range(path: &Path, from: u64, len: usize) -> std::io::Result<String> {
    let mut text = open(path)?;
    std::io::copy(&mut (&mut text).take(from), &mut std::io::sink())?;
    let mut buf = Vec::with_capacity(len.min(1 << 20));
    text.take(len as u64).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// A tail read starts mid-line; the first fragment is never a whole record.
fn lines_after_first(s: &str) -> &str {
    s.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
}

fn records(text: &str) -> impl Iterator<Item = Value> + '_ {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Text from either a bare string or an array of content blocks.
pub(crate) fn block_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// A snippet a person would recognise: whitespace collapsed, at most 120 characters, and
/// never injected context, which providers wrap in angle-bracket tags.
fn snippet_of(text: &str) -> Option<String> {
    let clean = text.split_whitespace().collect::<Vec<_>>().join(" ");
    // A Claude Desktop scheduled task wraps its whole prompt in a tag; the task's name is
    // what a person would recognise it by.
    if let Some(rest) = clean.strip_prefix("<scheduled-task name=\"") {
        return rest
            .split_once('"')
            .map(|(name, _)| format!("scheduled task: {name}"));
    }
    if clean.is_empty() || clean.starts_with('<') {
        return None;
    }
    Some(clean.chars().take(120).collect())
}

fn parse_claude_head(text: &str) -> SessionMeta {
    let mut meta = SessionMeta::default();
    for r in records(text) {
        if meta.cwd.is_none() {
            meta.cwd = str_at(&r, "cwd").map(str::to_string);
        }
        if meta.origin.is_none() {
            meta.origin = str_at(&r, "entrypoint").map(str::to_string);
        }
        // Issue #38: generated context cannot supply the first prompt the user typed.
        if meta.snippet.is_none()
            && str_at(&r, "type") == Some("user")
            && r.get("isMeta").and_then(Value::as_bool) != Some(true)
            && r.get("isCompactSummary").and_then(Value::as_bool) != Some(true)
        {
            meta.snippet = typed_snippet(r.pointer("/message/content").unwrap_or(&Value::Null));
        }
        apply_claude_latest_record(&mut meta, &r);
    }
    meta
}

fn apply_claude_latest(meta: &mut SessionMeta, text: &str) {
    for r in records(text) {
        apply_claude_latest_record(meta, &r);
    }
}

/// Records Claude rewrites as the session goes. Later is current, so last one wins.
fn apply_claude_latest_record(meta: &mut SessionMeta, r: &Value) {
    match str_at(r, "type") {
        Some("custom-title") => {
            if let Some(t) = str_at(r, "customTitle") {
                meta.title = Some(t.to_string());
                meta.title_source = Some(TitleSource::Custom);
            }
        }
        Some("ai-title") => {
            if let Some(t) = str_at(r, "aiTitle") {
                // A title the user typed outranks one the provider generated.
                if meta.title_source != Some(TitleSource::Custom) {
                    meta.title = Some(t.to_string());
                    meta.title_source = Some(TitleSource::Generated);
                }
            }
        }
        Some("agent-name") => {
            if let Some(n) = str_at(r, "agentName") {
                meta.agent_name = Some(n.to_string());
            }
        }
        Some("relocated") => {
            if let Some(c) = str_at(r, "relocatedCwd") {
                meta.relocated_cwd = Some(c.to_string());
            }
        }
        Some("frame-link") => push_unique(&mut meta.artifacts, str_at(r, "frameUrl")),
        Some("pr-link") => push_unique(&mut meta.pull_requests, str_at(r, "prUrl")),
        _ => {}
    }
}

fn push_unique(list: &mut Vec<String>, item: Option<&str>) {
    if let Some(s) = item
        && !list.iter().any(|x| x == s)
    {
        list.push(s.to_string());
    }
}

/// Codex has written the first user message three ways. Current versions emit an
/// `item_completed` event carrying a `UserMessage` item; older versions emit
/// `event_msg/user_message`; both also write the model-facing `response_item/message`,
/// which includes injected context. The first two are what the user typed, so they win.
/// The directory a Codex session file started in, from its first line alone: a cheap filter
/// before [`read_meta`]. None when the first line is unreadable or not the session's metadata.
pub(crate) fn codex_cwd(file: &Path) -> Option<String> {
    use std::io::BufRead;
    let mut line = Vec::new();
    open_buffered(file)
        .ok()?
        .take(4 * 1024 * 1024)
        .read_until(b'\n', &mut line)
        .ok()?;
    let first: Value = serde_json::from_slice(&line).ok()?;
    if str_at(&first, "type") != Some("session_meta") {
        return None;
    }
    str_at(first.get("payload")?, "cwd").map(str::to_owned)
}

fn parse_codex_head(text: &str, archived: bool, complete: bool) -> SessionMeta {
    let mut meta = SessionMeta::default();
    let (mut from_item, mut from_event, mut from_response) = (None, None, None);
    let mut seen_meta = false;
    let boundary = super::codex::fork_boundary(&mut std::io::Cursor::new(text), archived, complete)
        .expect("reading an in-memory head");
    let inherited_end = boundary.end.unwrap_or(boundary.scanned);
    let mut fork_snippet = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len() as u64;
        if line_start > 0 && boundary.end.is_none() && line_start >= boundary.scanned {
            break;
        }
        if line_start > 0 && line_start < inherited_end {
            continue;
        }
        let Ok(r) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let p = r.get("payload").unwrap_or(&Value::Null);
        match (str_at(&r, "type"), str_at(p, "type")) {
            // Only the first session_meta describes this file. In 510 of 3,746 local files
            // further ones followed, always for other sessions (a spawned child, a fork's
            // parent); reading them let a later record overwrite the origin, so the origin
            // depended on how far the head read reached, and a parent that embedded a
            // child's record was marked as a subagent.
            (Some("session_meta"), _) if !seen_meta => {
                seen_meta = true;
                meta.cwd = str_at(p, "cwd").map(str::to_string);
                meta.originator = str_at(p, "originator").map(str::to_string);
                apply_codex_source(&mut meta, p);
            }
            // Issue #35: a fork's initial assignment is an inter-agent message,
            // not a user turn. It still describes the child's work in session lists.
            (Some("response_item"), Some("agent_message"))
                if inherited_end > 0 && fork_snippet.is_none() =>
            {
                fork_snippet = typed_snippet(p.get("content").unwrap_or(&Value::Null));
            }
            (_, Some("item_completed")) if from_item.is_none() => {
                let item = p.get("item").unwrap_or(&Value::Null);
                if str_at(item, "type") == Some("UserMessage") {
                    from_item = typed_snippet(item.get("content").unwrap_or(&Value::Null));
                }
            }
            (_, Some("user_message")) if from_event.is_none() => {
                from_event = str_at(p, "message").and_then(snippet_of);
            }
            (Some("response_item"), Some("message"))
                if from_response.is_none() && str_at(p, "role") == Some("user") =>
            {
                from_response = typed_snippet(p.get("content").unwrap_or(&Value::Null));
            }
            // The oldest files have no envelope: the record is its own payload.
            (Some("message"), None)
                if from_response.is_none() && str_at(&r, "role") == Some("user") =>
            {
                from_response = typed_snippet(r.get("content").unwrap_or(&Value::Null));
            }
            _ => {}
        }
    }
    meta.snippet = fork_snippet.or(from_item).or(from_event).or(from_response);
    meta
}

/// `source` is a string for a top-level session and an object for a subagent:
/// `{"subagent": {"other": "guardian"}}` for an approval reviewer, or
/// `{"subagent": {"thread_spawn": {"agent_nickname": ..., "agent_path": ...}}}` for a
/// spawned agent.
fn apply_codex_source(meta: &mut SessionMeta, p: &Value) {
    match p.get("source") {
        Some(Value::String(s)) => meta.origin = Some(s.clone()),
        Some(Value::Object(o)) => {
            if let Some(sub) = o.get("subagent") {
                meta.subagent = true;
                meta.origin = Some(match sub {
                    Value::Object(kind) => match kind.iter().next() {
                        Some((k, Value::String(v))) if k == "other" => v.clone(),
                        Some((k, _)) => k.clone(),
                        None => "subagent".to_string(),
                    },
                    Value::String(v) => v.clone(),
                    _ => "subagent".to_string(),
                });
                if let Some(nick) = sub
                    .pointer("/thread_spawn/agent_nickname")
                    .and_then(Value::as_str)
                {
                    meta.agent_name = Some(nick.to_string());
                }
            }
        }
        _ => {}
    }
    if let Some(parent) = str_at(p, "parent_thread_id") {
        meta.parent_session_id = Some(parent.to_string());
        meta.subagent = true;
    }
}

/// The first content block a person typed. Both providers put injected context in its own
/// block ahead of what the user wrote: Claude a `<system-reminder>`, Codex Desktop the
/// project's AGENTS.md. Joining the blocks first hid the typed text in 50 top-level local
/// sessions on 2026-10-07. Tool results are not typed text, so only text blocks count.
fn typed_snippet(content: &Value) -> Option<String> {
    match content {
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) != Some("tool_result"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .filter(|t| !t.trim_start().starts_with("# AGENTS.md instructions"))
            .find_map(snippet_of),
        other => snippet_of(&block_text(other)),
    }
}

// --- resuming --------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resumability {
    /// Small enough for the provider to accept a resumed turn.
    pub resumable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub fn resumability(e: &ArchiveEntry) -> Resumability {
    // A compressed rollout is a finished, archived-by-Codex session; its size on disk is not
    // what a resume would load.
    let resumable = is_compressed(&e.file) || e.size_bytes < RESUME_LIMIT_BYTES;
    Resumability {
        resumable,
        reason: (!resumable).then(|| {
            format!(
                "transcript is {}KB, over the ~{}KB practical resume ceiling; to carry the work on, summarize it (sessions.summarize) and start a new session with the summary",
                e.size_bytes / 1024,
                RESUME_LIMIT_BYTES / 1024
            )
        }),
    }
}

/// The command a person would paste to pick the session up in a terminal.
pub fn open_with(backend: Backend, project: Option<&str>, id: &str) -> String {
    let cmd = match backend {
        Backend::Claude => format!("claude --resume {id}"),
        Backend::Codex => format!("codex resume {id}"),
    };
    match project {
        Some(p) => format!("cd {} && {cmd}", shell_quote(p)),
        None => cmd,
    }
}

/// Single-quoted, so a path containing `$`, backticks, or spaces pastes safely.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_codex_id_is_the_trailing_uuid_of_the_rollout_name() {
        assert_eq!(
            trailing_uuid("rollout-2026-10-07T12-16-06-01a117cb-5e0a-7c31-a391-fa9b66aa0b1c.jsonl"),
            Some("01a117cb-5e0a-7c31-a391-fa9b66aa0b1c")
        );
        assert_eq!(trailing_uuid("notes.jsonl"), None);
        assert_eq!(
            trailing_uuid("rollout-zzzzzzzz-5e0a-7c31-a391-fa9b66aa0b1c.jsonl"),
            None
        );
    }

    #[test]
    fn open_with_quotes_a_project_path_that_a_shell_would_split() {
        assert_eq!(
            open_with(
                Backend::Claude,
                Some("/Users/me/Application Support/it's"),
                "abc"
            ),
            r#"cd '/Users/me/Application Support/it'\''s' && claude --resume abc"#
        );
    }

    #[test]
    fn injected_context_is_never_a_snippet() {
        assert_eq!(
            snippet_of("<environment_context>cwd</environment_context>"),
            None
        );
        assert_eq!(
            snippet_of("  fix\n\nthe   build "),
            Some("fix the build".to_string())
        );
        assert_eq!(
            snippet_of(r#"<scheduled-task name="daily-scan" file="/x">do it</scheduled-task>"#),
            Some("scheduled task: daily-scan".to_string())
        );
    }

    #[test]
    fn a_typed_title_outranks_a_generated_one_whichever_comes_last() {
        let mut m = SessionMeta::default();
        apply_claude_latest(
            &mut m,
            r#"{"type":"custom-title","customTitle":"mine"}
{"type":"ai-title","aiTitle":"generated"}"#,
        );
        assert_eq!(m.title.as_deref(), Some("mine"));
        assert_eq!(m.title_source, Some(TitleSource::Custom));
    }
}

#[cfg(test)]
mod compressed {
    use super::*;

    #[test]
    fn a_compressed_rollout_name_has_the_same_id() {
        assert_eq!(
            trailing_uuid(
                "rollout-2026-10-07T12-16-06-01a117cb-5e0a-7c31-a391-fa9b66aa0b1c.jsonl.zst"
            ),
            Some("01a117cb-5e0a-7c31-a391-fa9b66aa0b1c")
        );
    }
}
