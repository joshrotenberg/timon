//! Script runs as records (#32) and resume from cached runs (#57), with fake providers.

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
        std::fs::create_dir_all(s.work()).unwrap();
        s
    }

    fn logs(&self) -> PathBuf {
        self.dir.path().join("logs")
    }

    fn work(&self) -> PathBuf {
        self.dir.path().join("work")
    }

    fn script(&self, name: &str, text: &str) -> PathBuf {
        let path = self.work().join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn timon(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_timon"));
        cmd.args(args)
            .current_dir(self.work())
            .env("TIMON_HOME", self.dir.path().join("timon"))
            .env("HOME", self.dir.path())
            .env("TIMON_CLAUDE_BIN", fixtures.join("fake-claude"))
            .env("TIMON_CODEX_BIN", fixtures.join("fake-codex"))
            .env("FAKE_LOG_DIR", self.logs());
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let o = self.timon(args, &[]);
        assert!(o.status.success(), "{args:?}: {}", err(&o));
        serde_json::from_slice(&o.stdout).unwrap()
    }

    /// How many times the fake providers have been started.
    fn calls(&self) -> usize {
        std::fs::read_dir(self.logs()).unwrap().count()
    }

    /// Makes the working directory a git repository with one empty commit.
    fn git_init(&self) {
        for args in [
            &["init", "-q", "-b", "main"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@e",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "start",
            ],
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(self.work())
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    /// The newest script run.
    fn last_script(&self) -> serde_json::Value {
        let list = self.json(&["runs", "--scripts", "--json"]);
        let id = list[0]["id"].as_str().unwrap().to_owned();
        self.json(&["runs", "show", &id])
    }
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn a_script_run_records_its_runs_and_their_totals() {
    let s = Sandbox::new();
    let path = s.script(
        "three.lua",
        "-- Three runs.\nfor i = 1, 3 do agent.run{ prompt = 'run ' .. i, output = 'yes_no' } end\nreturn 'done'",
    );
    let o = s.timon(&[path.to_str().unwrap(), "x"], &[("FAKE_AUTO_ANSWER", "1")]);
    assert!(o.status.success(), "{}", err(&o));
    let shown = s.last_script();
    let (request, result) = (&shown["request"], &shown["result"]);
    assert_eq!(request["name"], "three");
    assert_eq!(request["args"], serde_json::json!(["x"]));
    assert_eq!(request["trigger"], "manual");
    assert_eq!(request["source_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(result["status"], "ok");
    assert_eq!(result["runs"], 3);
    assert!(
        (result["usd"].as_f64().unwrap() - 0.0375).abs() < 1e-9,
        "{result}"
    );
    assert_eq!(shown["runs"].as_array().unwrap().len(), 3);
    let id = result["id"].as_str().unwrap();
    let runs = s.json(&["eval", &format!("return agent.runs{{ parent = '{id}' }}")]);
    let runs = runs.as_array().unwrap();
    assert_eq!(runs.len(), 3);
    assert!(
        runs.iter()
            .all(|r| r["parent"] == id && r["contract"] == "yes_no"),
        "{runs:?}"
    );
}

#[test]
fn a_resumed_script_spends_only_the_runs_that_did_not_succeed() {
    let s = Sandbox::new();
    let path = s.script(
        "steps.lua",
        "local a = agent.ask('plan the change')\nlocal b = agent.ask('write the change')\nreturn agent.ask('review it, FAIL here the first time')",
    );
    let path = path.to_str().unwrap();
    let o = s.timon(&[path], &[("FAKE_FAIL_ON", "FAIL")]);
    assert!(!o.status.success(), "the third run fails the script");
    assert_eq!(s.calls(), 3);
    let first = s.last_script();
    assert_eq!(first["result"]["status"], "failed");
    let first_id = first["result"]["id"].as_str().unwrap().to_owned();

    let o = s.timon(&["--resume", &first_id[..8], path], &[]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(s.calls(), 4, "only the failed run ran again");
    let second = s.last_script();
    assert_eq!(second["request"]["trigger"], "resume");
    assert_eq!(second["request"]["resumed_from"], first_id);
    assert_eq!(second["result"]["cached"], 2);
    let cached: Vec<bool> = second["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["cached"] == true)
        .collect();
    assert_eq!(cached, [true, true, false]);
}

#[test]
fn a_concurrent_fan_out_resumes_whatever_order_it_finishes_in() {
    let s = Sandbox::new();
    let path = s.script(
        "fan.lua",
        "return par.map({ 'a', 'b', 'a', 'c' }, function(x) return agent.run{ prompt = x }.text end)",
    );
    let path = path.to_str().unwrap();
    let o = s.timon(&[path], &[]);
    assert!(o.status.success(), "{}", err(&o));
    let first = String::from_utf8_lossy(&o.stdout).into_owned();
    let id = s.last_script()["result"]["id"].as_str().unwrap().to_owned();
    let o = s.timon(&["--resume", &id, path], &[]);
    assert!(o.status.success(), "{}", err(&o));
    // Four fakes start at once and race on their log files, so count the run records.
    let runs = s.json(&["runs", "--json", "--limit", "100"]);
    assert_eq!(runs.as_array().unwrap().len(), 4, "nothing ran again");
    assert_eq!(String::from_utf8_lossy(&o.stdout), first);
    assert_eq!(s.last_script()["result"]["cached"], 4);
}

#[test]
fn writes_are_reused_only_when_idempotent_or_in_an_unchanged_worktree() {
    let s = Sandbox::new();
    s.git_init();
    let path = s.script(
        "writes.lua",
        "agent.run{ prompt = 'edit here', access = 'write' }
         agent.run{ prompt = 'edit again', access = 'write', idempotent = true }
         return agent.run{ prompt = 'edit apart', access = 'write', worktree = true }.worktree.path",
    );
    let path = path.to_str().unwrap();
    let touch = [("FAKE_TOUCH", "note.txt")];
    let o = s.timon(&[path], &touch);
    assert!(o.status.success(), "{}", err(&o));
    let worktree = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    let id = s.last_script()["result"]["id"].as_str().unwrap().to_owned();

    let o = s.timon(&["--resume", &id, path], &touch);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(s.calls(), 4, "only the plain write ran again");
    let cached: Vec<bool> = s.last_script()["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["cached"] == true)
        .collect();
    assert_eq!(cached, [false, true, true]);

    std::fs::write(worktree.join("note.txt"), "changed since\n").unwrap();
    let o = s.timon(&["--resume", &id, path], &touch);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        s.calls(),
        6,
        "a worktree changed since the run is not reused"
    );
}

#[test]
fn a_worktree_fingerprint_reads_untracked_files_git_quotes() {
    let mut names = vec!["note.txt", "with space.txt", "caf\u{e9}.txt"];
    if cfg!(unix) {
        names.push("quote\"back\\slash\nnewline.txt");
    }
    for name in names {
        let s = Sandbox::new();
        s.git_init();
        let path = s.script(
            "apart.lua",
            "return agent.run{ prompt = 'synthetic write', access = 'write', worktree = true }.worktree.path",
        );
        let path = path.to_str().unwrap();
        let touch = [("FAKE_TOUCH", name)];
        let o = s.timon(&[path], &touch);
        assert!(o.status.success(), "{name:?}: {}", err(&o));
        let worktree = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
        let id = s.last_script()["result"]["id"].as_str().unwrap().to_owned();

        let o = s.timon(&["--resume", &id, path], &touch);
        assert!(o.status.success(), "{name:?}: {}", err(&o));
        assert_eq!(s.calls(), 1, "{name:?}: an unchanged worktree is reused");

        std::fs::write(worktree.join(name), "changed since\n").unwrap();
        let o = s.timon(&["--resume", &id, path], &touch);
        assert!(o.status.success(), "{name:?}: {}", err(&o));
        assert_eq!(
            s.calls(),
            2,
            "{name:?}: a changed untracked file is not reused"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_worktree_with_content_the_fingerprint_cannot_read_is_not_reused() {
    use std::os::unix::fs::PermissionsExt;
    let s = Sandbox::new();
    s.git_init();
    // Each new worktree gets a nested repository, which git lists as one untracked directory.
    let hook = s.work().join(".git/hooks/post-checkout");
    std::fs::write(
        &hook,
        "#!/bin/sh\nunset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE\ngit init -q sub && echo one > sub/f\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = s.script(
        "apart.lua",
        "return agent.run{ prompt = 'synthetic write', access = 'write', worktree = true }.worktree.path",
    );
    let path = path.to_str().unwrap();
    let touch = [("FAKE_TOUCH", "note.txt")];
    let o = s.timon(&[path], &touch);
    assert!(o.status.success(), "{}", err(&o));
    let worktree = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
    assert!(
        worktree.join("sub/f").exists(),
        "the hook made no nested repository"
    );
    let id = s.last_script()["result"]["id"].as_str().unwrap().to_owned();

    std::fs::write(worktree.join("sub/f"), "two\n").unwrap();
    let o = s.timon(&["--resume", &id, path], &touch);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        s.calls(),
        2,
        "content the fingerprint cannot read is not reused"
    );
}

#[test]
fn a_script_can_declare_its_metadata_and_budget() {
    let s = Sandbox::new();
    let path = s.script(
        "meta.lua",
        "return {
           description = 'Two runs, one allowed',
           phases = { 'try' },
           budget = { runs = 1 },
           run = function(n)
             agent.run{ prompt = 'one ' .. n }
             return agent.run{ prompt = 'two' }
           end,
         }",
    );
    let o = s.timon(&[path.to_str().unwrap(), "7"], &[]);
    assert!(
        err(&o).contains("budget_exceeded: the scope's budget has started 1 runs"),
        "{}",
        err(&o)
    );
    let shown = s.last_script();
    assert_eq!(
        shown["request"]["metadata"],
        serde_json::json!({ "description": "Two runs, one allowed", "phases": ["try"], "budget": { "runs": 1 } })
    );
    let prompt = std::fs::read_to_string(s.logs().join("claude-0.json")).unwrap();
    assert!(
        prompt.contains("one 7"),
        "run gets the script's arguments: {prompt}"
    );

    let bad = s.script(
        "bad.lua",
        "return { descripton = 'x', run = function() end }",
    );
    let o = s.timon(&[bad.to_str().unwrap()], &[]);
    assert!(
        err(&o).contains("bad_script: unknown script field \"descripton\""),
        "{}",
        err(&o)
    );
    assert!(
        err(&o).contains("did you mean \"description\"?"),
        "{}",
        err(&o)
    );
    let params = s.script("params.lua", "return { params = {}, run = function() end }");
    let o = s.timon(&[params.to_str().unwrap()], &[]);
    assert!(err(&o).contains("unsupported"), "{}", err(&o));
}

#[test]
fn eval_is_not_a_script_run() {
    let s = Sandbox::new();
    let o = s.timon(&["eval", "agent.run{ prompt = 'x' }"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    let list = s.json(&["runs", "--scripts", "--json"]);
    assert_eq!(list, serde_json::json!([]));
    let runs = s.json(&["runs", "--json"]);
    assert!(runs[0]["parent"].is_null());
}
