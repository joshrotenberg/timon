//! agent.run through the CLI with fake providers. No test contacts a model.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let s = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(s.logs()).unwrap();
        s
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn logs(&self) -> PathBuf {
        self.root().join("logs")
    }

    fn config(&self) -> PathBuf {
        self.root().join("timon/config")
    }

    fn write(&self, path: PathBuf, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn timon(&self, code: &str, env: &[(&str, &str)]) -> Output {
        self.cli(&["eval", code], env)
    }

    fn cli(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_timon"));
        cmd.args(args)
            .current_dir(self.root())
            .env("TIMON_HOME", self.root().join("timon"))
            .env("HOME", self.root())
            .env("TIMON_CLAUDE_BIN", fixtures.join("fake-claude"))
            .env("TIMON_CODEX_BIN", fixtures.join("fake-codex"))
            .env("FAKE_LOG_DIR", self.logs());
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    /// Runs `code` and parses its single stdout value as JSON.
    fn json(&self, code: &str, env: &[(&str, &str)]) -> serde_json::Value {
        let o = self.timon(code, env);
        assert!(
            o.status.success(),
            "{code}\n{}",
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
    }

    /// The fake's log of call `n` for `provider`.
    fn call(&self, provider: &str, n: usize) -> serde_json::Value {
        let text =
            std::fs::read_to_string(self.logs().join(format!("{provider}-{n}.json"))).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn args(&self, provider: &str, n: usize) -> Vec<String> {
        serde_json::from_value(self.call(provider, n)["args"].clone()).unwrap()
    }
}

fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2).any(|w| w[0] == flag && w[1] == value)
}

// Claude

#[test]
fn claude_runs_read_only_and_records_the_result() {
    let s = Sandbox::new();
    let r = s.json("agent.run{ prompt = 'look around' }", &[]);
    assert_eq!(r["status"], "ok");
    assert_eq!(r["text"], "echo: look around");
    assert_eq!(r["session"], "claude-session-1");
    assert_eq!(r["usage"]["usd"], 0.0125);
    assert_eq!(r["usage"]["input_tokens"], 120);
    assert!(r["id"].as_str().unwrap().starts_with("r_"));

    assert_eq!(s.call("claude", 0)["prompt"], "look around");
    let args = s.args("claude", 0);
    assert!(has_pair(&args, "--permission-mode", "dontAsk"), "{args:?}");
    assert!(has_pair(&args, "--tools", "Read,Glob,Grep"), "{args:?}");
    assert!(has_pair(&args, "--max-turns", "30"), "{args:?}");
    assert!(args.contains(&"--strict-mcp-config".to_owned()), "{args:?}");
    assert!(
        !args.iter().any(|a| a.contains("look around")),
        "the prompt goes on stdin"
    );
}

#[test]
fn write_access_adds_edit_tools() {
    let s = Sandbox::new();
    s.json("agent.run{ prompt = 'x', access = 'write' }", &[]);
    let args = s.args("claude", 0);
    assert!(
        has_pair(&args, "--permission-mode", "acceptEdits"),
        "{args:?}"
    );
    assert!(
        has_pair(&args, "--tools", "Read,Glob,Grep,Edit,Write,NotebookEdit"),
        "{args:?}"
    );
}

#[test]
fn full_access_needs_an_opt_in() {
    let s = Sandbox::new();
    let o = s.timon("agent.run{ prompt = 'x', access = 'full' }", &[]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("full_access_not_allowed"));
    s.write(
        s.config().join("init.lua"),
        "return { limits = { allow_full = true } }",
    );
    s.json("agent.run{ prompt = 'x', access = 'full' }", &[]);
    assert!(
        s.args("claude", 0)
            .contains(&"--dangerously-skip-permissions".to_owned())
    );
}

#[test]
fn a_contract_answer_is_validated() {
    let s = Sandbox::new();
    let good = r#"{"answer": true, "reason": "it is"}"#;
    let r = s.json(
        "agent.run{ prompt = 'q', output = 'yes_no' }",
        &[("FAKE_STRUCTURED", good)],
    );
    assert_eq!(r["status"], "ok");
    assert_eq!(
        r["answer"],
        serde_json::json!({ "answer": true, "reason": "it is" })
    );
    let schema: serde_json::Value = {
        let args = s.args("claude", 0);
        let i = args.iter().position(|a| a == "--json-schema").unwrap();
        serde_json::from_str(&args[i + 1]).unwrap()
    };
    assert_eq!(schema["required"], serde_json::json!(["answer", "reason"]));

    let bad = r#"{"answer": "maybe"}"#;
    let r = s.json(
        "agent.run{ prompt = 'q', output = 'yes_no' }",
        &[("FAKE_STRUCTURED", bad)],
    );
    assert_eq!(r["status"], "invalid");
    assert_eq!(r["error"]["kind"], "invalid_answer");
    assert_eq!(
        r["error"]["data"],
        serde_json::json!([
            { "path": "answer", "message": "expected a boolean, got a string" },
            { "path": "(root)", "message": "missing field \"reason\"" }
        ])
    );

    let r = s.json("agent.run{ prompt = 'q', output = 'yes_no' }", &[]);
    assert_eq!(r["status"], "invalid");
}

#[test]
fn claude_failures_are_classified() {
    let s = Sandbox::new();
    let r = s.json("agent.run{ prompt = 'x' }", &[("FAKE_MODE", "auth")]);
    assert_eq!(r["status"], "failed");
    assert_eq!(r["error"]["kind"], "not_logged_in");
    assert_eq!(r["error"]["hint"], "run claude, then /login");

    let r = s.json("agent.run{ prompt = 'x' }", &[("FAKE_MODE", "max_turns")]);
    assert_eq!(r["error"]["kind"], "max_turns");

    let r = s.json("agent.run{ prompt = 'x' }", &[("FAKE_MODE", "is_error")]);
    assert_eq!(r["error"]["kind"], "provider_failed");
    assert_eq!(r["error"]["message"], "it broke");

    let r = s.json(
        "agent.run{ prompt = 'x' }",
        &[("TIMON_CLAUDE_BIN", "/no/such/claude")],
    );
    assert_eq!(r["status"], "failed");
}

#[test]
fn a_slow_run_times_out() {
    let s = Sandbox::new();
    let r = s.json(
        "agent.run{ prompt = 'x', timeout = '300ms' }",
        &[("FAKE_DELAY", "10")],
    );
    assert_eq!(r["status"], "timeout");
    assert_eq!(r["error"]["kind"], "timeout");
}

#[test]
fn cancelling_a_run_stops_its_process() {
    let s = Sandbox::new();
    let pid_file = s.root().join("pid");
    let code = format!(
        r#"
        local v = par.race(
          function() return agent.run{{ prompt = 'slow' }} end,
          function()
            while not fs.exists('{}') do sleep('20ms') end
            return 'cancelled the run'
          end
        )
        return v
        "#,
        pid_file.display()
    );
    let o = s.timon(
        &code,
        &[
            ("FAKE_DELAY", "30"),
            ("FAKE_PID_FILE", pid_file.to_str().unwrap()),
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "cancelled the run\n");
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    let alive = Command::new("kill")
        .args(["-0", pid.trim()])
        .output()
        .unwrap();
    assert!(
        !alive.status.success(),
        "the fake claude {pid} is still running"
    );
}

#[test]
fn ask_returns_the_answer_or_raises() {
    let s = Sandbox::new();
    let good = r#"{"items": ["a", "b"]}"#;
    let o = s.timon(
        "agent.ask{ prompt = 'list', output = 'list' }.items[2]",
        &[("FAKE_STRUCTURED", good)],
    );
    assert_eq!(String::from_utf8_lossy(&o.stdout), "b\n");

    let code =
        "local ok, err = try(agent.ask, 'x')\nreturn err.kind, err.data.error.kind, err.hint";
    let o = s.timon(code, &[("FAKE_MODE", "auth")]);
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "agent_failed\nnot_logged_in\nrun claude, then /login\n"
    );
}

#[test]
fn continue_resumes_and_fork_branches() {
    let s = Sandbox::new();
    let r = s.json(
        "local r = agent.run{ prompt = 'first' }\nreturn agent.continue(r, 'second')",
        &[],
    );
    assert_eq!(r["session"], "claude-session-1");
    assert!(has_pair(
        &s.args("claude", 1),
        "--resume",
        "claude-session-1"
    ));
    let r = s.json("agent.run{ prompt = 'x', fork = 'abc' }", &[]);
    assert_eq!(r["session"], "claude-forked-1");
    let args = s.args("claude", 2);
    assert!(has_pair(&args, "--resume", "abc") && args.contains(&"--fork-session".to_owned()));
}

#[test]
fn definitions_and_defaults_merge_under_the_call() {
    let s = Sandbox::new();
    s.write(
        s.config().join("init.lua"),
        "return { defaults = { model = 'opus', effort = 'medium' } }",
    );
    s.write(
        s.config().join("agents/reviewer.lua"),
        "return { description = 'Reviews', model = 'sonnet', system = 'Be brief.', budget = { usd = 2, turns = 12 } }",
    );
    s.json(
        "agent.run{ agent = 'reviewer', prompt = 'go', effort = 'high' }",
        &[],
    );
    let args = s.args("claude", 0);
    assert!(has_pair(&args, "--model", "sonnet"), "{args:?}");
    assert!(has_pair(&args, "--effort", "high"), "{args:?}");
    assert!(
        has_pair(&args, "--append-system-prompt", "Be brief."),
        "{args:?}"
    );
    assert!(has_pair(&args, "--max-turns", "12"), "{args:?}");
    assert!(has_pair(&args, "--max-budget-usd", "2"), "{args:?}");

    let o = s.timon("agent.run{ agent = 'reviwer', prompt = 'go' }", &[]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("did you mean \"reviewer\"?"));
}

#[test]
fn options_are_checked_before_anything_runs() {
    let s = Sandbox::new();
    let err = |code: &str| String::from_utf8_lossy(&s.timon(code, &[]).stderr).into_owned();
    assert!(err("agent.run{ promt = 'x' }").contains("did you mean \"prompt\"?"));
    assert!(
        err("agent.run{ prompt = 'x', effort = 'huge' }").contains("claude has no effort \"huge\"")
    );
    assert!(err("agent.run{ prompt = 'x', provider = 'gpt' }").contains("unknown provider"));
    assert!(err("agent.run{ prompt = 'x', output = 'nope' }").contains("no contract named"));
    assert!(
        std::fs::read_dir(s.logs()).unwrap().next().is_none(),
        "nothing ran"
    );
}

#[test]
fn concurrency_is_capped_by_limits() {
    let s = Sandbox::new();
    let running = s.root().join("running");
    std::fs::create_dir_all(&running).unwrap();
    s.write(
        s.config().join("init.lua"),
        "return { limits = { concurrency = 2 } }",
    );
    let o = s.timon(
        "return #par.map({ 1, 2, 3, 4, 5 }, function(i) return agent.run{ prompt = 'p' .. i } end)",
        &[
            ("FAKE_DELAY", "0.3"),
            ("FAKE_RUNNING_DIR", running.to_str().unwrap()),
        ],
    );
    assert_eq!(String::from_utf8_lossy(&o.stdout), "5\n");
    let most = std::fs::read_dir(s.root())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("seen-"))
        .map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap()
                .trim()
                .parse::<u32>()
                .unwrap()
        })
        .max()
        .unwrap();
    assert_eq!(most, 2);
}

// Codex

#[test]
fn codex_runs_sandboxed_with_a_schema_file() {
    let s = Sandbox::new();
    let good = r#"{"answer": false, "reason": "no"}"#;
    let r = s.json(
        "agent.run{ provider = 'codex', prompt = 'q', output = 'yes_no', effort = 'high', system = 'Rules.' }",
        &[("FAKE_STRUCTURED", good)],
    );
    assert_eq!(r["status"], "ok");
    assert_eq!(r["answer"]["reason"], "no");
    assert_eq!(r["session"], "codex-thread-1");
    assert_eq!(r["usage"]["output_tokens"], 40);
    assert!(r["usage"]["usd"].is_null());
    let call = s.call("codex", 0);
    assert_eq!(
        call["schema"]["required"],
        serde_json::json!(["answer", "reason"])
    );
    assert_eq!(call["prompt"], "Rules.\n\nq");
    let args = s.args("codex", 0);
    assert!(has_pair(&args, "--sandbox", "read-only"), "{args:?}");
    assert!(
        has_pair(&args, "-c", "model_reasoning_effort=\"high\""),
        "{args:?}"
    );
}

#[test]
fn codex_resume_uses_the_sandbox_config_key() {
    let s = Sandbox::new();
    s.json(
        "agent.run{ provider = 'codex', prompt = 'x', resume = 'thread-9', access = 'write' }",
        &[],
    );
    let args = s.args("codex", 0);
    assert!(args.contains(&"resume".to_owned()), "{args:?}");
    assert!(
        has_pair(&args, "-c", "sandbox_mode=\"workspace-write\""),
        "{args:?}"
    );
}

#[test]
fn codex_failures_are_classified() {
    let s = Sandbox::new();
    let r = s.json(
        "agent.run{ provider = 'codex', prompt = 'x' }",
        &[("FAKE_MODE", "auth")],
    );
    assert_eq!(r["error"]["kind"], "not_logged_in");
    let r = s.json(
        "agent.run{ provider = 'codex', prompt = 'x', output = 'yes_no' }",
        &[("FAKE_MODE", "turn_failed")],
    );
    assert_eq!(r["status"], "failed");
    assert_eq!(r["error"]["kind"], "bad_schema");
    let o = s.timon(
        "agent.run{ provider = 'codex', prompt = 'x', fork = 't' }",
        &[],
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("timon: unsupported:"));
}

// Run records and budgets (#22)

#[test]
fn every_run_is_recorded_with_secrets_redacted() {
    let s = Sandbox::new();
    let secret = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123";
    let r = s.json(
        &format!("agent.run{{ prompt = 'use key {secret} please' }}"),
        &[],
    );
    let id = r["id"].as_str().unwrap();
    let dir = s.root().join("timon/state/runs").join(id);
    let request = std::fs::read_to_string(dir.join("request.json")).unwrap();
    let result = std::fs::read_to_string(dir.join("result.json")).unwrap();
    assert!(!request.contains(secret) && !result.contains(secret));
    assert!(request.contains("[redacted:anthropic]"), "{request}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("result.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let index = std::fs::read_to_string(s.root().join("timon/state/runs/index.jsonl")).unwrap();
    assert_eq!(index.lines().count(), 1);
    assert!(index.contains("[redacted:anthropic]"));
}

#[test]
fn runs_are_listed_newest_first_and_shown_by_prefix() {
    let s = Sandbox::new();
    s.json("agent.run{ prompt = 'first' }", &[]);
    let second = s.json("agent.run{ prompt = 'second', provider = 'codex' }", &[]);
    let o = s.cli(&["runs", "--json"], &[]);
    let list: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(list[0]["prompt"], "second");
    assert_eq!(list[0]["provider"], "codex");
    assert_eq!(list[1]["usd"], 0.0125);

    let id = second["id"].as_str().unwrap();
    let o = s.cli(&["runs", "show", &id[..id.len() - 2]], &[]);
    let shown: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(shown["request"]["prompt"], "second");
    assert_eq!(shown["result"]["status"], "ok");

    let o = s.timon("#agent.runs{ since = '1d' }", &[]);
    assert_eq!(String::from_utf8_lossy(&o.stdout), "2\n");
}

#[test]
fn max_runs_stops_the_next_run() {
    let s = Sandbox::new();
    let code = "for i = 1, 3 do agent.run{ prompt = 'p' .. i } end";
    let o = s.cli(&["eval", code, "--max-runs", "2"], &[]);
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("budget_exceeded: this invocation has started 2 runs; the limit is 2"),
        "{err}"
    );
    assert!(s.logs().join("claude-1.json").exists() && !s.logs().join("claude-2.json").exists());
}

#[test]
fn max_usd_from_settings_stops_the_next_run() {
    let s = Sandbox::new();
    s.write(
        s.config().join("init.lua"),
        "return { limits = { max_usd = 0.02 } }",
    );
    let code = "for i = 1, 3 do agent.run{ prompt = 'p' .. i } end";
    let o = s.timon(code, &[]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("this invocation has spent $0.0250; the limit is $0.02"),
        "{err}"
    );
    assert!(!s.logs().join("claude-2.json").exists());
}

// Worktrees (#23)

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

fn repo(s: &Sandbox) -> PathBuf {
    let repo = s.root().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

#[test]
fn a_clean_worktree_is_removed_with_its_branch() {
    let s = Sandbox::new();
    let repo = repo(&s);
    let r = s.json(
        &format!(
            "agent.run{{ prompt = 'look', worktree = true, cwd = '{}' }}",
            repo.display()
        ),
        &[],
    );
    let wt = &r["worktree"];
    assert_eq!(wt["kept"], false);
    let path = PathBuf::from(wt["path"].as_str().unwrap());
    assert!(path.starts_with(s.root().join("timon/state/worktrees")));
    assert!(!path.exists());
    let ran_in: String = serde_json::from_value(s.call("claude", 0)["cwd"].clone()).unwrap();
    assert_eq!(
        std::fs::canonicalize(path.parent().unwrap()).unwrap(),
        std::fs::canonicalize(PathBuf::from(&ran_in).parent().unwrap()).unwrap()
    );
    assert!(git(&repo, &["branch", "--list", "timon/*"]).is_empty());
}

#[test]
fn a_changed_worktree_is_kept() {
    let s = Sandbox::new();
    let repo = repo(&s);
    let r = s.json(
        &format!(
            "agent.run{{ prompt = 'edit', access = 'write', worktree = true, cwd = '{}' }}",
            repo.display()
        ),
        &[("FAKE_TOUCH", "README.md")],
    );
    let wt = &r["worktree"];
    assert_eq!(wt["kept"], true);
    let path = PathBuf::from(wt["path"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(path.join("README.md")).unwrap(),
        "changed by the fake\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "hello\n"
    );
    let branch = wt["branch"].as_str().unwrap();
    assert!(git(&repo, &["branch", "--list", branch]).contains(branch));
}

#[test]
fn worktree_outside_a_repository_is_an_error() {
    let s = Sandbox::new();
    let o = s.timon("agent.run{ prompt = 'x', worktree = true }", &[]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("needs a git repository"));
}

// timon agent run and timon agents (#24)

#[test]
fn agent_run_takes_the_options_as_flags() {
    let s = Sandbox::new();
    let good = r#"{"answer": true, "reason": "yes"}"#;
    let o = s.cli(
        &[
            "agent",
            "run",
            "--model",
            "sonnet",
            "--effort",
            "high",
            "--output",
            "yes_no",
            "--budget-turns",
            "7",
            "--native",
            "--add-dir",
            "--native",
            "/tmp",
            "is it?",
        ],
        &[("FAKE_STRUCTURED", good)],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(r["status"], "ok");
    assert_eq!(r["answer"]["reason"], "yes");
    let args = s.args("claude", 0);
    assert!(has_pair(&args, "--model", "sonnet"), "{args:?}");
    assert!(has_pair(&args, "--max-turns", "7"), "{args:?}");
    assert!(has_pair(&args, "--add-dir", "/tmp"), "{args:?}");
    assert_eq!(s.call("claude", 0)["prompt"], "is it?");
}

#[test]
fn agent_run_reads_the_prompt_from_stdin_and_runs_codex() {
    use std::io::Write;
    let s = Sandbox::new();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut child = Command::new(env!("CARGO_BIN_EXE_timon"))
        .args([
            "agent",
            "run",
            "--provider",
            "codex",
            "--access",
            "write",
            "-",
        ])
        .current_dir(s.root())
        .env("TIMON_HOME", s.root().join("timon"))
        .env("HOME", s.root())
        .env("TIMON_CODEX_BIN", fixtures.join("fake-codex"))
        .env("FAKE_LOG_DIR", s.logs())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"from stdin")
        .unwrap();
    let o = child.wait_with_output().unwrap();
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(r["provider"], "codex");
    assert_eq!(r["text"], "echo: from stdin");
    assert!(has_pair(
        &s.args("codex", 0),
        "--sandbox",
        "workspace-write"
    ));
}

#[test]
fn agent_run_errors_like_lua_does() {
    let s = Sandbox::new();
    let o = s.cli(&["agent", "run", "--effort", "huge", "x"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("claude has no effort \"huge\""));
}

#[test]
fn agents_lists_definitions_with_descriptions() {
    let s = Sandbox::new();
    s.write(
        s.config().join("agents/reviewer.lua"),
        "return { description = 'Reviews one PR', model = 'opus' }",
    );
    s.write(
        s.config().join("agents/quick.lua"),
        "return { extends = 'reviewer', model = 'haiku' }",
    );
    let o = s.cli(&["agents", "--json"], &[]);
    let list: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(list[0]["name"], "quick");
    assert_eq!(list[0]["description"], "Reviews one PR");
    assert_eq!(list[1]["layer"], "user");
}
