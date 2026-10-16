//! Skills: every runnable example in a bundled skill runs, and skills layer by name.

use std::path::Path;
use std::process::{Command, Output};

/// Fenced blocks tagged exactly `lua` or `fennel`. Blocks tagged `lua fragment` need context a
/// standalone run does not have, such as script arguments or a user's module.
fn runnable_blocks(markdown: &str) -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for line in markdown.lines() {
        match &mut current {
            None => {
                if let Some(lang) = line.strip_prefix("```")
                    && (lang == "lua" || lang == "fennel")
                {
                    current = Some((lang.to_owned(), Vec::new()));
                }
            }
            Some((lang, body)) => {
                if line.starts_with("```") {
                    blocks.push((lang.clone(), body.join("\n")));
                    current = None;
                } else {
                    body.push(line);
                }
            }
        }
    }
    blocks
}

/// Session archives in a sandbox home, for the `timon-sessions` examples.
fn seed_archives(home: &Path) {
    let write = |path: std::path::PathBuf, lines: &[serde_json::Value]| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, text).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        let two_hours = std::time::Duration::from_secs(2 * 3600);
        file.set_modified(std::time::SystemTime::now() - two_hours)
            .unwrap();
    };
    let text = |role: &str, text: &str| {
        serde_json::json!({ "type": role, "cwd": "/work/app", "entrypoint": "cli",
            "message": { "role": role, "content": [{ "type": "text", "text": text }] } })
    };
    write(
        home.join(".claude/projects/-work-app/01a117cb-5e55-4f1e-9c1a-2b3c4d5e6f70.jsonl"),
        &[
            text("user", "the parser test is flaky, can you look"),
            serde_json::json!({ "type": "assistant", "message": { "role": "assistant", "content": [
                { "type": "tool_use", "id": "t1", "name": "Grep", "input": { "pattern": "fn parse" } } ] } }),
            serde_json::json!({ "type": "user", "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "src/parse.rs:12" } ] } }),
            text(
                "assistant",
                "The parser drops quoted colons, so the flaky parser test depends on order.",
            ),
            text("user", "fix it in src/parse.rs"),
            text("assistant", "Fixed in src/parse.rs; the test passes."),
        ],
    );
    write(
        home.join(".codex/sessions/2026/10/07/rollout-2026-10-07T12-00-00-019a2b3c-4d5e-7f60-8a1b-2c3d4e5f6a7b.jsonl"),
        &[
            serde_json::json!({ "type": "session_meta", "payload": {
                "id": "019a2b3c-4d5e-7f60-8a1b-2c3d4e5f6a7b", "cwd": "/work/app",
                "originator": "codex_cli_rs", "source": "cli" } }),
            serde_json::json!({ "type": "event_msg",
                "payload": { "type": "user_message", "message": "add a --verbose flag" } }),
        ],
    );
}

/// Every runnable example runs through the binary in a sandbox, with fake providers that
/// answer any output schema and session archives to read, so examples run without a model.
#[test]
fn every_runnable_skill_example_runs() {
    use std::io::Write;
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut ran = 0;
    for skill in timon::skills::builtin() {
        let text = skill.text().unwrap();
        for (lang, code) in runnable_blocks(&text) {
            let sandbox = tempfile::tempdir().unwrap();
            seed_archives(sandbox.path());
            let mut args = vec!["eval"];
            if lang == "fennel" {
                args.push("--fnl");
            }
            args.push("-");
            let mut child = Command::new(env!("CARGO_BIN_EXE_timon"))
                .args(&args)
                .current_dir(sandbox.path())
                .env("TIMON_HOME", sandbox.path().join("timon"))
                .env("HOME", sandbox.path())
                .env("TIMON_CLAUDE_BIN", fixtures.join("fake-claude"))
                .env("TIMON_CODEX_BIN", fixtures.join("fake-codex"))
                .env("FAKE_AUTO_ANSWER", "1")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(code.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "{} example failed:\n{code}\n{}",
                skill.name,
                String::from_utf8_lossy(&out.stderr)
            );
            ran += 1;
        }
    }
    assert!(ran >= 24, "only {ran} examples ran");
}

fn timon(root: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_timon"))
        .args(args)
        .current_dir(cwd)
        .env("TIMON_HOME", root.join("timon"))
        .env("HOME", root.join("home"))
        .output()
        .unwrap()
}

#[test]
fn user_and_project_skills_layer_over_builtin_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let write = |path: std::path::PathBuf, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let user = root.join("timon/config/skills");
    write(
        user.join("release/SKILL.md"),
        "---\nname: release\ndescription: How we release.\n---\n# Release\n",
    );
    write(user.join("release/references/checklist.md"), "1. tag\n");
    write(
        user.join("timon/SKILL.md"),
        "---\nname: timon\ndescription: My own timon notes.\n---\n# Mine\n",
    );
    let project = root.join("home/proj");
    write(
        project.join(".timon/skills/release/SKILL.md"),
        "---\nname: release\ndescription: This project's release.\n---\n# Project release\n",
    );

    let list = |cwd: &Path| -> serde_json::Value {
        let o = timon(root, cwd, &["skill", "list", "--json"]);
        serde_json::from_slice(&o.stdout).unwrap()
    };
    let by_name = |list: &serde_json::Value, name: &str| -> serde_json::Value {
        list.as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .cloned()
            .unwrap()
    };

    let skills = list(&project);
    assert_eq!(by_name(&skills, "release")["layer"], "user");
    assert_eq!(
        by_name(&skills, "timon")["description"],
        "My own timon notes."
    );
    assert_eq!(by_name(&skills, "timon-scripting")["layer"], "builtin");

    let o = timon(
        root,
        &project,
        &["skill", "show", "release", "--ref", "checklist"],
    );
    assert_eq!(String::from_utf8_lossy(&o.stdout), "1. tag\n");

    timon(root, &project, &["trust"]);
    let skills = list(&project);
    assert_eq!(by_name(&skills, "release")["layer"], "project");
    let o = timon(root, &project, &["skill", "show", "release"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("# Project release"));
}
