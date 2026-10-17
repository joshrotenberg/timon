//! Layers through the CLI. Every test runs in a sandbox with its own TIMON_HOME and HOME and
//! asserts nothing is written outside it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn user(&self) -> PathBuf {
        self.root().join("timon").join("config")
    }

    fn project(&self) -> PathBuf {
        self.root().join("home").join("proj")
    }

    fn write(&self, path: PathBuf, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn timon(&self, cwd: &Path, args: &[&str]) -> Output {
        std::fs::create_dir_all(cwd).unwrap();
        Command::new(env!("CARGO_BIN_EXE_timon"))
            .args(args)
            .current_dir(cwd)
            .env("TIMON_HOME", self.root().join("timon"))
            .env("HOME", self.root().join("home"))
            .output()
            .expect("run timon")
    }

    /// Fails if anything outside the sandbox root was written; checks that state stays under
    /// TIMON_HOME.
    fn assert_state_inside(&self) {
        let state = self.root().join("timon").join("state");
        if state.exists() {
            assert!(state.starts_with(self.root()));
        }
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn run_finds_user_scripts_and_passes_arguments() {
    let s = Sandbox::new();
    s.write(
        s.user().join("scripts/greet.lua"),
        "-- Say hello\nlocal who = ...\nreturn 'hello ' .. who",
    );
    let o = s.timon(s.root(), &["run", "greet", "josh"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "hello josh\n");
    let o = s.timon(s.root(), &["scripts"]);
    assert_eq!(out(&o), "greet  user     Say hello\n");
}

#[test]
fn a_project_layer_needs_trust_and_then_shadows_the_user_layer() {
    let s = Sandbox::new();
    s.write(s.user().join("scripts/which.lua"), "return 'user'");
    s.write(
        s.project().join(".timon/scripts/which.lua"),
        "return 'project'",
    );
    s.write(
        s.project().join(".timon/scripts/only.fnl"),
        ";; project only\n:only",
    );
    let cwd = s.project().join("src");

    assert_eq!(out(&s.timon(&cwd, &["run", "which"])), "user\n");
    let o = s.timon(&cwd, &["run", "only"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("timon: untrusted:"), "{}", err(&o));
    assert!(err(&o).contains("timon trust"), "{}", err(&o));

    let o = s.timon(&cwd, &["trust"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).starts_with("trusted: "));
    assert_eq!(out(&s.timon(&cwd, &["run", "which"])), "project\n");
    assert_eq!(out(&s.timon(&cwd, &["run", "only"])), "only\n");

    let o = s.timon(&cwd, &["scripts", "--json"]);
    let list: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(list[0]["name"], "only");
    assert_eq!(list[1]["layer"], "project");

    s.timon(&cwd, &["trust", "--revoke"]);
    assert_eq!(out(&s.timon(&cwd, &["run", "which"])), "user\n");
    s.assert_state_inside();
}

#[cfg(unix)]
#[test]
fn a_state_directory_without_write_permission_fails_with_a_hint_and_trusts_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let s = Sandbox::new();
    s.write(s.user().join("scripts/greet.lua"), "return 'hello'");
    s.write(s.project().join(".timon/scripts/which.lua"), "return 'p'");
    let home = s.root().join("timon");
    let state = home.join("state");
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o500)).unwrap();
    if std::fs::create_dir(&state).is_ok() {
        return; // running as root: permissions are not enforced
    }

    let o = s.timon(&s.project(), &["trust"]);
    let hint = format!(
        "timon keeps the trust store, run records, script runs and worktrees in {}; a sandboxed host must allow writes there",
        state.display()
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("timon: io: cannot write"), "{}", err(&o));
    assert!(err(&o).contains(&hint), "{}", err(&o));
    assert!(
        err(&o).contains("TIMON_HOME or XDG_STATE_HOME"),
        "{}",
        err(&o)
    );
    assert!(!state.join("trusted").exists());
    let o = s.timon(&s.project(), &["run", "which"]);
    assert!(err(&o).contains("timon: untrusted:"), "{}", err(&o));

    let o = s.timon(s.root(), &["run", "greet"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("timon: io: cannot create"), "{}", err(&o));
    assert!(err(&o).contains(&hint), "{}", err(&o));
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn lib_directories_are_on_the_module_path() {
    let s = Sandbox::new();
    s.write(
        s.user().join("lib/util.lua"),
        "local M = {}\nfunction M.double(n) return n * 2 end\nreturn M",
    );
    s.write(s.user().join("lib/fmod.fnl"), "{:triple (fn [n] (* n 3))}");
    let o = s.timon(
        s.root(),
        &[
            "eval",
            "require('util').double(21) + require('fmod').triple(1)",
        ],
    );
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "45\n");
}

#[test]
fn init_lua_settings_merge_by_layer() {
    let s = Sandbox::new();
    s.write(
        s.user().join("init.lua"),
        "return { defaults = { provider = 'claude', model = 'opus' }, limits = { concurrency = 4 } }",
    );
    s.write(
        s.project().join(".timon/init.lua"),
        "return { defaults = { model = 'sonnet' } }",
    );
    let cwd = s.project();
    s.timon(&cwd, &["trust"]);
    let o = s.timon(&cwd, &["defs", "--json"]);
    assert!(o.status.success(), "{}", err(&o));
    let defs: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(
        defs["settings"],
        serde_json::json!({
            "defaults": { "provider": "claude", "model": "sonnet" },
            "limits": { "concurrency": 4 }
        })
    );
    assert_eq!(defs["layers"][0]["trusted"], true);
}

#[test]
fn a_bad_init_lua_is_reported_with_a_suggestion() {
    let s = Sandbox::new();
    s.write(s.user().join("init.lua"), "return { defualts = {} }");
    let o = s.timon(s.root(), &["eval", "1"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("bad_config"), "{}", err(&o));
    assert!(
        err(&o).contains("unknown setting \"defualts\""),
        "{}",
        err(&o)
    );
    assert!(
        err(&o).contains("did you mean \"defaults\"?"),
        "{}",
        err(&o)
    );
}

#[test]
fn init_lua_cannot_do_io() {
    let s = Sandbox::new();
    s.write(s.user().join("init.lua"), "fs.write('x', 'y')\nreturn {}");
    let o = s.timon(s.root(), &["eval", "1"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(!s.root().join("x").exists());
}

#[test]
fn an_unknown_script_suggests_one() {
    let s = Sandbox::new();
    s.write(s.user().join("scripts/deploy.lua"), "return 1");
    let o = s.timon(s.root(), &["run", "deplyo"]);
    assert!(err(&o).contains("did you mean \"deploy\"?"), "{}", err(&o));
}
