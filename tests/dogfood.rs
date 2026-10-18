//! The repository's own `.timon`, copied into a sandbox project: it loads, and its scripts run.
//! `em-dash` also runs over the repository itself, which enforces the AGENTS.md rule.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Project {
    dir: tempfile::TempDir,
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

impl Project {
    fn new() -> Self {
        let p = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        copy_dir(&repo.join(".timon"), &p.root().join(".timon"));
        std::fs::create_dir_all(p.dir.path().join("logs")).unwrap();
        let o = p.timon(&["trust"], &[]);
        assert!(o.status.success(), "{}", err(&o));
        p
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("home/timon")
    }

    fn timon(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_timon"));
        cmd.args(args)
            .current_dir(self.root())
            .env("TIMON_HOME", self.dir.path().join("state"))
            .env("HOME", self.dir.path().join("home"))
            .env("TIMON_CLAUDE_BIN", fixtures.join("fake-claude"))
            .env("TIMON_CODEX_BIN", fixtures.join("fake-codex"))
            .env("FAKE_LOG_DIR", self.dir.path().join("logs"));
        for var in [
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "CLAUDE_CODE_SESSION_ID",
            "CODEX_THREAD_ID",
        ] {
            cmd.env_remove(var);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn names(&self, args: &[&str]) -> Vec<(String, String)> {
        let o = self.timon(args, &[]);
        assert!(o.status.success(), "{}", err(&o));
        let list: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap();
        list.iter()
            .filter(|e| e["layer"] == "project")
            .map(|e| {
                (
                    e["name"].as_str().unwrap().to_owned(),
                    e["description"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn the_project_layer_loads_agents_tasks_scripts_and_skills() {
    let p = Project::new();
    let agents = p.names(&["agents", "--json"]);
    assert_eq!(
        agents.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["fixer", "namer", "reviewer"]
    );
    assert!(
        agents
            .iter()
            .all(|(_, d)| !d.is_empty() && !d.starts_with("(cannot load")),
        "{agents:?}"
    );
    let tasks = p.timon(&["tasks", "--json"], &[]);
    assert!(tasks.status.success(), "{}", err(&tasks));
    let tasks: serde_json::Value = serde_json::from_slice(&tasks.stdout).unwrap();
    let mut scripts: Vec<&str> = tasks
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["layer"] == "project" && t["kind"] == "script")
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    scripts.sort();
    assert_eq!(
        scripts,
        ["em-dash", "fix-issues", "naming-audit", "parity", "recent"]
    );
    let check = tasks
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "check")
        .unwrap();
    assert_eq!(
        check["depends"],
        serde_json::json!(["format", "lint", "test", "docs", "plugin-check"])
    );
    let gates: Vec<&str> = tasks
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["group"] == "gates")
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(gates, ["docs", "format", "lint", "plugin-check", "test"]);
    let release = tasks
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "release")
        .unwrap();
    assert_eq!(release["depends"], serde_json::json!(["check"]));
    assert_eq!(release["group"], "release");
    assert!(release["confirm"].is_string());
    let update = tasks
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "update")
        .unwrap();
    assert_eq!(update["group"], "release");
    // A dry run plans the update without installing anything; a bad source is a usage error.
    let plan = p.timon(&["--dry-run", "update", "--from", "checkout"], &[]);
    assert!(plan.status.success(), "{}", err(&plan));
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(plan["plan"][0]["opts"]["from"], "checkout");
    let bad = p.timon(&["--dry-run", "update", "--from", "crates"], &[]);
    assert_eq!(bad.status.code(), Some(2), "{}", err(&bad));
    let skills = p.names(&["skill", "list", "--json"]);
    assert!(skills.iter().any(|(n, _)| n == "timon-dev"), "{skills:?}");
}

#[test]
fn the_repository_has_no_em_dashes() {
    let p = Project::new();
    let o = p.timon(&["run", "em-dash", env!("CARGO_MANIFEST_DIR")], &[]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("no em dashes"), "{}", out(&o));

    let docs = p.root().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("x.md"), "fine\nbad \u{2014} here\n").unwrap();
    let o = p.timon(&["run", "em-dash"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("em_dash: 1 line(s)"), "{}", err(&o));
    assert!(err(&o).contains("docs/x.md:2"), "{}", err(&o));
}

#[test]
fn recent_and_the_naming_audit_run() {
    let p = Project::new();
    let o = p.timon(&["run", "recent"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).starts_with("sessions since 2d: 0\n"), "{}", out(&o));

    let report = p.dir.path().join("audit.json");
    let o = p.timon(
        &["run", "naming-audit", report.to_str().unwrap()],
        &[("FAKE_AUTO_ANSWER", "1")],
    );
    assert!(o.status.success(), "{}", err(&o));
    // Two runs at a time (init.lua), so count the run records, not the fake's log files.
    let runs: Vec<serde_json::Value> =
        serde_json::from_slice(&p.timon(&["runs", "--json"], &[]).stdout).unwrap();
    assert_eq!(runs.len(), 6, "one namer run per area");
    assert!(
        runs.iter()
            .all(|r| r["agent"] == "namer" && r["status"] == "ok")
    );
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(written, serde_json::json!({ "findings": [], "failed": [] }));
    let o = p.timon(&["run", "recent"], &[]);
    assert!(out(&o).contains("agent runs since 2d: 6"), "{}", out(&o));
}

/// Runs `timon book --no-build` over `root` into the project's `book/`, describing this build's
/// commands. mdbook is not needed.
fn book(p: &Project, root: &Path) -> Output {
    let out = p.dir.path().join("book");
    p.timon(
        &[
            "book",
            "--no-build",
            "--root",
            root.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--bin",
            env!("CARGO_BIN_EXE_timon"),
        ],
        &[],
    )
}

#[test]
fn the_book_is_generated_from_the_repository() {
    let p = Project::new();
    let o = book(&p, Path::new(env!("CARGO_MANIFEST_DIR")));
    assert!(o.status.success(), "{}", err(&o));
    let out = p.dir.path().join("book");
    let read = |page: &str| std::fs::read_to_string(out.join("src").join(page)).unwrap();
    assert!(out.join("book.toml").exists());
    assert!(!out.join("html").exists(), "--no-build ran mdbook");
    let summary = read("SUMMARY.md");
    for page in [
        "install.md",
        "use.md",
        "guide/basics.md",
        "guide/tasks.md",
        "guide/sessions.md",
        "reference/commands.md",
        "reference/modules.md",
        "reference/json.md",
        "reference/agent.md",
        "design.md",
        "contributing.md",
        "live-tests.md",
    ] {
        assert!(
            summary.contains(&format!("]({page})")),
            "{page} is not in the summary"
        );
        assert!(
            out.join("src").join(page).exists(),
            "{page} was not written"
        );
    }
    // A skill loses its metadata and the fragment tags only tests/skills.rs reads.
    let tasks = read("guide/tasks.md");
    assert!(!tasks.contains("\nname: timon-tasks"), "{tasks}");
    assert!(!tasks.contains("lua fragment"));
    // Links go to the page that holds their target, and skill mentions link to the guide.
    assert!(
        read("install.md").contains("](guide/tasks.md#as-hooks)"),
        "{}",
        read("install.md")
    );
    assert!(read("README.md").contains("](design.md)"));
    assert!(read("guide/basics.md").contains("[`timon skill show timon-tasks`](tasks.md)"));
    // Reference pages come from help() and the built-in task declarations.
    assert!(read("reference/fs.md").contains("- `mkdir` (`boolean`): create missing parent"));
    let commands = read("reference/commands.md");
    assert!(commands.contains("## `timon eval [CODE]`"), "{commands}");
    assert!(
        commands.contains("- `-j, --jobs` (`integer`, default `1`)"),
        "{commands}"
    );
}

#[test]
fn the_book_refuses_a_link_to_a_missing_file() {
    let p = Project::new();
    let root = p.dir.path().join("repo");
    let write = |file: &str, text: &str| {
        let f = root.join(file);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, text).unwrap();
    };
    let readme = "# timon\n\nSee [the guide](skills/timon/SKILL.md) and [the manifest](Cargo.toml).\n\n## Status\n\n## Install\n\n## Use\n\n## Working on timon\n\n## License\n";
    write("README.md", readme);
    write("Cargo.toml", "[package]\n");
    for skill in [
        "timon",
        "timon-tasks",
        "timon-scripting",
        "timon-agents",
        "timon-workflows",
        "timon-sessions",
    ] {
        write(
            &format!("skills/{skill}/SKILL.md"),
            &format!("---\nname: {skill}\n---\n\n# {skill}\n"),
        );
    }
    for doc in ["design", "prior-art", "live-tests"] {
        write(&format!("docs/{doc}.md"), &format!("# {doc}\n"));
    }
    // A link to a book page is rewritten to it; a file outside the book goes to GitHub.
    let o = book(&p, &root);
    assert!(o.status.success(), "{}", err(&o));
    let intro = std::fs::read_to_string(p.dir.path().join("book/src/README.md")).unwrap();
    assert!(intro.contains("](guide/basics.md)"), "{intro}");
    assert!(
        intro.contains("](https://github.com/joshrotenberg/timon/blob/main/Cargo.toml)"),
        "{intro}"
    );

    // A link to a file the repository does not have fails, with the page and the link.
    write(
        "skills/timon/SKILL.md",
        "# timon\n\nSee [design](../../docs/design.md#x) and [gone](../../docs/gone.md).\n",
    );
    let o = book(&p, &root);
    assert!(!o.status.success());
    assert!(
        err(&o).contains("skills/timon/SKILL.md -> ../../docs/gone.md"),
        "{}",
        err(&o)
    );

    // A skill or README section the book does not place fails too, rather than dropping out.
    write("skills/timon/SKILL.md", "# timon\n");
    write("skills/timon-extra/SKILL.md", "# extra\n");
    let o = book(&p, &root);
    assert!(
        err(&o).contains("skill timon-extra has no page in the book"),
        "{}",
        err(&o)
    );
    std::fs::remove_dir_all(root.join("skills/timon-extra")).unwrap();
    write("README.md", &format!("{readme}\n## Changelog\n"));
    let o = book(&p, &root);
    assert!(
        err(&o).contains("README section \"Changelog\" has no page in the book"),
        "{}",
        err(&o)
    );
}
