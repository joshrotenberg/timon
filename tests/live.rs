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

#[test]
fn live_tests_skip_without_the_variable() {
    // The suite compiles and its gate works in a normal `cargo test`.
    if !live() {
        assert!(std::env::var("TIMON_LIVE").as_deref() != Ok("1"));
    }
    let _ = Live::costs;
}
