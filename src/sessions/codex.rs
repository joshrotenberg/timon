//! Shared interpretation of Codex fork headers and rollout boundaries.
//!
//! Ported from trovador's `src/codex.rs` (bottega d89c451); reads through
//! `archive::open_buffered` so compressed rollouts work.

use std::io::BufRead;

use serde::Deserialize;

use super::Backend;
use super::archive::ArchiveEntry;

#[derive(Default, Deserialize)]
struct Header {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    payload: HeaderPayload,
}

#[derive(Default, Deserialize)]
struct HeaderPayload {
    forked_from_id: Option<String>,
    subagent_history_start_ordinal: Option<u64>,
}

impl Header {
    fn forked(&self) -> bool {
        self.kind == "session_meta"
            && self.payload.forked_from_id.is_some()
            && self.payload.subagent_history_start_ordinal.is_some()
    }
}

#[allow(dead_code)]
pub(crate) fn is_forked_child(e: &ArchiveEntry) -> bool {
    if e.backend != Backend::Codex {
        return false;
    }
    let Ok(mut reader) = super::archive::open_buffered(&e.file) else {
        return false;
    };
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).is_ok()
        && serde_json::from_slice::<Header>(&line).is_ok_and(|h| h.forked())
}

pub(crate) struct ForkBoundary {
    pub end: Option<u64>,
    pub scanned: u64,
    // Issue #35: a copied parent's terminal event cannot prove the child finished,
    // so its readable fallback must never become an index checkpoint.
    pub display_fallback: bool,
}

// Issue #35: rollout ordinals count all records from zero, including the child's
// session_meta and copied parent session_meta, not just response items. In child
// 01a11d57-d878-7570-a3b2-e98a31cac9e8, boundary 41 is line 42 (settings);
// its assignment is ordinal 47, line 48, after six setup records. Child
// 01a101d4-ff63-7e50-b725-07fd9cbaf6b3 has boundary 7 at line 8 and its
// assignment at ordinal 13, line 14. Explicit ordinals are authoritative. For
// older files without them, use zero-based physical lines, including metadata,
// blank and malformed lines, so metadata and projection cannot shift differently.
fn ordinal(line: &[u8], physical: u64) -> u64 {
    let key = br#""ordinal""#;
    let Some(pos) = line.windows(key.len()).position(|w| w == key) else {
        return physical;
    };
    // Codex puts ordinal before type/payload. Reading that numeric header avoids
    // parsing copied messages and potentially megabytes of compacted history.
    let prefix = &line[..pos];
    if !prefix.windows(6).any(|w| w == br#""type""#)
        && prefix.iter().filter(|b| **b == b'{').count() == 1
        && !prefix.contains(&b'[')
    {
        let tail = &line[pos + key.len()..];
        if let Some(colon) = tail.iter().position(|b| !b.is_ascii_whitespace())
            && tail[colon] == b':'
        {
            let digits = &tail[colon + 1..];
            let begin = digits
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .unwrap_or(digits.len());
            let end = begin
                + digits[begin..]
                    .iter()
                    .take_while(|b| b.is_ascii_digit())
                    .count();
            if let Ok(text) = std::str::from_utf8(&digits[begin..end])
                && let Ok(value) = text.parse::<u64>()
            {
                return value;
            }
        }
    }
    #[derive(Deserialize)]
    struct Ordinal {
        ordinal: Option<u64>,
    }
    serde_json::from_slice::<Ordinal>(line)
        .ok()
        .and_then(|r| r.ordinal)
        .unwrap_or(physical)
}

pub(crate) fn fork_boundary(
    reader: &mut impl BufRead,
    archived: bool,
    complete: bool,
) -> std::io::Result<ForkBoundary> {
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line)?;
    // Issue #35: a header still being written cannot establish a permanent
    // non-fork checkpoint; the next pass must read its completed metadata.
    if line.is_empty() || !line.ends_with(b"\n") {
        return Ok(ForkBoundary {
            end: None,
            scanned: 0,
            display_fallback: false,
        });
    }
    let header = serde_json::from_slice::<Header>(&line).unwrap_or_default();
    let Some(start) = header
        .payload
        .subagent_history_start_ordinal
        .filter(|_| header.forked())
    else {
        return Ok(ForkBoundary {
            end: Some(0),
            scanned: line.len() as u64,
            display_fallback: false,
        });
    };
    let mut scanned = line.len() as u64;
    let mut physical = 1;
    let mut last = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            break;
        }
        if ordinal(&line, physical) >= start {
            return Ok(ForkBoundary {
                end: Some(scanned),
                scanned,
                display_fallback: false,
            });
        }
        scanned += line.len() as u64;
        physical += 1;
        if !line.iter().all(|b| b.is_ascii_whitespace()) {
            std::mem::swap(&mut last, &mut line);
        }
    }
    #[derive(Deserialize)]
    struct Tail {
        #[serde(rename = "type")]
        kind: String,
        payload: TailPayload,
    }
    #[derive(Deserialize)]
    struct TailPayload {
        #[serde(rename = "type")]
        kind: Option<String>,
    }
    let terminal_tail = serde_json::from_slice::<Tail>(&last).is_ok_and(|r| {
        r.kind == "event_msg"
            && matches!(
                r.payload.kind.as_deref(),
                Some("task_complete" | "turn_aborted")
            )
    });
    // Issue #35: an impossible boundary must not erase a finished transcript.
    // Otherwise stop at the scanned prefix: a writer may append the child before
    // this pass ends, and checkpointing those unseen bytes would lose its turns.
    let display_fallback = complete && !archived && terminal_tail;
    Ok(ForkBoundary {
        end: (complete && (archived || terminal_tail)).then_some(0),
        scanned,
        display_fallback,
    })
}
