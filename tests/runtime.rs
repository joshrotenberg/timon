use std::time::Duration;

use mlua::Function;
use timon::{Error, Failure, Runtime};

/// Runs `code` in a fresh runtime with a few host functions installed, returning each value as
/// `timon eval` would print it.
fn eval(code: &str) -> Result<Vec<String>, Failure> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, async {
        let runtime = Runtime::new().unwrap();
        install_test_functions(&runtime);
        let values = runtime.eval(code, "=eval").await?;
        Ok(values
            .iter()
            .map(|v| timon::value::render(v, false).unwrap())
            .collect())
    })
}

fn install_test_functions(runtime: &Runtime) {
    let lua = runtime.lua();
    let fail: Function = lua
        .create_function(|_, ()| -> mlua::Result<()> {
            Err(Error::new("test_kind", "it failed")
                .with_hint("do something else")
                .into())
        })
        .unwrap();
    let slow_fail: Function = lua
        .create_async_function(|_, ms: u64| async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Err::<(), _>(mlua::Error::from(Error::new("slow_kind", "failed late")))
        })
        .unwrap();
    let slow_ok: Function = lua
        .create_async_function(|_, ms: u64| async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(ms)
        })
        .unwrap();
    let globals = lua.globals();
    globals
        .set("fail", runtime.host_function(fail).unwrap())
        .unwrap();
    globals
        .set("slow_fail", runtime.host_function(slow_fail).unwrap())
        .unwrap();
    globals
        .set("slow_ok", runtime.host_function(slow_ok).unwrap())
        .unwrap();
}

fn error_of(code: &str) -> Failure {
    eval(code).expect_err("expected the chunk to fail")
}

// Errors as tables (#3)

#[test]
fn pcall_sees_a_host_error_as_a_table() {
    let out = eval("local ok, e = pcall(fail); return ok, e.kind, e.message, e.hint, tostring(e)");
    assert_eq!(
        out.unwrap(),
        [
            "false",
            "test_kind",
            "it failed",
            "do something else",
            "test_kind: it failed"
        ]
    );
}

#[test]
fn try_returns_values_or_the_error_table() {
    assert_eq!(
        eval("return try(function(a, b) return a + b, 'x' end, 1, 2)").unwrap(),
        ["true", "3", "x"]
    );
    assert_eq!(
        eval("local ok, e = try(error, { kind = 'mine', message = 'm' }); return ok, tostring(e)")
            .unwrap(),
        ["false", "mine: m"]
    );
    assert_eq!(
        eval("local ok, e = try(error, 'plain'); return e.kind, e.message").unwrap(),
        ["lua", "plain"]
    );
}

#[test]
fn async_host_functions_yield_through_the_wrapper() {
    assert_eq!(eval("return slow_ok(5) + 1").unwrap(), ["6"]);
    assert_eq!(
        eval("local ok, e = try(slow_fail, 5); return ok, e.kind").unwrap(),
        ["false", "slow_kind"]
    );
}

#[test]
fn an_uncaught_error_keeps_its_kind_hint_and_place() {
    let failure = error_of("local x = 1\nfail()");
    assert_eq!(failure.error.kind, "test_kind");
    assert_eq!(failure.error.hint.as_deref(), Some("do something else"));
    assert_eq!(failure.traceback, ["eval:2"]);
}

#[test]
fn a_raised_table_keeps_its_fields() {
    let failure =
        error_of("error({ kind = 'auth_expired', message = 'log in', hint = 'run /login' })");
    assert_eq!(
        failure.error,
        Error::new("auth_expired", "log in").with_hint("run /login")
    );
}

#[test]
fn plain_lua_errors_are_kind_lua_with_their_position() {
    let failure = error_of("local t = nil\nreturn t.x");
    assert_eq!(failure.error.kind, "lua");
    assert!(failure.error.message.contains("eval:2"), "{failure}");
}

#[test]
fn errors_inside_functions_show_the_frames() {
    let failure =
        error_of("local function inner() fail() end\nlocal function outer() inner() end\nouter()");
    assert_eq!(
        failure.traceback,
        [
            "eval:1 in function 'inner'",
            "eval:2 in function 'outer'",
            "eval:3"
        ]
    );
}

#[test]
fn syntax_errors_are_kind_syntax() {
    assert_eq!(error_of("local = 1").error.kind, "syntax");
}

// Strict globals (#4)

#[test]
fn a_misspelled_global_suggests_the_real_one() {
    let failure = error_of("prnt('hi')");
    assert_eq!(failure.error.kind, "undefined_global");
    assert_eq!(failure.error.message, "undefined global \"prnt\"");
    assert_eq!(
        failure.error.hint.as_deref(),
        Some("did you mean \"print\"?")
    );
    assert_eq!(failure.traceback, ["eval:1"]);
}

#[test]
fn top_level_globals_are_allowed() {
    assert_eq!(eval("x = 5\nreturn x * 2").unwrap(), ["10"]);
}

#[test]
fn a_new_global_inside_a_function_is_refused() {
    let failure = error_of("local function f() y = 1 end\nf()");
    assert_eq!(failure.error.kind, "undeclared_global");
    assert!(failure.error.message.contains("\"y\""), "{failure}");
}

#[test]
fn an_existing_global_can_be_assigned_anywhere() {
    assert_eq!(
        eval("z = 1\nlocal function f() z = z + 1 end\nf()\nreturn z").unwrap(),
        ["2"]
    );
}

#[test]
fn rawget_tests_for_a_global_without_raising() {
    assert_eq!(eval("return rawget(_G, 'nope') == nil").unwrap(), ["true"]);
}

#[test]
fn globals_are_shared_across_chunks_through_g() {
    assert_eq!(eval("w = 3\nreturn _G.w").unwrap(), ["3"]);
}
