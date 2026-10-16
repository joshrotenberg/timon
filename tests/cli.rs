use std::io::Write;
use std::process::{Command, Output, Stdio};

fn timon(args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_timon"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start timon");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
    }
    child.wait_with_output().expect("wait for timon")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn script(name: &str, source: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, source).unwrap();
    (dir, path.display().to_string())
}

#[test]
fn eval_prints_an_expression() {
    let output = timon(&["eval", "1 + 1"], None);
    assert!(output.status.success());
    assert_eq!(stdout(&output), "2\n");
}

#[test]
fn eval_prints_each_returned_value() {
    let output = timon(&["eval", "return 'a', { b = true }"], None);
    assert!(output.status.success());
    assert_eq!(stdout(&output), "a\n{\"b\":true}\n");
}

#[test]
fn eval_reads_standard_input() {
    let output = timon(&["eval", "-"], Some("local n = 20\nreturn n + 22\n"));
    assert!(output.status.success());
    assert_eq!(stdout(&output), "42\n");
}

#[test]
fn eval_runs_a_file() {
    let (_dir, path) = script("x.lua", "return 'from a file'");
    let output = timon(&["eval", "-f", &path], None);
    assert_eq!(stdout(&output), "from a file\n");
}

#[test]
fn a_script_gets_its_arguments_and_skips_the_shebang() {
    let (_dir, path) = script(
        "args.lua",
        "#!/usr/bin/env timon\nlocal a, b = ...\nreturn arg[1], b, #arg",
    );
    let output = timon(&[&path, "one", "two"], None);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "one\ntwo\n2\n");
}

#[test]
fn an_error_reports_kind_hint_and_place_on_stderr_and_exits_1() {
    let output = timon(&["eval", "prnt('x')"], None);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        "timon: undefined_global: undefined global \"prnt\"\n  hint: did you mean \"print\"?\n  at eval:1\n"
    );
}

#[test]
fn a_script_error_names_the_file_and_line() {
    let (_dir, path) = script(
        "bad.lua",
        "local x = 1\nerror({ kind = 'mine', message = 'no' })\n",
    );
    let output = timon(&[&path], None);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("timon: mine: no"));
    assert!(stderr(&output).contains("bad.lua:2"), "{}", stderr(&output));
}

#[test]
fn an_unknown_command_is_a_usage_error() {
    let output = timon(&["frobnicate"], None);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("unknown command \"frobnicate\""));
}

#[test]
fn version_prints_the_package_version() {
    let output = timon(&["--version"], None);
    assert!(output.status.success());
    assert_eq!(
        stdout(&output),
        format!("timon {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn log_and_pp_write_to_stderr_and_leave_stdout_as_data() {
    let output = timon(
        &[
            "eval",
            "log.info('reviewing', 2, 'PRs', { 12, 14 }); local x = pp({ a = 1 }); return x.a",
        ],
        None,
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "1\n");
    assert_eq!(
        stderr(&output),
        "info: reviewing 2 PRs [12,14]\n{\n  \"a\": 1\n}\n"
    );
}

#[test]
fn help_prints_through_eval() {
    let output = timon(&["eval", "help(str.split)"], None);
    assert!(stdout(&output).starts_with("str.split(s, sep) -> array\n"));
}

#[test]
fn eval_fnl_and_fennel_scripts() {
    let output = timon(&["eval", "--fnl", "(+ 40 2)"], None);
    assert_eq!(stdout(&output), "42\n");
    let (_dir, path) = script("args.fnl", "(let [[a] [...]] (.. a \"!\"))");
    let output = timon(&[&path, "hi"], None);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "hi!\n");
}
