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
fn a_lua_error_exits_nonzero_with_the_message_on_stderr() {
    let output = timon(&["eval", "error('boom')"], None);
    assert!(!output.status.success());
    assert!(stdout(&output).is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("boom"));
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
