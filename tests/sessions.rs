//! sessions against fixture archives in a sandbox. No test reads the real archives.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

const ID_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const ID_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const ID_C: &str = "cccccccc-3333-4333-8333-333333333333";
const ID_A2: &str = "aaaaaaaa-9999-4999-8999-999999999999";

struct Archive {
    dir: tempfile::TempDir,
}

impl Archive {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn write(&self, path: &Path, lines: &[serde_json::Value], age: Duration, compress: bool) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        if compress {
            let bytes = zstd::encode_all(text.as_bytes(), 3).unwrap();
            std::fs::write(path, bytes).unwrap();
        } else {
            std::fs::write(path, text).unwrap();
        }
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
    }

    fn claude(&self, project: &str, id: &str, lines: &[serde_json::Value], age_mins: u64) {
        let path = self
            .home()
            .join(".claude/projects")
            .join(project)
            .join(format!("{id}.jsonl"));
        self.write(&path, lines, Duration::from_secs(age_mins * 60), false);
    }

    fn codex(&self, id: &str, lines: &[serde_json::Value], age_mins: u64, compress: bool) {
        let ext = if compress { "jsonl.zst" } else { "jsonl" };
        let path = self
            .home()
            .join(".codex/sessions/2026/10/07")
            .join(format!("rollout-2026-10-07T12-00-00-{id}.{ext}"));
        self.write(&path, lines, Duration::from_secs(age_mins * 60), compress);
    }

    fn timon(&self, code: &str, env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_timon"));
        cmd.args(["eval", code])
            .current_dir(self.dir.path())
            .env("TIMON_HOME", self.dir.path().join("timon"))
            .env("HOME", self.home());
        for var in [
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "CLAUDE_CODE_SESSION_ID",
            "CODEX_THREAD_ID",
            "CODEX_SESSION_ID",
        ] {
            cmd.env_remove(var);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn json(&self, code: &str, env: &[(&str, &str)]) -> serde_json::Value {
        let o = self.timon(code, env);
        assert!(
            o.status.success(),
            "{code}\n{}",
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice(&o.stdout).unwrap()
    }
}

fn claude_user(text: &str, cwd: &str) -> serde_json::Value {
    serde_json::json!({ "type": "user", "cwd": cwd, "entrypoint": "cli",
        "message": { "role": "user", "content": text } })
}

fn codex_meta(id: &str, cwd: &str, source: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "type": "session_meta",
        "payload": { "id": id, "cwd": cwd, "originator": "codex_cli_rs", "source": source } })
}

fn codex_user(text: &str) -> serde_json::Value {
    serde_json::json!({ "type": "event_msg", "payload": { "type": "user_message", "message": text } })
}

fn fixture() -> Archive {
    let a = Archive::new();
    a.claude(
        "-work-timon",
        ID_A,
        &[
            serde_json::json!({ "type": "user", "cwd": "/work/timon", "entrypoint": "cli",
                "message": { "role": "user", "content": [
                    { "type": "text", "text": "<system-reminder>injected</system-reminder>" },
                    { "type": "text", "text": "fix   the\nbuild" } ] } }),
            serde_json::json!({ "type": "ai-title", "aiTitle": "Generated" }),
            serde_json::json!({ "type": "custom-title", "customTitle": "Build fix" }),
            serde_json::json!({ "type": "pr-link", "prUrl": "https://github.com/o/r/pull/7" }),
        ],
        5,
    );
    a.codex(
        ID_B,
        &[
            codex_meta(ID_B, "/work/codex", serde_json::json!("cli")),
            codex_user("add a flag"),
        ],
        10,
        true,
    );
    a.codex(
        ID_C,
        &[
            codex_meta(
                ID_C,
                "/work/codex",
                serde_json::json!({ "subagent": { "other": "guardian" } }),
            ),
            codex_user("review"),
        ],
        15,
        false,
    );
    a.codex(
        ID_A2,
        &[
            codex_meta(ID_A2, "/work/other", serde_json::json!("exec")),
            codex_user("scripted"),
        ],
        20,
        false,
    );
    a
}

#[test]
fn list_shows_both_backends_newest_first_and_counts_what_it_hides() {
    let a = fixture();
    let r = a.json("sessions.list{}", &[]);
    let rows = r["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], ID_A);
    assert_eq!(rows[0]["title"], "Build fix");
    assert_eq!(rows[0]["snippet"], "fix the build");
    assert_eq!(rows[0]["project"], "/work/timon");
    assert_eq!(
        rows[0]["pull_requests"],
        serde_json::json!(["https://github.com/o/r/pull/7"])
    );
    assert_eq!(rows[1]["id"], ID_B);
    assert_eq!(rows[1]["snippet"], "add a flag");
    assert_eq!(
        r["hidden"],
        serde_json::json!({ "subagents": 1, "automated": 1 })
    );

    let all = a.json("sessions.list{ subagents = true, automated = true }", &[]);
    assert_eq!(all["sessions"].as_array().unwrap().len(), 4);
    let codex = a.json("sessions.list{ backend = 'codex', automated = true }", &[]);
    assert!(
        codex["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["backend"] == "codex")
    );
}

#[test]
fn a_compressed_rollout_reads_like_a_plain_one() {
    let a = fixture();
    let s = a.json(&format!("sessions.show('{ID_B}')"), &[]);
    assert_eq!(s["compressed"], true);
    assert_eq!(s["cwd"], "/work/codex");
    assert_eq!(s["origin"], "cli");
    assert_eq!(
        s["open_with"],
        format!("cd '/work/codex' && codex resume {ID_B}")
    );
}

#[test]
fn ids_resolve_by_prefix_and_ambiguity_is_an_error() {
    let a = fixture();
    let s = a.json("sessions.show('bbbb')", &[]);
    assert_eq!(s["id"], ID_B);
    let o = a.timon("sessions.show('aaaaaaaa')", &[]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("ambiguous"), "{err}");
    assert!(
        err.contains(&format!("claude/{ID_A}")) && err.contains(&format!("codex/{ID_A2}")),
        "{err}"
    );
    let s = a.json("sessions.show('aaaaaaaa', { backend = 'codex' })", &[]);
    assert_eq!(s["id"], ID_A2);
    let o = a.timon("sessions.show('ffff')", &[]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("not_found"));
}

#[test]
fn a_moved_claude_session_is_listed_once_from_its_newest_file() {
    let a = fixture();
    a.claude(
        "-work-moved",
        ID_A,
        &[claude_user("moved here", "/work/moved")],
        1,
    );
    let r = a.json("sessions.list{}", &[]);
    let rows: Vec<&serde_json::Value> = r["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["id"] == ID_A)
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["project"], "/work/moved");
}

#[test]
fn filters_by_project_and_since() {
    let a = fixture();
    let r = a.json("sessions.list{ project = 'TIMON' }", &[]);
    assert_eq!(r["sessions"].as_array().unwrap().len(), 1);
    let r = a.json("sessions.list{ since = '7m' }", &[]);
    let ids: Vec<&str> = r["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [ID_A]);
}

#[test]
fn current_comes_from_the_host_session_variable() {
    let a = fixture();
    let o = a.timon("sessions.current() == nil", &[]);
    assert_eq!(String::from_utf8_lossy(&o.stdout), "true\n");
    let s = a.json("sessions.current()", &[("CLAUDE_CODE_SESSION_ID", ID_A)]);
    assert_eq!(s["title"], "Build fix");
    let s = a.json("sessions.current()", &[("CODEX_THREAD_ID", ID_B)]);
    assert_eq!(s["backend"], "codex");
}

#[test]
fn the_clis_own_home_variables_are_honored() {
    let a = fixture();
    let elsewhere = a.dir.path().join("elsewhere");
    a.write(
        &elsewhere
            .join("claude/projects/-p")
            .join(format!("{ID_C}.jsonl")),
        &[claude_user("from a custom config dir", "/p")],
        Duration::from_secs(1),
        false,
    );
    let r = a.json(
        "sessions.list{}",
        &[
            (
                "CLAUDE_CONFIG_DIR",
                elsewhere.join("claude").to_str().unwrap(),
            ),
            ("CODEX_HOME", elsewhere.join("codex").to_str().unwrap()),
        ],
    );
    let rows = r["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["snippet"], "from a custom config dir");
}

#[test]
fn sessions_timon_started_count_as_automated() {
    let a = fixture();
    let runs = a.dir.path().join("timon/state/runs");
    std::fs::create_dir_all(&runs).unwrap();
    let mut index = std::fs::File::create(runs.join("index.jsonl")).unwrap();
    writeln!(
        index,
        "{}",
        serde_json::json!({ "id": "r_1", "status": "ok", "session": ID_A })
    )
    .unwrap();
    let r = a.json("sessions.list{}", &[]);
    let ids: Vec<&str> = r["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [ID_B]);
    assert_eq!(r["hidden"]["automated"], 2);
}
