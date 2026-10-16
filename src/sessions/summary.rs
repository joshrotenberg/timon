//! What a summary is made from, and its cache. The model calls happen in the `sessions`
//! Lua module, through `agent.run`, so budgets, concurrency and run records apply.
//!
//! Prompts and chunking are taken from trovador's `src/summary.rs` (bottega d89c451).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::archive::ArchiveEntry;
use super::transcript::{self, Projection, RecordKind};
use super::{Backend, Limits};

/// Bumped when the prompts change, so old summaries are not served as current.
pub const PROMPT_VERSION: u32 = 1;
/// Characters of transcript per summarized chunk.
pub const CHUNK_CHARS: usize = 250_000;
/// Most chunks summarized; past it, the first and last halves are kept.
pub const MAX_CHUNKS: usize = 8;
/// The model used when none is given.
pub const DEFAULT_MODEL: &str = "haiku";

const FINAL_SHAPE: &str = "Cover: Goal; What happened; Key decisions; Current state; Open items. Under 300 words, no preamble.";

/// A cached summary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cached {
    pub backend: Backend,
    pub session_id: String,
    pub text: String,
    pub model: String,
    pub usd: f64,
    pub chunks: usize,
    pub chunks_summarized: usize,
    pub source_size: u64,
    pub source_mtime_ns: i64,
    pub prompt_version: u32,
    pub created: String,
}

/// The transcript as text, without omitted records or reasoning, redacted before any of it
/// is sent to a model.
pub fn render(e: &ArchiveEntry, limits: &Limits) -> std::io::Result<String> {
    let mut out = String::new();
    for r in Projection::open(e, limits)? {
        if matches!(r.kind, RecordKind::Omitted | RecordKind::Reasoning) {
            continue;
        }
        out.push_str(&transcript::render(&r));
        out.push('\n');
    }
    Ok(crate::redact::redact_str(&out).into_owned())
}

/// Cuts text into chunks of at most `size` bytes on character boundaries.
pub fn chunk(text: &str, size: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(size);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            end = rest.chars().next().map_or(rest.len(), char::len_utf8);
        }
        chunks.push(&rest[..end]);
        rest = &rest[end..];
    }
    chunks
}

/// Which chunks to summarize: all of them, or past `max` the first and last halves.
pub fn pick(n: usize, max: usize) -> Vec<usize> {
    if n <= max {
        return (0..n).collect();
    }
    let head = max.div_ceil(2);
    let tail = max - head;
    (0..head).chain(n - tail..n).collect()
}

pub fn whole_prompt(body: &str) -> String {
    format!("Summarize this agent session transcript. {FINAL_SHAPE}\n\n---\n{body}")
}

pub fn part_prompt(i: usize, of: usize, body: &str) -> String {
    format!(
        "This is part {} of {of} of one agent session transcript, in order. Summarize THIS PART in at most 10 tight bullets: goals, actions, decisions, outcomes, open threads. No preamble.\n\n---\n{body}",
        i + 1
    )
}

pub fn merge_prompt(partials: &[(usize, String)], chunks: usize) -> String {
    let total = partials.len();
    let gap = if chunks > total {
        format!(
            " Parts {} to {} of the original were left out.",
            total.div_ceil(2) + 1,
            chunks - total / 2
        )
    } else {
        String::new()
    };
    let merged: Vec<String> = partials
        .iter()
        .map(|(i, p)| format!("PART {}:\n{p}", i + 1))
        .collect();
    format!(
        "These are sequential partial summaries of one agent session.{gap} Merge them into a single summary. {FINAL_SHAPE}\n\n---\n{}",
        merged.join("\n\n")
    )
}

fn cache_file(dir: &Path, e: &ArchiveEntry) -> PathBuf {
    dir.join(format!("{}-{}.json", e.backend.as_str(), e.session_id))
}

fn mtime_ns(e: &ArchiveEntry) -> i64 {
    super::index::mtime_ns(e.mtime)
}

/// The cached summary for `e`, if it was made from this exact file with the current prompts.
pub fn cached(dir: &Path, e: &ArchiveEntry) -> Option<Cached> {
    let text = std::fs::read_to_string(cache_file(dir, e)).ok()?;
    let hit: Cached = serde_json::from_str(&text).ok()?;
    (hit.source_size == e.size_bytes
        && hit.source_mtime_ns == mtime_ns(e)
        && hit.prompt_version == PROMPT_VERSION)
        .then_some(hit)
}

/// Stores a summary, readable only by the user.
#[allow(clippy::too_many_arguments)]
pub fn store(
    dir: &Path,
    e: &ArchiveEntry,
    text: &str,
    model: &str,
    usd: f64,
    chunks: usize,
    chunks_summarized: usize,
) -> std::io::Result<Cached> {
    let entry = Cached {
        backend: e.backend,
        session_id: e.session_id.clone(),
        text: text.to_owned(),
        model: model.to_owned(),
        usd,
        chunks,
        chunks_summarized,
        source_size: e.size_bytes,
        source_mtime_ns: mtime_ns(e),
        prompt_version: PROMPT_VERSION,
        created: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    };
    super::private::private_dir(dir)?;
    super::private::write_private(
        &cache_file(dir, e),
        serde_json::to_string_pretty(&entry)?.as_bytes(),
    )?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_split_on_character_boundaries_and_picks_keep_both_ends() {
        assert_eq!(chunk("abcdef", 4), ["abcd", "ef"]);
        assert_eq!(chunk("éé", 1), ["é", "é"]);
        assert_eq!(pick(3, 8), [0, 1, 2]);
        assert_eq!(pick(10, 4), [0, 1, 8, 9]);
    }
}
