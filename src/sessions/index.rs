//! The SQLite index at `~/.bottega/trovador/index.db`: session metadata plus an FTS5 table of
//! projected record text, kept current incrementally.
//!
//! Records are keyed by rowid `session_row << 32 | record_index`, so one session's records
//! are a contiguous rowid range. FTS5 can delete and filter a rowid range directly, where a
//! filter on an UNINDEXED column would scan the whole table.
//!
//! Ported from trovador's `src/index.rs` (bottega d89c451). Changes: timon's paths, limits and
//! errors; the database lives in timon's cache directory.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::archive::{self, ArchiveEntry, SessionMeta};
use super::transcript::{
    CodexMode, PROJECTION_VERSION, Projection, ProjectionState, RecordKind, TRUNCATED,
};
use super::{Backend, Limits, Paths};
use crate::dirs::{Root, write_hint};

// Issue #122: version 4 counts the records each session has only in part.
const SCHEMA_VERSION: i64 = 4;
// Issue #36: title state needs one backfill, but projected text and record indexes agree.
const TITLE_METADATA_VERSION: i64 = 1;
const TOKENIZER: &str = "unicode61 remove_diacritics 2";

/// Origins whose text is not indexed. A Codex `guardian` session is an approval reviewer
/// whose input is a copy of its parent session's history: on 2026-10-07 that was 67 MB of
/// the 227 MB of projected text, and indexing it puts a duplicate hit on a reviewer session
/// for most queries that match a real one.
const TEXT_EXCLUDED_ORIGINS: &[&str] = &["guardian"];

#[derive(Debug)]
pub enum IndexError {
    Query(String),
    Sql(rusqlite::Error),
    Dir {
        path: String,
        source: std::io::Error,
    },
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Query(m) => write!(f, "invalid query: {m}"),
            IndexError::Sql(e) => write!(f, "index database: {e}"),
            IndexError::Dir { path, source } => write!(f, "index directory {path}: {source}"),
        }
    }
}

impl std::error::Error for IndexError {}

impl From<rusqlite::Error> for IndexError {
    fn from(e: rusqlite::Error) -> Self {
        IndexError::Sql(e)
    }
}

impl From<IndexError> for crate::error::Error {
    fn from(e: IndexError) -> Self {
        match &e {
            IndexError::Query(m) => crate::error::Error::new("bad_query", m.clone()),
            IndexError::Sql(_) => crate::error::Error::new("index", e.to_string())
                .with_hint("timon sessions index --rebuild starts the index over"),
            IndexError::Dir { path, source } => crate::error::Error::new("io", e.to_string())
                .with_hint_opt(write_hint(Root::Cache, std::path::Path::new(path), source)),
        }
    }
}

pub type Result<T> = std::result::Result<T, IndexError>;

pub struct Index {
    conn: Connection,
    path: PathBuf,
}

pub fn db_path(paths: &Paths) -> PathBuf {
    paths.home.join("index.db")
}

/// What a file's rows were built with: schema, projection, and every setting that shapes
/// a row or its stored metadata. A file built any other way is replaced rather than reused;
/// rows built with one record cap were otherwise served after it changed (#26).
fn built_with(limits: &Limits) -> (i64, i64, i64) {
    let settings = format!(
        "record_chars={} tool_chars={} head_steps={:?} reasoning={}",
        limits.record_chars, limits.tool_chars, limits.head_steps, limits.index_reasoning
    );
    (
        SCHEMA_VERSION,
        PROJECTION_VERSION,
        bytes_hash(settings.as_bytes()) as i64,
    )
}

/// FNV-1a over bytes: stable across builds, which matters because it is stored.
fn bytes_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

const REGION: u64 = 4096;

/// Hashes of the first 4 KB of a file and of the 4 KB before `offset`. An append leaves
/// both alone; a rewrite of the indexed part, even one that keeps or grows the size,
/// changes at least one of them (#25).
fn region_hashes(path: &std::path::Path, offset: u64) -> Option<(i64, i64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut read = |from: u64, len: u64| -> Option<i64> {
        f.seek(SeekFrom::Start(from)).ok()?;
        let mut buf = Vec::with_capacity(len as usize);
        (&mut f).take(len).read_to_end(&mut buf).ok()?;
        Some(bytes_hash(&buf) as i64)
    };
    let head = read(0, REGION.min(offset))?;
    let tail_from = offset.saturating_sub(REGION);
    let tail = read(tail_from, offset - tail_from)?;
    Some((head, tail))
}

/// The build key an existing file was made with.
fn stored_versions(path: &std::path::Path) -> Option<(i64, i64, i64)> {
    let conn =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let get = |k: &str| {
        conn.query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| {
            r.get::<_, i64>(0)
        })
        .ok()
    };
    Some((
        get("schema")?,
        get("projection").unwrap_or(0),
        get("settings").unwrap_or(0),
    ))
}

/// Origins of sessions started by a program rather than a person: `claude -p`, `codex
/// exec`, and the TypeScript SDK. 1,334 of 1,587 local Claude sessions were `sdk-cli`.
pub const AUTOMATED_ORIGINS: &[&str] = &["sdk-cli", "exec", "sdk-ts"];

/// What a search leaves out by default, beyond the caller's own filters.
#[derive(Clone, Debug, Default)]
pub struct Visibility {
    /// Configured archive roots. Rows outside these are out of scope, not hidden.
    pub archive_roots: Option<Vec<String>>,
    /// The caller's time window: last written at or after, and before. Not defaults, so
    /// what they leave out is not counted as hidden.
    pub since_ns: Option<i64>,
    pub until_ns: Option<i64>,
    /// Leave out sessions written at or after this time, in nanoseconds since the epoch.
    pub active_since_ns: Option<i64>,
    pub exclude_session: Option<String>,
    /// Leave out automated sessions, except these `backend/id` keys.
    pub automated_except: Option<Vec<String>>,
}

/// Matching sessions each default left out. They overlap: one session can be both
/// recently active and automated.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Hidden {
    /// Written within the active window; `include_active` shows them.
    pub active: usize,
    /// Automated and not started by trovador; `include_automated` shows them.
    pub automated: usize,
    /// Started by another session; `include_subagents` shows them.
    #[serde(default)]
    pub subagents: usize,
    /// The session named by `exclude_session`.
    pub excluded: usize,
    /// Matched in the index but in an archive root that could not be read, so not
    /// returned. See `coverage.refresh.unavailable_roots`.
    #[serde(default)]
    pub unavailable: usize,
}

impl Hidden {
    pub fn any(&self) -> bool {
        self.active + self.automated + self.subagents + self.excluded + self.unavailable > 0
    }
}

pub(crate) fn mtime_ns(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn key(backend: Backend, id: &str) -> (Backend, String) {
    (backend, id.to_string())
}

impl Index {
    /// Opens the index, creating it and its directory if needed.
    pub fn open(paths: &Paths, limits: &Limits) -> Result<Self> {
        super::private::private_dir(&paths.home).map_err(|source| IndexError::Dir {
            path: paths.home.display().to_string(),
            source,
        })?;
        let path = db_path(paths);
        // Everything in the index is derived, so a version change starts a fresh file.
        // Reindexing row by row after a projection change left a 456 MB file and took
        // 20.4 s, against 415 MB and 9.8 s fresh (2026-10-07).
        if path.is_file() && stored_versions(&path) != Some(built_with(limits)) {
            for suffix in ["", "-wal", "-shm"] {
                let mut p = path.clone().into_os_string();
                p.push(suffix);
                let _ = std::fs::remove_file(PathBuf::from(p));
            }
        }
        // SQLite gives the -wal and -shm files the database file's mode, so the file is
        // created 0600 before SQLite sees it, and an older 0644 one is tightened.
        let _ = super::private::private_file()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path);
        for suffix in ["", "-wal", "-shm"] {
            let mut p = path.clone().into_os_string();
            p.push(suffix);
            let _ = super::private::make_private(&PathBuf::from(p));
        }
        let conn = Connection::open(&path)?;
        let index = Self { conn, path };
        index.configure()?;
        index.migrate(limits)?;
        Ok(index)
    }

    /// Opens the index only if it already exists. Reading a list must not create state.
    pub fn open_existing(paths: &Paths, limits: &Limits) -> Option<Self> {
        let path = db_path(paths);
        // An index from another version is about to be replaced; its rows describe a
        // projection this build would not produce, so it is treated as absent.
        if !path.is_file() || stored_versions(&path) != Some(built_with(limits)) {
            return None;
        }
        let conn = Connection::open(&path).ok()?;
        let index = Self { conn, path };
        index.configure().ok()?;
        Some(index)
    }

    fn configure(&self) -> Result<()> {
        // WAL lets the CLI read while a running server writes, and the timeout makes two
        // writers wait for each other instead of failing.
        self.conn.busy_timeout(Duration::from_secs(5))?;
        self.conn
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        register_regexp(&self.conn)?;
        self.conn.create_scalar_function(
            "under_archive_root",
            2,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8
                | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| {
                let file: String = ctx.get(0)?;
                let root: String = ctx.get(1)?;
                Ok(std::path::Path::new(&file).starts_with(root))
            },
        )?;
        Ok(())
    }

    fn migrate(&self, limits: &Limits) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);",
        )?;
        let version: Option<i64> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |r| {
                r.get(0)
            })
            .optional()?;
        if version == Some(SCHEMA_VERSION) {
            // Issue #36: an additive migration preserves FTS records during title backfill.
            // Serialize the column check with the upgrade because CLI and MCP can open together.
            let tx = rusqlite::Transaction::new_unchecked(
                &self.conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            if tx
                .prepare("SELECT title_metadata_version FROM sessions")
                .is_err()
            {
                tx.execute_batch(
                    "ALTER TABLE sessions ADD COLUMN title_metadata_version INTEGER NOT NULL DEFAULT 0;
                     ALTER TABLE sessions ADD COLUMN custom_title TEXT;",
                )?;
            }
            if tx.prepare("SELECT title_offset FROM sessions").is_err() {
                tx.execute_batch(
                    "ALTER TABLE sessions ADD COLUMN title_offset INTEGER NOT NULL DEFAULT -1;",
                )?;
            }
            // Issue #35: preserve a known fork boundary across index opens without
            // rebuilding unrelated sessions just to add the checkpoint column.
            if tx
                .prepare("SELECT codex_inherited_end FROM sessions")
                .is_err()
            {
                tx.execute_batch("ALTER TABLE sessions ADD COLUMN codex_inherited_end INTEGER;")?;
            }
            // Issues #19 and #34: copy recognition must survive append-only resumes.
            if tx
                .prepare("SELECT last_user_filtered_hash FROM sessions")
                .is_err()
            {
                tx.execute_batch(
                    "ALTER TABLE sessions ADD COLUMN last_user_filtered_hash INTEGER;",
                )?;
            }
            if tx
                .prepare("SELECT last_user_streams FROM sessions")
                .is_err()
            {
                tx.execute_batch(
                    "ALTER TABLE sessions ADD COLUMN last_user_streams INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
            tx.commit()?;
            return Ok(());
        }
        // Everything here is derived from the archives, so an unknown schema is rebuilt
        // rather than migrated.
        self.conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS records;
             DROP TABLE IF EXISTS sessions;
             CREATE TABLE sessions (
                 id INTEGER PRIMARY KEY,
                 backend TEXT NOT NULL,
                 session_id TEXT NOT NULL,
                 file TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 mtime_ns INTEGER NOT NULL,
                 meta TEXT NOT NULL,
                 paths TEXT NOT NULL,
                 origin TEXT,
                 subagent INTEGER NOT NULL,
                 text_indexed INTEGER NOT NULL,
                 projection_version INTEGER NOT NULL,
                 offset INTEGER NOT NULL,
                 next_index INTEGER NOT NULL,
                 codex_mode INTEGER NOT NULL,
                 head_hash INTEGER NOT NULL,
                 tail_hash INTEGER NOT NULL,
                 run_category TEXT,
                 run_count INTEGER,
                 last_kind TEXT,
                 last_hash INTEGER,
                 last_user_filtered_hash INTEGER,
                 last_user_streams INTEGER NOT NULL DEFAULT 0,
                 title_metadata_version INTEGER NOT NULL DEFAULT 0,
                 custom_title TEXT,
                 title_offset INTEGER NOT NULL DEFAULT -1,
                 codex_inherited_end INTEGER,
                 truncated INTEGER NOT NULL DEFAULT 0,
                 UNIQUE (backend, session_id)
             );
             CREATE VIRTUAL TABLE records USING fts5(
                 text, kind UNINDEXED, name UNINDEXED,
                 tokenize = '{TOKENIZER}'
             );"
        ))?;
        let (schema, projection, settings) = built_with(limits);
        for (key, value) in [
            ("schema", schema),
            ("projection", projection),
            ("settings", settings),
        ] {
            self.conn.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
        }
        Ok(())
    }

    pub(crate) fn read_meta_entry(&self, e: &ArchiveEntry, limits: &Limits) -> Option<SessionMeta> {
        let stored = self
            .conn
            .query_row(
                "SELECT file, size, mtime_ns, meta, offset, head_hash, tail_hash, custom_title
             FROM sessions WHERE backend = ?1 AND session_id = ?2 AND title_metadata_version = ?3 AND title_offset = offset",
                params![e.backend.as_str(), e.session_id, TITLE_METADATA_VERSION],
                |r| {
                    Ok(IndexedTitle {
                        file: r.get(0)?,
                        size: r.get::<_, i64>(1)? as u64,
                        mtime: r.get(2)?,
                        json: r.get(3)?,
                        offset: r.get::<_, i64>(4)? as u64,
                        hashes: (r.get(5)?, r.get(6)?),
                        custom_title: r.get(7)?,
                    })
                },
            )
            .ok()?;
        if std::path::Path::new(&stored.file) != e.file {
            return None;
        }
        let mut meta = if stored.size == e.size_bytes && stored.mtime == mtime_ns(e.mtime) {
            serde_json::from_str(&stored.json).ok()?
        } else {
            // Issue #36: an indexed append needs only its new bytes, even before refresh.
            if stored.size >= e.size_bytes
                || region_hashes(&e.file, stored.offset) != Some(stored.hashes)
            {
                return None;
            }
            archive::read_meta_windows(e)
        };
        if e.backend == Backend::Claude {
            let mut title = stored.custom_title;
            // Issue #36: pending appends and final records without a newline share the
            // display budget; their titles must not change the persisted checkpoint.
            if stored.offset < e.size_bytes
                && e.size_bytes - stored.offset <= limits.title_scan_max_bytes as u64
            {
                archive::scan_custom_titles(&e.file, stored.offset, e.size_bytes, &mut title)
                    .ok()?;
            }
            archive::apply_custom_title(&mut meta, title);
        }
        Some(meta)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Stored metadata for every session, for callers that want it without reading files.
    /// An entry is only trustworthy while the file's size and mtime still match.
    pub fn meta_snapshot(&self) -> HashMap<(Backend, String), (u64, i64, SessionMeta)> {
        let mut out = HashMap::new();
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT backend, session_id, size, mtime_ns, meta FROM sessions WHERE title_metadata_version = ?1 AND title_offset = offset AND (backend != 'claude' OR offset = size)")
        else {
            return out;
        };
        let rows = stmt.query_map([TITLE_METADATA_VERSION], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        });
        for (b, id, size, mtime, meta) in rows.into_iter().flatten().flatten() {
            if let (Ok(b), Ok(meta)) = (b.parse::<Backend>(), serde_json::from_str(&meta)) {
                out.insert((b, id), (size as u64, mtime, meta));
            }
        }
        out
    }

    pub fn status(&self) -> Result<IndexStatus> {
        let (sessions, text_indexed): (i64, i64) = self.conn.query_row(
            "SELECT count(*), coalesce(sum(text_indexed), 0) FROM sessions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let records: i64 = self
            .conn
            .query_row("SELECT count(*) FROM records", [], |r| r.get(0))?;
        let size_bytes = ["", "-wal"]
            .iter()
            .filter_map(|s| {
                let mut p = self.path.clone().into_os_string();
                p.push(s);
                std::fs::metadata(PathBuf::from(p)).ok()
            })
            .map(|m| m.len())
            .sum();
        Ok(IndexStatus {
            path: self.path.display().to_string(),
            size_bytes,
            sessions: sessions as usize,
            text_indexed: text_indexed as usize,
            records: records as usize,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexStatus {
    pub path: String,
    pub size_bytes: u64,
    pub sessions: usize,
    pub text_indexed: usize,
    pub records: usize,
}

// --- refresh ---------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct RefreshOpts {
    /// Stop starting new files after this long. Files already started finish.
    /// Without a budget, also prune out-of-root rows if every root is readable or missing.
    pub budget: Option<Duration>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RefreshReport {
    /// Session files in the archives.
    pub sessions: usize,
    /// Files whose size and mtime matched the index, so nothing was read.
    pub unchanged: usize,
    /// Files continued from where the last pass stopped.
    pub appended: usize,
    /// Files indexed from the start: new, rewritten, moved, or from an older projection.
    pub reindexed: usize,
    /// Files whose title state was backfilled without rebuilding existing text (issue #36).
    pub metadata_refreshed: usize,
    /// Files the budget ran out before reaching. The index is stale for these.
    pub pending: usize,
    /// Index rows dropped because their file is gone.
    pub removed: usize,
    /// Files that could not be read.
    pub failed: usize,
    /// Archive roots that could not be read although the index holds sessions that
    /// could belong to them, including out-of-root rows with a different spelling (#29).
    /// Their rows are kept, since an unreadable root says nothing about whether its
    /// sessions still exist, but nothing in them can be checked or returned (#27).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unavailable_roots: Vec<String>,
    /// Why explicit out-of-root cleanup was skipped, including the blocking roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_skipped_reason: Option<String>,
    pub ms: u64,
}

struct IndexedTitle {
    file: String,
    size: u64,
    mtime: i64,
    json: String,
    offset: u64,
    hashes: (i64, i64),
    custom_title: Option<String>,
}

struct StoredRow {
    id: i64,
    hashes: (i64, i64),
    file: String,
    size: i64,
    mtime_ns: i64,
    text_indexed: bool,
    projection_version: i64,
    title_metadata_version: i64,
    title_offset: i64,
    state: ProjectionState,
}

enum Work {
    Full,
    Resume(i64, ProjectionState),
    Metadata(i64, ProjectionState),
}

struct Job {
    entry: ArchiveEntry,
    work: Work,
}

struct Done {
    entry: ArchiveEntry,
    meta: SessionMeta,
    text: bool,
    metadata_refresh: bool,
    /// Where this pass started, to detect another process indexing the same file meanwhile.
    resumed_from: Option<(i64, u64)>,
    records: Vec<(usize, RecordKind, Option<String>, String)>,
    /// Records in `records` cut at their cap, whose text past it is not indexed.
    truncated: usize,
    state: ProjectionState,
    /// `region_hashes` at `state.offset`, so the next pass can tell an append from a rewrite.
    hashes: (i64, i64),
}

fn text_eligible(meta: &SessionMeta) -> bool {
    !meta
        .origin
        .as_deref()
        .is_some_and(|o| TEXT_EXCLUDED_ORIGINS.contains(&o))
}

fn kind_str(k: RecordKind) -> &'static str {
    match k {
        RecordKind::User => "user",
        RecordKind::Assistant => "assistant",
        RecordKind::ToolCall => "tool_call",
        RecordKind::ToolResult => "tool_result",
        RecordKind::Attachment => "attachment",
        RecordKind::System => "system",
        RecordKind::Reasoning => "reasoning",
        RecordKind::Omitted => "omitted",
    }
}

fn project(job: Job, limits: &Limits) -> Option<Done> {
    // Issue #36: projection already reads new lines, so metadata must not scan them again.
    let mut meta = archive::read_meta_windows(&job.entry);
    let text = text_eligible(&meta);
    let metadata_refresh = matches!(job.work, Work::Metadata(..));
    let (resume, resumed_from) = match job.work {
        Work::Resume(id, s) | Work::Metadata(id, s) => {
            let from = (id, s.offset);
            (Some(s), Some(from))
        }
        Work::Full => (None, None),
    };
    let mut records = Vec::new();
    let mut truncated = 0;
    let resume = if metadata_refresh {
        let mut state = resume?;
        // Issue #36: an old binary can rewrite a file while retaining its previous title.
        // Backfill from an empty title so a rewrite without custom-title clears that name.
        state.custom_title = None;
        if job.entry.backend == Backend::Claude {
            archive::scan_custom_titles(&job.entry.file, 0, state.offset, &mut state.custom_title)
                .ok()?;
        }
        Some(state)
    } else {
        resume
    };
    let state = if text {
        let mut proj = Projection::for_index(&job.entry, limits, resume).ok()?;
        for r in proj.by_ref() {
            if r.kind == RecordKind::Reasoning && !limits.index_reasoning {
                continue;
            }
            if let Some(t) = r.text {
                truncated += t.ends_with(TRUNCATED) as usize;
                records.push((r.index, r.kind, r.name, t));
            }
        }
        proj.state().clone()
    } else {
        // Not projected at all: the row only carries metadata.
        ProjectionState {
            offset: job.entry.size_bytes,
            ..Default::default()
        }
    };
    archive::apply_custom_title(&mut meta, state.custom_title.clone());
    let hashes = region_hashes(&job.entry.file, state.offset)?;
    Some(Done {
        entry: job.entry,
        meta,
        text,
        metadata_refresh,
        resumed_from,
        records,
        truncated,
        state,
        hashes,
    })
}

impl Index {
    fn stored_rows(&self) -> Result<HashMap<(Backend, String), StoredRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, backend, session_id, file, size, mtime_ns, text_indexed,
                    projection_version, offset, next_index, codex_mode, head_hash, tail_hash,
                    run_category, run_count, last_kind, last_hash, title_metadata_version, custom_title, title_offset, codex_inherited_end, last_user_filtered_hash, last_user_streams
             FROM sessions",
        )?;
        let rows = stmt.query_map([], |r| {
            let run = match (
                r.get::<_, Option<String>>(13)?,
                r.get::<_, Option<i64>>(14)?,
            ) {
                (Some(c), Some(n)) => Some((c, n as usize)),
                _ => None,
            };
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                StoredRow {
                    id: r.get(0)?,
                    file: r.get(3)?,
                    size: r.get(4)?,
                    mtime_ns: r.get(5)?,
                    text_indexed: r.get::<_, i64>(6)? != 0,
                    projection_version: r.get(7)?,
                    title_metadata_version: r.get(17)?,
                    title_offset: r.get(19)?,
                    hashes: (r.get(11)?, r.get(12)?),
                    state: ProjectionState {
                        offset: r.get::<_, i64>(8)? as u64,
                        codex_inherited_end: r.get::<_, Option<i64>>(20)?.map(|n| n as u64),
                        next_index: r.get::<_, i64>(9)? as usize,
                        custom_title: r.get(18)?,
                        mode: CodexMode::from_i64(r.get(10)?),
                        run,
                        last_user_streams: r.get(22)?,
                        last_user_filtered_hash: r.get::<_, Option<i64>>(21)?.map(|h| h as u64),
                        last_turn: match (
                            r.get::<_, Option<String>>(15)?,
                            r.get::<_, Option<i64>>(16)?,
                        ) {
                            (Some(k), Some(h)) => kind_from_str(&k).map(|k| (k, h as u64)),
                            _ => None,
                        },
                    },
                },
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (b, id, stored) = row?;
            if let Ok(b) = b.parse::<Backend>() {
                out.insert((b, id), stored);
            }
        }
        Ok(out)
    }

    /// Brings the index up to date with the archives, newest sessions first.
    pub fn refresh(
        &mut self,
        paths: &Paths,
        limits: &Limits,
        opts: &RefreshOpts,
    ) -> Result<RefreshReport> {
        let started = Instant::now();
        // One file per session, the newest; scan_all chooses.
        let entries: Vec<ArchiveEntry> = archive::scan_all(&paths.roots());
        let stored = self.stored_rows()?;
        let mut report = RefreshReport {
            sessions: entries.len(),
            ..Default::default()
        };

        let mut jobs = Vec::new();
        for e in &entries {
            let file = e.file.display().to_string();
            let work = match stored.get(&key(e.backend, &e.session_id)) {
                Some(s)
                    if s.file == file
                        && s.size == e.size_bytes as i64
                        && s.mtime_ns == mtime_ns(e.mtime)
                        && s.projection_version == PROJECTION_VERSION
                        && s.title_metadata_version == TITLE_METADATA_VERSION
                        && s.title_offset == s.state.offset as i64 =>
                {
                    report.unchanged += 1;
                    continue;
                }
                Some(s)
                    if s.file == file
                        && s.size == e.size_bytes as i64
                        && s.mtime_ns == mtime_ns(e.mtime)
                        && s.projection_version == PROJECTION_VERSION =>
                {
                    Work::Metadata(s.id, s.state.clone())
                }
                // A file continues where the last pass stopped only if it grew and the
                // bytes it was indexed from are still the same. Growing alone did not prove
                // that: a rewrite of the same size or larger kept its old rows (#25).
                Some(s)
                    if s.file == file
                        && s.text_indexed
                        && s.projection_version == PROJECTION_VERSION
                        && e.size_bytes as i64 > s.size
                        && e.size_bytes >= s.state.offset
                        && region_hashes(&e.file, s.state.offset) == Some(s.hashes) =>
                {
                    if s.title_metadata_version == TITLE_METADATA_VERSION
                        && s.title_offset == s.state.offset as i64
                    {
                        Work::Resume(s.id, s.state.clone())
                    } else {
                        Work::Metadata(s.id, s.state.clone())
                    }
                }
                _ => Work::Full,
            };
            jobs.push(Job {
                entry: e.clone(),
                work,
            });
        }

        // Issue #29: search processes can share an index with different archive settings,
        // so only an explicit, unbudgeted index run may discard out-of-root rows. Paths
        // are compared as spelled (case-sensitive and lossy), so require every root to
        // be readable or missing before discarding rows that might use another spelling
        // of one. A missing provider archive must not block single-provider cleanup.
        let live: std::collections::HashSet<(Backend, String)> = entries
            .iter()
            .map(|e| key(e.backend, &e.session_id))
            .collect();
        let roots: Vec<(&std::path::Path, bool, Option<String>)> = paths
            .archive_roots()
            .into_iter()
            .map(|r| match std::fs::read_dir(r) {
                Ok(_) => (r.as_path(), true, None),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (r.as_path(), false, None),
                Err(e) => (r.as_path(), false, Some(e.to_string())),
            })
            .collect();
        let blocking_roots: Vec<String> = roots
            .iter()
            .filter_map(|(root, _, error)| {
                error
                    .as_ref()
                    .map(|error| format!("{}: {error}", root.display()))
            })
            .collect();
        let remove_out_of_root = opts.budget.is_none() && blocking_roots.is_empty();
        if opts.budget.is_none() && !blocking_roots.is_empty() {
            report.cleanup_skipped_reason = Some(format!(
                "out-of-root cleanup skipped because configured roots could not be read: {}",
                blocking_roots.join(", ")
            ));
        }
        let has_out_of_root_rows = stored.values().any(|s| {
            !roots
                .iter()
                .any(|(root, _, _)| std::path::Path::new(&s.file).starts_with(root))
        });
        let can_remove = |file: &str| {
            roots
                .iter()
                .find(|(root, _, _)| std::path::Path::new(file).starts_with(root))
                .map_or(remove_out_of_root, |(_, ok, _)| *ok)
        };
        report.unavailable_roots = roots
            .iter()
            .filter(|(root, ok, error)| {
                // Issue #29: an unreadable root may own out-of-root rows indexed under
                // another spelling, so search cannot prove absence by filtering them out.
                !*ok && ((error.is_some() && has_out_of_root_rows)
                    || stored
                        .values()
                        .any(|s| std::path::Path::new(&s.file).starts_with(root)))
            })
            .map(|(root, _, _)| root.display().to_string())
            .collect();
        let gone: Vec<(i64, &str)> = stored
            .iter()
            .filter(|(k, _)| !live.contains(*k))
            .filter(|(_, s)| can_remove(&s.file))
            .map(|(_, s)| (s.id, s.file.as_str()))
            .collect();
        if !gone.is_empty() {
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            for (id, file) in &gone {
                // Issue #29: the scan's snapshot can precede another process relocating
                // this row. Delete its records only if its file still matches that snapshot.
                let removed = tx.execute(
                    "DELETE FROM sessions WHERE id = ?1 AND file = ?2",
                    params![id, file],
                )?;
                if removed > 0 {
                    delete_records(&tx, *id)?;
                    report.removed += removed;
                }
            }
            tx.commit()?;
        }

        let deadline = opts.budget.map(|b| started + b);
        let total = jobs.len();
        let queue = Mutex::new(jobs.into_iter());
        let started_jobs = AtomicUsize::new(0);
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, 8);
        let (tx, rx) = mpsc::sync_channel::<Option<Done>>(workers * 2);

        let mut write_error = None;
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let tx = tx.clone();
                let queue = &queue;
                let started_jobs = &started_jobs;
                scope.spawn(move || {
                    loop {
                        if deadline.is_some_and(|d| Instant::now() >= d) {
                            break;
                        }
                        let Some(job) = queue.lock().expect("queue").next() else {
                            break;
                        };
                        started_jobs.fetch_add(1, Ordering::Relaxed);
                        if tx.send(project(job, limits)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(tx);
            for done in rx {
                if write_error.is_some() {
                    continue;
                }
                match done {
                    None => report.failed += 1,
                    Some(d) => {
                        let resumed = d.resumed_from.is_some();
                        let metadata_refresh = d.metadata_refresh;
                        match self.write(d) {
                            Ok(true) if metadata_refresh => report.metadata_refreshed += 1,
                            Ok(true) if resumed => report.appended += 1,
                            Ok(true) => report.reindexed += 1,
                            Ok(false) => {}
                            Err(e) => write_error = Some(e),
                        }
                    }
                }
            }
        });
        if let Some(e) = write_error {
            return Err(e);
        }
        report.pending = total - started_jobs.load(Ordering::Relaxed);
        report.ms = started.elapsed().as_millis() as u64;
        Ok(report)
    }

    /// One file in one transaction. Returns false when another process indexed the same
    /// file first, in which case this pass's work is discarded.
    fn write(&mut self, d: Done) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let k = key(d.entry.backend, &d.entry.session_id);
        let existing: Option<(i64, i64, i64)> = tx
            .query_row(
                "SELECT id, offset, truncated FROM sessions WHERE backend = ?1 AND session_id = ?2",
                params![k.0.as_str(), k.1],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let mut truncated = d.truncated as i64;
        if let Some((id, from)) = d.resumed_from {
            match existing {
                Some((i, offset, before)) if (i, offset) == (id, from as i64) => {
                    truncated += before;
                }
                _ => return Ok(false),
            }
        } else if let Some((id, _, _)) = existing {
            delete_records(&tx, id)?;
        }
        let paths_text = [d.meta.cwd.as_deref(), d.meta.relocated_cwd.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");
        let meta_json = serde_json::to_string(&d.meta).unwrap_or_else(|_| "{}".into());
        let s = &d.state;
        // Issue #36: old binaries leave title_offset behind when advancing offset, so
        // readers can detect that the cached title did not follow the new checkpoint.
        let id: i64 = tx.query_row(
            "INSERT INTO sessions (backend, session_id, file, size, mtime_ns, meta, paths, origin,
                 subagent, text_indexed, projection_version, offset, next_index, codex_mode,
                 head_hash, tail_hash, run_category, run_count, last_kind, last_hash, title_metadata_version, custom_title, title_offset, codex_inherited_end, last_user_filtered_hash, last_user_streams, truncated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)
             ON CONFLICT (backend, session_id) DO UPDATE SET
                 file = excluded.file, size = excluded.size, mtime_ns = excluded.mtime_ns,
                 meta = excluded.meta, paths = excluded.paths, origin = excluded.origin,
                 subagent = excluded.subagent, text_indexed = excluded.text_indexed,
                 projection_version = excluded.projection_version, offset = excluded.offset,
                 next_index = excluded.next_index, codex_mode = excluded.codex_mode,
                 head_hash = excluded.head_hash, tail_hash = excluded.tail_hash,
                 run_category = excluded.run_category,
                 run_count = excluded.run_count, last_kind = excluded.last_kind,
                 last_hash = excluded.last_hash,
                 title_metadata_version = excluded.title_metadata_version,
                 custom_title = excluded.custom_title,
                 title_offset = excluded.title_offset,
                 codex_inherited_end = excluded.codex_inherited_end,
                 last_user_filtered_hash = excluded.last_user_filtered_hash,
                 last_user_streams = excluded.last_user_streams,
                 truncated = excluded.truncated
             RETURNING id",
            params![
                k.0.as_str(),
                k.1,
                d.entry.file.display().to_string(),
                d.entry.size_bytes as i64,
                mtime_ns(d.entry.mtime),
                meta_json,
                paths_text,
                d.meta.origin,
                d.meta.subagent as i64,
                d.text as i64,
                PROJECTION_VERSION,
                s.offset as i64,
                s.next_index as i64,
                s.mode.as_i64(),
                d.hashes.0,
                d.hashes.1,
                s.run.as_ref().map(|(c, _)| c.clone()),
                s.run.as_ref().map(|(_, n)| *n as i64),
                s.last_turn.map(|(k, _)| kind_str(k)),
                s.last_turn.map(|(_, h)| h as i64),
                TITLE_METADATA_VERSION,
                s.custom_title,
                s.offset as i64,
                s.codex_inherited_end.map(|n| n as i64),
                s.last_user_filtered_hash.map(|h| h as i64),
                s.last_user_streams,
                truncated,
            ],
            |r| r.get(0),
        )?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO records (rowid, text, kind, name) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (idx, kind, name, text) in &d.records {
                insert.execute(params![
                    (id << 32) | (*idx as i64),
                    text,
                    kind_str(*kind),
                    name
                ])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }
}

fn kind_from_str(s: &str) -> Option<RecordKind> {
    match s {
        "user" => Some(RecordKind::User),
        "assistant" => Some(RecordKind::Assistant),
        _ => None,
    }
}

fn delete_records(tx: &rusqlite::Transaction, id: i64) -> rusqlite::Result<usize> {
    tx.execute(
        "DELETE FROM records WHERE rowid BETWEEN ?1 AND ?2",
        params![id << 32, (id << 32) | 0xffff_ffff],
    )
}

// Issue #33: validation must enforce SQL's regex limits before refreshing the index.
pub(crate) fn search_regex(pattern: &str) -> std::result::Result<regex::Regex, regex::Error> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 22)
        .build()
}

// Issue #33: SQLite loses the callback error kind, so retain its query origin.
pub(crate) const REGEXP_QUERY_ERROR_PREFIX: &str = "regexp query: ";

pub(crate) fn sql_search_regex(pattern: &str) -> std::io::Result<regex::Regex> {
    search_regex(pattern)
        .map_err(|err| std::io::Error::other(format!("{REGEXP_QUERY_ERROR_PREFIX}{err}")))
}

fn register_regexp(conn: &Connection) -> rusqlite::Result<()> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function(
        "regexp",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            // Compiled once per statement rather than once per row.
            let re = ctx.get_or_create_aux(0, |v| -> std::io::Result<_> {
                sql_search_regex(v.as_str().unwrap_or(""))
            })?;
            let text = ctx.get_raw(1).as_str().unwrap_or("");
            Ok(re.is_match(text))
        },
    )
}

// --- search ----------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    /// Sessions whose best match ranks highest first.
    #[default]
    Relevance,
    /// Most recently modified sessions first.
    Recent,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SearchOpts {
    /// Words and "quoted phrases", all required. A word or phrase matches as a sequence of
    /// tokens, so a UUID or a snake_case name matches as written.
    pub query: String,
    /// Treat `query` as an FTS5 expression (AND, OR, NOT, NEAR, prefix*, column filters).
    #[serde(default)]
    pub fts: bool,
    /// Treat `query` as a case-insensitive regular expression over the indexed text.
    #[serde(default)]
    pub regex: bool,
    pub backend: Option<Backend>,
    /// Only sessions whose project path contains this text (ASCII case-insensitive).
    pub project: Option<String>,
    /// Only sessions with this origin.
    pub origin: Option<String>,
    /// Include sessions started by another session.
    #[serde(default)]
    pub include_subagents: bool,
    /// Include sessions written in the last few minutes, which by default are left out
    /// because the session doing the search is always among them.
    #[serde(default)]
    pub include_active: bool,
    /// Include automated sessions (`claude -p`, `codex exec`) that trovador did not start.
    #[serde(default)]
    pub include_automated: bool,
    /// Leave out this session id, typically the caller's own.
    pub exclude_session: Option<String>,
    /// Only sessions last written at or after this: an RFC 3339 time, a date, or a
    /// duration back from now such as `2d`.
    pub since: Option<String>,
    /// Only sessions last written before this, in the same forms as `since`.
    pub until: Option<String>,
    /// Sessions to return.
    pub limit: Option<usize>,
    /// Matching records to show per session.
    pub per_session: Option<usize>,
    #[serde(default)]
    pub order: Order,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Match {
    /// Record index; pass it as the transcript offset to read around the match.
    pub index: usize,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub excerpt: String,
}

pub struct SessionHits {
    pub backend: Backend,
    pub session_id: String,
    pub meta: SessionMeta,
    pub match_count: usize,
    pub matches: Vec<Match>,
}

pub struct RawSearch {
    /// The FTS5 expression actually run, or the regex.
    pub expression: String,
    pub sessions_matched: usize,
    pub hidden: Hidden,
    pub truncated: Truncated,
    pub hits: Vec<SessionHits>,
}

/// Records in the searched sessions that were cut at `record_chars` or `tool_chars`.
/// Text past the cut is not indexed, so finding nothing in them proves nothing (#122).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Truncated {
    pub sessions: usize,
    pub records: usize,
}

/// Turns words and "quoted phrases" into an FTS5 expression where each is a quoted phrase
/// and all are required. Quoting every term keeps punctuation in identifiers from being
/// read as FTS5 operators.
pub fn terms_to_fts(query: &str) -> Option<String> {
    let terms = search_terms(query)
        .into_iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" "))
}

fn search_terms(query: &str) -> Vec<&str> {
    let mut terms = Vec::new();
    let mut rest = query.trim();
    while !rest.is_empty() {
        let (term, tail) = if let Some(r) = rest.strip_prefix('"') {
            match r.find('"') {
                Some(end) => (&r[..end], &r[end + 1..]),
                None => (r, ""),
            }
        } else {
            match rest.find(char::is_whitespace) {
                Some(end) => (&rest[..end], &rest[end..]),
                None => (rest, ""),
            }
        };
        if !term.trim().is_empty() {
            terms.push(term.trim());
        }
        rest = tail.trim_start();
    }
    terms
}

struct QueryTokenizer(Connection);

impl QueryTokenizer {
    fn new() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        // Issue #28: Rust character categories differ from SQLite's, including PUA
        // glyphs and combining marks. Share the records tokenizer to avoid guessing.
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE tokens USING fts5(text, tokenize = '{TOKENIZER}');
             CREATE VIRTUAL TABLE vocabulary USING fts5vocab(tokens, 'row');"
        ))?;
        Ok(Self(conn))
    }

    fn has_tokens(&self, text: &str) -> Result<bool> {
        self.0.execute("DELETE FROM tokens", [])?;
        self.0
            .execute("INSERT INTO tokens(text) VALUES (?1)", [text])?;
        Ok(self
            .0
            .query_row("SELECT EXISTS(SELECT 1 FROM vocabulary)", [], |r| r.get(0))?)
    }
}

pub fn validate_terms(query: &str) -> Result<()> {
    let tokenizer = QueryTokenizer::new()?;
    let terms = search_terms(query);
    if terms.is_empty() {
        return Err(IndexError::Query(
            "query cannot match: no searchable terms remain".into(),
        ));
    }
    for term in terms {
        if !tokenizer.has_tokens(term)? {
            return Err(IndexError::Query(format!(
                "term {term:?} cannot match: the records tokenizer produces no tokens; use regex for punctuation"
            )));
        }
    }
    Ok(())
}

/// A phrase is one sequence of tokens, so punctuation inside it is skipped as FTS5 skips it,
/// and only a phrase with no tokens at all cannot match (#132).
pub fn validate_phrase(query: &str) -> Result<()> {
    if !QueryTokenizer::new()?.has_tokens(query)? {
        return Err(IndexError::Query(format!(
            "phrase {query:?} cannot match: the records tokenizer produces no tokens; use regex for punctuation"
        )));
    }
    Ok(())
}

pub fn fts_absence_note(query: &str) -> Result<Option<String>> {
    let tokenizer = QueryTokenizer::new()?;
    let mut chars = query.chars().peekable();
    let mut tokens = Vec::new();
    let mut tokenless_phrase_or_word = false;
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '"' {
            let mut phrase = String::new();
            while let Some(c) = chars.next() {
                if c == '"' {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                    } else {
                        break;
                    }
                }
                phrase.push(c);
            }
            tokenless_phrase_or_word |= !tokenizer.has_tokens(&phrase)?;
            tokens.push(phrase);
        } else if ":{}()-+*^,".contains(c) {
            tokens.push(c.to_string());
        } else {
            let mut word = c.to_string();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || "\":{}()-+*^,".contains(c) {
                    break;
                }
                word.push(c);
                chars.next();
            }
            // Issue #28: FTS5 accepts non-ASCII barewords that unicode61 drops.
            // Operators and column names are syntax, so only search words affect proof.
            let column_name = chars.clone().find(|c| !c.is_whitespace()) == Some(':');
            if !column_name && !matches!(word.as_str(), "AND" | "OR" | "NOT" | "NEAR") {
                tokenless_phrase_or_word |= !tokenizer.has_tokens(word.trim_end_matches('*'))?;
            }
            tokens.push(word);
        }
    }
    // Issue #28: these references may be in OR branches or negated filters. They
    // cannot justify rejecting a valid expression, but make an empty result ambiguous.
    let unindexed = tokens.iter().enumerate().any(|(i, token)| {
        if token != ":" || i == 0 {
            return false;
        }
        let columns = if tokens[i - 1] == "}" {
            let start = tokens[..i - 1]
                .iter()
                .rposition(|t| t == "{")
                .unwrap_or(i - 1);
            &tokens[start..i - 1]
        } else {
            &tokens[i - 1..i]
        };
        columns
            .iter()
            .any(|c| c.eq_ignore_ascii_case("kind") || c.eq_ignore_ascii_case("name"))
    });
    let mut reasons = Vec::new();
    if unindexed {
        reasons.push("query references unindexed kind or name columns");
    }
    if tokenless_phrase_or_word {
        reasons.push("query contains a phrase or word with no tokens");
    }
    Ok((!reasons.is_empty()).then(|| format!("absence is not proof: {}", reasons.join("; "))))
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

impl Index {
    pub fn search(
        &self,
        opts: &SearchOpts,
        vis: &Visibility,
        limit: usize,
        per_session: usize,
        redact: bool,
    ) -> Result<RawSearch> {
        let expression = if opts.regex || opts.fts {
            opts.query.clone()
        } else {
            terms_to_fts(&opts.query).unwrap_or_default()
        };
        if expression.trim().is_empty() {
            return Ok(RawSearch {
                expression,
                sessions_matched: 0,
                hidden: Hidden::default(),
                truncated: Truncated::default(),
                hits: Vec::new(),
            });
        }

        let mut filters = vec!["s.text_indexed = 1".to_string()];
        let mut args: Vec<rusqlite::types::Value> = vec![expression.clone().into()];
        // Issue #29: scope before counting matches or hidden defaults, so another
        // process's archive rows cannot invalidate this process's absence proof.
        if let Some(roots) = &vis.archive_roots {
            let mut scoped = Vec::new();
            for root in roots {
                args.push(root.clone().into());
                scoped.push(format!("under_archive_root(s.file, ?{})", args.len()));
            }
            filters.push(if scoped.is_empty() {
                "0".into()
            } else {
                format!("({})", scoped.join(" OR "))
            });
        }
        if let Some(b) = opts.backend {
            args.push(b.as_str().to_string().into());
            filters.push(format!("s.backend = ?{}", args.len()));
        }
        if let Some(p) = opts.project.as_deref().filter(|p| !p.is_empty()) {
            args.push(format!("%{}%", like_escape(p)).into());
            filters.push(format!("s.paths LIKE ?{} ESCAPE '\\'", args.len()));
        }
        if let Some(o) = opts.origin.as_deref() {
            args.push(o.to_string().into());
            filters.push(format!("s.origin = ?{}", args.len()));
        }
        if let Some(t) = vis.since_ns {
            args.push(t.into());
            filters.push(format!("s.mtime_ns >= ?{}", args.len()));
        }
        if let Some(t) = vis.until_ns {
            args.push(t.into());
            filters.push(format!("s.mtime_ns < ?{}", args.len()));
        }
        // The defaults, each kept apart so what it hid can be counted.
        let mut defaults: Vec<(&str, String, rusqlite::types::Value)> = Vec::new();
        // Issue #15: an uncounted subagent filter made hidden matches look provably absent.
        if !opts.include_subagents {
            defaults.push(("subagents", "s.subagent = ?".into(), 0.into()));
        }
        if let Some(t) = vis.active_since_ns {
            defaults.push(("active", "s.mtime_ns < ?".into(), t.into()));
        }
        if let Some(id) = &vis.exclude_session {
            defaults.push(("excluded", "s.session_id <> ?".into(), id.clone().into()));
        }
        if let Some(except) = &vis.automated_except {
            let list = AUTOMATED_ORIGINS
                .iter()
                .map(|o| format!("'{o}'"))
                .collect::<Vec<_>>()
                .join(", ");
            defaults.push((
                "automated",
                format!(
                    "(coalesce(s.origin, '') NOT IN ({list}) OR (s.backend || '/' || s.session_id) IN (SELECT value FROM json_each(?)))"
                ),
                serde_json::to_string(except).unwrap_or_else(|_| "[]".into()).into(),
            ));
        }
        let base_args = args.clone();
        let base_where = filters.join(" AND ");
        for (_, pred, arg) in &defaults {
            args.push(arg.clone());
            filters.push(pred.replace('?', &format!("?{}", args.len())));
        }
        let where_s = filters.join(" AND ");
        let matcher = if opts.regex {
            "regexp(?1, records.text)"
        } else {
            "records MATCH ?1"
        };
        // bm25() is only allowed in a query whose sole table is the FTS table, so records
        // are scored in a materialized CTE and joined and aggregated outside it. A plain
        // subquery is flattened back into the join and fails the same way.
        let score = if opts.regex { "0" } else { "bm25(records)" };
        let cte = format!(
            "WITH m AS MATERIALIZED (
                 SELECT records.rowid >> 32 AS sid, {score} AS score FROM records WHERE {matcher})"
        );
        let matched = "m JOIN sessions s ON s.id = m.sid";
        let order = match (opts.order, opts.regex) {
            (Order::Relevance, false) => "best ASC, s.mtime_ns DESC",
            _ => "s.mtime_ns DESC",
        };

        let sessions_matched: i64 = self.conn.query_row(
            &format!("{cte} SELECT count(DISTINCT m.sid) FROM {matched} WHERE {where_s}"),
            rusqlite::params_from_iter(args.iter()),
            |r| r.get(0),
        )?;
        let mut hidden = Hidden::default();
        for (name, pred, arg) in &defaults {
            let mut a = base_args.clone();
            a.push(arg.clone());
            let n: i64 = self.conn.query_row(
                &format!(
                    "{cte} SELECT count(DISTINCT m.sid) FROM {matched} WHERE {base_where} AND NOT {}",
                    pred.replace('?', &format!("?{}", a.len()))
                ),
                rusqlite::params_from_iter(a.iter()),
                |r| r.get(0),
            )?;
            match *name {
                "active" => hidden.active = n as usize,
                "automated" => hidden.automated = n as usize,
                "subagents" => hidden.subagents = n as usize,
                _ => hidden.excluded = n as usize,
            }
        }

        // Every session in scope, matching or not. ?1 is the query, which only the
        // matcher uses, so it is bound here to keep the parameter numbers.
        let truncated = self.conn.query_row(
            &format!(
                "SELECT count(*), coalesce(sum(s.truncated), 0) FROM sessions s
                 WHERE ?1 IS NOT NULL AND {where_s} AND s.truncated > 0"
            ),
            rusqlite::params_from_iter(args.iter()),
            |r| {
                Ok(Truncated {
                    sessions: r.get::<_, i64>(0)? as usize,
                    records: r.get::<_, i64>(1)? as usize,
                })
            },
        )?;

        let mut top = self.conn.prepare(&format!(
            "{cte} SELECT s.id, s.backend, s.session_id, s.meta, count(*) AS n, min(m.score) AS best
             FROM {matched} WHERE {where_s}
             GROUP BY s.id ORDER BY {order} LIMIT {limit}"
        ))?;
        let rows: Vec<(i64, String, String, String, i64)> = top
            .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;

        // Whole records, not snippet(): an excerpt is cut and highlighted only after the
        // full text is redacted. snippet() bracketed a match inside a key before the doors
        // redacted, which split the key past recognition and returned it whole (#22).
        let order_by = if opts.regex { "rowid" } else { "rank" };
        let mut excerpts = self.conn.prepare(&format!(
            "SELECT rowid & 4294967295, kind, name, text FROM records
             WHERE {matcher} AND rowid BETWEEN ?2 AND ?3
             ORDER BY {order_by} LIMIT {per_session}"
        ))?;
        let re = if opts.regex {
            search_regex(&opts.query).ok()
        } else {
            fts_highlighter(&expression)
        };

        let mut hits = Vec::new();
        for (id, backend, session_id, meta, n) in rows {
            let Ok(backend) = backend.parse::<Backend>() else {
                continue;
            };
            let matches = excerpts
                .query_map(
                    params![expression, id << 32, (id << 32) | 0xffff_ffff],
                    |r| {
                        let text: String = r.get(3)?;
                        Ok(Match {
                            index: r.get::<_, i64>(0)? as usize,
                            kind: r.get(1)?,
                            name: r.get(2)?,
                            excerpt: excerpt(&text, re.as_ref(), redact),
                        })
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            hits.push(SessionHits {
                backend,
                session_id,
                meta: serde_json::from_str(&meta).unwrap_or_default(),
                match_count: n as usize,
                matches,
            });
        }
        Ok(RawSearch {
            expression,
            sessions_matched: sessions_matched as usize,
            hidden,
            truncated,
            hits,
        })
    }

    /// One session's indexed records as (index, type, text), in order. What search can
    /// find for that session, for checking it against the transcript projection.
    pub fn session_records(
        &self,
        backend: Backend,
        id: &str,
    ) -> Result<Vec<(usize, String, String)>> {
        let sid: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM sessions WHERE backend = ?1 AND session_id = ?2",
                params![backend.as_str(), id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(sid) = sid else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            "SELECT rowid & 4294967295, kind, text FROM records
             WHERE rowid BETWEEN ?1 AND ?2 ORDER BY rowid",
        )?;
        let rows = stmt.query_map(params![sid << 32, (sid << 32) | 0xffff_ffff], |r| {
            Ok((r.get::<_, i64>(0)? as usize, r.get(1)?, r.get(2)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Sessions the index has metadata for but no text, by origin.
    pub fn text_excluded(&self) -> Result<HashMap<String, usize>> {
        let mut stmt = self.conn.prepare(
            "SELECT coalesce(origin, ''), count(*) FROM sessions WHERE text_indexed = 0 GROUP BY 1",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.flatten().map(|(o, n)| (o, n as usize)).collect())
    }
}

/// A pattern that finds a terms or FTS5 query's words and phrases in a record, for
/// highlighting. Operators, column names, and grouping are dropped; `word*` matches as a
/// prefix. Approximate by design: it only decides where the brackets go.
pub fn fts_highlighter(expression: &str) -> Option<regex::Regex> {
    let mut pieces: Vec<(String, bool)> = Vec::new();
    let mut chars = expression.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            let mut phrase = String::new();
            while let Some(c) = chars.next() {
                if c == '"' {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        phrase.push('"');
                        continue;
                    }
                    break;
                }
                phrase.push(c);
            }
            let prefix = chars.peek() == Some(&'*');
            pieces.push((phrase, prefix));
        } else if !c.is_whitespace() && !"(),^+-".contains(c) {
            let mut word = String::from(c);
            while let Some(&n) = chars.peek() {
                if n.is_whitespace() || "()\",".contains(n) {
                    break;
                }
                word.push(n);
                chars.next();
            }
            let word = word.rsplit(':').next().unwrap_or("").to_string();
            if matches!(word.as_str(), "AND" | "OR" | "NOT") || word.starts_with("NEAR") {
                continue;
            }
            let prefix = word.ends_with('*');
            pieces.push((word.trim_end_matches('*').to_string(), prefix));
        }
    }
    let alternatives: Vec<String> = pieces
        .iter()
        .filter_map(|(text, prefix)| {
            let words: Vec<String> = text
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .map(regex::escape)
                .collect();
            if words.is_empty() {
                return None;
            }
            let body = words.join(r"[^\p{L}\p{N}]+");
            Some(if *prefix {
                format!(r"\b{body}[\p{{L}}\p{{N}}]*")
            } else {
                format!(r"\b{body}\b")
            })
        })
        .collect();
    if alternatives.is_empty() {
        return None;
    }
    regex::RegexBuilder::new(&alternatives.join("|"))
        .case_insensitive(true)
        .build()
        .ok()
}

/// About 90 characters either side of the first match, with every match in that window
/// bracketed and whitespace collapsed. With `redact`, the whole text is redacted first, so
/// neither a bracket inside a key nor the window's edge can keep a key from being caught.
/// A match that lay inside a secret is gone from the redacted text, and the excerpt shows
/// the start of the record instead.
pub fn excerpt(text: &str, re: Option<&regex::Regex>, redact: bool) -> String {
    let text: std::borrow::Cow<str> = if redact {
        crate::redact::redact_str(text)
    } else {
        std::borrow::Cow::Borrowed(text)
    };
    let collapse = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(first) = re.and_then(|re| re.find(&text)) else {
        let head: String = text.chars().take(180).collect();
        let more = head.len() < text.len();
        return format!("{}{}", collapse(&head), if more { "..." } else { "" });
    };
    let start = text[..first.start()]
        .char_indices()
        .rev()
        .nth(89)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end = text[first.end()..]
        .char_indices()
        .nth(90)
        .map(|(i, _)| first.end() + i)
        .unwrap_or(text.len());
    let window = &text[start..end];
    let mut body = String::with_capacity(window.len() + 8);
    let mut last = 0;
    for m in re.into_iter().flat_map(|re| re.find_iter(window)) {
        body.push_str(&window[last..m.start()]);
        body.push('[');
        body.push_str(m.as_str());
        body.push(']');
        last = m.end();
    }
    body.push_str(&window[last..]);
    format!(
        "{}{}{}",
        if start > 0 { "..." } else { "" },
        collapse(&body),
        if end < text.len() { "..." } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_term_is_quoted_so_punctuation_is_not_an_operator() {
        assert_eq!(
            terms_to_fts(r#"idempotency_key "exact phrase" NOT-an-op"#).as_deref(),
            Some(r#""idempotency_key" "exact phrase" "NOT-an-op""#)
        );
        assert_eq!(terms_to_fts(r#"say "hi"#).as_deref(), Some(r#""say" "hi""#));
        assert_eq!(terms_to_fts(r#"a"b"#).as_deref(), Some(r#""a""b""#));
        assert_eq!(terms_to_fts("   "), None);
    }

    #[test]
    fn an_excerpt_brackets_every_match_in_its_window() {
        let re = regex::Regex::new("needle").unwrap();
        assert_eq!(
            excerpt("a needle, another needle", Some(&re), false),
            "a [needle], another [needle]"
        );
    }

    #[test]
    fn a_match_inside_a_key_cannot_expose_it() {
        // The #22 reproduction: a terms match on the key's prefix and a regex match on its
        // body both used to return the whole key with brackets inside it.
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";
        let text = format!("fixture key {key} here");
        let terms = fts_highlighter("\"api03\"").unwrap();
        let body = regex::RegexBuilder::new("abc")
            .case_insensitive(true)
            .build()
            .unwrap();
        for re in [&terms, &body] {
            let e = excerpt(&text, Some(re), true);
            assert!(!e.contains("sk-ant-api03"), "{e}");
            assert!(!e.contains("defghijklmnop"), "{e}");
            assert!(e.contains("[redacted:anthropic]"), "{e}");
        }
        // --no-redact still highlights inside the raw text.
        assert!(excerpt(&text, Some(&terms), false).contains("sk-ant-[api03]-"));
        assert!(excerpt(&text, Some(&body), false).contains("api03-[abc]def"));
    }

    #[test]
    fn a_key_at_the_window_edge_is_redacted_before_the_window_is_cut() {
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";
        // The match sits 90 characters before the key's middle, so the window ends inside it.
        let text = format!("needle {} {key}", "x".repeat(70));
        let re = regex::Regex::new("needle").unwrap();
        let e = excerpt(&text, Some(&re), true);
        assert!(!e.contains("sk-ant-api03"), "{e}");
    }

    #[test]
    fn the_highlighter_drops_operators_and_keeps_phrases_and_prefixes() {
        let re = fts_highlighter(r#""exact phrase" OR deploy* NOT text:skip"#).unwrap();
        let hits: Vec<&str> = re
            .find_iter("an Exact  Phrase, deploying, skip, OR not")
            .map(|m| m.as_str())
            .collect();
        assert_eq!(hits, vec!["Exact  Phrase", "deploying", "skip"]);
    }
}
