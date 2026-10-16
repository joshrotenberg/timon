//! Live tests against the real Claude Code and Codex CLIs (#45). Most of them spend tokens, so
//! every test is `#[ignore]` and also needs `TIMON_LIVE=1`; they never run in CI. Test names
//! carry their phase. See docs/live-tests.md.
//!
//! ```sh
//! TIMON_LIVE=1 cargo test --test live -- --ignored --test-threads 1 --nocapture live_0_
//! ```
//!
//! Every command runs with a cleared environment that keeps only HOME, PATH and USER, as a
//! person's terminal would, so variables of a host session (a Claude Code session running
//! these tests, say) do not leak into the CLIs. State goes to a temporary `TIMON_HOME`; work
//! happens in a temporary git repository; logins come from the real home directory.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn live() -> bool {
    std::env::var("TIMON_LIVE").as_deref() == Ok("1")
}

/// Returns early from a test unless `TIMON_LIVE=1`.
macro_rules! require_live {
    () => {
        if !live() {
            eprintln!("skipped: set TIMON_LIVE=1 to run live tests");
            return;
        }
    };
}

struct Live {
    dir: tempfile::TempDir,
}

impl Live {
    /// A sandbox with a git repository to work in, holding one committed file.
    fn new() -> Self {
        let live = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        let work = live.work();
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(
            work.join("README.md"),
            "# scratch\n\nA repository for live tests.\n",
        )
        .unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "README.md"],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-qm",
                "start",
            ],
        ] {
            let o = live.command("git").args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", err(&o));
        }
        live
    }

    fn work(&self) -> PathBuf {
        self.dir.path().join("work")
    }

    /// A temporary directory of its own, for an empty config directory and the like.
    fn empty_dir(&self, name: &str) -> PathBuf {
        let dir = self.dir.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `program` with a cleared environment, run in the work repository.
    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .current_dir(self.work())
            .env("TIMON_HOME", self.dir.path().join("timon"))
            .env("TERM", "dumb");
        for var in ["HOME", "PATH", "USER", "LANG"] {
            if let Some(value) = std::env::var_os(var) {
                cmd.env(var, value);
            }
        }
        cmd
    }

    fn timon(&self, args: &[&str], env: &[(&str, &Path)]) -> Output {
        let mut cmd = self.command(env!("CARGO_BIN_EXE_timon"));
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    /// Runs timon and parses its stdout as one JSON value.
    fn json(&self, args: &[&str], env: &[(&str, &Path)]) -> serde_json::Value {
        let o = self.timon(args, env);
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?}: stdout is not JSON ({e})\nstdout: {}\nstderr: {}",
                out(&o),
                err(&o)
            )
        })
    }

    /// Prints every recorded run with its cost, and returns the total in USD.
    fn costs(&self, label: &str) -> f64 {
        let runs = self.json(&["runs", "--json", "--limit", "1000"], &[]);
        let mut total = 0.0;
        eprintln!("cost: {label}");
        for r in runs.as_array().unwrap().iter().rev() {
            let usd = r["usd"].as_f64().unwrap_or(0.0);
            total += usd;
            eprintln!(
                "  {:7} {:9} {:>8}ms  ${usd:.4}  {}",
                r["provider"].as_str().unwrap_or("?"),
                r["status"].as_str().unwrap_or("?"),
                r["ms"].as_u64().unwrap_or(0),
                r["prompt"].as_str().unwrap_or("")
            );
        }
        eprintln!("  total ${total:.4}");
        total
    }

    /// Runs Lua through `timon eval`, capped at [`MAX_USD`] for the invocation.
    fn eval_raw(&self, code: &str) -> Output {
        self.timon(&["--max-usd", MAX_USD, "eval", code], &[])
    }

    /// Runs Lua and parses the one value it returns as JSON.
    fn eval(&self, code: &str) -> serde_json::Value {
        let o = self.eval_raw(code);
        assert!(o.status.success(), "{code}\nstderr: {}", err(&o));
        serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|e| panic!("{code}\nstdout is not JSON ({e}): {}", out(&o)))
    }

    /// Starts `timon eval` without waiting, for tests that interrupt it.
    fn spawn(&self, code: &str) -> std::process::Child {
        let mut cmd = self.command(env!("CARGO_BIN_EXE_timon"));
        cmd.args(["--max-usd", MAX_USD, "eval", code])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cmd.spawn().unwrap()
    }

    /// The latest state of every recorded run, newest first.
    fn runs(&self) -> Vec<serde_json::Value> {
        serde_json::from_value(self.json(&["runs", "--json", "--limit", "1000"], &[])).unwrap()
    }
}

/// The most one `timon` invocation in these tests may spend.
const MAX_USD: &str = "0.30";
const CLAUDE: &str = "provider = 'claude', model = 'haiku'";
const CODEX: &str = "provider = 'codex', effort = 'low'";
const BOTH: [(&str, &str); 2] = [("claude", CLAUDE), ("codex", CODEX)];

/// Every process below `root`, with its command line.
fn descendants(root: u32) -> Vec<(u32, String)> {
    let o = Command::new("ps")
        .args(["-axww", "-o", "pid=,ppid=,command="])
        .output()
        .unwrap();
    let table: Vec<(u32, u32, String)> = out(&o)
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let ppid = parts.next()?.parse().ok()?;
            Some((pid, ppid, parts.collect::<Vec<_>>().join(" ")))
        })
        .collect();
    let mut found = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for (pid, ppid, command) in &table {
            if *ppid == parent {
                found.push((*pid, command.clone()));
                frontier.push(*pid);
            }
        }
    }
    found
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends nothing; it only checks that the process exists.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn signal(pid: u32, sig: i32) {
    // SAFETY: sends a signal to a process this test started.
    unsafe {
        libc::kill(pid as i32, sig);
    }
}

/// Waits until `timon` has started a provider process, and returns the provider's processes.
fn provider_processes(timon: u32) -> Vec<(u32, String)> {
    for _ in 0..300 {
        let found = descendants(timon);
        if found
            .iter()
            .any(|(_, c)| c.contains("claude") || c.contains("codex"))
        {
            return found;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("timon started no provider process within 30 s");
}

/// Waits up to `secs` for every process in `pids` to exit; returns the ones still alive.
fn survivors(pids: &[(u32, String)], secs: u64) -> Vec<(u32, String)> {
    for _ in 0..secs * 10 {
        if pids.iter().all(|(p, _)| !alive(*p)) {
            return Vec::new();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    pids.iter().filter(|(p, _)| alive(*p)).cloned().collect()
}

fn text(r: &serde_json::Value) -> String {
    r["text"].as_str().unwrap_or("").to_lowercase()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

// Phase 0: preflight. Nothing here may spend tokens.

#[test]
#[ignore = "live: needs TIMON_LIVE=1 and the real CLIs"]
fn live_0_versions() {
    require_live!();
    let live = Live::new();
    let version = |cli: &str| {
        let o = live.command(cli).arg("--version").output().unwrap();
        assert!(o.status.success(), "{cli} --version: {}", err(&o));
        out(&o)
    };

    use claude_wrapper::version as claude;
    let text = version("claude");
    let v = claude::CliVersion::parse_version_output(&text).unwrap();
    let status = v.status_within(
        &claude::TESTED_CLI_VERSION_MIN,
        &claude::TESTED_CLI_VERSION_MAX,
    );
    eprintln!(
        "claude {v}: claude-wrapper tested {} to {}: {status:?}",
        claude::TESTED_CLI_VERSION_MIN,
        claude::TESTED_CLI_VERSION_MAX
    );

    use codex_wrapper::version as codex;
    let text = version("codex");
    let v = codex::CliVersion::parse_version_output(&text).unwrap();
    let status = v.status_within(
        &codex::TESTED_CLI_VERSION_MIN,
        &codex::TESTED_CLI_VERSION_MAX,
    );
    eprintln!(
        "codex {v}: codex-wrapper tested {} to {}: {status:?}",
        codex::TESTED_CLI_VERSION_MIN,
        codex::TESTED_CLI_VERSION_MAX
    );
}

#[test]
#[ignore = "live: needs TIMON_LIVE=1 and the real CLIs"]
fn live_0_claude_not_logged_in() {
    require_live!();
    let live = Live::new();
    let config = live.empty_dir("claude-config");
    // Refuse to go on if Claude finds a login anyway: the run would spend.
    let o = live
        .command("claude")
        .args(["auth", "status"])
        .env("CLAUDE_CONFIG_DIR", &config)
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("claude auth status is not JSON ({e}): {}", out(&o)));
    assert_eq!(
        status["loggedIn"], false,
        "claude is logged in with an empty config directory; not running: {status}"
    );

    let r = live.json(
        &[
            "agent",
            "run",
            "--model",
            "haiku",
            "--budget-usd",
            "0.01",
            "Reply with the word ok.",
        ],
        &[("CLAUDE_CONFIG_DIR", &config)],
    );
    eprintln!("record: {r}");
    assert_eq!(r["status"], "failed", "{r}");
    assert_eq!(r["error"]["kind"], "not_logged_in", "{r}");
    assert_eq!(
        r["error"]["message"], "Not logged in · Please run /login",
        "{r}"
    );
    assert!(r["usage"]["usd"].as_f64().unwrap_or(0.0) == 0.0, "{r}");
}

#[test]
#[ignore = "live: needs TIMON_LIVE=1 and the real CLIs"]
fn live_0_codex_not_logged_in() {
    require_live!();
    let live = Live::new();
    let home = live.empty_dir("codex-home");
    // Refuse to go on if Codex finds a login anyway: the run would spend.
    let o = live
        .command("codex")
        .args(["login", "status"])
        .env("CODEX_HOME", &home)
        .output()
        .unwrap();
    let said = format!("{}{}", out(&o), err(&o));
    assert!(
        !o.status.success() && said.contains("Not logged in"),
        "codex is logged in with an empty CODEX_HOME; not running: {said}"
    );

    let r = live.json(
        &[
            "agent",
            "run",
            "--provider",
            "codex",
            "--effort",
            "low",
            "Reply with the word ok.",
        ],
        &[("CODEX_HOME", &home)],
    );
    eprintln!("record: {r}");
    assert_eq!(r["status"], "failed", "{r}");
    assert_eq!(r["error"]["kind"], "not_logged_in", "{r}");
    let message = r["error"]["message"].as_str().unwrap();
    assert!(message.contains("401 Unauthorized"), "{r}");
    assert!(
        !message.contains(" ERROR "),
        "tracing prefix left in: {message}"
    );
}

// Phase 1: agent runs. Spends tokens.

#[test]
#[ignore = "live: spends tokens"]
fn live_1_contracts_validate() {
    require_live!();
    let live = Live::new();
    for (name, p) in BOTH {
        let r = live.eval(&format!(
            "return agent.run{{ {p}, ephemeral = true, output = 'yes_no', prompt = 'Is 17 a prime number?' }}"
        ));
        assert_eq!(r["status"], "ok", "{name}: {r}");
        assert_eq!(r["answer"]["answer"], true, "{name}: {r}");
        assert!(r["answer"]["reason"].is_string(), "{name}: {r}");

        let r = live.eval(&format!(
            "return agent.run{{ {p}, ephemeral = true, output = 'review', prompt = [[Review this Rust function and report what is wrong with it. Do not read any files.\n\nfn first(xs: &[i32]) -> i32 {{ xs[0] }}]] }}"
        ));
        assert_eq!(r["status"], "ok", "{name}: {r}");
        eprintln!("{name} review: {}", r["answer"]);
        assert!(r["answer"]["findings"].is_array(), "{name}: {r}");
    }
    live.costs("contracts");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_read_access_cannot_write() {
    require_live!();
    let live = Live::new();
    for (name, p) in BOTH {
        let r = live.eval(&format!(
            "return agent.run{{ {p}, ephemeral = true, access = 'read', prompt = 'Create a file named note.txt in the current directory containing the word hello. If you cannot, say so.' }}"
        ));
        eprintln!("{name}: {} {}", r["status"], r["text"]);
        assert!(
            !live.work().join("note.txt").exists(),
            "{name} wrote with read access"
        );
    }
    live.costs("read access");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_write_access_in_a_worktree() {
    require_live!();
    let live = Live::new();
    for (name, p) in BOTH {
        let r = live.eval(&format!(
            "return agent.run{{ {p}, ephemeral = true, access = 'write', worktree = true, prompt = 'Create a file named note.txt in the current directory containing exactly the word hello. Do nothing else.' }}"
        ));
        assert_eq!(r["status"], "ok", "{name}: {r}");
        assert_eq!(r["worktree"]["kept"], true, "{name}: {r}");
        let path = PathBuf::from(r["worktree"]["path"].as_str().unwrap());
        let note = std::fs::read_to_string(path.join("note.txt"))
            .unwrap_or_else(|e| panic!("{name}: no note.txt in the worktree ({e}): {r}"));
        assert!(note.contains("hello"), "{name}: {note:?}");
        assert!(
            !live.work().join("note.txt").exists(),
            "{name} wrote outside the worktree"
        );
    }
    live.costs("write access in a worktree");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_continue_fork_and_resume() {
    require_live!();
    let live = Live::new();
    let remember = "prompt = 'Remember this code word: PELICAN. Reply only with OK.'";
    let recall = "'What was the code word I gave you? Reply with just the word.'";

    let r = live.eval(&format!(
        "local first = agent.run{{ {CLAUDE}, {remember} }}
         local again = agent.continue(first, {recall}, {{ model = 'haiku' }})
         local fork = agent.run{{ {CLAUDE}, fork = first.session, prompt = {recall} }}
         return {{ first = first, again = again, fork = fork }}"
    ));
    for k in ["first", "again", "fork"] {
        assert_eq!(r[k]["status"], "ok", "claude {k}: {r}");
    }
    assert!(
        text(&r["again"]).contains("pelican"),
        "claude continue forgot: {r}"
    );
    assert!(
        text(&r["fork"]).contains("pelican"),
        "claude fork forgot: {r}"
    );
    assert_ne!(
        r["fork"]["session"], r["first"]["session"],
        "a fork is a new session"
    );

    let r = live.eval(&format!(
        "local first = agent.run{{ {CODEX}, {remember} }}
         local again = agent.continue(first, {recall}, {{ effort = 'low' }})
         local wrote = agent.continue(first, 'Create a file named resumed.txt in the current directory containing the word hi.', {{ effort = 'low', access = 'write' }})
         return {{ first = first, again = again, wrote = wrote }}"
    ));
    for k in ["first", "again", "wrote"] {
        assert_eq!(r[k]["status"], "ok", "codex {k}: {r}");
    }
    assert!(
        text(&r["again"]).contains("pelican"),
        "codex continue forgot: {r}"
    );
    assert!(
        live.work().join("resumed.txt").exists(),
        "a codex resume with access = write could not write: {r}"
    );
    live.costs("continue, fork and resume");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_events_from_both_providers() {
    require_live!();
    let live = Live::new();
    let read = "prompt = 'Read README.md and reply with its first line only.'";
    for (name, p) in BOTH {
        let r = live.eval(&format!(
            "return agent.run{{ {p}, ephemeral = true, events = true, {read} }}"
        ));
        assert_eq!(r["status"], "ok", "{name}: {r}");
        let events = r["events"].as_array().unwrap();
        eprintln!("{name} events:");
        for e in events {
            eprintln!("  {}", e.to_string().chars().take(160).collect::<String>());
        }
        assert!(
            events.iter().any(|e| e["kind"] == "tool_call"),
            "{name}: {r}"
        );
        assert!(
            events.iter().any(|e| e["kind"] == "tool_result"
                && e["text"].as_str().unwrap_or("").contains("scratch")),
            "{name}: no tool result with the README: {r}"
        );
        assert!(text(&r).contains("scratch"), "{name}: {r}");
    }
    let r = live.eval(&format!(
        "return agent.run{{ {CLAUDE}, ephemeral = true, events = true, output = 'yes_no', prompt = 'Read README.md. Does it mention live tests?' }}"
    ));
    assert_eq!(r["status"], "ok", "a contract in stream mode: {r}");
    assert_eq!(r["answer"]["answer"], true, "{r}");
    live.costs("events");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_failures_are_classified() {
    require_live!();
    let live = Live::new();
    let r = live.eval(&format!(
        "return agent.run{{ {CLAUDE}, ephemeral = true, budget = {{ turns = 1 }}, prompt = 'Read README.md, then reply with its first line.' }}"
    ));
    assert_eq!(
        (r["status"].as_str(), r["error"]["kind"].as_str()),
        (Some("failed"), Some("max_turns")),
        "{r}"
    );

    let r = live.eval(&format!(
        "return agent.run{{ {CLAUDE}, ephemeral = true, budget = {{ usd = 0.0001 }}, prompt = 'Reply with OK.' }}"
    ));
    eprintln!("tiny budget: {r}");
    assert_eq!(
        (r["status"].as_str(), r["error"]["kind"].as_str()),
        (Some("failed"), Some("max_budget")),
        "{r}"
    );

    for (name, p) in BOTH {
        let r = live.eval(&format!(
            "return agent.run{{ {p}, ephemeral = true, timeout = '1s', prompt = 'Write a 500 word story.' }}"
        ));
        assert_eq!(
            (r["status"].as_str(), r["error"]["kind"].as_str()),
            (Some("timeout"), Some("timeout")),
            "{name}: {r}"
        );

        let provider = name;
        let r = live.eval(&format!(
            "return agent.run{{ provider = '{provider}', model = 'no-such-model-xyz', ephemeral = true, prompt = 'Reply with OK.' }}"
        ));
        eprintln!(
            "{name} unknown model: {} {}",
            r["error"]["kind"], r["error"]["message"]
        );
        assert_eq!(r["status"], "failed", "{name}: {r}");
        assert_eq!(r["error"]["kind"], "unknown_model", "{name}: {r}");
        assert!(
            !r["error"]["message"].as_str().unwrap_or("").is_empty(),
            "{name}: {r}"
        );
    }
    live.costs("failures");
}

const LONG: &str = "prompt = 'Write a 3000 word essay about rivers. Do not use any tools.'";

#[test]
#[ignore = "live: spends tokens"]
fn live_1_race_cancels_the_slower_run() {
    require_live!();
    let live = Live::new();
    let child = live.spawn(&format!(
        "return par.race(
           function() return agent.run{{ {CLAUDE}, ephemeral = true, {LONG} }} end,
           function() sleep('8s') return 'timer' end)"
    ));
    let pid = child.id();
    let providers = provider_processes(pid);
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "timer\n2\n");
    assert_eq!(
        survivors(&providers, 5),
        Vec::new(),
        "provider processes outlived the race"
    );
    let runs = live.runs();
    assert_eq!(runs[0]["status"], "cancelled", "{runs:?}");
    live.costs("race");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_ctrl_c_cancels_and_records() {
    require_live!();
    let live = Live::new();
    let mut child = live.spawn(&format!(
        "return agent.run{{ {CLAUDE}, ephemeral = true, {LONG} }}"
    ));
    let providers = provider_processes(child.id());
    std::thread::sleep(std::time::Duration::from_secs(3));
    signal(child.id(), libc::SIGINT);
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    assert_eq!(
        survivors(&providers, 5),
        Vec::new(),
        "provider processes outlived Ctrl-C"
    );
    assert_eq!(live.runs()[0]["status"], "cancelled");
    live.costs("ctrl-c");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_a_killed_timon_is_settled_as_interrupted() {
    require_live!();
    let live = Live::new();
    let mut child = live.spawn(&format!(
        "return agent.run{{ {CLAUDE}, ephemeral = true, access = 'write', {LONG} }}"
    ));
    let providers = provider_processes(child.id());
    std::thread::sleep(std::time::Duration::from_secs(3));
    child.kill().unwrap();
    child.wait().unwrap();
    // timon gets no chance to stop its provider; nothing is signalled on its behalf.
    let orphans = survivors(&providers, 2);
    eprintln!("provider processes still running after timon was killed: {orphans:?}");
    for (pid, _) in &orphans {
        signal(*pid, libc::SIGKILL);
    }
    let run = &live.runs()[0];
    assert_eq!(run["status"], "interrupted", "{run}");
    assert_eq!(run["effects_uncertain"], true, "{run}");
    live.costs("killed");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_limits_hold() {
    require_live!();
    let live = Live::new();
    let ok = format!("agent.run{{ {CLAUDE}, ephemeral = true, prompt = 'Reply with OK.' }}");
    let o = live.timon(
        &["--max-runs", "1", "eval", &format!("{ok}\nreturn {ok}")],
        &[],
    );
    assert!(!o.status.success());
    assert!(err(&o).contains("budget_exceeded"), "{}", err(&o));
    assert_eq!(live.runs().len(), 1, "the second run must not start");

    let config = live.dir.path().join("timon/config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("init.lua"),
        "return { limits = { concurrency = 1 } }\n",
    )
    .unwrap();
    let r = live.eval(&format!(
        "local a = agent.spawn{{ {CLAUDE}, ephemeral = true, prompt = 'Reply with OK.' }}
         local b = agent.spawn{{ {CLAUDE}, ephemeral = true, prompt = 'Reply with OK.' }}
         return {{ a:await(), b:await() }}"
    ));
    let start = |r: &serde_json::Value| {
        chrono::DateTime::parse_from_rfc3339(r["started_at"].as_str().unwrap()).unwrap()
    };
    let (a, b) = (&r[0], &r[1]);
    let (first, second) = if start(a) <= start(b) { (a, b) } else { (b, a) };
    let gap = (start(second) - start(first)).num_milliseconds();
    let first_ms = first["ms"].as_i64().unwrap();
    eprintln!("first ran {first_ms} ms; the second started {gap} ms after the first");
    assert!(
        gap + 50 >= first_ms,
        "with concurrency 1 the second run started early: {r}"
    );
    live.costs("limits");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_ephemeral_and_no_tools() {
    require_live!();
    let live = Live::new();
    for (name, p) in BOTH {
        let r = live.eval(&format!(
            "return agent.run{{ {p}, ephemeral = true, prompt = 'Reply with OK.' }}"
        ));
        assert_eq!(r["status"], "ok", "{name}: {r}");
        if let Some(session) = r["session"].as_str() {
            let o = live.eval_raw(&format!("return sessions.show('{session}')"));
            assert!(
                !o.status.success() && err(&o).contains("not_found"),
                "{name}: an ephemeral run left session {session}: {}",
                out(&o)
            );
        }
    }
    let r = live.eval(&format!(
        "return agent.run{{ {CLAUDE}, ephemeral = true, events = true, tools = {{}}, prompt = 'List the files in the current directory. If you cannot, reply exactly: NO TOOLS' }}"
    ));
    assert_eq!(r["status"], "ok", "{r}");
    let calls = r["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "tool_call")
        .count();
    assert_eq!(calls, 0, "tools = {{}} still called tools: {r}");
    live.costs("ephemeral and no tools");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_records_are_redacted() {
    require_live!();
    let live = Live::new();
    let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123";
    let r = live.eval(&format!(
        "return agent.run{{ {CLAUDE}, ephemeral = true, prompt = 'Reply with OK. This is not a real key: {key}' }}"
    ));
    assert_eq!(r["status"], "ok", "{r}");
    let dir = live
        .dir
        .path()
        .join("timon/state/runs")
        .join(r["id"].as_str().unwrap());
    for file in ["request.json", "result.json"] {
        let body = std::fs::read_to_string(dir.join(file)).unwrap();
        assert!(!body.contains(key), "{file} keeps the key");
    }
    assert!(
        std::fs::read_to_string(dir.join("request.json"))
            .unwrap()
            .contains("[redacted:anthropic]")
    );
    live.costs("records");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_record_raw_streams() {
    // Runs timon with a shim in front of each CLI that keeps the exact arguments and the raw
    // output, for comparing the fakes with real output. Files go to target/tmp/live-recorded.
    require_live!();
    let live = Live::new();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("live-recorded");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, p) in BOTH {
        let real = String::from_utf8(
            live.command("sh")
                .args(["-c", &format!("command -v {name}")])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let shim = live.dir.path().join(format!("shim-{name}"));
        let out_file = dir.join(format!("{name}-read.jsonl"));
        std::fs::write(
            &shim,
            format!(
                "#!/bin/bash\nprintf '%s\\n' \"$@\" > '{args}'\n'{real}' \"$@\" | tee '{out}'\nexit ${{PIPESTATUS[0]}}\n",
                args = dir.join(format!("{name}-read.args")).display(),
                real = real.trim(),
                out = out_file.display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let var = if name == "claude" {
            "TIMON_CLAUDE_BIN"
        } else {
            "TIMON_CODEX_BIN"
        };
        let code = format!(
            "return agent.run{{ {p}, ephemeral = true, events = true, prompt = 'Read README.md and reply with its first line only.' }}"
        );
        let o = live.timon(
            &["--max-usd", MAX_USD, "eval", &code],
            &[(var, shim.as_path())],
        );
        assert!(o.status.success(), "{name}: {}", err(&o));
        let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(r["status"], "ok", "{name}: {r}");
        eprintln!(
            "{name}: {} bytes to {}",
            std::fs::metadata(&out_file).unwrap().len(),
            out_file.display()
        );
    }
}

// Phase 2: sessions with real runs. Spends tokens.

#[test]
#[ignore = "live: spends tokens"]
fn live_2_timon_sessions_are_listed_read_and_summarized() {
    require_live!();
    let live = Live::new();
    let work = live.work().display().to_string();
    let r = live.eval(&format!(
        "return agent.run{{ {CLAUDE}, events = true, prompt = 'Read README.md and reply with its first line only.' }}"
    ));
    assert_eq!(r["status"], "ok", "{r}");
    let session = r["session"].as_str().unwrap().to_owned();

    let listed = live.eval(&format!(
        "return {{ all = sessions.list{{ project = [[{work}]], automated = true }}, default = sessions.list{{ project = [[{work}]] }} }}"
    ));
    let ids = |list: &serde_json::Value| -> Vec<String> {
        list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(
        ids(&listed["all"]).contains(&session),
        "the run's session is not listed: {listed}"
    );
    assert!(
        !ids(&listed["default"]).contains(&session),
        "a timon run is not hidden by default: {listed}"
    );
    assert!(
        listed["default"]["hidden"]["automated"].as_u64().unwrap() >= 1,
        "{listed}"
    );

    let page = live.eval(&format!(
        "return sessions.transcript('{session}', {{ kinds = {{ 'tool_call' }} }})"
    ));
    let names: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["name"].as_str())
        .collect();
    let called: Vec<&str> = r["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "tool_call")
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert_eq!(
        names, called,
        "the transcript and the events disagree: {page}"
    );

    let first = live.eval(&format!("return sessions.summarize('{session}')"));
    eprintln!("summary: {}", first["text"]);
    assert_eq!(
        (first["cached"].as_bool(), first["model"].as_str()),
        (Some(false), Some("haiku")),
        "{first}"
    );
    assert!(!first["text"].as_str().unwrap().is_empty());
    let again = live.eval(&format!("return sessions.summarize('{session}')"));
    assert_eq!(
        (again["cached"].as_bool(), again["usd"].as_f64()),
        (Some(true), Some(0.0)),
        "{again}"
    );
    let forced = live.eval(&format!(
        "return sessions.summarize('{session}', {{ force = true }})"
    ));
    assert_eq!(forced["cached"], false, "{forced}");

    let r = live.eval(&format!(
        "return agent.run{{ {CODEX}, prompt = 'Reply with OK.' }}"
    ));
    assert_eq!(r["status"], "ok", "{r}");
    let thread = r["session"].as_str().unwrap().to_owned();
    let listed = live.eval(&format!(
        "return sessions.list{{ project = [[{work}]], automated = true, backend = 'codex' }}"
    ));
    assert!(
        ids(&listed).contains(&thread),
        "the codex run's session is not listed: {listed}"
    );
    live.costs("sessions");
}
