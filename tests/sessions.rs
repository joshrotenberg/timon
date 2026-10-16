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
        self.cli(&["eval", code], env)
    }

    fn cli(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_timon"));
        cmd.args(args)
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
fn filters_by_directory_whole_components_only() {
    let a = fixture();
    let ids = |code: &str| -> Vec<String> {
        a.json(code, &[])["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(ids("sessions.list{ dir = '/work/timon' }"), [ID_A]);
    assert_eq!(ids("sessions.list{ dir = '/work' }"), [ID_A, ID_B]);
    assert!(ids("sessions.list{ dir = '/work/tim' }").is_empty());
    assert_eq!(
        ids("sessions.list{ dir = '/work/codex', subagents = true, automated = true }"),
        [ID_B, ID_C]
    );
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

// Transcripts (#41)

fn kinds(page: &serde_json::Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let kind = i["kind"].as_str().unwrap().to_owned();
            match i["count"].as_u64() {
                Some(n) => format!("{kind}x{n}"),
                None => kind,
            }
        })
        .collect()
}

fn claude_transcript(a: &Archive) {
    a.claude(
        "-work-t",
        ID_C,
        &[
            claude_user("hello there", "/work/t"),
            serde_json::json!({ "type": "assistant", "message": { "role": "assistant", "content": [
                { "type": "thinking", "thinking": "considering it" },
                { "type": "text", "text": "Reading the file." },
                { "type": "tool_use", "id": "t1", "name": "Read", "input": { "file_path": "a.rs" } } ] } }),
            serde_json::json!({ "type": "user", "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "fn main() {}" } ] } }),
            serde_json::json!({ "type": "ai-title", "aiTitle": "t" }),
            serde_json::json!({ "type": "summary", "summary": "s" }),
            serde_json::json!({ "type": "assistant", "message": { "role": "assistant", "content": [
                { "type": "text", "text": "The key is sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123" } ] } }),
        ],
        1,
    );
}

#[test]
fn a_claude_transcript_projects_every_record_kind() {
    let a = Archive::new();
    claude_transcript(&a);
    let p = a.json(&format!("sessions.transcript('{ID_C}')"), &[]);
    assert_eq!(
        kinds(&p),
        [
            "user",
            "reasoning",
            "assistant",
            "tool_call",
            "tool_result",
            "omittedx1",
            "omittedx1",
            "assistant"
        ]
    );
    let items = p["items"].as_array().unwrap();
    assert_eq!(items[3]["name"], "Read");
    assert!(items[3]["text"].as_str().unwrap().contains("a.rs"));
    assert_eq!(items[4]["text"], "fn main() {}");
    assert_eq!(
        (items[5]["name"].as_str(), items[6]["name"].as_str()),
        (Some("ai-title"), Some("summary"))
    );
    assert!(
        items[7]["text"]
            .as_str()
            .unwrap()
            .contains("[redacted:anthropic]")
    );
    assert_eq!(p["has_more"], false);
    assert_eq!(p["total"], 8);
}

#[test]
fn transcripts_page_and_filter_by_kind_keeping_indexes() {
    let a = Archive::new();
    claude_transcript(&a);
    let first = a.json(
        &format!("sessions.transcript('{ID_C}', {{ limit = 3 }})"),
        &[],
    );
    assert_eq!(first["items"].as_array().unwrap().len(), 3);
    assert_eq!(first["has_more"], true);
    assert_eq!(first["next_offset"], 3);
    let rest = a.json(
        &format!("sessions.transcript('{ID_C}', {{ offset = 3 }})"),
        &[],
    );
    assert_eq!(rest["items"][0]["index"], 3);
    let talk = a.json(
        &format!("sessions.transcript('{ID_C}', {{ kinds = {{ 'user', 'assistant' }} }})"),
        &[],
    );
    let indexes: Vec<u64> = talk["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["index"].as_u64().unwrap())
        .collect();
    assert_eq!(indexes, [0, 2, 7]);
    let o = a.timon(
        &format!("sessions.transcript('{ID_C}', {{ kinds = {{ 'chat' }} }})"),
        &[],
    );
    assert!(
        String::from_utf8_lossy(&o.stderr)
            .contains("option \"kinds[1]\" to sessions.transcript must be one of \"user\""),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn codex_lines(id: &str) -> Vec<serde_json::Value> {
    vec![
        codex_meta(id, "/work/c", serde_json::json!("cli")),
        codex_user("add a flag"),
        serde_json::json!({ "type": "response_item", "payload": { "type": "message", "role": "user",
            "content": [{ "type": "input_text", "text": "add a flag" }] } }),
        serde_json::json!({ "type": "response_item", "payload": { "type": "function_call",
            "name": "shell", "arguments": "{\"command\":[\"ls\"]}", "call_id": "c1" } }),
        serde_json::json!({ "type": "response_item", "payload": { "type": "function_call_output",
            "call_id": "c1", "output": "src" } }),
        serde_json::json!({ "type": "event_msg", "payload": { "type": "agent_message", "message": "Added --verbose." } }),
        serde_json::json!({ "type": "response_item", "payload": { "type": "message", "role": "assistant",
            "content": [{ "type": "output_text", "text": "Added --verbose." }] } }),
    ]
}

#[test]
fn a_codex_transcript_drops_the_copies_between_streams_and_reads_compressed_files() {
    let a = Archive::new();
    a.codex(ID_A, &codex_lines(ID_A), 1, false);
    a.codex(ID_B, &codex_lines(ID_B), 2, true);
    let plain = a.json(&format!("sessions.transcript('{ID_A}')"), &[]);
    let talk: Vec<String> = kinds(&plain)
        .into_iter()
        .filter(|k| !k.starts_with("omitted"))
        .collect();
    assert_eq!(
        talk,
        ["user", "tool_call", "tool_result", "assistant"],
        "{plain}"
    );
    let compressed = a.json(&format!("sessions.transcript('{ID_B}')"), &[]);
    assert_eq!(kinds(&plain), kinds(&compressed));
    assert_eq!(plain["items"], compressed["items"]);
}

// Search and the index (#42)

/// Sessions old enough to be outside the 10-minute active window.
fn searchable() -> Archive {
    let a = Archive::new();
    a.claude(
        "-work-s",
        ID_A,
        &[
            claude_user("please fix the flaky parser test", "/work/s"),
            serde_json::json!({ "type": "assistant", "message": { "role": "assistant", "content": [
                { "type": "text", "text": "The parser drops quoted colons." },
                { "type": "tool_use", "id": "t1", "name": "Grep", "input": { "pattern": "zanzibar" } } ] } }),
        ],
        60,
    );
    a.codex(
        ID_B,
        &[
            codex_meta(ID_B, "/work/c", serde_json::json!("cli")),
            codex_user("compress the quokka archive"),
        ],
        90,
        true,
    );
    a.codex(
        ID_C,
        &[
            codex_meta(ID_C, "/work/c", serde_json::json!("exec")),
            codex_user("scripted quokka run"),
        ],
        120,
        false,
    );
    a
}

fn hit_ids(r: &serde_json::Value) -> Vec<String> {
    r["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn search_finds_words_in_both_backends_with_excerpts() {
    let a = searchable();
    let r = a.json("sessions.search{ query = 'parser colons' }", &[]);
    assert_eq!(hit_ids(&r), [ID_A]);
    let m = &r["hits"][0]["matches"][0];
    assert_eq!(m["kind"], "assistant");
    assert_eq!(m["index"], 1);
    assert!(m["excerpt"].as_str().unwrap().contains("[parser]"), "{m}");
    assert_eq!(r["exhaustive"], true);

    let r = a.json("sessions.search{ query = 'quokka' }", &[]);
    assert_eq!(
        hit_ids(&r),
        [ID_B],
        "the compressed rollout is searchable; exec is hidden"
    );
    assert_eq!(r["hidden"]["automated"], 1);
    assert!(r["note"].as_str().unwrap().contains("automated = true"));
    let r = a.json("sessions.search{ query = 'quokka', automated = true }", &[]);
    assert_eq!(r["returned"], 2);
}

#[test]
fn match_indexes_are_transcript_indexes() {
    let a = searchable();
    let r = a.json("sessions.search{ query = 'zanzibar' }", &[]);
    let m = &r["hits"][0]["matches"][0];
    assert_eq!(m["kind"], "tool_call");
    let page = a.json(
        &format!(
            "sessions.transcript('{ID_A}', {{ offset = {}, limit = 1 }})",
            m["index"]
        ),
        &[],
    );
    assert_eq!(page["items"][0]["kind"], "tool_call");
    assert!(
        page["items"][0]["text"]
            .as_str()
            .unwrap()
            .contains("zanzibar")
    );
}

#[test]
fn search_modes() {
    let a = searchable();
    let terms = a.json("sessions.search{ query = 'colons quoted' }", &[]);
    assert_eq!(terms["returned"], 1, "terms match in any order");
    let phrase = a.json(
        "sessions.search{ query = 'colons quoted', mode = 'phrase' }",
        &[],
    );
    assert_eq!(phrase["returned"], 0, "a phrase keeps its order");
    assert_eq!(phrase["absence_is_proof"], true);
    let phrase = a.json(
        "sessions.search{ query = 'quoted colons', mode = 'phrase' }",
        &[],
    );
    assert_eq!(phrase["returned"], 1);
    let fts = a.json(
        "sessions.search{ query = 'zanzibar OR quokka', mode = 'fts' }",
        &[],
    );
    assert_eq!(fts["returned"], 2);
    let regex = a.json("sessions.search{ query = 'fla.y', mode = 'regex' }", &[]);
    assert_eq!(hit_ids(&regex), [ID_A]);
    let o = a.timon("sessions.search{ query = 'x', mode = 'fuzzy' }", &[]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("must be one of \"terms\", \"phrase\""));
    let o = a.timon("sessions.search{ query = '   ' }", &[]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("bad_query"));
}

#[test]
fn the_current_session_and_recent_ones_are_left_out_and_counted() {
    let a = searchable();
    let r = a.json(
        "sessions.search{ query = 'parser' }",
        &[("CLAUDE_CODE_SESSION_ID", ID_A)],
    );
    assert_eq!(r["returned"], 0);
    assert_eq!(r["hidden"]["excluded"], 1);
    assert_eq!(r["absence_is_proof"], false);
    let r = a.json(
        "sessions.search{ query = 'parser', exclude = false }",
        &[("CLAUDE_CODE_SESSION_ID", ID_A)],
    );
    assert_eq!(r["returned"], 1);

    a.claude(
        "-work-new",
        ID_A2,
        &[claude_user("fresh parser work", "/work/new")],
        0,
    );
    let r = a.json("sessions.search{ query = 'fresh parser' }", &[]);
    assert_eq!(r["hidden"]["active"], 1);
    let r = a.json(
        "sessions.search{ query = 'fresh parser', active = true }",
        &[],
    );
    assert_eq!(r["returned"], 1);
}

#[test]
fn the_index_follows_appends_removals_and_rebuilds() {
    let a = searchable();
    assert_eq!(
        a.json("sessions.search{ query = 'kumquat' }", &[])["returned"],
        0
    );
    let file = a
        .home()
        .join(".claude/projects/-work-s")
        .join(format!("{ID_A}.jsonl"));
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap();
    writeln!(f, "{}", claude_user("now about kumquat", "/work/s")).unwrap();
    drop(f);
    let old = SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let r = a.json("sessions.search{ query = 'kumquat' }", &[]);
    assert_eq!(r["returned"], 1);
    assert_eq!(r["coverage"]["refresh"]["appended"], 1);

    std::fs::remove_file(&file).unwrap();
    let r = a.json("sessions.search{ query = 'kumquat' }", &[]);
    assert_eq!(r["returned"], 0);
    assert_eq!(r["coverage"]["refresh"]["removed"], 1);

    let fixtures_home = a.home();
    let o = Command::new(env!("CARGO_BIN_EXE_timon"))
        .args(["sessions", "index", "--rebuild"])
        .current_dir(a.dir.path())
        .env("TIMON_HOME", a.dir.path().join("timon"))
        .env("HOME", &fixtures_home)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(report["reindexed"], 2);
    assert_eq!(report["pending"], 0);
}

/// #49: an index built in pieces (a file cut mid-line, then appended to twice) finds what an
/// index of the whole file finds, with the same match counts, across record kinds.
#[test]
fn an_index_built_in_pieces_counts_what_a_fresh_one_does() {
    let mut text = String::new();
    for i in 0..40 {
        for line in [
            claude_user(&format!("step {i}: look at the walrus module"), "/work/w"),
            serde_json::json!({ "type": "assistant", "message": { "role": "assistant", "content": [
                { "type": "text", "text": format!("walrus check {i}") },
                { "type": "tool_use", "id": format!("t{i}"), "name": "Bash",
                  "input": { "command": format!("grep walrus src/m{i}.rs") } } ] } }),
            serde_json::json!({ "type": "user", "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": format!("t{i}"),
                  "content": format!("src/m{i}.rs: a walrus here") } ] } }),
        ] {
            text.push_str(&format!("{line}\n"));
        }
    }
    let file = |a: &Archive| {
        let dir = a.home().join(".claude/projects/-work-w");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{ID_A}.jsonl"))
    };
    let search = |a: &Archive| {
        a.json(
            "sessions.search{ query = 'walrus', per_session = 1, active = true }",
            &[],
        )
    };
    let counts = |r: &serde_json::Value| -> Vec<(String, i64)> {
        r["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["id"].as_str().unwrap().to_owned(),
                    h["match_count"].as_i64().unwrap(),
                )
            })
            .collect()
    };

    let whole = Archive::new();
    std::fs::write(file(&whole), &text).unwrap();
    let expected = counts(&search(&whole));
    assert_eq!(
        expected,
        [(ID_A.to_owned(), 160)],
        "four kinds of record, 40 times"
    );

    let pieces = Archive::new();
    let path = file(&pieces);
    let cuts = [0, text.len() / 3 + 7, 2 * text.len() / 3 + 11, text.len()];
    assert_ne!(
        text.as_bytes()[cuts[1] - 1],
        b'\n',
        "the first cut is mid-line"
    );
    for w in cuts.windows(2) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(&text.as_bytes()[w[0]..w[1]]).unwrap();
        drop(f);
        let r = search(&pieces);
        if w[1] == text.len() {
            assert_eq!(counts(&r), expected);
            assert_eq!(r["coverage"]["refresh"]["appended"], 1, "{r}");
        }
    }
}

// Summaries (#43)

fn fake_claude() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-claude")
        .display()
        .to_string()
}

fn logged(dir: &Path) -> Vec<serde_json::Value> {
    let mut calls: Vec<(usize, serde_json::Value)> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n = name["claude-".len()..name.len() - ".json".len()]
                .parse()
                .unwrap();
            let text = std::fs::read_to_string(e.path()).unwrap();
            (n, serde_json::from_str(&text).unwrap())
        })
        .collect();
    calls.sort_by_key(|(n, _)| *n);
    calls.into_iter().map(|(_, c)| c).collect()
}

#[test]
fn a_summary_is_redacted_ephemeral_and_cached_until_the_session_changes() {
    let a = Archive::new();
    let secret = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123";
    let lines = [
        claude_user(&format!("deploy with key {secret}"), "/work/app"),
        serde_json::json!({ "type": "assistant", "message": { "role": "assistant",
            "content": [{ "type": "text", "text": "deployed" }] } }),
    ];
    a.claude("-work-app", ID_A, &lines, 10);
    let log = a.dir.path().join("log");
    std::fs::create_dir_all(&log).unwrap();
    let claude = fake_claude();
    let log_dir = log.display().to_string();
    let env = [
        ("TIMON_CLAUDE_BIN", claude.as_str()),
        ("FAKE_LOG_DIR", log_dir.as_str()),
        ("FAKE_TEXT", "Goal: deploy."),
    ];
    let summarize = |opts: &str| {
        a.json(
            &format!("return sessions.summarize(\"aaaaaaaa-1111\", {opts})"),
            &env,
        )
    };

    let s = summarize("{}");
    assert_eq!(s["text"], "Goal: deploy.");
    assert_eq!(s["id"], ID_A);
    assert_eq!(s["model"], "haiku");
    assert_eq!(s["cached"], false);
    assert_eq!(s["usd"], 0.0125);
    assert_eq!(
        (s["chunks"].as_u64(), s["chunks_summarized"].as_u64()),
        (Some(1), Some(1))
    );
    let calls = logged(&log);
    assert_eq!(calls.len(), 1);
    let prompt = calls[0]["prompt"].as_str().unwrap();
    assert!(prompt.contains("deployed") && prompt.contains("[redacted:anthropic]"));
    assert!(!prompt.contains(secret));
    let args: Vec<&str> = calls[0]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(args.contains(&"--no-session-persistence"), "{args:?}");
    assert!(
        args.windows(2).any(|w| w == ["--model", "haiku"]),
        "{args:?}"
    );
    assert!(args.windows(2).any(|w| w == ["--tools", ""]), "{args:?}");

    let again = summarize("{}");
    assert_eq!(
        (again["cached"].as_bool(), again["usd"].as_f64()),
        (Some(true), Some(0.0))
    );
    assert_eq!(logged(&log).len(), 1);

    assert_eq!(summarize("{ force = true }")["cached"], false);
    let sonnet = summarize("{ model = \"sonnet\" }");
    assert_eq!(
        (sonnet["cached"].as_bool(), sonnet["model"].as_str()),
        (Some(false), Some("sonnet"))
    );
    assert_eq!(summarize("{}")["model"], "sonnet");
    assert_eq!(logged(&log).len(), 3);

    let mut more = lines.to_vec();
    more.push(claude_user("and roll back", "/work/app"));
    a.claude("-work-app", ID_A, &more, 1);
    assert_eq!(summarize("{}")["cached"], false);
    assert_eq!(logged(&log).len(), 4);

    let runs = a.json("return agent.runs{}", &[]);
    assert_eq!(runs.as_array().unwrap().len(), 4);
    let o = a.timon(
        "return sessions.summarize(\"aaaaaaaa-1111\", { modle = \"x\" })",
        &env,
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("did you mean \"model\""));
}

#[test]
fn a_long_session_is_summarized_in_parts_and_merged_within_budget() {
    let a = Archive::new();
    // Records are capped at 4,000 characters each, so length comes from their number.
    let block = "word ".repeat(700);
    let lines: Vec<serde_json::Value> = (0..80)
        .map(|i| claude_user(&format!("part{i} {block}"), "/work/app"))
        .collect();
    a.claude("-work-app", ID_A, &lines, 10);
    let log = a.dir.path().join("log");
    std::fs::create_dir_all(&log).unwrap();
    let claude = fake_claude();
    let log_dir = log.display().to_string();
    let env = [
        ("TIMON_CLAUDE_BIN", claude.as_str()),
        ("FAKE_LOG_DIR", log_dir.as_str()),
        ("FAKE_TEXT", "- a bullet"),
    ];
    let s = a.json("return sessions.summarize(\"aaaaaaaa\")", &env);
    assert_eq!(
        (s["chunks"].as_u64(), s["chunks_summarized"].as_u64()),
        (Some(2), Some(2))
    );
    assert_eq!(s["usd"], 0.0125 * 3.0);
    let prompts: Vec<String> = logged(&log)
        .iter()
        .map(|c| c["prompt"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(prompts.len(), 3);
    assert!(prompts[0].starts_with("This is part 1 of 2"));
    assert!(prompts[1].starts_with("This is part 2 of 2"));
    assert!(prompts[2].contains("Merge them") && prompts[2].contains("PART 2:\n- a bullet"));

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_timon"));
    let o = cmd
        .args([
            "eval",
            "return sessions.summarize(\"aaaaaaaa\", { force = true })",
            "--max-runs",
            "2",
        ])
        .current_dir(a.dir.path())
        .env("TIMON_HOME", a.dir.path().join("timon"))
        .env("HOME", a.home())
        .envs(env)
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("budget_exceeded"));
}

// The CLI door (#44)

fn out(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn the_cli_lists_shows_and_finds_the_current_session() {
    let a = fixture();
    let o = a.cli(&["sessions", "list"], &[]);
    let (stdout, stderr) = out(&o);
    assert!(o.status.success(), "{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert!(lines[0].starts_with("aaaaaaaa  claude"), "{stdout}");
    assert!(lines[0].ends_with("/work/timon  Build fix"), "{stdout}");
    assert!(lines[1].starts_with("bbbbbbbb  codex"), "{stdout}");
    assert_eq!(
        stderr.trim(),
        "left out: 1 automated (--automated), 1 subagents (--subagents)"
    );

    let o = a.cli(&["sessions", "list", "--dir", "/work/codex"], &[]);
    let (stdout, stderr) = out(&o);
    assert!(o.status.success(), "{stderr}");
    assert!(
        stdout.lines().count() == 1 && stdout.starts_with("bbbbbbbb  codex"),
        "the CLI door takes --dir like the module: {stdout}"
    );

    let o = a.cli(
        &[
            "sessions",
            "list",
            "--backend",
            "codex",
            "--automated",
            "--json",
        ],
        &[],
    );
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let ids: Vec<&str> = r["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [ID_B, ID_A2]);

    let (stdout, _) = out(&a.cli(&["sessions", "show", "aaaaaaaa-1111"], &[]));
    assert!(stdout.contains("title     Build fix"), "{stdout}");
    assert!(
        stdout.contains("prs       https://github.com/o/r/pull/7"),
        "{stdout}"
    );
    let o = a.cli(&["sessions", "show", "ffff"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).1.contains("not_found"));

    let (stdout, _) = out(&a.cli(
        &["sessions", "current"],
        &[("CLAUDE_CODE_SESSION_ID", ID_A)],
    ));
    assert!(stdout.starts_with(&format!("id        {ID_A}")), "{stdout}");
    let o = a.cli(&["sessions", "current"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).1.contains("CLAUDE_CODE_SESSION_ID"));
}

#[test]
fn the_cli_pages_transcripts_and_searches() {
    let a = searchable();
    let (stdout, stderr) = out(&a.cli(&["sessions", "transcript", ID_A, "--limit", "1"], &[]));
    assert_eq!(stdout.trim(), "[0] USER: please fix the flaky parser test");
    assert!(stderr.contains("more with --offset 1"), "{stderr}");
    let (stdout, _) = out(&a.cli(
        &["sessions", "transcript", ID_A, "--kind", "tool_call"],
        &[],
    ));
    assert!(stdout.starts_with("[2] TOOL CALL: Grep("), "{stdout}");

    let (stdout, stderr) = out(&a.cli(&["sessions", "search", "parser colons"], &[]));
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines[0].starts_with("aaaaaaaa  claude"), "{stdout}");
    assert!(lines[1].starts_with("  [1] assistant: "), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
    let (_, stderr) = out(&a.cli(&["sessions", "search", "quokka"], &[]));
    assert!(stderr.contains("automated = true"), "{stderr}");
    let (stdout, _) = out(&a.cli(
        &["sessions", "search", "quokka", "--automated", "--json"],
        &[],
    ));
    let r: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(r["returned"], 2);
    let (stdout, stderr) = out(&a.cli(&["sessions", "search", "nowhere"], &[]));
    assert!(
        stdout.is_empty() && stderr.starts_with("no matches"),
        "{stderr}"
    );
    let o = a.cli(&["sessions", "search", "x", "--mode", "fuzzy"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).1.contains("fuzzy"));
}

#[test]
fn the_cli_summarizes_and_says_what_it_cost() {
    let a = searchable();
    let claude = fake_claude();
    let env = [
        ("TIMON_CLAUDE_BIN", claude.as_str()),
        ("FAKE_TEXT", "Goal: fix the parser."),
    ];
    let (stdout, stderr) = out(&a.cli(&["sessions", "summarize", "aaaaaaaa"], &env));
    assert_eq!(stdout.trim(), "Goal: fix the parser.");
    assert_eq!(stderr.trim(), "haiku, $0.0125");
    let (_, stderr) = out(&a.cli(&["sessions", "summarize", "aaaaaaaa"], &env));
    assert_eq!(stderr.trim(), "haiku, cached");
    let o = a.cli(
        &[
            "--max-runs",
            "0",
            "sessions",
            "summarize",
            "aaaaaaaa",
            "--force",
        ],
        &env,
    );
    assert!(out(&o).1.contains("budget_exceeded"), "{}", out(&o).1);
}
