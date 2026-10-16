//! Run records on disk: `state/runs/<id>/request.json` and `result.json`, and one line per run
//! in `state/runs/index.jsonl`. Everything written is redacted first.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use super::{Record, Request};
use crate::error::Error;
use crate::layers::create_private_dir;
use crate::redact::redact_value;

/// Prompt characters kept in the index.
const SNIPPET: usize = 120;

/// The run records under a state directory.
pub struct Store {
    dir: PathBuf,
}

fn io(action: &str, path: &Path, e: std::io::Error) -> Error {
    Error::new("io", format!("cannot {action} {}: {e}", path.display()))
}

fn write_private(path: &Path, text: &str, append: bool) -> Result<(), Error> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true);
    if append {
        options.append(true);
    } else {
        options.truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| io("write", path, e))?;
    file.write_all(text.as_bytes())
        .map_err(|e| io("write", path, e))
}

fn snippet(prompt: &str) -> String {
    let flat: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SNIPPET {
        flat
    } else {
        let cut: String = flat.chars().take(SNIPPET).collect();
        format!("{cut}...")
    }
}

impl Store {
    pub fn new(state: &Path) -> Self {
        Self {
            dir: state.join("runs"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes a run's request and result, and adds it to the index.
    pub fn save(
        &self,
        request: &Request,
        record: &Record,
        agent: Option<&str>,
    ) -> Result<(), Error> {
        let run_dir = self.dir.join(&record.id);
        create_private_dir(&run_dir).map_err(|e| io("create", &run_dir, e))?;
        let mut request_json = request.to_json();
        if let Some(agent) = agent {
            request_json["agent"] = json!(agent);
        }
        redact_value(&mut request_json);
        let mut result_json = record.to_json();
        redact_value(&mut result_json);
        write_private(
            &run_dir.join("request.json"),
            &serde_json::to_string_pretty(&request_json).unwrap_or_default(),
            false,
        )?;
        write_private(
            &run_dir.join("result.json"),
            &serde_json::to_string_pretty(&result_json).unwrap_or_default(),
            false,
        )?;
        let mut line = json!({
            "id": record.id,
            "started_at": record.started_at,
            "provider": record.provider.name(),
            "model": record.model,
            "agent": agent,
            "status": record.status.name(),
            "ms": record.ms,
            "usd": record.usage.usd,
            "output_tokens": record.usage.output_tokens,
            "prompt": snippet(&request.prompt),
        });
        redact_value(&mut line);
        write_private(&self.dir.join("index.jsonl"), &format!("{line}\n"), true)
    }

    /// Index entries, newest first, started at or after `since`, at most `limit`.
    pub fn list(&self, since: Option<DateTime<Utc>>, limit: usize) -> Result<Vec<Json>, Error> {
        let path = self.dir.join("index.jsonl");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io("read", &path, e)),
        };
        let mut entries: Vec<Json> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Json>(l).ok())
            .filter(|entry| match since {
                None => true,
                Some(since) => entry["started_at"]
                    .as_str()
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .is_some_and(|t| t.with_timezone(&Utc) >= since),
            })
            .collect();
        entries.reverse();
        entries.truncate(limit);
        Ok(entries)
    }

    /// One run's request and result. `id` may be an unambiguous prefix.
    pub fn show(&self, id: &str) -> Result<Json, Error> {
        let ids: Vec<String> = std::fs::read_dir(&self.dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.path().is_dir())
                    .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                    .filter(|name| name.starts_with(id))
                    .collect()
            })
            .unwrap_or_default();
        let found = match ids.as_slice() {
            [one] => one.clone(),
            [] => {
                return Err(Error::new("not_found", format!("no run \"{id}\""))
                    .with_hint("timon runs lists them"));
            }
            many => {
                return Err(Error::new(
                    "ambiguous",
                    format!("\"{id}\" matches {} runs", many.len()),
                )
                .with_hint("give more of the id"));
            }
        };
        let read = |name: &str| -> Result<Json, Error> {
            let path = self.dir.join(&found).join(name);
            let text = std::fs::read_to_string(&path).map_err(|e| io("read", &path, e))?;
            serde_json::from_str(&text)
                .map_err(|e| Error::new("bad_record", format!("{}: {e}", path.display())))
        };
        Ok(json!({ "request": read("request.json")?, "result": read("result.json")? }))
    }
}
