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

/// Every runnable example runs through the binary in a sandbox, with fake providers that
/// answer any output schema, so agent examples run without a model.
#[test]
fn every_runnable_skill_example_runs() {
    use std::io::Write;
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut ran = 0;
    for skill in timon::skills::builtin() {
        let text = skill.text().unwrap();
        for (lang, code) in runnable_blocks(&text) {
            let sandbox = tempfile::tempdir().unwrap();
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
    assert!(ran >= 18, "only {ran} examples ran");
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
