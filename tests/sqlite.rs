//! The sqlite module (#92).

use std::path::Path;
use std::process::{Command, Output};

fn timon(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_timon"))
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("TIMON_HOME", dir.join("timon"))
        .output()
        .unwrap()
}

fn eval(dir: &Path, code: &str) -> String {
    let o = timon(dir, &["eval", code]);
    assert!(
        o.status.success(),
        "{code}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn fails(dir: &Path, code: &str) -> String {
    let o = timon(dir, &["eval", code]);
    assert!(!o.status.success(), "{code} should fail");
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn values_parameters_and_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    assert_eq!(
        eval(
            d,
            r#"local db = sqlite.open("data.db")
               db:exec("create table t (i integer, r real, s text, b blob, z)")
               local changed = db:exec("insert into t values (?, ?, ?, ?, ?)", { 1, 2.5, "x", "\0\1", json.null })
               db:exec("insert into t (i, s) values (:i, @s)", { i = 2, s = "named" })
               local rows = db:query("select * from t order by i")
               return changed, #rows, rows[1].r, rows[1].s, #rows[1].b, rows[1].z, rows[2].s, db:last_id(), json.encode(rows[2])"#
        ),
        "1\n2\n2.5\nx\n2\nnil\nnamed\n2\n{\"i\":2,\"s\":\"named\"}\n"
    );
    assert_eq!(
        eval(
            d,
            r#"return sqlite.open("data.db"):one("select count(*) as n from t").n"#
        ),
        "2\n",
        "the file keeps the rows"
    );
    assert_eq!(
        eval(
            d,
            r#"return sqlite.open("data.db"):one("select * from t where i = 99")"#
        ),
        "nil\n"
    );
    assert_eq!(
        eval(
            d,
            r#"return json.encode(sqlite.open():query("select 1 as a where 0"))"#
        ),
        "[]\n"
    );
    assert_eq!(
        eval(
            d,
            r#"local db = sqlite.open()
               return db:exec("create table a (x); create table b (x); insert into a values (1); insert into a values (2)")"#
        ),
        "2\n",
        "a batch returns the rows it changed"
    );
}

#[test]
fn transactions_commit_or_roll_back() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        eval(
            tmp.path(),
            r#"local db = sqlite.open()
               db:exec("create table t (i integer)")
               local ok, err = try(function()
                 db:transaction(function()
                   db:exec("insert into t values (1)")
                   error{ kind = "stop", message = "changed my mind" }
                 end)
               end)
               local n = db:transaction(function()
                 db:exec("insert into t values (2)")
                 sleep("10ms")
                 db:exec("insert into t values (3)")
                 return db:one("select count(*) as n from t").n
               end)
               return ok, err.kind, n, db:one("select group_concat(i) as all_ from t").all_"#
        ),
        "false\nstop\n2\n2,3\n"
    );
}

#[test]
fn mistakes_are_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    eval(d, r#"sqlite.open("ro.db"):exec("create table t (i)")"#);
    assert!(
        fails(
            d,
            r#"sqlite.open("ro.db", { readonly = true }):exec("insert into t values (1)")"#
        )
        .contains("sqlite_failed: attempt to write a readonly database")
    );
    assert!(
        fails(d, r#"sqlite.open("missing.db", { create = false })"#)
            .contains("sqlite_failed: cannot open missing.db")
    );
    assert!(!d.join("missing.db").exists());
    assert!(
        fails(d, r#"sqlite.open():query("select * from nope")"#).contains("no such table: nope")
    );
    assert!(
        fails(d, r#"sqlite.open():query("select :x", { y = 1 })"#)
            .contains("the SQL has no parameter :y")
    );
    assert!(
        fails(
            d,
            r#"local db = sqlite.open() db:close() db:query("select 1")"#
        )
        .contains("closed: the database :memory: is closed")
    );
    assert!(
        fails(d, r#"sqlite.open():query("select ?", { {} })"#)
            .contains("a table cannot be an SQL parameter")
    );
    assert!(
        fails(d, r#"sqlite.open("x.db", { readonyl = true })"#)
            .contains("did you mean \"readonly\"?")
    );
}

#[test]
fn opening_is_refused_while_tasks_load() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("p");
    std::fs::create_dir_all(project.join(".timon")).unwrap();
    std::fs::write(
        project.join(".timon/tasks.lua"),
        "sqlite.open('x.db')\nreturn {}",
    )
    .unwrap();
    // HOME is above the project: a project's .timon is found below HOME, not at it.
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_timon"))
            .args(args)
            .current_dir(&project)
            .env("HOME", tmp.path())
            .env("TIMON_HOME", tmp.path().join("timon"))
            .output()
            .unwrap()
    };
    assert!(run(&["trust"]).status.success());
    let o = run(&["tasks"]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("sqlite.open cannot run while tasks load"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!project.join("x.db").exists());
}
