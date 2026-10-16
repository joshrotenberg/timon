//! timon's commands described like tasks, and shell completion from the same declarations (#80).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new(tasks: &str) -> Self {
        let s = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        let file = s.project().join(".timon/tasks.lua");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, tasks).unwrap();
        let o = s.timon(&["trust"]);
        assert!(o.status.success(), "{}", err(&o));
        s
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("home/proj")
    }

    fn command(&self, program: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(self.project())
            .env("TIMON_HOME", self.dir.path().join("timon"))
            .env("HOME", self.dir.path().join("home"));
        cmd
    }

    fn timon(&self, args: &[&str]) -> Output {
        self.command(Path::new(env!("CARGO_BIN_EXE_timon")))
            .args(args)
            .output()
            .unwrap()
    }

    fn candidates(&self, words: &[&str]) -> Vec<String> {
        let mut args = vec!["__complete", "--"];
        args.extend_from_slice(words);
        let o = self.timon(&args);
        assert!(o.status.success(), "{words:?}: {}", err(&o));
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const TASKS: &str = r#"return {
  review = {
    description = "Review the diff",
    aliases = { "rv" },
    options = {
      base = { type = "string", default = "main", short = "b", doc = "the branch" },
      model = '("sonnet" | "opus")?',
    },
    run = function(opts) return opts.base end,
  },
  release = "true",
  hidden = { private = true, run = "true" },
}"#;

#[test]
fn all_lists_timons_commands_described_like_tasks() {
    let s = Sandbox::new(TASKS);
    let o = s.timon(&["tasks"]);
    assert!(!String::from_utf8_lossy(&o.stdout).contains("builtin"));
    let o = s.timon(&["tasks", "--all"]);
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("\ntimon\n  eval "), "{text}");
    assert!(
        text.contains("builtin  Find, read, search and summarize"),
        "{text}"
    );

    let o = s.timon(&["tasks", "--all", "--json"]);
    let list: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap();
    let find = |name: &str| list.iter().find(|t| t["name"] == name).unwrap().clone();
    let search = find("sessions search");
    assert_eq!(search["kind"], "builtin");
    let limit = search["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "limit")
        .unwrap();
    assert_eq!(limit["type"], "integer");
    assert_eq!(limit["flag"], "--limit");
    assert_eq!(search["positionals"][0]["name"], "query");
    let globals = find("timon");
    let jobs = globals["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "jobs")
        .unwrap();
    assert_eq!(
        (&jobs["type"], &jobs["default"], &jobs["short"]),
        (
            &serde_json::json!("integer"),
            &serde_json::json!(1),
            &serde_json::json!("-j")
        )
    );
    let progress = globals["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "progress")
        .unwrap();
    assert_eq!(progress["type"], "\"text\" | \"json\" | \"none\"");

    let review = find("review");
    let keys = |v: &serde_json::Value| {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(
        keys(&review["options"][0]),
        keys(&limit.clone()),
        "a task's options and a command's have the same shape"
    );
    assert!(list.iter().all(|t| t["name"] != "__complete"));
}

#[test]
fn completion_offers_names_flags_and_choices() {
    let s = Sandbox::new(TASKS);
    let cases: &[(&[&str], &[&str])] = &[
        (&["re"], &["release", "review"]),
        (&["rv", "-"], &["--base", "--help", "--model"]),
        (&["review", "--model", ""], &["opus", "sonnet"]),
        (&["review", "-b", "x", "--model", "s"], &["sonnet"]),
        (&["sessions", "se"], &["search"]),
        (&["--progress", ""], &["json", "none", "text"]),
        (&["-j", "2", "rev"], &["review"]),
        (&["task", "re"], &["release", "review"]),
        (&["run", "review", "--b"], &["--base"]),
        (&["review", "--", "--"], &[]),
        (&["nope", "--"], &[]),
        (&["hid"], &[]),
    ];
    for (words, expected) in cases {
        assert_eq!(s.candidates(words), *expected, "{words:?}");
    }
    let flags = s.candidates(&["sessions", "search", "--"]);
    assert!(flags.contains(&"--limit".to_owned()), "{flags:?}");
}

#[test]
fn a_tasks_file_that_fails_leaves_the_commands() {
    let s = Sandbox::new("return { x = 'cargo test | tee log' }");
    assert_eq!(s.candidates(&["ses"]), ["sessions"]);
}

#[test]
fn the_bash_script_completes_through_timon() {
    let Ok(bash) = which("bash") else {
        return;
    };
    let s = Sandbox::new(TASKS);
    let script = String::from_utf8(s.timon(&["completions", "bash"]).stdout).unwrap();
    assert!(script.contains("complete -o default -F _timon timon"));
    let bin = Path::new(env!("CARGO_BIN_EXE_timon")).parent().unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let o = s
        .command(&bash)
        .env("PATH", path)
        .arg("-c")
        .arg(format!(
            "{script}\nCOMP_WORDS=(timon review --model s); COMP_CWORD=3; _timon; printf '%s\\n' \"${{COMPREPLY[@]}}\""
        ))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "sonnet\n",
        "{}",
        err(&o)
    );
    for shell in ["zsh", "fish"] {
        let o = s.timon(&["completions", shell]);
        assert!(String::from_utf8_lossy(&o.stdout).contains("timon __complete --"));
    }
}

fn which(name: &str) -> Result<PathBuf, ()> {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
        .ok_or(())
}
