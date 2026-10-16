//! Skills: every runnable example in a bundled skill runs, and skills layer by name.

use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use timon::Runtime;

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

#[test]
fn every_runnable_skill_example_runs() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut ran = 0;
    for skill in timon::skills::builtin() {
        let text = skill.text().unwrap();
        for (lang, code) in runnable_blocks(&text) {
            let result = tokio::task::LocalSet::new().block_on(&rt, async {
                let runtime = Runtime::new().unwrap();
                let run = async {
                    if lang == "fennel" {
                        runtime.eval_fennel(&code, "=example").await
                    } else {
                        runtime.eval(&code, "=example").await
                    }
                };
                tokio::time::timeout(Duration::from_secs(10), run)
                    .await
                    .map(|r| r.map(|_| ()))
            });
            match result {
                Ok(Ok(())) => ran += 1,
                Ok(Err(failure)) => panic!("{} example failed:\n{code}\n{failure}", skill.name),
                Err(_) => panic!("{} example timed out:\n{code}", skill.name),
            }
        }
    }
    assert!(ran >= 10, "only {ran} examples ran");
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
