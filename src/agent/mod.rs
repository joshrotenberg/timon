//! Agent runs: one non-interactive turn of Claude Code or Codex, as a request in and a record
//! out. Provider differences stay in the provider modules; everything else sees these types.

mod claude;
mod codex;
pub mod events;
pub mod records;
pub mod worktree;

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};

use crate::error::Error;
use crate::modules::schema::Shape;

/// Which CLI runs the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub fn parse(name: &str) -> Result<Self, Error> {
        match name {
            "claude" => Ok(Provider::Claude),
            "codex" => Ok(Provider::Codex),
            other => Err(
                Error::new("bad_option", format!("unknown provider \"{other}\""))
                    .with_hint("provider is \"claude\" or \"codex\""),
            ),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        }
    }
}

/// The portable permission level, mapped onto each provider's own flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Read files and run read-only commands.
    Read,
    /// Also edit files in the working directory.
    Write,
    /// No restrictions.
    Full,
}

impl Access {
    pub fn parse(name: &str) -> Result<Self, Error> {
        match name {
            "read" => Ok(Access::Read),
            "write" => Ok(Access::Write),
            "full" => Ok(Access::Full),
            other => Err(
                Error::new("bad_option", format!("unknown access \"{other}\""))
                    .with_hint("access is \"read\", \"write\" or \"full\""),
            ),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
            Access::Full => "full",
        }
    }
}

/// Limits for one run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Budget {
    pub usd: Option<f64>,
    pub turns: Option<u32>,
}

/// Everything needed to run one turn.
#[derive(Debug, Clone)]
pub struct Request {
    pub prompt: String,
    pub provider: Provider,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub access: Access,
    pub tools: Option<Vec<String>>,
    pub cwd: Option<PathBuf>,
    pub system: Option<String>,
    /// The shape of a structured answer.
    pub output: Option<Rc<Shape>>,
    pub timeout: Option<Duration>,
    pub budget: Budget,
    /// Continue a session timon started.
    pub resume: Option<String>,
    /// Branch from any session; the original is untouched.
    pub fork: Option<String>,
    pub events: bool,
    pub native: Vec<String>,
    /// Leave no session behind: Claude's `--no-session-persistence`, Codex's `--ephemeral`.
    pub ephemeral: bool,
    /// A short name for the run, for progress lines and records; never sent to the provider.
    pub label: Option<String>,
    /// The workflow phase the run belongs to; never sent to the provider.
    pub phase: Option<String>,
    /// The script run this run belongs to.
    pub parent: Option<String>,
    /// Its request key within the script run, for resume.
    pub key: Option<String>,
    /// Safe to reuse on resume even though it can write.
    pub idempotent: bool,
    /// Run in a new git worktree rather than in `cwd` itself.
    pub worktree: bool,
    /// The name of the output contract, when `output` named one.
    pub contract: Option<String>,
    /// timon's state directory, which holds the `CODEX_HOME` of Codex runs; never sent to the
    /// provider.
    pub state: Option<PathBuf>,
}

impl Request {
    /// The request as JSON, for the run's request.json.
    pub fn to_json(&self) -> Json {
        json!({
            "prompt": self.prompt,
            "provider": self.provider.name(),
            "model": self.model,
            "effort": self.effort,
            "access": self.access.name(),
            "tools": self.tools,
            "cwd": self.cwd,
            "system": self.system,
            "output": self.output.as_ref().map(|s| s.json_schema()),
            "timeout_ms": self.timeout.map(|t| t.as_millis() as u64),
            "budget": { "usd": self.budget.usd, "turns": self.budget.turns },
            "resume": self.resume,
            "fork": self.fork,
            "native": self.native,
            "ephemeral": self.ephemeral,
            "events": self.events,
            "label": self.label,
            "phase": self.phase,
            "parent": self.parent,
            "key": self.key,
            "idempotent": self.idempotent,
            "worktree": self.worktree,
            "contract": self.contract,
        })
    }

    /// The sha256 of what decides a run's answer: provider, model, effort, access, tools,
    /// system prompt, working directory and whether it runs in a worktree of it, prompt, output
    /// schema, session and native arguments.
    /// Timeout, budget, events, label and phase are left out.
    pub fn hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let cwd = self.cwd.clone().or_else(|| std::env::current_dir().ok());
        let identity = json!({
            "provider": self.provider.name(),
            "model": self.model,
            "effort": self.effort,
            "access": self.access.name(),
            "tools": self.tools,
            "system": self.system,
            "cwd": cwd,
            "worktree": self.worktree,
            "prompt": self.prompt,
            "output": self.output.as_ref().map(|s| s.json_schema()),
            "resume": self.resume,
            "fork": self.fork,
            "native": self.native,
        });
        hex::encode(Sha256::digest(identity.to_string().as_bytes()))
    }

    pub fn new(provider: Provider, prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            provider,
            model: None,
            effort: None,
            access: Access::Read,
            tools: None,
            cwd: None,
            system: None,
            output: None,
            timeout: None,
            budget: Budget::default(),
            resume: None,
            fork: None,
            events: false,
            native: Vec::new(),
            ephemeral: false,
            label: None,
            phase: None,
            parent: None,
            key: None,
            idempotent: false,
            worktree: false,
            contract: None,
            state: None,
        }
    }
}

/// How a run ended. A review that requests changes is still `Ok`: execution status and the
/// answer's verdict are separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// The provider failed: not logged in, an error result, a crash.
    Failed,
    /// The provider answered, but the answer does not match the output contract.
    Invalid,
    Timeout,
    Cancelled,
    /// The timon process ended before the run did; found and settled later.
    Interrupted,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Failed => "failed",
            Status::Invalid => "invalid",
            Status::Timeout => "timeout",
            Status::Cancelled => "cancelled",
            Status::Interrupted => "interrupted",
        }
    }
}

/// Token counts and cost. Codex reports no cost.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub usd: Option<f64>,
}

/// The outcome of one run.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub id: String,
    /// When the run started, RFC 3339 in UTC.
    pub started_at: String,
    pub status: Status,
    /// The validated structured output, when the request asked for one.
    pub answer: Option<Json>,
    pub text: String,
    pub error: Option<Error>,
    pub provider: Provider,
    pub model: Option<String>,
    /// The provider's session id, for `resume`.
    pub session: Option<String>,
    pub usage: Usage,
    pub ms: u64,
    pub events: Option<Vec<Json>>,
    /// `{ path, branch, kept }` when the run used a worktree, plus `head` and `state` (the
    /// fingerprint a resume compares) when it was kept.
    pub worktree: Option<Json>,
    pub label: Option<String>,
    pub phase: Option<String>,
}

impl Record {
    /// The record of a run stopped before it finished. Its cost is unknown.
    pub fn cancelled(request: &Request, id: String, started_at: String, ms: u64) -> Self {
        Self {
            id,
            started_at,
            status: Status::Cancelled,
            answer: None,
            text: String::new(),
            error: Some(
                Error::new("cancelled", "the run was cancelled before it finished")
                    .with_hint("its cost is not known; the provider may have spent tokens"),
            ),
            provider: request.provider,
            model: request.model.clone(),
            session: None,
            usage: Usage::default(),
            ms,
            events: None,
            worktree: None,
            label: request.label.clone(),
            phase: request.phase.clone(),
        }
    }

    /// The record as JSON, the shape Lua sees.
    pub fn to_json(&self) -> Json {
        json!({
            "id": self.id,
            "started_at": self.started_at,
            "status": self.status.name(),
            "answer": self.answer,
            "text": self.text,
            "error": self.error.as_ref().map(|e| json!({
                "kind": e.kind,
                "message": e.message,
                "hint": e.hint,
                "data": e.data.as_deref(),
            })),
            "provider": self.provider.name(),
            "model": self.model,
            "session": self.session,
            "usage": {
                "input_tokens": self.usage.input_tokens,
                "output_tokens": self.usage.output_tokens,
                "usd": self.usage.usd,
            },
            "ms": self.ms,
            "events": self.events,
            "worktree": self.worktree,
            "label": self.label,
            "phase": self.phase,
        })
    }
}

/// What a provider produced before timon checks it.
pub(crate) struct Answer {
    pub text: String,
    pub structured: Option<Json>,
    pub session: Option<String>,
    pub usage: Usage,
}

/// How a run failed, with the session and usage the provider reported before it did.
pub(crate) struct Failure {
    pub status: Status,
    pub error: Error,
    pub session: Option<String>,
    pub usage: Usage,
}

impl From<(Status, Error)> for Box<Failure> {
    /// A failure that reported nothing: no session, and an unknown cost.
    fn from((status, error): (Status, Error)) -> Self {
        Box::new(Failure {
            status,
            error,
            session: None,
            usage: Usage::default(),
        })
    }
}

/// A provider's result, or how it failed.
pub(crate) type Outcome = Result<Answer, Box<Failure>>;

/// Told the provider process's pid and process group as soon as it starts, so a record can
/// name the process to stop if timon dies first.
pub type SpawnHook = std::sync::Arc<dyn Fn(u32, Option<u32>) + Send + Sync>;

/// The binary for `provider`: `TIMON_CLAUDE_BIN` or `TIMON_CODEX_BIN`, else the wrapper finds it
/// on PATH.
pub fn binary(provider: Provider) -> Option<PathBuf> {
    let var = match provider {
        Provider::Claude => "TIMON_CLAUDE_BIN",
        Provider::Codex => "TIMON_CODEX_BIN",
    };
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Checks the provider-specific parts of a request before anything runs, so a bad option
/// raises instead of becoming a failed record.
pub fn check(request: &Request) -> Result<(), Error> {
    match request.provider {
        Provider::Claude => {
            if let Some(effort) = &request.effort {
                claude::effort(effort)?;
            }
        }
        Provider::Codex => {
            if request.fork.is_some() {
                return Err(codex::no_fork());
            }
        }
    }
    Ok(())
}

/// Runs one turn and returns its record. It never fails: how the run ended is in the record.
pub async fn run(request: &Request) -> Record {
    run_as(request, new_id(), None).await
}

/// [`run`] with a chosen record id, for callers that need it before the run starts, and a hook
/// told the provider's pid.
pub async fn run_as(request: &Request, id: String, on_spawn: Option<SpawnHook>) -> Record {
    let started = Instant::now();
    let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let binary = binary(request.provider);
    let mut events = Vec::new();
    let outcome = match request.provider {
        Provider::Claude => claude::run(request, binary.as_deref(), on_spawn, &mut events).await,
        Provider::Codex => codex::run(request, binary.as_deref(), on_spawn, &mut events).await,
    };
    let mut record = Record {
        id,
        started_at,
        status: Status::Ok,
        answer: None,
        text: String::new(),
        error: None,
        provider: request.provider,
        model: request.model.clone(),
        session: None,
        usage: Usage::default(),
        ms: started.elapsed().as_millis() as u64,
        events: request.events.then(|| events::finish(events)),
        worktree: None,
        label: request.label.clone(),
        phase: request.phase.clone(),
    };
    match outcome {
        Ok(answer) => {
            record.text = answer.text;
            record.session = answer.session;
            record.usage = answer.usage;
            if let Some(shape) = &request.output {
                match answer.structured {
                    None => {
                        record.status = Status::Invalid;
                        record.error = Some(
                            Error::new(
                                "invalid_answer",
                                "the provider returned no structured answer",
                            )
                            .with_hint("the text is in record.text"),
                        );
                    }
                    Some(value) => {
                        let errors = shape.check(&value);
                        if errors.is_empty() {
                            record.answer = Some(value);
                        } else {
                            let list: Vec<Json> = errors
                                .iter()
                                .map(|(path, message)| json!({ "path": path, "message": message }))
                                .collect();
                            record.status = Status::Invalid;
                            record.error = Some(
                                Error::new(
                                    "invalid_answer",
                                    format!(
                                        "the answer does not match the output contract: {} problem(s)",
                                        errors.len()
                                    ),
                                )
                                .with_hint("the problems are in error.data, the raw answer in text")
                                .with_data(Json::Array(list)),
                            );
                            if record.text.is_empty() {
                                record.text = value.to_string();
                            }
                        }
                    }
                }
            }
        }
        Err(failure) => {
            record.status = failure.status;
            record.error = Some(failure.error);
            record.session = failure.session;
            record.usage = failure.usage;
        }
    }
    record
}

/// A new run id: `r_` and a sortable timestamp-based suffix.
pub fn new_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let nanos = now.as_nanos();
    format!(
        "r_{:x}{:04x}",
        nanos / 1_000_000,
        (nanos % 1_000_000) as u32 & 0xffff
    )
}
