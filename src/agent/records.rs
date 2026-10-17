//! Run records on disk: `state/runs/<id>/request.json` and `result.json`, and one line per run
//! state in `state/runs/index.jsonl`, with the directory the run works for (`cwd`; lines
//! written before it was added have none). Everything written is redacted first.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use super::{Access, Record, Request};
use crate::error::Error;
use crate::layers::create_private_dir;
use crate::redact::redact_value;

/// Prompt characters kept in the index.
const SNIPPET: usize = 120;

/// Whether a process with this pid exists.
pub fn alive(pid: u64) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: kill with signal 0 sends nothing; it only checks that the process exists.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// What became of an interrupted run's provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// It was still running, and was stopped.
    Stopped,
    /// It had already exited.
    Gone,
    /// Something runs under its pid, but it may not be the provider, so it was left alone.
    Unverified,
    /// The record names no provider process.
    Unknown,
}

/// Stops an orphaned provider's process group, after checking that the pid still leads that
/// group and runs claude or codex: after a crash the pid may belong to something else.
fn stop_provider(pid: u32, pgid: u32) -> Stop {
    if !alive(u64::from(pid)) {
        return Stop::Gone;
    }
    #[cfg(unix)]
    {
        // SAFETY: getpgid only reads the process table.
        let leads = unsafe { libc::getpgid(pid as i32) } == pgid as i32 && pid == pgid;
        let command = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if !(leads && (command.contains("claude") || command.contains("codex"))) {
            return Stop::Unverified;
        }
        // SAFETY: signals a process group that was just verified as the provider's.
        unsafe { libc::killpg(pgid as i32, libc::SIGTERM) };
        for _ in 0..40 {
            if !alive(u64::from(pid)) {
                return Stop::Stopped;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // SAFETY: as above; it did not stop on TERM.
        unsafe { libc::killpg(pgid as i32, libc::SIGKILL) };
        Stop::Stopped
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        Stop::Unverified
    }
}

/// The run records under a state directory.
pub struct Store {
    dir: PathBuf,
}

fn io(action: &str, path: &Path, e: std::io::Error) -> Error {
    Error::new("io", format!("cannot {action} {}: {e}", path.display()))
}

fn denied(state: &Path, action: &str, path: &Path, e: std::io::Error) -> Error {
    crate::dirs::write_error(crate::dirs::Root::State, state, action, path, e)
}

fn write_private(state: &Path, path: &Path, text: &str, append: bool) -> Result<(), Error> {
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
    let mut file = options
        .open(path)
        .map_err(|e| denied(state, "write", path, e))?;
    file.write_all(text.as_bytes())
        .map_err(|e| denied(state, "write", path, e))
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

/// The directory a run works for, for its index line: the one its worktree was made from,
/// else its request's cwd made absolute, else this process's current directory.
fn run_cwd(request: &Request, worktree: Option<&Json>) -> Json {
    match worktree.map(|w| &w["from"]) {
        Some(from @ Json::String(_)) => from.clone(),
        _ => json!(match &request.cwd {
            Some(cwd) => std::path::absolute(cwd).ok(),
            None => std::env::current_dir().ok(),
        }),
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

    /// The state directory the records are under.
    fn state(&self) -> &Path {
        self.dir.parent().unwrap_or(&self.dir)
    }

    /// Records that a run is starting: its request, a `running` result, and an index line with
    /// this process's pid, so a crash leaves evidence that [`Store::reconcile`] can settle.
    /// Returns the index line, for [`Store::spawned`].
    pub fn begin(
        &self,
        request: &Request,
        id: &str,
        started_at: &str,
        agent: Option<&str>,
        worktree: Option<&Json>,
    ) -> Result<Json, Error> {
        self.reconcile()?;
        let run_dir = self.dir.join(id);
        create_private_dir(&run_dir).map_err(|e| denied(self.state(), "create", &run_dir, e))?;
        let mut request_json = request.to_json();
        if let Some(agent) = agent {
            request_json["agent"] = json!(agent);
        }
        redact_value(&mut request_json);
        write_private(
            self.state(),
            &run_dir.join("request.json"),
            &serde_json::to_string_pretty(&request_json).unwrap_or_default(),
            false,
        )?;
        let mut running = json!({
            "id": id,
            "started_at": started_at,
            "status": "running",
            "provider": request.provider.name(),
            "model": request.model,
            "access": request.access.name(),
            "pid": std::process::id(),
            "worktree": worktree,
        });
        redact_value(&mut running);
        write_private(
            self.state(),
            &run_dir.join("result.json"),
            &serde_json::to_string_pretty(&running).unwrap_or_default(),
            false,
        )?;
        let mut line = json!({
            "id": id,
            "started_at": started_at,
            "provider": request.provider.name(),
            "model": request.model,
            "agent": agent,
            "label": request.label,
            "phase": request.phase,
            "parent": request.parent,
            "key": request.key,
            "status": "running",
            "access": request.access.name(),
            "pid": std::process::id(),
            "cwd": run_cwd(request, worktree),
            "prompt": snippet(&request.prompt),
        });
        redact_value(&mut line);
        write_private(
            self.state(),
            &self.dir.join("index.jsonl"),
            &format!("{line}\n"),
            true,
        )?;
        Ok(line)
    }

    /// Adds the provider's pid and process group to a running run, so the run can be stopped
    /// if this timon dies before it.
    pub fn spawned(&self, line: &Json, pid: u32, pgid: Option<u32>) -> Result<(), Error> {
        let mut line = line.clone();
        line["provider_pid"] = json!(pid);
        line["provider_pgid"] = json!(pgid);
        write_private(
            self.state(),
            &self.dir.join("index.jsonl"),
            &format!("{line}\n"),
            true,
        )?;
        let id = line["id"].as_str().unwrap_or_default();
        let path = self.dir.join(id).join("result.json");
        if let Some(mut result) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Json>(&t).ok())
        {
            result["provider_pid"] = json!(pid);
            result["provider_pgid"] = json!(pgid);
            write_private(
                self.state(),
                &path,
                &serde_json::to_string_pretty(&result).unwrap_or_default(),
                false,
            )?;
        }
        Ok(())
    }

    /// Settles runs left `running` by a timon process that no longer exists: they become
    /// `interrupted`, with `effects_uncertain` unless the run was read-only. A provider still
    /// running for one is stopped when it can be identified (`provider_stopped`), and flagged
    /// otherwise (`provider_may_be_running`). Nothing is replayed. Returns how many were
    /// settled.
    pub fn reconcile(&self) -> Result<usize, Error> {
        let mut settled = 0;
        for entry in self.latest()? {
            if entry["status"] != "running" {
                continue;
            }
            let Some(pid) = entry["pid"].as_u64() else {
                continue;
            };
            if pid == u64::from(std::process::id()) || alive(pid) {
                continue;
            }
            let id = entry["id"].as_str().unwrap_or_default().to_owned();
            let uncertain = entry["access"].as_str() != Some(Access::Read.name());
            let result_path = self.dir.join(&id).join("result.json");
            let mut result: Json = std::fs::read_to_string(&result_path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_else(|| json!({ "id": id }));
            result["status"] = json!("interrupted");
            result["effects_uncertain"] = json!(uncertain);
            let provider = match (
                entry["provider_pid"].as_u64(),
                entry["provider_pgid"].as_u64(),
            ) {
                (Some(pid), Some(pgid)) => stop_provider(pid as u32, pgid as u32),
                _ => Stop::Unknown,
            };
            match provider {
                Stop::Stopped => result["provider_stopped"] = json!(true),
                Stop::Unverified => result["provider_may_be_running"] = json!(true),
                Stop::Gone | Stop::Unknown => {}
            }
            result["error"] = json!({
                "kind": "interrupted",
                "message": "the timon process running this ended before the run finished",
                "hint": if uncertain {
                    "the agent could write; check its working directory or worktree"
                } else {
                    "it was read-only, so nothing was changed"
                },
            });
            write_private(
                self.state(),
                &result_path,
                &serde_json::to_string_pretty(&result).unwrap_or_default(),
                false,
            )?;
            let mut line = entry.clone();
            line["status"] = json!("interrupted");
            line["effects_uncertain"] = json!(uncertain);
            match provider {
                Stop::Stopped => line["provider_stopped"] = json!(true),
                Stop::Unverified => line["provider_may_be_running"] = json!(true),
                Stop::Gone | Stop::Unknown => {}
            }
            write_private(
                self.state(),
                &self.dir.join("index.jsonl"),
                &format!("{line}\n"),
                true,
            )?;
            settled += 1;
        }
        Ok(settled)
    }

    /// The last index line of every run, in the order the runs started.
    fn latest(&self) -> Result<Vec<Json>, Error> {
        let path = self.dir.join("index.jsonl");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io("read", &path, e)),
        };
        let mut order: Vec<String> = Vec::new();
        let mut last: std::collections::HashMap<String, Json> = std::collections::HashMap::new();
        for entry in text
            .lines()
            .filter_map(|l| serde_json::from_str::<Json>(l).ok())
        {
            let Some(id) = entry["id"].as_str().map(str::to_owned) else {
                continue;
            };
            if !last.contains_key(&id) {
                order.push(id.clone());
            }
            last.insert(id, entry);
        }
        Ok(order
            .into_iter()
            .filter_map(|id| last.remove(&id))
            .collect())
    }

    /// Writes a run's request and result, and adds it to the index.
    pub fn save(
        &self,
        request: &Request,
        record: &Record,
        agent: Option<&str>,
    ) -> Result<(), Error> {
        let run_dir = self.dir.join(&record.id);
        create_private_dir(&run_dir).map_err(|e| denied(self.state(), "create", &run_dir, e))?;
        let mut request_json = request.to_json();
        if let Some(agent) = agent {
            request_json["agent"] = json!(agent);
        }
        redact_value(&mut request_json);
        let mut result_json = record.to_json();
        redact_value(&mut result_json);
        write_private(
            self.state(),
            &run_dir.join("request.json"),
            &serde_json::to_string_pretty(&request_json).unwrap_or_default(),
            false,
        )?;
        write_private(
            self.state(),
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
            "label": request.label,
            "phase": request.phase,
            "parent": request.parent,
            "key": request.key,
            "contract": request.contract,
            "access": request.access.name(),
            "status": record.status.name(),
            "session": record.session,
            "ms": record.ms,
            "usd": record.usage.usd,
            "output_tokens": record.usage.output_tokens,
            "cwd": run_cwd(request, record.worktree.as_ref()),
            "prompt": snippet(&request.prompt),
        });
        redact_value(&mut line);
        write_private(
            self.state(),
            &self.dir.join("index.jsonl"),
            &format!("{line}\n"),
            true,
        )
    }

    /// Index entries, newest first, started at or after `since`, at most `limit`; each run's
    /// latest state, after settling runs whose process is gone.
    pub fn list(&self, since: Option<DateTime<Utc>>, limit: usize) -> Result<Vec<Json>, Error> {
        self.list_in(since, None, limit)
    }

    /// [`Store::list`], only the runs of script run `parent` when one is given.
    pub fn list_in(
        &self,
        since: Option<DateTime<Utc>>,
        parent: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Json>, Error> {
        self.reconcile()?;
        let mut entries: Vec<Json> = self
            .latest()?
            .into_iter()
            .filter(|entry| parent.is_none_or(|p| entry["parent"].as_str() == Some(p)))
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

    /// The provider session ids of every recorded run, so sessions timon started can be told
    /// apart from a person's.
    pub fn sessions(&self) -> std::collections::HashSet<String> {
        self.latest()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| e["session"].as_str().map(str::to_owned))
            .collect()
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
