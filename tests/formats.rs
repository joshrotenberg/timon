//! Data formats (#72) and `timon eval`'s input and output flags (#71).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Output, Stdio};

fn timon(dir: &std::path::Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_timon"))
        .args(args)
        .current_dir(dir)
        .env("TIMON_HOME", dir.join("timon"))
        .env("HOME", dir)
        .env_remove("NO_COLOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn ok(dir: &std::path::Path, args: &[&str], stdin: &str) -> String {
    let o = timon(dir, args, stdin);
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn eval(dir: &std::path::Path, code: &str) -> String {
    ok(dir, &["eval", code], "")
}

#[test]
fn each_format_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    assert_eq!(
        eval(
            d,
            r#"local t = toml.decode('[package]\nname = "timon"\nkeywords = ["lua", "tasks"]\n[package.metadata]\nwhen = 1979-05-27T07:32:00Z\n')
               return json.encode(toml.decode(toml.encode(t)))"#
        ),
        "{\"package\":{\"keywords\":[\"lua\",\"tasks\"],\"metadata\":{\"when\":\"1979-05-27T07:32:00Z\"},\"name\":\"timon\"}}\n"
    );
    assert_eq!(
        eval(
            d,
            r#"local v = { name = "timon", tags = json.array{ "yes", "1.0", "a: b" }, n = 2, f = 1.5, none = json.null, empty = json.array{} }
               return json.encode(yaml.decode(yaml.encode(v)))"#
        ),
        "{\"empty\":[],\"f\":1.5,\"n\":2,\"name\":\"timon\",\"none\":null,\"tags\":[\"yes\",\"1.0\",\"a: b\"]}\n"
    );
    assert_eq!(
        eval(
            d,
            r#"local rows = csv.decode('name,note\nmain.rs,"has, a comma"\nlib.rs,"says ""hi"""\n')
               return json.encode(csv.decode(csv.encode(rows, { columns = { "name", "note" } })))"#
        ),
        "[{\"name\":\"main.rs\",\"note\":\"has, a comma\"},{\"name\":\"lib.rs\",\"note\":\"says \\\"hi\\\"\"}]\n"
    );
    assert_eq!(
        eval(
            d,
            r#"return json.encode(csv.decode("a;b\n1;2\n", { header = false, separator = ";" }))"#
        ),
        "[[\"a\",\"b\"],[\"1\",\"2\"]]\n"
    );
    assert_eq!(
        eval(
            d,
            r#"return json.encode(yaml.decode("---\na: 1\n---\nb: &x [1, 2]\nc: *x\n", { all = true }))"#
        ),
        "[{\"a\":1},{\"b\":[1,2],\"c\":[1,2]}]\n"
    );
    let o = timon(
        d,
        &["eval", "return yaml.decode('a: 1\\n---\\nb: 2\\n')"],
        "",
    );
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("pass { all = true }"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn toml_edit_keeps_every_other_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let cargo = "# The package\n[package]\nname    = \"timon\"   # aligned\nversion = \"0.1.0\" # bumped by release\n\n[dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\n";
    std::fs::write(d.join("Cargo.toml"), cargo).unwrap();
    assert_eq!(
        eval(
            d,
            r#"return toml.edit("Cargo.toml", "package.version", "0.2.0")"#
        ),
        "0.1.0\n"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("Cargo.toml")).unwrap(),
        cargo.replace("0.1.0", "0.2.0")
    );
    eval(
        d,
        r#"toml.edit("Cargo.toml", { "dependencies", "serde", "version" }, "1.0.229")"#,
    );
    assert!(
        std::fs::read_to_string(d.join("Cargo.toml"))
            .unwrap()
            .contains("serde = { version = \"1.0.229\", features = [\"derive\"] }\n")
    );
    let o = timon(
        d,
        &["eval", r#"toml.edit("Cargo.toml", "package.name.x", 1)"#],
        "",
    );
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("\"package.name\" is not a table"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn markdown_finds_headings_and_code_outside_code() {
    let tmp = tempfile::tempdir().unwrap();
    let out = eval(
        tmp.path(),
        r##"local text = "# timon\n\n```sh\n# not a heading\n```\n\nInstall\n-------\n\n```lua fragment\nreturn 1\n```\n"
           return json.encode({ h = markdown.headings(text), c = markdown.code(text, { lang = "lua" }) })"##,
    );
    assert_eq!(
        out,
        "{\"c\":[{\"code\":\"return 1\\n\",\"lang\":\"lua\",\"line\":10}],\"h\":[{\"level\":1,\"line\":1,\"text\":\"timon\"},{\"level\":2,\"line\":7,\"text\":\"Install\"}]}\n"
    );
}

#[test]
fn term_colors_only_on_a_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        eval(tmp.path(), r#"return term.color("red", "x"), term.tty()"#),
        "x\nfalse\n"
    );
    let o = timon(tmp.path(), &["eval", r#"term.color("gren", "x")"#], "");
    assert!(String::from_utf8_lossy(&o.stderr).contains("did you mean \"green\"?"));
}

#[test]
fn eval_binds_stdin_and_prints_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    assert_eq!(ok(d, &["eval", "-i", "return #input"], "a\nb\nc\n"), "3\n");
    assert_eq!(
        ok(
            d,
            &[
                "eval",
                "-i",
                "-o",
                "return tbl.filter(input, function(l) return l:find('%.rs$') end)"
            ],
            "main.rs\nREADME.md\nlib.rs\n"
        ),
        "main.rs\nlib.rs\n"
    );
    assert_eq!(
        ok(
            d,
            &[
                "eval",
                "-I",
                "return json.query(input[1], 'items[?ok].name')"
            ],
            "{\n  \"items\": [{\"name\": \"a\", \"ok\": true}, {\"name\": \"b\", \"ok\": false}]\n}\n"
        ),
        "[\"a\"]\n"
    );
    let events = "{\"kind\":\"a\",\"n\":1}\n{\"kind\":\"b\",\"n\":2}\n{\"kind\":\"a\",\"n\":3}\n";
    assert_eq!(
        ok(
            d,
            &[
                "eval",
                "-I",
                "--stream",
                "if input.kind == 'a' then return input.n end"
            ],
            events
        ),
        "1\n3\n"
    );
    assert_eq!(
        ok(
            d,
            &[
                "eval",
                "-I",
                "-O",
                "return tbl.map(input, function(e) return e.kind end)"
            ],
            events
        ),
        "\"a\"\n\"b\"\n\"a\"\n"
    );
    assert_eq!(
        ok(d, &["eval", "-o", "return json.array{}"], ""),
        "",
        "an empty list prints nothing"
    );
    for (args, expected) in [
        (
            &["eval", "-o", "return { a = 1 }"][..],
            "-o and -O print a list",
        ),
        (&["eval", "-i", "-"], "stdin is the input"),
        (&["eval", "--stream", "return 1"], "--stream needs -i or -I"),
        (&["eval", "return input"], "undefined global \"input\""),
    ] {
        let o = timon(d, args, "return 1");
        assert!(!o.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&o.stderr).contains(expected),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

#[test]
fn eval_io_flags_take_a_file_and_fennel_sees_input() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    std::fs::write(
        d.join("up.lua"),
        "local out = {}\nfor _, l in ipairs(input) do out[#out + 1] = l:upper() end\nreturn out\n",
    )
    .unwrap();
    std::fs::write(
        d.join("bang.fnl"),
        "(icollect [_ l (ipairs input)] (.. l \"!\"))\n",
    )
    .unwrap();
    assert_eq!(
        ok(d, &["eval", "-f", "up.lua", "-i", "-o"], "a\nb\n"),
        "A\nB\n"
    );
    assert_eq!(
        ok(d, &["eval", "-f", "bang.fnl", "-i", "-O"], "a\nb\n"),
        "\"a!\"\n\"b!\"\n"
    );
    assert_eq!(
        ok(d, &["eval", "--fnl", "-i", "(length input)"], "a\nb\n"),
        "2\n"
    );
    let o = timon(d, &["eval", "-f", "missing.lua", "-o"], "");
    assert!(String::from_utf8_lossy(&o.stderr).contains("io: cannot read missing.lua"));
}

/// #115: a reader that stops early ends timon quietly, without a panic on the broken pipe.
#[test]
fn a_reader_that_stops_early_ends_timon_quietly() {
    let tmp = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_timon"))
        .args([
            "eval",
            "-o",
            "local t = {} for i = 1, 200000 do t[i] = i end return t",
        ])
        .current_dir(tmp.path())
        .env("TIMON_HOME", tmp.path().join("timon"))
        .env("HOME", tmp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    assert_eq!(line, "1\n");
    drop(out);
    let o = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{:?}: {stderr}", o.status);
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn a_stream_answers_each_line_before_the_input_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_timon"))
        .args(["eval", "-i", "--stream", "return input:upper()"])
        .current_dir(tmp.path())
        .env("TIMON_HOME", tmp.path().join("timon"))
        .env("HOME", tmp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    for word in ["one", "two"] {
        writeln!(stdin, "{word}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        assert_eq!(line, format!("{}\n", word.to_uppercase()));
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn toml_edit_is_refused_while_tasks_load() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("p");
    std::fs::create_dir_all(project.join(".timon")).unwrap();
    std::fs::write(
        project.join(".timon/tasks.lua"),
        "toml.edit('Cargo.toml', 'package.version', '9')\nreturn {}",
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_timon"))
            .args(args)
            .current_dir(&project)
            .env("TIMON_HOME", tmp.path().join("timon"))
            .env("HOME", tmp.path())
            .output()
            .unwrap()
    };
    assert!(run(&["trust"]).status.success());
    let o = run(&["tasks"]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("toml.edit cannot run while tasks load"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}
