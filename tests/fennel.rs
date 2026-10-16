use std::path::Path;

use timon::{Failure, Runtime};

fn block_on<T>(f: impl AsyncFnOnce(&Runtime) -> T) -> T {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, async {
        let runtime = Runtime::new().unwrap();
        f(&runtime).await
    })
}

fn render(values: mlua::MultiValue) -> Vec<String> {
    values
        .iter()
        .map(|v| timon::value::render(v, false).unwrap())
        .collect()
}

fn fennel(code: &str) -> Result<Vec<String>, Failure> {
    block_on(async |rt| rt.eval_fennel(code, "=eval").await.map(render))
}

fn script(path: &Path, args: &[&str]) -> Result<Vec<String>, Failure> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    block_on(async |rt| rt.run_script(path, &args).await.map(render))
}

#[test]
fn fennel_sees_the_same_modules() {
    assert_eq!(
        fennel("(json.encode {:name \"timon\" :tags [\"lua\" \"fennel\"]})").unwrap(),
        [r#"{"name":"timon","tags":["lua","fennel"]}"#]
    );
    assert_eq!(
        fennel("(-> [3 1 2] (tbl.sort_by (fn [n] n)) (json.encode))").unwrap(),
        ["[1,2,3]"]
    );
}

#[test]
fn fennel_uses_concurrency() {
    assert_eq!(
        fennel("(par.map [1 2 3] (fn [n] (sleep 0.001) (* n 2)) {:limit 2})").unwrap(),
        ["[2,4,6]"]
    );
}

#[test]
fn fennel_errors_are_tables_too() {
    let failure = fennel("(error {:kind :fennel_kind :message \"from fennel\"})").unwrap_err();
    assert_eq!(failure.error.kind, "fennel_kind");
    let failure = fennel("(prnt 1)").unwrap_err();
    assert_eq!(failure.error.kind, "undefined_global");
}

#[test]
fn a_compile_error_is_kind_syntax_with_fennels_message() {
    let failure = fennel("(let [x] x)").unwrap_err();
    assert_eq!(failure.error.kind, "syntax");
    assert!(failure.error.message.contains("eval:1"), "{failure}");
    assert!(
        !failure.error.message.contains("stack traceback"),
        "{failure}"
    );
}

#[test]
fn a_fennel_script_gets_arguments_and_skips_the_shebang() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("greet.fnl");
    std::fs::write(
        &path,
        "#!/usr/bin/env timon\n(local [who] [...])\n(str.fmt \"hello ${w} (${n})\" {:w who :n (length arg)})\n",
    )
    .unwrap();
    assert_eq!(script(&path, &["josh"]).unwrap(), ["hello josh (1)"]);
}

#[test]
fn require_finds_fennel_modules() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("helpers.fnl"),
        "(fn double [n] (* n 2))\n{: double}\n",
    )
    .unwrap();
    let main = dir.path().join("main.lua");
    std::fs::write(
        &main,
        format!(
            "package.path = '{}/?.lua;' .. package.path\nlocal h = require('helpers')\nreturn h.double(21)",
            dir.path().display()
        ),
    )
    .unwrap();
    assert_eq!(script(&main, &[]).unwrap(), ["42"]);
}
