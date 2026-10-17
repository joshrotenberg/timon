//! Script runs as records. `timon NAME`, `timon FILE` and task runs write `state/scripts/<id>/`:
//!
//! ```text
//! request.json   the script, its arguments, its source's sha256, the trigger, metadata
//! result.json    status and totals; for a task run, each task's status and time
//! runs.jsonl     { id, key, status, usd, cached } per agent run
//! ```
//!
//! and a line per change in `state/scripts/index.jsonl`, with the directory it ran in (`cwd`;
//! lines written before it was added have none). Each agent run in the script points
//! back with `parent`. A script run can be resumed: agent runs whose request key matches a
//! reusable run of the earlier script run return that run's record instead of running again.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value as Json, json};

use crate::error::Error;
use crate::layers::create_private_dir;
use crate::redact::redact_value;

/// What the totals of a script run count.
#[derive(Default)]
struct Totals {
    runs: u64,
    cached: u64,
    usd: f64,
    input_tokens: u64,
    output_tokens: u64,
    ids: Vec<String>,
}

/// A script run in progress.
pub struct ScriptRun {
    pub id: String,
    /// `state/scripts`.
    dir: PathBuf,
    /// `state/runs`, where its agent runs are.
    runs_dir: PathBuf,
    started: Instant,
    started_at: String,
    request: RefCell<Json>,
    totals: RefCell<Totals>,
    /// How many runs with each request hash have started, for keys.
    seen: RefCell<HashMap<String, u32>>,
    /// Reusable runs of the script run being resumed, by key.
    cache: HashMap<String, String>,
    /// `{ name, status, ms }` for each task of a task run, in the order they ran.
    tasks: RefCell<Vec<Json>>,
    done: Cell<bool>,
}

fn denied(state: &Path, action: &str, path: &Path, e: std::io::Error) -> Error {
    crate::dirs::write_error(crate::dirs::Root::State, state, action, path, e)
}

fn write_private(state: &Path, path: &Path, text: &str, append: bool) -> Result<(), Error> {
    use std::io::Write;
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

fn read_json(path: &Path) -> Option<Json> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

fn pretty(value: &Json) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// A new script run id: `s_` and a sortable timestamp-based suffix.
pub fn new_id() -> String {
    format!("s_{}", &crate::agent::new_id()[2..])
}

/// The sha256 of a script's source, as hex.
pub fn source_hash(source: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(source.as_bytes()))
}

impl ScriptRun {
    /// Records a script run that is starting. With `resume`, the reusable runs of that earlier
    /// script run (an id or an unambiguous prefix) become the cache.
    pub fn begin(state: &Path, mut request: Json, resume: Option<&str>) -> Result<Self, Error> {
        let dir = state.join("scripts");
        create_private_dir(&dir).map_err(|e| denied(state, "create", &dir, e))?;
        reconcile(&dir);
        let runs_dir = state.join("runs");
        let mut cache = HashMap::new();
        if let Some(earlier) = resume {
            let earlier = resolve(&dir, earlier)?;
            cache = reusable_runs(&dir.join(&earlier), &runs_dir);
            request["trigger"] = json!("resume");
            request["resumed_from"] = json!(earlier);
        }
        let id = new_id();
        let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        request["id"] = json!(id);
        redact_value(&mut request);
        let run = Self {
            id,
            dir,
            runs_dir,
            started: Instant::now(),
            started_at,
            request: RefCell::new(request),
            totals: RefCell::default(),
            seen: RefCell::default(),
            cache,
            tasks: RefCell::default(),
            done: Cell::new(false),
        };
        let own = run.dir.join(&run.id);
        create_private_dir(&own).map_err(|e| denied(state, "create", &own, e))?;
        write_private(
            state,
            &own.join("request.json"),
            &pretty(&run.request.borrow()),
            false,
        )?;
        write_private(state, &own.join("runs.jsonl"), "", false)?;
        run.write_result("running", None)?;
        Ok(run)
    }

    /// Adds the metadata a script returned (`description`, `phases`, `budget`) to its request.
    pub fn describe(&self, metadata: Json) -> Result<(), Error> {
        self.request.borrow_mut()["metadata"] = metadata;
        write_private(
            self.state(),
            &self.dir.join(&self.id).join("request.json"),
            &pretty(&self.request.borrow()),
            false,
        )
    }

    /// The key of the next run with this request hash: the hash and how many runs with the
    /// same hash started before it in this script run.
    pub fn key(&self, hash: &str) -> String {
        let mut seen = self.seen.borrow_mut();
        let n = seen.entry(hash.to_owned()).or_insert(0);
        let key = format!("{hash}:{n}");
        *n += 1;
        key
    }

    /// The earlier run a resumed run can reuse, by key.
    pub fn cached(&self, key: &str) -> Option<&str> {
        self.cache.get(key).map(String::as_str)
    }

    pub fn runs_dir(&self) -> &Path {
        &self.runs_dir
    }

    /// The state directory the script run is under.
    fn state(&self) -> &Path {
        self.dir.parent().unwrap_or(&self.dir)
    }

    /// Records an agent run of this script: its line in `runs.jsonl`, and the totals.
    pub fn add(&self, run: &Json) {
        {
            let mut t = self.totals.borrow_mut();
            t.runs += 1;
            if run["cached"] == true {
                t.cached += 1;
            } else {
                t.usd += run["usd"].as_f64().unwrap_or(0.0);
                t.input_tokens += run["input_tokens"].as_u64().unwrap_or(0);
                t.output_tokens += run["output_tokens"].as_u64().unwrap_or(0);
            }
            t.ids
                .push(run["id"].as_str().unwrap_or_default().to_owned());
        }
        let path = self.dir.join(&self.id).join("runs.jsonl");
        if let Err(err) = write_private(self.state(), &path, &format!("{run}\n"), true) {
            eprintln!("timon: the script run's runs were not recorded: {err}");
        }
    }

    /// Records how one task of a task run ended.
    pub fn task(&self, name: &str, status: &str, ms: u64) {
        self.tasks
            .borrow_mut()
            .push(json!({ "name": name, "status": status, "ms": ms }));
    }

    fn write_result(&self, status: &str, error: Option<Json>) -> Result<(), Error> {
        let t = self.totals.borrow();
        let request = self.request.borrow();
        let mut result = json!({
            "id": self.id,
            "status": status,
            "started_at": self.started_at,
            "ms": self.started.elapsed().as_millis() as u64,
            "pid": std::process::id(),
            "runs": t.runs,
            "cached": t.cached,
            "usd": t.usd,
            "input_tokens": t.input_tokens,
            "output_tokens": t.output_tokens,
            "run_ids": t.ids,
            "error": error,
        });
        let tasks = self.tasks.borrow();
        if !tasks.is_empty() {
            result["tasks"] = json!(*tasks);
        }
        redact_value(&mut result);
        write_private(
            self.state(),
            &self.dir.join(&self.id).join("result.json"),
            &pretty(&result),
            false,
        )?;
        let line = json!({
            "id": self.id,
            "started_at": self.started_at,
            "name": request["name"],
            "status": status,
            "pid": std::process::id(),
            "cwd": request["cwd"],
            "runs": t.runs,
            "cached": t.cached,
            "usd": t.usd,
            "ms": result["ms"],
            "resumed_from": request["resumed_from"],
        });
        write_private(
            self.state(),
            &self.dir.join("index.jsonl"),
            &format!("{line}\n"),
            true,
        )
    }

    /// Settles the script run as `ok`, `failed` or `cancelled`.
    pub fn finish(&self, status: &str, error: Option<Json>) {
        if self.done.replace(true) {
            return;
        }
        if let Err(err) = self.write_result(status, error) {
            eprintln!("timon: the script run was not recorded: {err}");
        }
    }
}

impl Drop for ScriptRun {
    /// A script run dropped before it finished was cancelled, as on Ctrl-C.
    fn drop(&mut self) {
        self.finish("cancelled", None);
    }
}

/// The last index line of every script run, in the order they started.
fn latest(dir: &Path) -> Vec<Json> {
    let Ok(text) = std::fs::read_to_string(dir.join("index.jsonl")) else {
        return Vec::new();
    };
    let mut order: Vec<String> = Vec::new();
    let mut last: HashMap<String, Json> = HashMap::new();
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
    order
        .into_iter()
        .filter_map(|id| last.remove(&id))
        .collect()
}

/// Settles script runs left `running` by a timon process that no longer exists.
fn reconcile(dir: &Path) {
    let state = dir.parent().unwrap_or(dir);
    for entry in latest(dir) {
        if entry["status"] != "running" {
            continue;
        }
        let Some(pid) = entry["pid"].as_u64() else {
            continue;
        };
        if pid == u64::from(std::process::id()) || crate::agent::records::alive(pid) {
            continue;
        }
        let id = entry["id"].as_str().unwrap_or_default();
        let path = dir.join(id).join("result.json");
        let mut result = read_json(&path).unwrap_or_else(|| json!({ "id": id }));
        result["status"] = json!("interrupted");
        let _ = write_private(state, &path, &pretty(&result), false);
        let mut line = entry.clone();
        line["status"] = json!("interrupted");
        let _ = write_private(state, &dir.join("index.jsonl"), &format!("{line}\n"), true);
    }
}

/// A script run id from an id or an unambiguous prefix.
fn resolve(dir: &Path, id: &str) -> Result<String, Error> {
    let ids: Vec<String> = latest(dir)
        .into_iter()
        .filter_map(|e| e["id"].as_str().map(str::to_owned))
        .filter(|s| s.starts_with(id))
        .collect();
    match ids.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(Error::new("not_found", format!("no script run \"{id}\""))
            .with_hint("timon runs --scripts lists them")),
        many => Err(Error::new(
            "ambiguous",
            format!("\"{id}\" matches {} script runs", many.len()),
        )
        .with_hint("give more of the id")),
    }
}

/// The runs of an earlier script run that a resume may reuse, by key (decided in #60):
/// `ok` runs that were read-only, marked idempotent, or wrote in a kept worktree that is still
/// at the state the run left.
fn reusable_runs(script_dir: &Path, runs_dir: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(script_dir.join("runs.jsonl")) else {
        return HashMap::new();
    };
    let mut cache = HashMap::new();
    for entry in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Json>(l).ok())
    {
        let (Some(key), Some(id)) = (entry["key"].as_str(), entry["id"].as_str()) else {
            continue;
        };
        if entry["status"] != "ok" {
            continue;
        }
        let run = runs_dir.join(id);
        let (Some(request), Some(result)) = (
            read_json(&run.join("request.json")),
            read_json(&run.join("result.json")),
        ) else {
            continue;
        };
        if result["status"] != "ok" {
            continue;
        }
        let worktree = &result["worktree"];
        let reusable = request["access"] == "read"
            || request["idempotent"] == true
            || (worktree["kept"] == true && worktree_unchanged(worktree));
        if reusable {
            cache.insert(key.to_owned(), id.to_owned());
        }
    }
    cache
}

fn worktree_unchanged(worktree: &Json) -> bool {
    let Some(path) = worktree["path"].as_str() else {
        return false;
    };
    match crate::agent::worktree::fingerprint(Path::new(path)) {
        Some((head, state)) => worktree["head"] == head && worktree["state"] == state,
        None => false,
    }
}

/// Script runs, newest first, at most `limit`, after settling any whose timon died.
pub fn list(state: &Path, limit: usize) -> Vec<Json> {
    let dir = state.join("scripts");
    reconcile(&dir);
    let mut entries = latest(&dir);
    entries.reverse();
    entries.truncate(limit);
    entries
}

/// How many agent runs of script run `id` finished `ok`, reused ones included.
pub fn finished_runs(state: &Path, id: &str) -> usize {
    std::fs::read_to_string(state.join("scripts").join(id).join("runs.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Json>(l).ok())
        .filter(|e| e["status"] == "ok")
        .count()
}

/// One script run's request, result and runs. `id` may be an unambiguous prefix.
pub fn show(state: &Path, id: &str) -> Result<Json, Error> {
    let dir = state.join("scripts");
    reconcile(&dir);
    let id = resolve(&dir, id)?;
    let own = dir.join(&id);
    let runs: Vec<Json> = std::fs::read_to_string(own.join("runs.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    Ok(json!({
        "request": read_json(&own.join("request.json")),
        "result": read_json(&own.join("result.json")),
        "runs": runs,
    }))
}
