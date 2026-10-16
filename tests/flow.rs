//! The flow patterns (#59), with plain functions, commands and the fake providers.

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
        std::fs::create_dir_all(s.project().join(".timon")).unwrap();
        std::fs::create_dir_all(s.dir.path().join("logs")).unwrap();
        s
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("home/proj")
    }

    fn timon(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_timon"));
        cmd.args(args)
            .current_dir(self.project())
            .env("TIMON_HOME", self.dir.path().join("timon"))
            .env("HOME", self.dir.path().join("home"))
            .env("TIMON_CLAUDE_BIN", fixtures.join("fake-claude"))
            .env("TIMON_CODEX_BIN", fixtures.join("fake-codex"))
            .env("FAKE_LOG_DIR", self.dir.path().join("logs"));
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn eval(&self, code: &str, env: &[(&str, &str)]) -> String {
        let o = self.timon(&["eval", code], env);
        assert!(
            o.status.success(),
            "{code}\n{}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn fails(&self, code: &str) -> String {
        let o = self.timon(&["eval", code], &[]);
        assert!(!o.status.success(), "{code} should fail");
        String::from_utf8_lossy(&o.stderr).into_owned()
    }
}

#[test]
fn a_gate_is_a_predicate_a_command_or_an_agent() {
    let s = Sandbox::new();
    assert_eq!(
        s.eval(
            r#"local a = flow.check(function(v) return #v < 5, "at most 4" end, "toolong")
               local b = flow.check({ "true" })
               local c = flow.check({ "sh", "-c", "echo broken >&2; exit 3" })
               return a.status, a.passed, a.reason, b.passed, c.passed, c.code, c.reason"#,
            &[]
        ),
        "ok\nfalse\nat most 4\ntrue\nfalse\n3\nbroken\n"
    );
    let answers = r#"{"is it real": {"answer": false, "reason": "no such line"}, "review this": {"verdict": "request_changes", "summary": "two bugs", "findings": []}}"#;
    assert_eq!(
        s.eval(
            r#"local a = flow.check({ prompt = "is it real" }, { line = 3 })
               local r = flow.check(function() return agent.run{ prompt = "review this", output = "review" } end)
               return a.status, a.passed, a.reason, #a.runs, r.passed, r.reason, r.usage.usd"#,
            &[("FAKE_ANSWERS", answers)]
        ),
        "ok\nfalse\nno such line\n1\nfalse\ntwo bugs\n0.0125\n"
    );
    assert_eq!(
        s.eval(
            r#"local v = flow.check({ prompt = "FAIL here" })
               return v.status, v.passed"#,
            &[("FAKE_FAIL_ON", "FAIL")]
        ),
        "failed\nnil\n",
        "a check that did not run concludes nothing"
    );
    assert!(
        s.fails("flow.check(function() return 42 end)")
            .contains("a check function returns true or false")
    );
    assert!(
        s.fails("flow.check(12)")
            .contains("a gate is a function, a command, or an agent spec")
    );
}

#[test]
fn pipeline_stops_an_item_at_its_first_failing_stage() {
    let s = Sandbox::new();
    assert_eq!(
        s.eval(
            r#"local r = flow.pipeline({ "a", "bad", "c" }, {
                 { name = "upper", run = function(v) return v:upper() end },
                 { name = "check", run = function(v) if v == "BAD" then error("no") end return v .. "!" end },
               })
               return r.status, json.encode(r.values), r.items[2].stopped_at, r.items[2].stages[2].status, #r.items[1].stages"#,
            &[]
        ),
        "failed\n[\"A!\",null,\"C!\"]\ncheck\nfailed\n2\n"
    );
}

#[test]
fn vote_stops_at_quorum_and_reports_no_agreement() {
    let s = Sandbox::new();
    assert_eq!(
        s.eval(
            r#"local r = flow.vote{ n = 5, quorum = 2, run = function(i)
                 sleep((i * 30) .. "ms")
                 return i <= 2 and "a" or "b"
               end }
               return r.winner, r.agreed, #r.samples"#,
            &[]
        ),
        "a\ntrue\n2\n"
    );
    assert_eq!(
        s.eval(
            r#"local r = flow.vote{ n = 2, run = function(i) return i == 1 and "x" or "y" end }
               return r.status, r.winner, r.agreed"#,
            &[]
        ),
        "ok\nnil\nfalse\n"
    );
    assert_eq!(
        s.eval(
            r#"local r = flow.vote{ n = 3, run = function() error("down", 0) end }
               return r.status, r.winner, #r.samples, r.samples[1].error.message"#,
            &[]
        ),
        "failed\nnil\n3\ndown\n"
    );
    let answers = r#"{"sample 1": {"answer": true, "reason": "r"}, "sample 2": {"answer": false, "reason": "r"}, "sample 3": {"answer": true, "reason": "r"}}"#;
    assert_eq!(
        s.eval(
            r#"local r = flow.vote{ n = 3, quorum = 3, run = function(i)
                 return agent.run{ prompt = "sample " .. i, output = "yes_no" }
               end }
               return r.winner, r.agreed, json.encode(r.tally), r.usage.runs, r.usage.usd"#,
            &[("FAKE_ANSWERS", answers)]
        ),
        "true\nfalse\n{\"false\":1,\"true\":2}\n3\n0.0375\n"
    );
    assert!(
        s.fails("flow.vote{ runn = function() end }")
            .contains("did you mean \"run\"?")
    );
}

#[test]
fn best_of_scores_or_judges() {
    let s = Sandbox::new();
    assert_eq!(
        s.eval(
            r#"local r = flow.best_of{ n = 3, run = function(i) if i == 2 then error("x") end return i * 10 end,
                 score = function(v) return v end }
               return r.winner, r.value, r.candidates[2].status"#,
            &[]
        ),
        "3\n30\nfailed\n"
    );
    assert_eq!(
        s.eval(
            r#"local r = flow.best_of{ n = 3, run = function(i) return "c" .. i end,
                 judge = function(values) return { index = 2 } end }
               return r.winner, r.value"#,
            &[]
        ),
        "2\nc2\n"
    );
    assert_eq!(
        s.eval(
            r#"local r = flow.best_of{ n = 2, run = function(i) return i end, judge = function() return 9 end }
               return r.status, r.stopped_at"#,
            &[]
        ),
        "invalid\njudge\n"
    );
    assert!(
        s.fails("flow.best_of{ run = function() end }")
            .contains("score or judge")
    );
}

#[test]
fn refine_revises_until_a_gate_passes() {
    let s = Sandbox::new();
    std::fs::write(s.project().join("ok.txt"), "").unwrap();
    assert_eq!(
        s.eval(
            r#"local r = flow.refine{
                 draft = function() return "a draft that is far too long" end,
                 check = function(v) return #v <= 10, "at most 10" end,
                 revise = function(v, verdict, round) return v:sub(1, #v - 10) end,
               }
               return r.status, r.accepted, r.value, r.rounds, r.verdicts[1].reason"#,
            &[]
        ),
        "ok\ntrue\na draft \n3\nat most 10\n"
    );
    assert_eq!(
        s.eval(
            r#"local r = flow.refine{ rounds = 2,
                 draft = function() return "x" end,
                 check = { "test", "-f", "missing.txt" },
                 revise = function(v) return v .. "x" end }
               return r.status, r.accepted, r.value, r.rounds"#,
            &[]
        ),
        "ok\nfalse\nxx\n2\n"
    );
    assert_eq!(
        s.eval(
            r#"local r = flow.refine{ draft = function() error("blank page") end, check = { "true" }, revise = function() end }
               return r.status, r.stopped_at, r.accepted"#,
            &[]
        ),
        "failed\ndraft\nfalse\n"
    );
}

#[test]
fn verify_keeps_by_majority_all_or_any() {
    let s = Sandbox::new();
    let code = |keep: &str| {
        format!(
            r#"local r = flow.verify{{ items = {{ 1, 2, 3 }}, keep = "{keep}", checks = {{
                 function(n) return n >= 2 end,
                 function(n) return n >= 3 end,
                 function(n) return n ~= 2 end,
               }} }}
               return json.encode(r.kept), json.encode(r.rejected)"#
        )
    };
    assert_eq!(s.eval(&code("majority"), &[]), "[3]\n[1,2]\n");
    assert_eq!(s.eval(&code("all"), &[]), "[3]\n[1,2]\n");
    assert_eq!(s.eval(&code("any"), &[]), "[1,2,3]\n[]\n");
    let answers = r#"{"off by one": {"answer": true, "reason": "line 3 reads past the end"}, "style": {"answer": false, "reason": "not a bug"}}"#;
    assert_eq!(
        s.eval(
            r#"local findings = { { title = "off by one" }, { title = "style" } }
               local r = flow.verify{ items = findings, checks = {
                 { prompt = "Is this finding real?" },
                 { prompt = "Is this finding real? Check again." },
               } }
               return #r.kept, r.kept[1].title, r.rejected[1].title, r.usage.runs"#,
            &[("FAKE_ANSWERS", answers)]
        ),
        "1\noff by one\nstyle\n4\n"
    );
    assert_eq!(
        s.eval(
            r#"local r = flow.verify{ items = { "a" }, keep = "all", check = function() error("down") end }
               return #r.kept, r.items[1].verdicts[1].status"#,
            &[]
        ),
        "0\nfailed\n",
        "a gate that did not run keeps nothing under all"
    );
}

#[test]
fn retry_repeats_failures_and_passes_the_last_outcome() {
    let s = Sandbox::new();
    assert_eq!(
        s.eval(
            r#"local r = flow.retry(function(attempt, last)
                 if attempt < 3 then error("flaky " .. attempt, 0) end
                 return last.error.message
               end)
               return r.status, r.value, r.attempts"#,
            &[]
        ),
        "ok\nflaky 2\n3\n"
    );
    assert_eq!(
        s.eval(
            r#"local r = flow.retry(function() error("down") end, { times = 1 })
               return r.status, r.attempts"#,
            &[]
        ),
        "failed\n2\n"
    );
    assert_eq!(
        s.eval(
            r#"local r = flow.retry(function(attempt)
                 return agent.run{ prompt = attempt == 1 and "FAIL first" or "fine" }
               end)
               return r.status, r.attempts, r.usage.runs"#,
            &[("FAKE_FAIL_ON", "FAIL")]
        ),
        "ok\n2\n2\n"
    );
}

#[test]
fn plan_runs_steps_after_their_needs_and_stops_at_a_failure() {
    let s = Sandbox::new();
    assert_eq!(
        s.eval(
            r#"local r = flow.plan{ steps = {
                 plan = { run = function() return { "a", "b" } end },
                 build = { needs = { "plan" }, run = function(deps) return #deps.plan end },
                 docs = { needs = { "plan" }, run = function() return "docs" end },
                 ship = { needs = { "build", "docs" }, run = function(deps) return deps.build .. deps.docs end },
               } }
               return r.status, r.value.ship, r.order[1], r.order[4], #r.skipped"#,
            &[]
        ),
        "ok\n2docs\nplan\nship\n0\n"
    );
    assert_eq!(
        s.eval(
            r#"local r = flow.plan{ steps = {
                 a = { run = function() error("broke") end },
                 b = { needs = { "a" }, run = function() return 1 end },
                 c = { run = function() return 2 end },
               } }
               return r.status, r.stopped_at, json.encode(r.skipped), r.steps.c.status"#,
            &[]
        ),
        "failed\na\n[\"b\"]\nok\n"
    );
    assert!(
        s.fails("flow.plan{ steps = { a = { needs = { 'b' }, run = function() end } } }")
            .contains("step \"a\" needs \"b\", which is not a step")
    );
    assert!(
        s.fails("flow.plan{ steps = { a = { needs = { 'b' }, run = function() end }, b = { needs = { 'a' }, run = function() end } } }")
            .contains("a cycle in the steps: a -> b -> a")
    );
}

#[test]
fn a_cancelled_pattern_stops_with_its_scope() {
    let s = Sandbox::new();
    let start = std::time::Instant::now();
    assert_eq!(
        s.eval(
            r#"return par.race(
                 function() return flow.vote{ n = 3, run = function() sleep("5s") return "slow" end } end,
                 function() return "fast" end
               )"#,
            &[]
        ),
        "fast\n2\n"
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(4));
}

#[test]
fn a_review_with_a_verifier_and_a_vote_of_three_run_as_tasks() {
    let s = Sandbox::new();
    std::fs::write(
        s.project().join(".timon/tasks.lua"),
        r#"return {
  review = {
    description = "Review, then keep the findings two verifiers agree are real",
    run = function()
      local review = agent.run{ prompt = "Review the change", output = "review" }
      if review.status ~= "ok" then return review end
      local r = flow.verify{ items = review.answer.findings, checks = {
        { prompt = "Is this finding real? Verifier A." },
        { prompt = "Is this finding real? Verifier B." },
      } }
      return { kept = tbl.map(r.kept, function(f) return f.title end), runs = r.usage.runs + 1 }
    end,
  },
  decide = {
    description = "Ask three times whether to ship, and take the majority",
    run = function()
      local r = flow.vote{ n = 3, run = function(i)
        return agent.run{ prompt = "Ship it? Voter " .. i, output = "yes_no" }
      end }
      return { ship = r.winner, agreed = r.agreed }
    end,
  },
}
"#,
    )
    .unwrap();
    assert!(s.timon(&["trust"], &[]).status.success());
    let answers = serde_json::json!({
        "Review the change": { "verdict": "request_changes", "summary": "two findings", "findings": [
            { "severity": "high", "file": "src/a.rs", "title": "off by one", "detail": "d" },
            { "severity": "low", "file": "src/b.rs", "title": "naming", "detail": "d" },
        ] },
        "off by one": { "answer": true, "reason": "real" },
        "naming": { "answer": false, "reason": "a matter of taste" },
        "Voter 2": { "answer": false, "reason": "wait" },
        "Ship it?": { "answer": true, "reason": "ready" },
    })
    .to_string();
    let o = s.timon(&["review"], &[("FAKE_ANSWERS", &answers)]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "{\"kept\":[\"off by one\"],\"runs\":5}\n"
    );
    let o = s.timon(&["decide"], &[("FAKE_ANSWERS", &answers)]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "{\"agreed\":true,\"ship\":\"true\"}\n"
    );
    let runs = s.timon(&["runs", "--scripts", "--json"], &[]);
    let list: serde_json::Value = serde_json::from_slice(&runs.stdout).unwrap();
    let counts: Vec<i64> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["runs"].as_i64().unwrap())
        .collect();
    assert_eq!(counts.len(), 2);
    assert_eq!(
        counts[1], 5,
        "the review task's record counts all five agent runs"
    );
}
