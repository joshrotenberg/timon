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

impl Live {
    /// Copies the repository's `.timon` into the sandbox's user layer, which needs no trust.
    fn with_dogfood_layer(&self) {
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(".timon"),
            &self.dir.path().join("timon/config"),
        );
    }

    /// Copies files from this repository into the work repository, at the same paths, and
    /// commits them.
    fn copy_from_repo(&self, paths: &[&str]) {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        for p in paths {
            let to = self.work().join(p);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(repo.join(p), to).unwrap();
        }
        self.commit("copy from the repository");
    }

    fn write(&self, path: &str, text: &str) {
        let to = self.work().join(path);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::write(to, text).unwrap();
    }

    fn commit(&self, message: &str) {
        for args in [
            &["add", "-A"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-qm",
                message,
            ],
        ] {
            let o = self.command("git").args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", err(&o));
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
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
fn live_1_continue_in_the_kept_worktree() {
    // agent.continue keeps access and runs in the kept worktree (#58), where the provider
    // finds the session it started.
    require_live!();
    let live = Live::new();
    for (name, p) in BOTH {
        let r = live.eval(&format!(
            "local first = agent.run{{ {p}, access = 'write', worktree = true, prompt = 'Create a file named note.txt in the current directory containing exactly the word hello. Do nothing else.' }}
             local again = agent.continue(first, 'Add a second line to note.txt containing exactly the word world. Do nothing else.')
             return {{ first = first, again = again }}"
        ));
        assert_eq!(r["first"]["status"], "ok", "{name}: {r}");
        assert_eq!(r["again"]["status"], "ok", "{name}: {r}");
        let path = PathBuf::from(r["first"]["worktree"]["path"].as_str().unwrap());
        let note = std::fs::read_to_string(path.join("note.txt")).unwrap();
        assert!(
            note.contains("hello") && note.contains("world"),
            "{name}: {note:?}"
        );
        assert!(
            !live.work().join("note.txt").exists(),
            "{name} wrote outside the worktree"
        );
    }
    live.costs("continue in the worktree");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_a_fan_out_of_eight_at_concurrency_four() {
    // The scale timon aims at (design rule 10): a handful of runs at once. Progress lines show
    // how many overlapped; every run must succeed, with no rate-limit failures.
    require_live!();
    let live = Live::new();
    let config = live.dir.path().join("timon/config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("init.lua"),
        "return { limits = { concurrency = 4 } }\n",
    )
    .unwrap();
    let code = format!(
        "phase('fan-out')
         local started = os.time()
         local runs = par.map({{ 1, 2, 3, 4, 5, 6, 7, 8 }}, function(i)
           return agent.run{{ {CLAUDE}, ephemeral = true, events = true, label = 'n' .. i,
             prompt = 'Read README.md, then reply with its first line followed by the number ' .. i .. '.' }}
         end)
         return {{ runs = runs, seconds = os.time() - started }}"
    );
    let o = live.timon(
        &["--max-usd", "0.50", "--progress", "json", "eval", &code],
        &[],
    );
    assert!(o.status.success(), "{}", err(&o));
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let runs = r["runs"].as_array().unwrap();
    let statuses: Vec<&str> = runs.iter().map(|r| r["status"].as_str().unwrap()).collect();
    assert!(
        statuses.iter().all(|s| *s == "ok"),
        "{statuses:?}: {}",
        runs.iter()
            .map(|r| r["error"].to_string())
            .collect::<Vec<_>>()
            .join("; ")
    );
    let (mut running, mut most) = (0i32, 0i32);
    for line in err(&o).lines() {
        let Ok(e) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match e["event"].as_str() {
            Some("run_started") => running += 1,
            Some("run_finished") => running -= 1,
            _ => {}
        }
        most = most.max(running);
    }
    let ms: Vec<u64> = runs.iter().map(|r| r["ms"].as_u64().unwrap()).collect();
    let tools = runs
        .iter()
        .filter(|r| {
            r["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["kind"] == "tool_call")
        })
        .count();
    eprintln!(
        "8 runs in {} s wall, {} to {} ms each ({} ms in all); at most {most} at once; {tools} of 8 used a tool",
        r["seconds"],
        ms.iter().min().unwrap(),
        ms.iter().max().unwrap(),
        ms.iter().sum::<u64>(),
    );
    assert!(most <= 4, "the concurrency limit was passed: {most}");
    assert!(most >= 2, "the runs did not overlap: {most}");
    live.costs("fan-out of eight");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_1_resume_reuses_what_succeeded() {
    // A script whose third run fails with max_turns until a file exists. The turn budget is
    // not part of the request key, so the resume reuses the first two and reruns the third.
    require_live!();
    let live = Live::new();
    live.write(
        "steps.lua",
        &format!(
            "local turns = fs.exists('allow') and 6 or 1
             local a = agent.run{{ {CLAUDE}, prompt = 'Reply with the word one.' }}
             local b = agent.run{{ {CLAUDE}, prompt = 'Reply with the word two.' }}
             local c = agent.run{{ {CLAUDE}, budget = {{ turns = turns }},
               prompt = 'Read README.md, then reply with its first line.' }}
             return {{ a.status, b.status, c.status }}"
        ),
    );
    let first = live.timon(&["--max-usd", MAX_USD, "steps.lua"], &[]);
    assert!(first.status.success(), "{}", err(&first));
    assert_eq!(out(&first).trim(), r#"["ok","ok","failed"]"#);
    let scripts = live.json(&["runs", "--scripts", "--json"], &[]);
    let id = scripts[0]["id"].as_str().unwrap().to_owned();

    live.write("allow", "");
    let second = live.timon(&["--max-usd", MAX_USD, "--resume", &id, "steps.lua"], &[]);
    assert!(second.status.success(), "{}", err(&second));
    assert_eq!(out(&second).trim(), r#"["ok","ok","ok"]"#);
    let shown = live.json(
        &[
            "runs",
            "show",
            live.json(&["runs", "--scripts", "--json"], &[])[0]["id"]
                .as_str()
                .unwrap(),
        ],
        &[],
    );
    assert_eq!(shown["result"]["cached"], 2, "{shown}");
    assert_eq!(
        live.runs().len(),
        4,
        "three runs, then only the third again"
    );
    live.costs("resume");
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
    // timon had no chance to stop its provider. The next command settles the run and stops
    // the provider the record names (#63).
    let run = live.runs()[0].clone();
    let orphans = survivors(&providers, 3);
    for (pid, _) in &orphans {
        signal(*pid, libc::SIGKILL);
    }
    assert_eq!(orphans, Vec::new(), "the provider was not stopped: {run}");
    assert_eq!(run["status"], "interrupted", "{run}");
    assert_eq!(run["effects_uncertain"], true, "{run}");
    assert_eq!(run["provider_stopped"], true, "{run}");
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

// Phase 3: the dogfood agents in .timon/. Spends tokens (sonnet).

#[test]
#[ignore = "live: spends tokens"]
fn live_3_reviewer_finds_a_seeded_bug() {
    require_live!();
    let live = Live::new();
    live.with_dogfood_layer();
    live.copy_from_repo(&["AGENTS.md"]);
    live.write(
        "src/sum.rs",
        "/// Sums the first `n` values of `xs`.\npub fn sum_first(xs: &[i64], n: usize) -> i64 {\n    let mut total = 0;\n    for i in 0..=n {\n        total += xs[i];\n    }\n    total\n}\n",
    );
    live.commit("add sum_first");
    let r = live.eval(
        "return agent.run{ agent = 'reviewer', ephemeral = true, prompt = 'Review src/sum.rs.' }",
    );
    assert_eq!(r["status"], "ok", "{r}");
    eprintln!("reviewer: {} on {}", r["answer"]["verdict"], r["model"]);
    for f in r["answer"]["findings"].as_array().unwrap() {
        eprintln!(
            "  [{}] {}:{} {}",
            f["severity"], f["file"], f["line"], f["title"]
        );
    }
    assert_eq!(r["answer"]["verdict"], "request_changes", "{r}");
    let findings = r["answer"]["findings"].as_array().unwrap();
    assert!(
        findings.iter().any(|f| {
            let all = f.to_string().to_lowercase();
            f["file"].as_str().unwrap_or("").contains("sum.rs")
                && [
                    "off-by-one",
                    "off by one",
                    "0..=n",
                    "out of bounds",
                    "panic",
                    "n + 1",
                ]
                .iter()
                .any(|w| all.contains(w))
        }),
        "the off-by-one was not reported: {r}"
    );
    live.costs("reviewer");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_3_namer_returns_naming_findings() {
    require_live!();
    let live = Live::new();
    live.with_dogfood_layer();
    live.copy_from_repo(&[
        "AGENTS.md",
        "src/agent/mod.rs",
        "src/agent/claude.rs",
        "src/agent/codex.rs",
        "src/agent/events.rs",
        "src/agent/records.rs",
        "src/agent/worktree.rs",
    ]);
    let r = live.eval(
        "return agent.run{ agent = 'namer', ephemeral = true, prompt = 'Audit the names in this area: run records, events and their files, in src/agent/*.rs' }",
    );
    assert_eq!(r["status"], "ok", "{r}");
    let a = &r["answer"];
    assert!(a["area"].is_string(), "{r}");
    eprintln!("namer on {}: {}", r["model"], a["area"]);
    for f in a["findings"].as_array().unwrap() {
        assert!(f["places"].is_array(), "{f}");
        eprintln!(
            "  {} -> {}: {} ({})",
            f["current"], f["proposed"], f["rule"], f["places"]
        );
    }
    live.costs("namer");
}

// Phase 4: timon as a plugin (#46). Spends tokens.

/// What a host agent did: the shell commands it ran, its final text and its cost.
#[derive(Debug, Default)]
struct HostRun {
    commands: Vec<String>,
    /// The text of every tool result, Claude only.
    results: Vec<String>,
    /// Every tool call: its name and input.
    tools: Vec<(String, serde_json::Value)>,
    denied: Vec<String>,
    skills: Vec<String>,
    /// The host's session id, from its init event.
    session: String,
    text: String,
    usd: f64,
}

/// Whether a Claude Code host turn keeps its session, and which session it resumes.
#[derive(Clone, Copy)]
enum Session<'a> {
    /// `--no-session-persistence`: nothing to resume.
    None,
    /// A session a later turn can resume.
    Keep,
    /// `--resume ID`: a second turn in that session.
    Resume(&'a str),
}

impl HostRun {
    fn ran_timon(&self) -> bool {
        self.commands.iter().any(|c| c.contains("timon"))
    }

    fn print(&self, label: &str) {
        eprintln!("{label}: ${:.4}", self.usd);
        for c in &self.commands {
            eprintln!("  $ {}", c.chars().take(200).collect::<String>());
        }
        for (name, input) in self.tools.iter().filter(|(n, _)| n != "Bash") {
            eprintln!(
                "  tool {name} {}",
                input.to_string().chars().take(200).collect::<String>()
            );
        }
        for d in &self.denied {
            eprintln!("  denied: {}", d.chars().take(200).collect::<String>());
        }
        eprintln!(
            "  answer: {}",
            self.text.chars().take(300).collect::<String>()
        );
    }
}

impl Live {
    /// Runs Claude Code as the host, with only timon's plugin loaded (no user settings, no MCP
    /// servers) and only `timon` commands allowed in Bash, plus `tools`.
    fn claude_host(&self, prompt: &str, tools: &[&str]) -> HostRun {
        self.claude_host_with(prompt, tools, &[])
    }

    fn claude_host_with(&self, prompt: &str, tools: &[&str], env: &[(&str, &Path)]) -> HostRun {
        let mut allowed = vec!["Bash(timon:*)".to_owned()];
        allowed.extend(tools.iter().map(|t| (*t).to_owned()));
        self.claude_host_custom(prompt, true, r#"{"mcpServers":{}}"#, &allowed, env)
    }

    /// Runs Claude Code as the host with timon's plugin or without it, these MCP servers, and
    /// only these tools allowed.
    fn claude_host_custom(
        &self,
        prompt: &str,
        plugin: bool,
        mcp: &str,
        allowed: &[String],
        env: &[(&str, &Path)],
    ) -> HostRun {
        self.claude_host_session(prompt, plugin, mcp, allowed, env, Session::None)
    }

    /// One host turn as [`Live::claude_host`] runs it, in a session a later turn can resume:
    /// `Session::Keep` starts one, `Session::Resume(id)` continues it. The session lives in the
    /// real Claude Code home, like a person's, under the temporary work directory's name.
    fn claude_turn(&self, prompt: &str, tools: &[&str], session: Session) -> HostRun {
        let mut allowed = vec!["Bash(timon:*)".to_owned()];
        allowed.extend(tools.iter().map(|t| (*t).to_owned()));
        self.claude_host_session(prompt, true, r#"{"mcpServers":{}}"#, &allowed, &[], session)
    }

    fn claude_host_session(
        &self,
        prompt: &str,
        plugin: bool,
        mcp: &str,
        allowed: &[String],
        env: &[(&str, &Path)],
        session: Session,
    ) -> HostRun {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut cmd = self.command("claude");
        cmd.arg("-p");
        if plugin {
            cmd.arg("--plugin-dir").arg(repo.join("plugin"));
        }
        match session {
            Session::None => {
                cmd.arg("--no-session-persistence");
            }
            Session::Keep => {}
            Session::Resume(id) => {
                cmd.args(["--resume", id]);
            }
        }
        // The host's model: haiku unless TIMON_LIVE_MODEL names another.
        let model = std::env::var("TIMON_LIVE_MODEL").unwrap_or_else(|_| "haiku".to_owned());
        let mut child = cmd
            .args([
                "--setting-sources",
                "project",
                "--strict-mcp-config",
                "--mcp-config",
                mcp,
                "--model",
                &model,
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "dontAsk",
                "--allowedTools",
                &allowed.join(","),
                "--max-turns",
                "25",
            ])
            .env("TIMON_BIN", env!("CARGO_BIN_EXE_timon"))
            .envs(env.iter().map(|(k, v)| (*k, *v)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(prompt.as_bytes())
            .unwrap();
        let o = child.wait_with_output().unwrap();
        let mut run = HostRun::default();
        for line in out(&o).lines() {
            let Ok(e) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            match e["type"].as_str() {
                Some("system") if e["subtype"] == "init" => {
                    run.session = e["session_id"].as_str().unwrap_or("").to_owned();
                    run.skills = e["skills"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|s| s.as_str().map(str::to_owned))
                        .collect();
                }
                Some("assistant") => {
                    for b in e["message"]["content"].as_array().into_iter().flatten() {
                        if b["type"] != "tool_use" {
                            continue;
                        }
                        run.tools.push((
                            b["name"].as_str().unwrap_or("").to_owned(),
                            b["input"].clone(),
                        ));
                        if b["name"] == "Bash" {
                            run.commands
                                .push(b["input"]["command"].as_str().unwrap_or("").to_owned());
                        }
                    }
                }
                Some("user") => {
                    for b in e["message"]["content"].as_array().into_iter().flatten() {
                        if b["type"] == "tool_result" {
                            run.results.push(match &b["content"] {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            });
                        }
                    }
                }
                Some("result") => {
                    run.text = e["result"].as_str().unwrap_or("").to_owned();
                    run.usd = e["total_cost_usd"].as_f64().unwrap_or(0.0);
                    for d in e["permission_denials"].as_array().into_iter().flatten() {
                        run.denied.push(d["tool_input"].to_string());
                    }
                }
                _ => {}
            }
        }
        assert!(
            !run.text.is_empty() || o.status.success(),
            "claude failed: {}",
            err(&o)
        );
        run
    }
}

#[test]
#[ignore = "live: spends tokens"]
fn live_4_claude_uses_timon_to_reshape_json() {
    require_live!();
    let live = Live::new();
    live.write(
        "data.json",
        r#"[{"name":"alpha","size":3,"kind":"a"},{"name":"beta","size":10,"kind":"b"},{"name":"gamma","size":7,"kind":"a"}]"#,
    );
    let run = live.claude_host(
        "Use timon to read data.json and give me the names of the entries whose size is over 5, as a JSON array. Reply with only the array.",
        &[],
    );
    run.print("reshape");
    assert!(
        run.skills.iter().any(|s| s.contains("timon")),
        "the plugin's skills did not load: {:?}",
        run.skills
    );
    assert!(run.ran_timon(), "the agent never ran timon");
    let answer: Vec<String> = serde_json::from_str(
        run.text
            .trim()
            .trim_matches('`')
            .trim_start_matches("json")
            .trim(),
    )
    .unwrap_or_else(|e| panic!("not a JSON array ({e}): {}", run.text));
    let mut answer = answer;
    answer.sort();
    assert_eq!(answer, ["beta", "gamma"]);
}

#[test]
#[ignore = "live: spends tokens"]
fn live_4_claude_reads_the_reference_before_an_unfamiliar_module() {
    require_live!();
    let live = Live::new();
    live.write(
        "data.json",
        r#"[{"name":"alpha","kind":"a"},{"name":"beta","kind":"b"},{"name":"gamma","kind":"a"}]"#,
    );
    let run = live.claude_host(
        "Use timon to count the entries in data.json by their kind. Check timon's API reference for a function that groups, and use it. Reply with only a JSON object from kind to count.",
        &[],
    );
    run.print("reference");
    assert!(
        run.commands
            .iter()
            .any(|c| c.contains("timon skill") || c.contains("help(")),
        "the agent did not read the reference"
    );
    let answer: serde_json::Value = serde_json::from_str(
        run.text
            .trim()
            .trim_matches('`')
            .trim_start_matches("json")
            .trim(),
    )
    .unwrap_or_else(|e| panic!("not JSON ({e}): {}", run.text));
    assert_eq!(answer, serde_json::json!({ "a": 2, "b": 1 }));
}

#[test]
#[ignore = "live: spends tokens"]
fn live_4_claude_writes_and_runs_a_named_script() {
    require_live!();
    let live = Live::new();
    std::fs::create_dir_all(live.work().join(".timon/scripts")).unwrap();
    let o = live.timon(&["trust"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    let run = live.claude_host(
        "Write a timon script named count-lines in this project's .timon/scripts that returns the number of lines in README.md, then run it by name with timon and reply with only the number.",
        &["Write", "Edit", "Read"],
    );
    run.print("script");
    let script = live.work().join(".timon/scripts/count-lines.lua");
    let fennel = live.work().join(".timon/scripts/count-lines.fnl");
    assert!(script.exists() || fennel.exists(), "no script was written");
    assert!(
        run.commands
            .iter()
            .any(|c| c.contains("timon run count-lines") || c.contains("timon count-lines")),
        "the script was not run by name"
    );
    assert_eq!(run.text.trim(), "3", "README.md has 3 lines");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_4_claude_finds_an_earlier_session() {
    require_live!();
    let live = Live::new();
    // A Codex session in a sandbox CODEX_HOME, so the search covers a known archive.
    let codex_home = live.empty_dir("codex-home");
    let rollout = codex_home.join("sessions/2026/10/16/rollout-2026-10-16T09-00-00-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0001.jsonl");
    std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    std::fs::write(
        &rollout,
        [
            serde_json::json!({ "type": "session_meta", "payload": { "id": "0199aaaa-bbbb-7ccc-8ddd-eeeeffff0001", "cwd": "/work/ledger", "originator": "codex_cli_rs", "source": "cli" } }),
            serde_json::json!({ "type": "event_msg", "payload": { "type": "user_message", "message": "the zanzibar ledger import drops every row with a semicolon in the memo field" } }),
            serde_json::json!({ "type": "event_msg", "payload": { "type": "agent_message", "message": "The importer splits on semicolons before it parses quotes; fixed in import.rs." } }),
        ]
        .iter()
        .map(|l| format!("{l}\n"))
        .collect::<String>(),
    )
    .unwrap();
    let file = std::fs::File::options().write(true).open(&rollout).unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
    let env = [("CODEX_HOME", codex_home.as_path())];
    // Build the index first, so the agent's search is complete and quick.
    let o = live.timon(&["sessions", "index"], &env);
    assert!(o.status.success(), "{}", err(&o));

    // The host inherits CODEX_HOME, and so does the timon it runs.
    let run = live.claude_host_with(
        "Earlier I worked on a bug where a ledger import dropped rows. Use timon to find that session in my Codex sessions and reply with only its session id.",
        &[],
        &env,
    );
    run.print("sessions");
    assert!(
        run.commands.iter().any(|c| c.contains("sessions")),
        "the agent did not use timon's sessions"
    );
    assert!(
        run.text.contains("0199aaaa-bbbb-7ccc-8ddd-eeeeffff0001"),
        "{}",
        run.text
    );
}

impl Live {
    /// Runs Codex as the host. Codex has no plugin directory flag, so the plugin's skills go in
    /// the repository's `.agents/skills`, which Codex reads, and `timon` goes on PATH. Nothing is
    /// installed into the real `CODEX_HOME`.
    fn codex_host_with(&self, prompt: &str, env: &[(&str, &Path)]) -> HostRun {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        copy_dir(
            &repo.join("plugin/skills"),
            &self.work().join(".agents/skills"),
        );
        let bin = self.dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let link = bin.join("timon");
        if !link.exists() {
            std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_timon"), &link).unwrap();
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut child = self
            .command("codex")
            .args([
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--sandbox",
                "workspace-write",
                "--ephemeral",
                "--ignore-user-config",
                "-c",
                "model_reasoning_effort=\"low\"",
                "-",
            ])
            .env("PATH", path)
            .envs(env.iter().map(|(k, v)| (*k, *v)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(prompt.as_bytes())
            .unwrap();
        let o = child.wait_with_output().unwrap();
        let mut run = HostRun::default();
        for line in out(&o).lines() {
            let Ok(e) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let item = &e["item"];
            if e["type"] != "item.completed" {
                continue;
            }
            match item["type"].as_str() {
                Some("command_execution") => {
                    let command = item["command"].as_str().unwrap_or("").to_owned();
                    if item["exit_code"].as_i64().is_some_and(|c| c != 0) {
                        run.denied.push(format!(
                            "exit {}: {command}: {}",
                            item["exit_code"],
                            item["aggregated_output"]
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(200)
                                .collect::<String>()
                        ));
                    }
                    run.commands.push(command);
                }
                Some("agent_message") => run.text = item["text"].as_str().unwrap_or("").to_owned(),
                _ => {}
            }
        }
        assert!(!run.text.is_empty(), "codex gave no answer: {}", err(&o));
        run
    }
}

fn json_answer(text: &str) -> serde_json::Value {
    let t = text.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t).trim();
    serde_json::from_str(t).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

#[test]
#[ignore = "live: spends tokens"]
fn live_4_codex_uses_timon() {
    require_live!();
    let live = Live::new();
    live.write(
        "data.json",
        r#"[{"name":"alpha","size":3,"kind":"a"},{"name":"beta","size":10,"kind":"b"},{"name":"gamma","size":7,"kind":"a"}]"#,
    );
    live.commit("data");
    let run = live.codex_host_with(
        "Use the timon tool (see its skill) to read data.json and give me the names of the entries whose size is over 5, as a JSON array. Reply with only the array.",
        &[],
    );
    run.print("codex reshape");
    assert!(run.ran_timon(), "codex never ran timon");
    let mut names: Vec<String> = serde_json::from_value(json_answer(&run.text)).unwrap();
    names.sort();
    assert_eq!(names, ["beta", "gamma"]);

    std::fs::create_dir_all(live.work().join(".timon/scripts")).unwrap();
    let o = live.timon(&["trust"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    let run = live.codex_host_with(
        "Write a timon script named count-lines in this project's .timon/scripts that returns the number of lines in README.md, then run it with timon run and reply with only the number.",
        &[],
    );
    run.print("codex script");
    assert!(
        run.commands
            .iter()
            .any(|c| c.contains("timon run count-lines")),
        "the script was not run with timon run"
    );
    assert_eq!(run.text.trim(), "3", "README.md has 3 lines");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_4_codex_finds_an_earlier_session() {
    require_live!();
    let live = Live::new();
    // A Claude session in a sandbox CLAUDE_CONFIG_DIR; the Codex host does not need Claude's login.
    let config = live.empty_dir("claude-config");
    let file = config.join("projects/-work-ledger/0199aaaa-bbbb-4ccc-8ddd-eeeeffff0002.jsonl");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        [
            serde_json::json!({ "type": "user", "cwd": "/work/ledger", "entrypoint": "cli", "sessionId": "0199aaaa-bbbb-4ccc-8ddd-eeeeffff0002",
                "message": { "role": "user", "content": "the zanzibar ledger import drops every row with a semicolon in the memo field" } }),
            serde_json::json!({ "type": "assistant", "message": { "role": "assistant",
                "content": [{ "type": "text", "text": "The importer splits on semicolons before it parses quotes; fixed in import.rs." }] } }),
        ]
        .iter()
        .map(|l| format!("{l}\n"))
        .collect::<String>(),
    )
    .unwrap();
    let f = std::fs::File::options().write(true).open(&file).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
    let env = [("CLAUDE_CONFIG_DIR", config.as_path())];
    let run = live.codex_host_with(
        "Earlier I worked on a bug where a ledger import dropped rows. Use timon to find that session in my Claude Code sessions and reply with only its session id.",
        &env,
    );
    run.print("codex sessions");
    assert!(
        run.commands.iter().any(|c| c.contains("sessions")),
        "codex did not use timon's sessions"
    );
    assert!(
        run.text.contains("0199aaaa-bbbb-4ccc-8ddd-eeeeffff0002"),
        "{}",
        run.text
    );
}

#[test]
#[ignore = "live: spends tokens"]
fn live_4_codex_reads_the_reference_before_an_unfamiliar_module() {
    require_live!();
    let live = Live::new();
    live.write(
        "data.json",
        r#"[{"name":"alpha","kind":"a"},{"name":"beta","kind":"b"},{"name":"gamma","kind":"a"}]"#,
    );
    live.commit("data");
    let run = live.codex_host_with(
        "Use timon to count the entries in data.json by their kind. Check timon's API reference for a function that groups, and use it. Reply with only a JSON object from kind to count.",
        &[],
    );
    run.print("codex reference");
    assert!(
        run.commands.iter().any(|c| c.contains("timon skill")
            || c.contains("help(")
            || c.contains("references/api.md")),
        "codex did not read the reference"
    );
    assert_eq!(
        json_answer(&run.text),
        serde_json::json!({ "a": 2, "b": 1 })
    );
}

// Phases 5, 8 and 9: tasks through the plugin, flow on real runs, tasks over timon serve.
// Spends tokens.

const RELEASE_TASKS: &str = r###"return {
  version = {
    description = "The project's version, from Cargo.toml",
    run = function() return toml.decode(fs.read("Cargo.toml")).package.version end,
  },
  ["release-notes"] = {
    description = "Release notes for the current version",
    depends = { "version" },
    options = {
      audience = { type = '"users" | "developers"', default = "users", doc = "who the notes are for" },
    },
    run = function(opts, deps)
      return ("## %s (for %s)\n- faster builds"):format(deps.version, opts.audience)
    end,
  },
  wipe = {
    description = "Delete the build cache",
    confirm = "Delete the build cache?",
    run = function() fs.write("wiped.txt", "yes") return "wiped" end,
  },
}
"###;

impl Live {
    /// A crate at version 1.4.2 with release tasks in its trusted project layer.
    fn with_release_tasks(&self) {
        self.write(
            "Cargo.toml",
            "[package]\nname = \"scratch\"\nversion = \"1.4.2\"\nedition = \"2021\"\n",
        );
        self.write(".timon/tasks.lua", RELEASE_TASKS);
        self.commit("release tasks");
        let o = self.timon(&["trust"], &[]);
        assert!(o.status.success(), "{}", err(&o));
    }
}

#[test]
#[ignore = "live: spends tokens"]
fn live_5_claude_runs_tasks_through_the_plugin() {
    require_live!();
    let live = Live::new();
    live.with_release_tasks();
    let run = live.claude_host(
        "Using timon, find this project's tasks, then run the one that writes release notes, for developers. Reply with only the release notes it printed.",
        &[],
    );
    run.print("tasks through the plugin");
    assert!(
        run.commands
            .iter()
            .any(|c| c.trim() == "timon tasks" || c.trim().starts_with("timon tasks ")),
        "the agent did not list the tasks: {:?}",
        run.commands
    );
    assert!(
        run.commands
            .iter()
            .any(|c| c.contains("release-notes") && c.contains("developers")),
        "the agent did not run release-notes for developers: {:?}",
        run.commands
    );
    // The agent may reformat the notes; the version and the audience are what must come back.
    assert!(
        run.text.contains("1.4.2") && run.text.to_lowercase().contains("developers"),
        "{}",
        run.text
    );
    assert!(run.denied.is_empty(), "denied: {:?}", run.denied);
    assert!(!live.work().join("wiped.txt").exists());
}

#[test]
#[ignore = "live: spends tokens"]
fn live_9_claude_calls_tasks_through_timon_serve() {
    require_live!();
    let live = Live::new();
    live.with_release_tasks();
    let mcp = serde_json::json!({ "mcpServers": { "timon": {
        "command": env!("CARGO_BIN_EXE_timon"),
        "args": ["serve"],
        "env": { "TIMON_HOME": live.dir.path().join("timon") },
    } } })
    .to_string();
    let allowed: Vec<String> = [
        "mcp__timon",
        "mcp__timon__release-notes",
        "mcp__timon__version",
        "mcp__timon__wipe",
        "mcp__timon__eval",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let run = live.claude_host_custom(
        "Use the timon tools to produce this project's release notes for developers. Then try to delete the build cache, but I have not agreed to that, so do not confirm anything on my behalf. Reply with the release notes, then say what happened with the cache.",
        false,
        &mcp,
        &allowed,
        &[],
    );
    run.print("tasks over timon serve");
    let calls: Vec<&(String, serde_json::Value)> = run
        .tools
        .iter()
        .filter(|(n, _)| n.starts_with("mcp__timon__"))
        .collect();
    assert!(
        calls.iter().any(|(n, input)| n == "mcp__timon__release-notes" && input["audience"] == "developers"),
        "no release-notes call for developers: {:?}",
        run.tools
    );
    assert!(
        run.commands.is_empty(),
        "Bash was not allowed: {:?}",
        run.commands
    );
    assert!(
        !calls
            .iter()
            .any(|(n, input)| n == "mcp__timon__wipe" && input["confirmed"] == true),
        "the agent confirmed on the user's behalf: {:?}",
        run.tools
    );
    assert!(
        !live.work().join("wiped.txt").exists(),
        "the cache was wiped without the user"
    );
    // The agent may reformat the notes; the version and the audience are what must come back.
    assert!(
        run.text.contains("1.4.2") && run.text.to_lowercase().contains("developers"),
        "{}",
        run.text
    );
    let scripts = live.json(&["runs", "--scripts", "--json"], &[]);
    assert!(
        scripts
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == "release-notes"),
        "the task call left a script run record: {scripts}"
    );
}

const FLOW_TASKS: &str = r#"local function verifier(who)
  return {
    provider = "claude", model = "haiku", access = "read",
    prompt = "You are verifier " .. who .. ". Read src/sum.rs. Is the finding below a real bug in that file? Answer yes only if the code shows it.",
  }
end
return {
  review = {
    description = "Review src/sum.rs, then keep the findings both verifiers think are real",
    run = function()
      local review = agent.run{
        provider = "claude", model = "haiku", access = "read", output = "review",
        prompt = "Review src/sum.rs for bugs. Report only real bugs, each with its file and line.",
      }
      if review.status ~= "ok" then return { status = review.status } end
      local r = flow.verify{ items = review.answer.findings, keep = "all", checks = { verifier("A"), verifier("B") } }
      return {
        status = r.status,
        findings = #review.answer.findings,
        kept = tbl.map(r.kept, function(f) return f.title end),
        runs = r.usage.runs + 1,
        usd = r.usage.usd + review.usage.usd,
      }
    end,
  },
  decide = {
    description = "Ask three times whether 7919 is prime, and take the majority",
    run = function()
      local r = flow.vote{ n = 3, run = function(i)
        return agent.run{
          provider = "claude", model = "haiku", output = "yes_no",
          prompt = "Is 7919 a prime number? (question " .. i .. " of 3)",
        }
      end }
      return { status = r.status, winner = r.winner, agreed = r.agreed, runs = r.usage.runs, usd = r.usage.usd }
    end,
  },
}
"#;

#[test]
#[ignore = "live: spends tokens"]
fn live_8_a_review_with_a_verifier_and_a_vote_of_three() {
    require_live!();
    let live = Live::new();
    live.write(
        "src/sum.rs",
        "/// The sum of the first n elements of xs.\npub fn sum_first(xs: &[i64], n: usize) -> i64 {\n    let mut total = 0;\n    for i in 0..=n {\n        total += xs[i];\n    }\n    total\n}\n",
    );
    live.write(".timon/tasks.lua", FLOW_TASKS);
    live.commit("a seeded bug and flow tasks");
    let o = live.timon(&["trust"], &[]);
    assert!(o.status.success(), "{}", err(&o));

    let review = live.json(&["--max-usd", MAX_USD, "review"], &[]);
    eprintln!("review: {review}");
    assert_eq!(review["status"], "ok", "{review}");
    let findings = review["findings"].as_i64().unwrap();
    let kept = review["kept"].as_array().unwrap().len() as i64;
    assert!(
        findings >= 1,
        "the seeded off-by-one was not found: {review}"
    );
    assert!(kept >= 1 && kept <= findings, "{review}");
    assert_eq!(
        review["runs"].as_i64().unwrap(),
        1 + 2 * findings,
        "{review}"
    );

    let decide = live.json(&["--max-usd", MAX_USD, "decide"], &[]);
    eprintln!("decide: {decide}");
    assert_eq!(decide["status"], "ok");
    assert_eq!(decide["winner"], "true", "7919 is prime: {decide}");
    assert_eq!(decide["agreed"], true);
    let runs = decide["runs"].as_i64().unwrap();
    assert!(
        (2..=3).contains(&runs),
        "the vote stops at a quorum of 2: {decide}"
    );
    live.costs("flow");
}

/// A trusted project with two tasks and a guard, for the hook checks. Returns a path outside
/// it and a `TIMON_BIN` that logs each call to `hook.log` before running timon.
fn hooks_project(live: &Live) -> (PathBuf, PathBuf) {
    live.write(
        ".timon/tasks.lua",
        "return {\n  lint = { description = \"Lint the scratch files\", run = \"true\" },\n  ship = { description = \"Ship the release notes\", run = \"true\" },\n}\n",
    );
    live.write(
        ".timon/init.lua",
        "return { guard = { commands = { \"git push --force\" } } }\n",
    );
    live.commit("tasks and a guard");
    let o = live.timon(&["trust"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    let bin = live.empty_dir("bin").join("timon-logged");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\necho \"$*\" >> {}\nexec {} \"$@\"\n",
            live.dir.path().join("hook.log").display(),
            env!("CARGO_BIN_EXE_timon")
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    (live.dir.path().join("outside.txt"), bin)
}

fn hooks_prompt(outside: &Path, tool: &str) -> String {
    format!(
        "Do two things. 1) If your context has a timon brief, list the task names it gives, exactly. 2) Create the file {} containing the word x with your file editing tool ({tool}). Do not try any other way if it is refused. Then report what happened to the edit, quoting any reason you were given.",
        outside.display()
    )
}

fn hook_calls(live: &Live) -> usize {
    std::fs::read_to_string(live.dir.path().join("hook.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("hook"))
        .count()
}

#[test]
#[ignore = "live: spends tokens"]
fn live_10_claude_gets_the_brief_and_the_guard() {
    require_live!();
    let live = Live::new();
    let (outside, bin) = hooks_project(&live);
    let run = live.claude_host_custom(
        &hooks_prompt(&outside, "Write"),
        true,
        r#"{"mcpServers":{}}"#,
        &["Bash(timon:*)".to_owned(), "Write".to_owned()],
        &[("TIMON_BIN", &bin)],
    );
    run.print("live_10 claude");
    assert!(
        hook_calls(&live) >= 2,
        "SessionStart and PreToolUse ran timon hook"
    );
    assert!(
        run.text.contains("lint") && run.text.contains("ship"),
        "the brief reached the model: {}",
        run.text
    );
    assert!(
        run.tools.iter().any(|(n, _)| n == "Write"),
        "it tried the write"
    );
    assert!(
        run.results.iter().any(|r| r.contains("timon guard:")),
        "the guard refused it: {:?}",
        run.results
    );
    assert!(!outside.exists());
}

impl Live {
    /// Runs `codex exec` with timon's plugin installed from this checkout into a sandbox
    /// `CODEX_HOME` whose login is a link to the real one. Hooks run without the `/hooks`
    /// review, which a person gives once.
    fn codex_plugin_host(&self, prompt: &str, env: &[(&str, &Path)]) -> HostRun {
        let home = self.empty_dir("codex-home");
        let real = Path::new(&std::env::var("HOME").unwrap()).join(".codex/auth.json");
        if !home.join("auth.json").exists() {
            std::os::unix::fs::symlink(real, home.join("auth.json")).unwrap();
        }
        let repo = env!("CARGO_MANIFEST_DIR");
        for args in [
            &["plugin", "marketplace", "add", repo][..],
            &["plugin", "add", "timon@timon"],
        ] {
            let o = self
                .command("codex")
                .args(args)
                .env("CODEX_HOME", &home)
                .output()
                .unwrap();
            assert!(o.status.success(), "codex {args:?}: {}", err(&o));
        }
        let o = self
            .command("codex")
            .args([
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--sandbox",
                "workspace-write",
                "--ephemeral",
                "--dangerously-bypass-hook-trust",
                "-c",
                "model_reasoning_effort=\"low\"",
                prompt,
            ])
            .env("CODEX_HOME", &home)
            .envs(env.iter().map(|(k, v)| (*k, *v)))
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let mut run = HostRun::default();
        for line in out(&o).lines() {
            let Ok(e) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let item = &e["item"];
            if e["type"] != "item.completed" {
                continue;
            }
            match item["type"].as_str() {
                Some("command_execution") => {
                    run.commands
                        .push(item["command"].as_str().unwrap_or("").to_owned());
                }
                Some("file_change") => run.tools.push(("file_change".into(), item.clone())),
                Some("agent_message") => run.text = item["text"].as_str().unwrap_or("").to_owned(),
                _ => {}
            }
        }
        assert!(!run.text.is_empty(), "codex gave no answer: {}", err(&o));
        run
    }
}

#[test]
#[ignore = "live: spends tokens"]
fn live_10_codex_gets_the_brief_and_the_guard() {
    require_live!();
    let live = Live::new();
    let (outside, bin) = hooks_project(&live);
    let run = live.codex_plugin_host(
        &hooks_prompt(&outside, "apply_patch"),
        &[("TIMON_BIN", &bin)],
    );
    run.print("live_10 codex");
    assert!(
        hook_calls(&live) >= 2,
        "SessionStart and PreToolUse ran timon hook"
    );
    assert!(
        !run.commands.iter().any(|c| c.contains("timon")),
        "the task names came from the brief, not a command: {:?}",
        run.commands
    );
    assert!(
        run.text.contains("lint") && run.text.contains("ship"),
        "the brief reached the model: {}",
        run.text
    );
    assert!(
        run.text.contains("outside the directories"),
        "the guard's reason: {}",
        run.text
    );
    assert!(!outside.exists());
}

// Phase 11: offering to keep what proves useful (#141). An agent does ordinary work with an
// operation or a review worth repeating, and nobody mentions tasks or agents. In turn one it does
// the job, writes nothing under .timon/ and offers what it would keep; in turn two, the same
// session, the person says yes and it keeps it. The project layer is trusted before the agent
// starts, as a person would have done; the agent never trusts it. Spends tokens.

/// Markdown with one broken relative link in docs/ and one in guides/.
fn with_docs(live: &Live, tasks: &str) {
    live.write(
        "docs/intro.md",
        "See [setup](setup.md) and [api](api.md).\n",
    );
    live.write(
        "docs/setup.md",
        "Back to [intro](intro.md) or [the site](https://example.com).\n",
    );
    live.write(
        "guides/index.md",
        "Read [deploy](deploy.md) and [ops](ops/run.md).\n",
    );
    live.write("guides/deploy.md", "# Deploy\n");
    live.write(".timon/tasks.lua", tasks);
    live.commit("docs and tasks");
    let o = live.timon(&["trust"], &[]);
    assert!(o.status.success(), "{}", err(&o));
}

/// A project layer with one task unrelated to links.
const GREET_TASKS: &str = r#"return {
  greet = { description = "Say hello", run = function() return "hello" end },
}
"#;

/// A project layer whose links task checks docs/ only.
const LINKS_TASKS: &str = r#"return {
  links = {
    description = "Broken relative links in docs/",
    run = function()
      local broken = {}
      for _, file in ipairs(fs.glob("docs/**/*.md")) do
        for target in fs.read(file):gmatch("%]%(([^)#%s]+)") do
          if not target:find("^%a+:") and not fs.exists(path.join(path.dir(file), target)) then
            broken[#broken + 1] = file .. " -> " .. target
          end
        end
      end
      if #broken > 0 then error{ kind = "broken_links", message = table.concat(broken, "\n") } end
      return "no broken links"
    end,
  },
}
"#;

const REPORT: &str = "Reply with each broken link as FILE -> TARGET, then say what you changed in the repository, if anything, and what you ran to check it.";

impl Live {
    /// The tasks in reach, as `(name, description)`.
    fn tasks(&self) -> Vec<(String, String)> {
        self.json(&["tasks", "--json"], &[])
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["name"].as_str().unwrap_or("").to_owned(),
                    t["description"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect()
    }

    /// Every file under the project layer, relative to the work repository, with its text, so
    /// an edit shows as well as a new file.
    fn layer_files(&self) -> Vec<(String, String)> {
        fn walk(dir: &Path, root: &Path, files: &mut Vec<(String, String)>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                if e.path().is_dir() {
                    walk(&e.path(), root, files);
                } else {
                    let rel = e.path().strip_prefix(root).unwrap().to_owned();
                    let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                    files.push((rel.to_string_lossy().into_owned(), text));
                }
            }
        }
        let mut files = Vec::new();
        walk(&self.work().join(".timon"), &self.work(), &mut files);
        files.sort();
        files
    }
}

/// Whether the agent looked at the definitions in reach. The session-start brief alone does
/// not count.
fn inspected_definitions(run: &HostRun) -> bool {
    run.commands.iter().any(|c| {
        ["timon tasks", "timon defs", "timon agents"]
            .iter()
            .any(|s| c.contains(s))
    }) || run.tools.iter().any(|(name, input)| {
        name == "Read" && input["file_path"].as_str().unwrap_or("").contains(".timon")
    })
}

/// Whether a shell command ran timon with `name` as one of its words (`timon NAME`, `timon run
/// NAME`, timon's flags first), and a word containing `word`. Whole words, so `links` does not
/// match `links-all`.
fn ran_task(run: &HostRun, name: &str, word: &str) -> bool {
    run.commands.iter().any(|c| {
        let words: Vec<&str> = c.split_whitespace().collect();
        let Some(at) = words
            .iter()
            .position(|w| *w == "timon" || w.ends_with("/timon"))
        else {
            return false;
        };
        let rest = &words[at + 1..];
        rest.contains(&name) && rest.iter().any(|w| w.contains(word))
    })
}

/// Whether a reply offers to keep something, mentioning one of `what`. Loose on purpose: the
/// files on disk are the strict check, and the wording is judged by eye from the transcript.
fn offers(text: &str, what: &[&str]) -> bool {
    let lower = text.to_lowercase();
    [
        "keep",
        "save",
        "add",
        "want",
        "would you",
        "should i",
        "shall i",
        "could ",
        "i can ",
        "you'd like",
        "let me know",
    ]
    .iter()
    .any(|w| lower.contains(w))
        && what.iter().any(|w| lower.contains(w))
}

/// The person's answer to an offer, in the same session.
const YES: &str = "Yes, keep it.";

/// Whether a report names `name` and says it ran or checked something. The wording is judged
/// by eye as well: this only refuses a report that says nothing about a run.
fn reports_a_run(text: &str, name: &str) -> bool {
    let lower = text.to_lowercase();
    text.contains(name)
        && ["ran ", "run ", "verified", "tested", "checked", "timon "]
            .iter()
            .any(|w| lower.contains(w))
}

#[test]
#[ignore = "live: spends tokens"]
fn live_11_claude_offers_a_check_keeps_it_on_yes_and_a_later_session_reuses_it() {
    require_live!();
    let live = Live::new();
    with_docs(&live, GREET_TASKS);
    let before = live.tasks();
    let layer = live.layer_files();
    let tools = &["Read", "Write", "Edit"];

    let offer = live.claude_turn(
        &format!(
            "Using timon, find the broken relative links in the Markdown files under docs/. We run this check before every release. {REPORT}"
        ),
        tools,
        Session::Keep,
    );
    offer.print("live_11 offer");
    assert!(offer.text.contains("api.md"), "the job: {}", offer.text);
    assert_eq!(live.layer_files(), layer, "turn one wrote under .timon/");
    assert!(
        inspected_definitions(&offer),
        "the agent did not look for an existing definition: {:?}",
        offer.commands
    );
    assert!(
        offers(&offer.text, &["task"]),
        "no task offered: {}",
        offer.text
    );
    assert!(offer.denied.is_empty(), "denied: {:?}", offer.denied);

    let kept = live.claude_turn(YES, tools, Session::Resume(&offer.session));
    kept.print("live_11 kept");
    let after = live.tasks();
    let added: Vec<_> = after.iter().filter(|t| !before.contains(t)).collect();
    assert_eq!(added.len(), 1, "one task added and listed: {after:?}");
    let name = added[0].0.clone();
    assert!(
        ran_task(&kept, &name, ""),
        "the agent did not validate {name} by running it: {:?}",
        kept.commands
    );
    assert!(
        reports_a_run(&kept.text, &name),
        "the report names what it added and what ran: {}",
        kept.text
    );
    assert!(kept.denied.is_empty(), "denied: {:?}", kept.denied);

    // A later session, with a different input and no memory of the first.
    let reuse = live.claude_host(
        &format!(
            "Using timon, check the Markdown files under guides/ for broken relative links before the release. {REPORT}"
        ),
        tools,
    );
    reuse.print("live_11 later session");
    assert!(
        ran_task(&reuse, &name, "guides"),
        "the later session did not run {name} on guides/: {:?}",
        reuse.commands
    );
    assert_eq!(live.tasks(), after, "the later session added no task");
    assert!(reuse.text.contains("ops/run.md"), "{}", reuse.text);
    assert!(reuse.denied.is_empty(), "denied: {:?}", reuse.denied);
}

#[test]
#[ignore = "live: spends tokens"]
fn live_11_claude_offers_nothing_for_a_one_off() {
    require_live!();
    let live = Live::new();
    with_docs(&live, GREET_TASKS);
    let layer = live.layer_files();
    let run = live.claude_host(
        "Use timon to count the lines in README.md. Reply with only the number.",
        &["Read", "Write", "Edit"],
    );
    run.print("live_11 one-off");
    assert!(run.ran_timon(), "the agent never ran timon");
    let numbers: Vec<&str> = run
        .text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|w| !w.is_empty())
        .collect();
    assert_eq!(numbers, ["3"], "README.md has 3 lines: {}", run.text);
    assert!(
        !offers(&run.text, &["task", "script", "agent"]),
        "a one-off was offered: {}",
        run.text
    );
    assert_eq!(live.layer_files(), layer, "a one-off wrote under .timon/");
    let o = live
        .command("git")
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert_eq!(out(&o), "", "a one-off changed the repository");
}

#[test]
#[ignore = "live: spends tokens"]
fn live_11_claude_offers_to_extend_a_task_rather_than_copy_it() {
    require_live!();
    let live = Live::new();
    with_docs(&live, LINKS_TASKS);
    let layer = live.layer_files();
    let tools = &["Read", "Write", "Edit"];
    let offer = live.claude_turn(
        &format!(
            "Using timon, check the Markdown files under guides/ for broken relative links before the release, the way we already check docs/. {REPORT}"
        ),
        tools,
        Session::Keep,
    );
    offer.print("live_11 extend offer");
    assert!(offer.text.contains("ops/run.md"), "the job: {}", offer.text);
    assert_eq!(live.layer_files(), layer, "turn one wrote under .timon/");
    assert!(
        offers(&offer.text, &["links"])
            && ["option", "extend", "dir", "parameter", "argument"]
                .iter()
                .any(|w| offer.text.to_lowercase().contains(w)),
        "no offer to extend links: {}",
        offer.text
    );
    assert!(offer.denied.is_empty(), "denied: {:?}", offer.denied);

    let kept = live.claude_turn(YES, tools, Session::Resume(&offer.session));
    kept.print("live_11 extend kept");
    let tasks = live.tasks();
    let links: Vec<_> = tasks
        .iter()
        .filter(|(n, d)| n.contains("link") || d.to_lowercase().contains("link"))
        .collect();
    assert_eq!(links.len(), 1, "one task checks links: {tasks:?}");
    assert_eq!(
        links[0].0, "links",
        "the existing task was extended: {tasks:?}"
    );
    assert!(
        ran_task(&kept, "links", "guides"),
        "the agent did not run links on guides/: {:?}",
        kept.commands
    );
    // The use that was there before still works: an extension keeps docs/ as the default.
    let o = live.timon(&["links"], &[]);
    assert!(err(&o).contains("api.md"), "timon links: {}", err(&o));
    assert!(kept.denied.is_empty(), "denied: {:?}", kept.denied);
}

/// A crate whose docs make one claim the code contradicts and one it supports, a trusted
/// project layer with `agents`, and user limits that cap the subagents at haiku and six runs.
fn with_role_docs(live: &Live, agents: &[(&str, &str)]) {
    live.write(
        "src/lib.rs",
        "/// How many times a request is retried.\npub const RETRIES: u32 = 3;\n/// Seconds before a request times out.\npub const TIMEOUT_SECS: u64 = 30;\n",
    );
    live.write(
        "docs/config.md",
        "# Config\n\nA request is retried 5 times.\n",
    );
    live.write(
        "guides/limits.md",
        "# Limits\n\nA request times out after 30 seconds.\n",
    );
    live.write(".timon/tasks.lua", GREET_TASKS);
    for (name, text) in agents {
        live.write(&format!(".timon/agents/{name}.lua"), text);
    }
    live.commit("docs and definitions");
    let o = live.timon(&["trust"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    let config = live.dir.path().join("timon/config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("init.lua"),
        "return { defaults = { model = \"haiku\" }, limits = { max_runs = 6, max_usd = 0.5 } }\n",
    )
    .unwrap();
}

const REVIEWER: &str = r#"return {
  description = "Reviews a change for bugs",
  access = "read", output = "review", budget = { usd = 0.2, turns = 10 },
  system = "Report bugs in the change, with file and line. Do not edit files.",
}
"#;

const DOCS_CHECK: &str = "Using timon, have a read-only subagent check docs/config.md against src/lib.rs for claims the code contradicts, then give guides/limits.md the same check. Every changed doc gets this check before a release. Reply with what each check found, then say what you changed in the repository, if anything, and what you ran to check it.";

impl Live {
    /// The agent definitions in reach, as `(name, path)`.
    fn agents(&self) -> Vec<(String, String)> {
        self.json(&["agents", "--json"], &[])
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["name"].as_str().unwrap_or("").to_owned(),
                    a["path"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect()
    }

    /// Each recorded agent run's definition and whole prompt, from its `request.json`: the
    /// index keeps only the first 120 characters of a prompt.
    fn run_agents(&self) -> Vec<(String, String)> {
        let dir = self.dir.path().join("timon/state/runs");
        self.runs()
            .iter()
            .map(|r| {
                let id = r["id"].as_str().unwrap_or("");
                let request: serde_json::Value =
                    std::fs::read_to_string(dir.join(id).join("request.json"))
                        .ok()
                        .and_then(|t| serde_json::from_str(&t).ok())
                        .unwrap_or_default();
                (
                    r["agent"].as_str().unwrap_or("").to_owned(),
                    request["prompt"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect()
    }
}

/// Whether the recorded agent runs checked both docs, with or without a definition.
fn checked_both_docs(runs: &[(String, String)]) -> bool {
    ["config.md", "limits.md"]
        .iter()
        .all(|doc| runs.iter().any(|(_, prompt)| prompt.contains(doc)))
}

#[test]
#[ignore = "live: spends tokens"]
fn live_11_claude_offers_a_review_role_and_keeps_it_on_yes() {
    require_live!();
    let live = Live::new();
    with_role_docs(&live, &[]);
    let layer = live.layer_files();
    let tools = &["Read", "Write", "Edit"];
    let offer = live.claude_turn(DOCS_CHECK, tools, Session::Keep);
    offer.print("live_11 role offer");
    assert!(
        offer.text.contains('5'),
        "the contradiction: {}",
        offer.text
    );
    assert!(
        checked_both_docs(&live.run_agents()),
        "both docs were not checked: {:?}",
        live.run_agents()
    );
    assert_eq!(live.layer_files(), layer, "turn one wrote under .timon/");
    assert!(
        inspected_definitions(&offer),
        "the agent did not look for an existing definition: {:?}",
        offer.commands
    );
    assert!(
        offers(&offer.text, &["agent", "role"]),
        "no role offered: {}",
        offer.text
    );
    assert!(offer.denied.is_empty(), "denied: {:?}", offer.denied);

    let kept = live.claude_turn(YES, tools, Session::Resume(&offer.session));
    kept.print("live_11 role kept");
    live.costs("live_11 role");
    let agents = live.agents();
    assert_eq!(agents.len(), 1, "one role added and listed: {agents:?}");
    let (name, file) = &agents[0];
    let text = std::fs::read_to_string(file).unwrap();
    assert!(
        !text.contains("\"write\"") && !text.contains("\"full\""),
        "the role asks for more than read access: {text}"
    );
    assert!(
        live.run_agents().iter().any(|(agent, _)| agent == name),
        "{name} was never run: {:?}",
        live.run_agents()
    );
    assert!(
        reports_a_run(&kept.text, name),
        "the report names the role and what ran: {}",
        kept.text
    );
    assert!(kept.denied.is_empty(), "denied: {:?}", kept.denied);
}

#[test]
#[ignore = "live: spends tokens"]
fn live_11_claude_offers_to_build_on_an_existing_reviewer() {
    require_live!();
    let live = Live::new();
    with_role_docs(&live, &[("reviewer", REVIEWER)]);
    let layer = live.layer_files();
    let tools = &["Read", "Write", "Edit"];
    let offer = live.claude_turn(DOCS_CHECK, tools, Session::Keep);
    offer.print("live_11 reviewer offer");
    assert!(
        offer.text.contains('5'),
        "the contradiction: {}",
        offer.text
    );
    assert!(
        checked_both_docs(&live.run_agents()),
        "both docs were not checked: {:?}",
        live.run_agents()
    );
    assert_eq!(live.layer_files(), layer, "turn one wrote under .timon/");
    assert!(
        inspected_definitions(&offer),
        "the agent did not look for an existing definition: {:?}",
        offer.commands
    );
    assert!(
        offers(&offer.text, &["reviewer"]),
        "no offer that builds on the reviewer: {}",
        offer.text
    );
    assert!(offer.denied.is_empty(), "denied: {:?}", offer.denied);

    let kept = live.claude_turn(YES, tools, Session::Resume(&offer.session));
    kept.print("live_11 reviewer kept");
    live.costs("live_11 reviewer");
    let agents = live.agents();
    assert!(
        agents.iter().any(|(n, _)| n == "reviewer"),
        "the reviewer is listed: {agents:?}"
    );
    let added: Vec<_> = agents.iter().filter(|(n, _)| n != "reviewer").collect();
    assert!(added.len() <= 1, "at most one role added: {agents:?}");
    for (name, file) in &added {
        let text = std::fs::read_to_string(file).unwrap();
        assert!(
            text.contains("extends") && text.contains("reviewer"),
            "{name} copies the reviewer instead of extending it: {text}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(live.work().join(".timon/agents/reviewer.lua")).unwrap(),
        REVIEWER,
        "the reviewer the project had is unchanged"
    );
    let mut names = vec!["reviewer"];
    names.extend(added.iter().map(|(n, _)| n.as_str()));
    assert!(
        live.run_agents()
            .iter()
            .any(|(agent, _)| names.contains(&agent.as_str())),
        "neither the reviewer nor a role built on it ran: {:?}",
        live.run_agents()
    );
    assert!(kept.denied.is_empty(), "denied: {:?}", kept.denied);
}
