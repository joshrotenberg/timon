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
fn ephemeral_runs_leave_no_session_and_no_tools_means_none() {
    let s = Sandbox::new();
    s.json(
        "agent.run{ prompt = 'x', ephemeral = true, tools = {} }",
        &[],
    );
    let args = s.args("claude", 0);
    assert!(
        args.contains(&"--no-session-persistence".to_owned()),
        "{args:?}"
    );
    assert!(has_pair(&args, "--tools", ""), "{args:?}");
    s.json(
        "agent.run{ provider = 'codex', prompt = 'x', ephemeral = true }",
        &[],
    );
    let args = s.args("codex", 0);
    assert!(args.contains(&"--ephemeral".to_owned()), "{args:?}");
    s.json("agent.run{ prompt = 'x' }", &[]);
    assert!(
        !s.args("claude", 1)
            .contains(&"--no-session-persistence".to_owned())
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
    // The real CLI prints its result JSON and exits 1; the message is the result's text.
    assert_eq!(r["error"]["message"], "Not logged in · Please run /login");

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
fn continue_keeps_the_earlier_request_and_spec_overrides_it() {
    let s = Sandbox::new();
    s.json(
        "local r = agent.run{ prompt = 'first', access = 'write', model = 'sonnet', effort = 'high', output = 'yes_no', system = 'Be brief.' }
         agent.continue(r, 'second')
         return agent.continue(r, 'third', { access = 'read', model = 'haiku' })",
        &[("FAKE_AUTO_ANSWER", "1")],
    );
    let second = s.args("claude", 1);
    assert!(
        has_pair(&second, "--resume", "claude-session-1"),
        "{second:?}"
    );
    assert!(
        has_pair(&second, "--permission-mode", "acceptEdits"),
        "{second:?}"
    );
    assert!(has_pair(&second, "--model", "sonnet"), "{second:?}");
    assert!(has_pair(&second, "--effort", "high"), "{second:?}");
    assert!(
        has_pair(&second, "--append-system-prompt", "Be brief."),
        "{second:?}"
    );
    assert!(second.contains(&"--json-schema".to_owned()), "{second:?}");
    let third = s.args("claude", 2);
    assert!(
        has_pair(&third, "--permission-mode", "dontAsk"),
        "{third:?}"
    );
    assert!(has_pair(&third, "--model", "haiku"), "{third:?}");
    assert!(third.contains(&"--json-schema".to_owned()), "{third:?}");
}

#[test]
fn continue_runs_in_the_kept_worktree() {
    let s = Sandbox::new();
    let repo = repo(&s);
    let code = format!(
        "local r = agent.run{{ prompt = 'edit', access = 'write', worktree = true, cwd = '{}' }}
         local again = agent.continue(r, 'edit more')
         return {{ first = r, again = again }}",
        repo.display()
    );
    let r = s.json(&code, &[("FAKE_TOUCH", "note.txt")]);
    assert_eq!(r["first"]["worktree"]["kept"], true, "{r}");
    let path = r["first"]["worktree"]["path"].as_str().unwrap();
    let cwd = s.call("claude", 1)["cwd"].as_str().unwrap().to_owned();
    assert_eq!(
        std::fs::canonicalize(&cwd).unwrap(),
        std::fs::canonicalize(path).unwrap(),
        "the continuation did not run in the worktree"
    );
    assert!(
        r["again"]["worktree"].is_null(),
        "a continuation makes no new worktree: {r}"
    );
}

#[test]
fn an_ephemeral_run_cannot_be_continued() {
    let s = Sandbox::new();
    let o = s.timon(
        "local r = agent.run{ prompt = 'x', ephemeral = true }\nreturn agent.continue(r, 'y')",
        &[],
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.contains("no_session: that run was ephemeral"),
        "{stderr}"
    );
    assert!(
        stderr.contains("hint: run it without ephemeral = true"),
        "{stderr}"
    );
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
    assert!(err("agent.run{ prompt = 'x', provider = 'gpt' }").contains(
        "option \"provider\" to agent.run must be one of \"claude\", \"codex\", got \"gpt\""
    ));
    let raed = err("agent.run{ prompt = 'x', access = 'raed' }");
    assert!(
        raed.contains("must be one of \"read\", \"write\", \"full\", got \"raed\""),
        "{raed}"
    );
    assert!(raed.contains("hint: did you mean \"read\"?"), "{raed}");
    assert!(
        err("agent.run{ prompt = 'x', budget = { tunrs = 3 } }")
            .contains("unknown field \"tunrs\" in option \"budget\" to agent.run\n  hint: did you mean \"turns\"?")
    );
    assert!(
        err("agent.run{ prompt = 'x', budget = { turns = 'many' } }")
            .contains("option \"budget.turns\" to agent.run must be an integer, got a string")
    );
    assert!(
        err("agent.run{ prompt = 'x', output = true }")
            .contains("must be a string or a schema, got a boolean")
    );
    assert!(
        err("agent.run{ prompt = 'x', tools = { 'Read', 3 } }").contains("option \"tools[2]\"")
    );
    assert!(err("agent.run{ prompt = 'x', output = 'nope' }").contains("no contract named"));
    assert!(
        std::fs::read_dir(s.logs()).unwrap().next().is_none(),
        "nothing ran"
    );
}

#[test]
fn definitions_are_checked_like_the_options_they_set() {
    let s = Sandbox::new();
    s.write(
        s.config().join("agents/careless.lua"),
        "return { access = 'raed', budget = { turns = 'lots' } }",
    );
    let stderr = String::from_utf8_lossy(
        &s.timon("agent.run{ agent = 'careless', prompt = 'x' }", &[])
            .stderr,
    )
    .into_owned();
    assert!(stderr.contains("bad_config"), "{stderr}");
    assert!(stderr.contains("careless.lua"), "{stderr}");
    assert!(
        stderr.contains(
            "field \"access\" must be one of \"read\", \"write\", \"full\", got \"raed\""
        ) || stderr.contains("field \"budget.turns\" must be an integer, got a string"),
        "{stderr}"
    );
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
    let states: Vec<serde_json::Value> = index
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        states.len(),
        3,
        "a running line, the provider's pid, then the result"
    );
    let statuses: Vec<&str> = states
        .iter()
        .map(|l| l["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["running", "running", "ok"]);
    assert!(
        states[1]["provider_pid"].as_u64().is_some(),
        "{}",
        states[1]
    );
    assert!(index.contains("[redacted:anthropic]"));
    assert!(!index.contains(secret));
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

// Settled records (#31)

fn runs_json(s: &Sandbox) -> serde_json::Value {
    let o = s.cli(&["runs", "--json"], &[]);
    serde_json::from_slice(&o.stdout).unwrap()
}

#[test]
fn a_cancelled_run_is_recorded() {
    let s = Sandbox::new();
    let pid_file = s.root().join("pid");
    let code = format!(
        r#"
        return par.race(
          function() return agent.run{{ prompt = 'slow' }} end,
          function()
            while not fs.exists('{}') do sleep('20ms') end
            return 'done'
          end
        )
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
    let runs = runs_json(&s);
    assert_eq!(runs[0]["status"], "cancelled");
    let id = runs[0]["id"].as_str().unwrap();
    let result = std::fs::read_to_string(
        s.root()
            .join("timon/state/runs")
            .join(id)
            .join("result.json"),
    )
    .unwrap();
    assert!(result.contains("\"cancelled\""), "{result}");
}

/// Starts `timon eval code` in the background and waits until the fake provider is running.
fn start_slow(s: &Sandbox, code: &str, pid_file: &Path) -> std::process::Child {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut child = Command::new(env!("CARGO_BIN_EXE_timon"))
        .args(["eval", code])
        .current_dir(s.root())
        .env("TIMON_HOME", s.root().join("timon"))
        .env("HOME", s.root())
        .env("TIMON_CLAUDE_BIN", fixtures.join("fake-claude"))
        .env("FAKE_DELAY", "30")
        .env("FAKE_PID_FILE", pid_file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..500 {
        if pid_file.exists() {
            return child;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("the fake provider never started");
}

fn kill(pid: &str, signal: &str) {
    let _ = Command::new("kill").args([signal, pid.trim()]).status();
}

#[test]
fn ctrl_c_records_the_run_as_cancelled() {
    let s = Sandbox::new();
    let pid_file = s.root().join("pid");
    let mut child = start_slow(&s, "agent.run{ prompt = 'slow' }", &pid_file);
    kill(&child.id().to_string(), "-INT");
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    assert_eq!(runs_json(&s)[0]["status"], "cancelled");
    kill(&std::fs::read_to_string(&pid_file).unwrap(), "-9");
}

#[test]
fn a_killed_timon_leaves_an_interrupted_run() {
    let s = Sandbox::new();
    let pid_file = s.root().join("pid");
    let mut child = start_slow(
        &s,
        "agent.run{ prompt = 'edit', access = 'write' }",
        &pid_file,
    );
    child.kill().unwrap();
    child.wait().unwrap();
    let provider = std::fs::read_to_string(&pid_file).unwrap();
    let alive = |pid: &str| {
        Command::new("kill")
            .args(["-0", pid.trim()])
            .output()
            .unwrap()
            .status
            .success()
    };
    if cfg!(target_os = "linux") {
        // The kernel stops the provider with timon (die_with_parent), so it is gone before
        // the next command looks.
        let gone = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            !alive(&provider)
        });
        if !gone {
            kill(&provider, "-9");
        }
        assert!(gone, "the provider outlived timon on Linux");
        let run = &runs_json(&s)[0];
        assert_eq!(run["status"], "interrupted");
        assert_eq!(run["effects_uncertain"], true);
        return;
    }
    assert!(
        alive(&provider),
        "the provider outlives a killed timon until the next command"
    );
    // The next command settles the run and stops the provider the record names (#63).
    let run = &runs_json(&s)[0];
    let gone = (0..50).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        !alive(&provider)
    });
    if !gone {
        kill(&provider, "-9");
    }
    assert!(gone, "the orphaned provider was not stopped");
    assert_eq!(run["status"], "interrupted");
    assert_eq!(run["effects_uncertain"], true);
    assert_eq!(run["provider_stopped"], true, "{run}");
    let shown = s.cli(&["runs", "show", run["id"].as_str().unwrap()], &[]);
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["result"]["status"], "interrupted");
    assert_eq!(shown["result"]["error"]["kind"], "interrupted");
}

// Events (#25)

fn kinds(record: &serde_json::Value) -> Vec<String> {
    record["events"]
        .as_array()
        .unwrap_or_else(|| panic!("no events: {record}"))
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn claude_events_stream_tool_calls_into_the_record() {
    let s = Sandbox::new();
    let plain = s.json("agent.run{ prompt = 'x' }", &[]);
    assert!(plain["events"].is_null());
    assert!(has_pair(&s.args("claude", 0), "--output-format", "json"));

    let r = s.json("agent.run{ prompt = 'look', events = true }", &[]);
    assert_eq!(r["status"], "ok");
    assert_eq!(r["text"], "echo: look");
    assert_eq!(r["usage"]["usd"], 0.0125);
    assert_eq!(kinds(&r), ["reasoning", "tool_call", "tool_result", "text"]);
    assert_eq!(
        r["events"][1],
        serde_json::json!({ "kind": "tool_call", "id": "t1", "name": "Read",
            "input": { "file_path": "README.md" } })
    );
    assert_eq!(
        r["events"][2],
        serde_json::json!({ "kind": "tool_result", "id": "t1", "error": false, "text": "# fake readme" })
    );
    let args = s.args("claude", 1);
    assert!(
        has_pair(&args, "--output-format", "stream-json"),
        "{args:?}"
    );
    assert!(args.contains(&"--verbose".to_owned()), "{args:?}");
}

#[test]
fn a_failed_claude_turn_keeps_its_events_and_its_classification() {
    let s = Sandbox::new();
    let r = s.json(
        "agent.run{ prompt = 'x', events = true }",
        &[("FAKE_MODE", "max_turns")],
    );
    assert_eq!(r["status"], "failed");
    assert_eq!(r["error"]["kind"], "max_turns");
    assert_eq!(kinds(&r)[..3], ["reasoning", "tool_call", "tool_result"]);
    let r = s.json(
        "agent.run{ prompt = 'x', events = true }",
        &[("FAKE_MODE", "auth")],
    );
    // In stream mode the error result comes first, and is classified as a login problem too.
    assert_eq!(r["error"]["kind"], "not_logged_in", "{r}");
    assert_eq!(r["error"]["message"], "Not logged in · Please run /login");
    assert!(!kinds(&r).iter().any(|k| k == "tool_call"), "{r}");
}

#[test]
fn codex_events_come_from_its_items() {
    let s = Sandbox::new();
    let r = s.json(
        "agent.run{ provider = 'codex', prompt = 'list', events = true }",
        &[],
    );
    assert_eq!(r["status"], "ok");
    assert_eq!(kinds(&r), ["reasoning", "tool_call", "tool_result", "text"]);
    assert_eq!(
        r["events"][1],
        serde_json::json!({ "kind": "tool_call", "id": "i1", "name": "shell",
            "input": { "command": "bash -lc ls" } })
    );
    assert_eq!(r["events"][2]["text"], "README.md\n");
    let r = s.json(
        "agent.run{ provider = 'codex', prompt = 'x', events = true }",
        &[("FAKE_MODE", "turn_failed")],
    );
    assert_eq!(r["error"]["kind"], "bad_schema");
    assert_eq!(kinds(&r), ["reasoning", "tool_call", "tool_result"]);
}

#[test]
fn events_are_redacted_in_the_run_record_and_flagged_on_the_cli() {
    let s = Sandbox::new();
    let secret = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123";
    let o = s.cli(
        &["agent", "run", "--events", "summarize"],
        &[("FAKE_TEXT", &format!("the key is {secret}"))],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(kinds(&r).last().map(String::as_str), Some("text"));
    let dir = s
        .root()
        .join("timon/state/runs")
        .join(r["id"].as_str().unwrap());
    let result = std::fs::read_to_string(dir.join("result.json")).unwrap();
    assert!(result.contains("\"events\""), "{result}");
    assert!(!result.contains(secret) && result.contains("[redacted:anthropic]"));
    let request = std::fs::read_to_string(dir.join("request.json")).unwrap();
    assert!(request.contains("\"events\": true"), "{request}");
}

#[test]
fn events_from_recorded_real_output_normalize() {
    let s = Sandbox::new();
    let recorded = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/recorded");
    let replay = |file: &str| recorded.join(file).display().to_string();

    let claude = replay("claude-read.stream.jsonl");
    let r = s.json(
        "agent.run{ prompt = 'x', events = true }",
        &[("FAKE_REPLAY", &claude)],
    );
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(kinds(&r), ["tool_call", "tool_result", "text"]);
    assert_eq!(r["events"][0]["name"], "Read");
    assert_eq!(r["events"][0]["id"], r["events"][1]["id"]);
    assert!(
        r["events"][1]["text"]
            .as_str()
            .unwrap()
            .contains("# scratch"),
        "{r}"
    );
    assert_eq!(r["text"], "# scratch");
    assert!(r["usage"]["usd"].as_f64().unwrap() > 0.0);

    let codex = replay("codex-read.jsonl");
    let r = s.json(
        "agent.run{ provider = 'codex', prompt = 'x', events = true }",
        &[("FAKE_REPLAY", &codex)],
    );
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(kinds(&r), ["text", "tool_call", "tool_result", "text"]);
    assert_eq!(r["events"][1]["name"], "shell");
    assert!(
        r["events"][2]["text"]
            .as_str()
            .unwrap()
            .contains("# scratch"),
        "{r}"
    );
    assert_eq!(r["events"][2]["error"], false);
    assert!(r["usage"]["output_tokens"].as_u64().unwrap() > 0);
}

#[test]
fn an_unknown_model_is_named_on_both_providers() {
    let s = Sandbox::new();
    let r = s.json(
        "agent.run{ prompt = 'x', model = 'no-such-model-xyz' }",
        &[("FAKE_MODE", "unknown_model")],
    );
    assert_eq!(r["error"]["kind"], "unknown_model", "{r}");
    assert_eq!(
        r["error"]["message"],
        "claude does not know the model \"no-such-model-xyz\""
    );
    let r = s.json(
        "agent.run{ provider = 'codex', prompt = 'x', model = 'no-such-model-xyz' }",
        &[("FAKE_MODE", "unknown_model")],
    );
    assert_eq!(r["error"]["kind"], "unknown_model", "{r}");
    assert_eq!(
        r["error"]["message"],
        "The 'no-such-model-xyz' model is not supported when using Codex with a ChatGPT account."
    );
}

// Label, phase and progress (#55)

#[test]
fn runs_record_their_label_and_phase() {
    let s = Sandbox::new();
    let r = s.json(
        "agent.run{ prompt = 'x', label = 'pr-12', phase = 'review' }",
        &[],
    );
    assert_eq!(
        (r["label"].as_str(), r["phase"].as_str()),
        (Some("pr-12"), Some("review"))
    );
    let o = s.cli(&["runs", "--json"], &[]);
    let runs: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(runs[0]["label"], "pr-12");
    assert_eq!(runs[0]["phase"], "review");
}

#[test]
fn a_run_takes_its_task_phase_and_siblings_keep_their_own() {
    let s = Sandbox::new();
    let r = s.json(
        "phase('plan')
         local first = agent.run{ prompt = 'x' }
         local a = spawn(function() phase('a'); sleep('20ms'); return agent.run{ prompt = 'a' } end)
         local b = spawn(function() phase('b'); return agent.run{ prompt = 'b' } end)
         local inherited = spawn(function() return agent.run{ prompt = 'c' } end)
         local spawned = agent.spawn{ prompt = 'd' }
         local inner = scope(function(sc)
           phase('inner')
           return sc:spawn(function() return agent.run{ prompt = 'e' } end):await()
         end)
         local asked = try(agent.ask, { prompt = 'f', label = 'asked' })
         local explicit = agent.run{ prompt = 'g', phase = 'given' }
         return {
           first = first.phase, a = a:await().phase, b = b:await().phase,
           inherited = inherited:await().phase, spawned = spawned:await().phase,
           inner = inner.phase, explicit = explicit.phase, now = phase(),
         }",
        &[],
    );
    assert_eq!(
        r,
        serde_json::json!({ "first": "plan", "a": "a", "b": "b", "inherited": "plan",
            "spawned": "plan", "inner": "inner", "explicit": "given", "now": "plan" })
    );
    let o = s.cli(&["runs", "--json", "--limit", "100"], &[]);
    let runs: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap();
    let asked = runs.iter().find(|r| r["label"] == "asked").unwrap();
    assert_eq!(asked["phase"], "plan", "agent.ask takes the task's phase");
}

#[test]
fn progress_lines_go_to_stderr_when_asked_for() {
    let s = Sandbox::new();
    let code = "phase('review')\nreturn agent.run{ prompt = 'x', label = 'pr-12' }.status";
    let o = s.cli(&["--progress", "text", "eval", code], &[]);
    let stderr = String::from_utf8_lossy(&o.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 2, "{stderr}");
    assert!(
        lines[0].starts_with("timon: r_") && lines[0].ends_with(" [review] pr-12 started: claude"),
        "{stderr}"
    );
    assert!(
        lines[1].contains(" [review] pr-12 ok in ") && lines[1].ends_with(", $0.0125"),
        "{stderr}"
    );
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "ok\n",
        "stdout stays data"
    );

    let o = s.cli(&["--progress", "json", "eval", code], &[]);
    let events: Vec<serde_json::Value> = String::from_utf8_lossy(&o.stderr)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events[0]["event"], "run_started");
    assert_eq!(events[1]["event"], "run_finished");
    assert_eq!(
        (events[1]["phase"].as_str(), events[1]["status"].as_str()),
        (Some("review"), Some("ok"))
    );

    let o = s.cli(&["eval", code], &[]);
    assert!(
        o.stderr.is_empty(),
        "quiet by default when stderr is not a terminal: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn an_unrelated_process_under_a_recorded_pid_is_left_alone() {
    use std::os::unix::process::CommandExt;
    let s = Sandbox::new();
    // A dead timon, and a pid that now belongs to something that is not a provider.
    let mut gone = Command::new("true").spawn().unwrap();
    let dead = gone.id();
    gone.wait().unwrap();
    let mut other = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let runs = s.root().join("timon/state/runs");
    std::fs::create_dir_all(runs.join("r_1")).unwrap();
    std::fs::write(
        runs.join("index.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({ "id": "r_1", "status": "running", "access": "write", "pid": dead,
                "started_at": "2026-10-16T00:00:00Z", "provider_pid": other.id(), "provider_pgid": other.id() })
        ),
    )
    .unwrap();
    let run = &runs_json(&s)[0];
    let still = other.try_wait().unwrap().is_none();
    let _ = other.kill();
    let _ = other.wait();
    assert!(still, "an unrelated process was signalled");
    assert_eq!(run["status"], "interrupted");
    assert_eq!(run["provider_may_be_running"], true, "{run}");
}

// Nested budgets (#56)

#[test]
fn a_scope_budget_limits_the_runs_inside_it_and_its_tasks() {
    let s = Sandbox::new();
    let o = s.cli(
        &[
            "--max-runs",
            "10",
            "eval",
            "local ok, err = try(scope, function()
               agent.run{ prompt = '1' }
               scope(function() agent.run{ prompt = '2' } end)
               agent.run{ prompt = '3' }
             end, { budget = { runs = 2 } })
             local spawned = try(scope, function(sc)
               for i = 1, 3 do sc:spawn(agent.run, { prompt = 'task ' .. i }) end
             end, { budget = { runs = 2 } })
             return { kind = err.kind, message = err.message, hint = err.hint,
                      spawned = spawned, after = agent.run{ prompt = 'after' }.status }",
        ],
        &[],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(r["kind"], "budget_exceeded", "{r}");
    assert_eq!(
        r["message"],
        "the scope's budget has started 2 runs; the limit is 2"
    );
    assert_eq!(r["hint"], "raise budget.runs on the scope");
    assert_eq!(r["spawned"], false, "a third spawned run fails its scope");
    assert_eq!(
        r["after"], "ok",
        "outside the scope only the invocation's limit applies"
    );
    let started = runs_json(&s).as_array().unwrap().len();
    assert_eq!(started, 5, "2 in the first scope, 2 spawned, 1 after");
}

#[test]
fn a_claude_run_may_spend_only_what_every_budget_has_left() {
    let s = Sandbox::new();
    let o = s.timon(
        "local ok, err = try(scope, function()
           for i = 1, 3 do agent.run{ prompt = 'x' } end
         end, { budget = { usd = 0.02 } })
         return err.message",
        &[],
    );
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim(),
        "the scope's budget has spent $0.0250; the limit is $0.02"
    );
    assert!(has_pair(&s.args("claude", 0), "--max-budget-usd", "0.02"));
    assert!(
        has_pair(&s.args("claude", 1), "--max-budget-usd", "0.0075"),
        "{:?}",
        s.args("claude", 1)
    );
}

#[test]
fn reserve_fails_before_the_first_run_and_gives_back_what_it_did_not_use() {
    let s = Sandbox::new();
    let o = s.cli(
        &[
            "--max-runs",
            "3",
            "eval",
            "local started = false
             local ok, err = try(scope, function() started = true end,
               { budget = { runs = 5 }, reserve = true })
             return { message = err.message, started = started }",
        ],
        &[],
    );
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        r["message"],
        "the scope reserves 5 runs, and this invocation has 3 left"
    );
    assert_eq!(r["started"], false);

    let o = s.cli(
        &[
            "--max-runs",
            "3",
            "eval",
            "scope(function() agent.run{ prompt = 'in' } end, { budget = { runs = 2 }, reserve = true })
             agent.run{ prompt = 'out 1' }
             agent.run{ prompt = 'out 2' }
             local ok, err = try(agent.run, { prompt = 'out 3' })
             return err.message",
        ],
        &[],
    );
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim(),
        "this invocation has started 3 runs; the limit is 3",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );

    let o = s.timon("scope(function() end, { reserve = true })", &[]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("bad_option: reserve = true needs budget.runs")
    );
}
