//! sh and proc (#70): running programs, with real child processes in a sandbox.

use std::path::Path;
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

    fn timon(&self, code: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_timon"))
            .args(["eval", code])
            .current_dir(self.root())
            .env("TIMON_HOME", self.root().join("timon"))
            .env("HOME", self.root())
            .output()
            .unwrap()
    }

    fn json(&self, code: &str) -> serde_json::Value {
        let o = self.timon(code);
        assert!(o.status.success(), "{code}\n{}", err(&o));
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", out(&o)))
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid.trim()])
        .output()
        .unwrap()
        .status
        .success()
}

/// Waits up to two seconds for `pid` to be gone.
fn gone(pid: &str) -> bool {
    (0..100).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        !alive(pid)
    })
}

#[test]
fn sh_streams_and_raises_on_a_nonzero_exit() {
    let s = Sandbox::new();
    let o = s.timon("sh('echo hello world')\nreturn 'after'");
    assert_eq!(out(&o), "hello world\nafter\n");
    let o = s.timon("sh{ 'sh', '-c', 'echo oops >&2; exit 3' }\nreturn 'not reached'");
    assert_eq!(
        o.status.code(),
        Some(3),
        "timon exits with the program's code"
    );
    assert!(
        err(&o).contains("command_failed: `sh -c 'echo oops >&2; exit 3'` exited 3"),
        "{}",
        err(&o)
    );
    assert!(!out(&o).contains("not reached"));
    let r = s.json("return sh({ 'sh', '-c', 'exit 3' }, { check = false })");
    assert_eq!(r["code"], 3);
    let r = s.json("local ok, e = try(sh, { 'sh', '-c', 'echo bad >&2; exit 4' }, { err = 'capture' })\nreturn e.data");
    assert_eq!(
        (r["code"].as_i64(), r["err"].as_str()),
        (Some(4), Some("bad"))
    );
}

#[test]
fn output_input_cwd_and_env() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.root().join("sub")).unwrap();
    let r = s.json(
        "return {
           out = sh.out('echo  spaced  '),
           input = sh.out('cat', { input = 'piped in' }),
           cwd = sh.out('pwd', { cwd = 'sub' }),
           set = sh.out({ 'sh', '-c', 'echo $TIMON_TEST_X' }, { env = { TIMON_TEST_X = 'set' } }),
           unset = sh.out({ 'sh', '-c', 'echo ${HOME:-unset}' }, { env = { HOME = false } }),
           clean = sh.out({ '/usr/bin/env' }, { clear_env = true, env = { ONLY = '1' } }),
           record = proc.run({ 'sh', '-c', 'echo out; echo err >&2; exit 2' }),
         }",
    );
    assert_eq!(r["out"], "spaced");
    assert_eq!(r["input"], "piped in");
    assert!(r["cwd"].as_str().unwrap().ends_with("/sub"), "{r}");
    assert_eq!(r["set"], "set");
    assert_eq!(r["unset"], "unset");
    assert_eq!(r["clean"], "ONLY=1");
    assert_eq!(
        r["record"],
        serde_json::json!({ "code": 2, "out": "out\n", "err": "err\n", "ms": r["record"]["ms"], "timed_out": false })
    );
}

#[test]
fn strings_split_into_words_and_shell_syntax_needs_a_shell() {
    let s = Sandbox::new();
    // Strings print raw, so compare stdout.
    let text = |code: &str| out(&s.timon(code)).trim_end().to_owned();
    assert_eq!(
        text("return sh.out({ 'printf', '%s|', 'a b', 'c' })"),
        "a b|c|"
    );
    assert_eq!(text("return sh.out([[printf '%s|' 'a b' c]])"), "a b|c|");
    let o = s.timon("sh('ls | wc -l')");
    assert!(
        err(&o).contains("bad_argument: `ls | wc -l` uses shell syntax (|)"),
        "{}",
        err(&o)
    );
    assert!(err(&o).contains("pass shell = true"), "{}", err(&o));
    assert_eq!(
        text("return sh.out('printf a | tr a b', { shell = true })"),
        "b"
    );
    let o = s.timon("sh('no-such-program-timon')");
    assert!(
        err(&o).contains("not_found: cannot run `no-such-program-timon`"),
        "{}",
        err(&o)
    );
    let o = s.timon("sh('echo hi', { shel = true })");
    assert!(err(&o).contains("did you mean \"shell\"?"), "{}", err(&o));
}

#[test]
fn a_timeout_stops_the_whole_process_group() {
    let s = Sandbox::new();
    let pid_file = s.root().join("grandchild");
    let r = s.json(&format!(
        "return proc.run({{ 'sh', '-c', 'sleep 30 & echo $! > {}; wait' }}, {{ timeout = '500ms' }})",
        pid_file.display()
    ));
    assert_eq!(
        (r["timed_out"].as_bool(), r["code"].as_i64()),
        (Some(true), Some(124))
    );
    let grandchild = std::fs::read_to_string(&pid_file).unwrap();
    assert!(gone(&grandchild), "the grandchild outlived the timeout");
    let o = s.timon("sh('sleep 5', { timeout = '100ms' })");
    assert!(
        err(&o).contains("timeout: `sleep 5` did not finish in 0.1 s"),
        "{}",
        err(&o)
    );
}

#[test]
fn a_spawned_process_stops_when_its_scope_ends() {
    let s = Sandbox::new();
    let r = s.json(
        "local pid
         scope(function()
           local p = proc.spawn{ 'sleep', '30' }
           pid = p:pid()
         end)
         return pid",
    );
    assert!(gone(&r.to_string()), "the process outlived its scope");
    // The main chunk is a scope too: nothing outlives the script.
    let pid = s.json("return proc.spawn{ 'sleep', '30' }:pid()");
    assert!(gone(&pid.to_string()), "the process outlived the script");
    let r = s.json(
        "local p = proc.spawn({ 'sh', '-c', 'echo a; echo b' }, { out = 'pipe' })
         local lines = {}
         for line in p:lines() do lines[#lines + 1] = line end
         return { lines = lines, code = p:wait().code, alive = p:alive() }",
    );
    assert_eq!(
        r,
        serde_json::json!({ "lines": ["a", "b"], "code": 0, "alive": false })
    );
    let r = s.json("local p = proc.spawn{ 'sleep', '30' }\np:kill()\nreturn p:wait().code");
    assert_eq!(r, 128 + 15, "stopped by TERM");
}

#[test]
fn prefixed_output_from_concurrent_commands_keeps_whole_lines() {
    let s = Sandbox::new();
    let o = s.timon(
        "par.all(
           function() sh({ 'sh', '-c', 'for i in 1 2 3 4 5; do echo line$i; done' }, { prefix = '[a] ' }) end,
           function() sh({ 'sh', '-c', 'for i in 1 2 3 4 5; do echo line$i; done' }, { prefix = '[b] ' }) end)",
    );
    assert!(o.status.success(), "{}", err(&o));
    let lines: Vec<String> = out(&o).lines().map(str::to_owned).collect();
    let tagged: Vec<&String> = lines.iter().filter(|l| l.starts_with('[')).collect();
    assert_eq!(tagged.len(), 10, "{lines:?}");
    assert!(
        tagged
            .iter()
            .all(|l| l.starts_with("[a] line") || l.starts_with("[b] line")),
        "{lines:?}"
    );
}
