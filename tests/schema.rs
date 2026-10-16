use std::process::Command;

use timon::{Failure, Runtime};

fn eval(code: &str) -> Result<Vec<String>, Failure> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, async {
        let runtime = Runtime::new().unwrap();
        let values = runtime.eval(code, "=eval").await?;
        Ok(values
            .iter()
            .map(|v| timon::value::render(v, false).unwrap())
            .collect())
    })
}

fn one(code: &str) -> String {
    let mut out = eval(code).unwrap_or_else(|f| panic!("{code}\n{f}"));
    assert_eq!(out.len(), 1, "{code}: {out:?}");
    out.remove(0)
}

#[test]
fn builders_produce_json_schema() {
    assert_eq!(
        one("json.encode(schema.json(schema.array(schema.enum{ 'a', 'b' }:describe('letters'))))"),
        r#"{"items":{"description":"letters","enum":["a","b"],"type":"string"},"type":"array"}"#
    );
    assert_eq!(
        one("json.encode(schema.json(schema.map(schema.int)))"),
        r#"{"additionalProperties":{"type":"integer"},"type":"object"}"#
    );
    assert_eq!(
        one("tostring(schema.object{ a = schema.any })"),
        "schema(object)"
    );
}

#[test]
fn check_passes_good_values_and_lists_every_problem() {
    assert_eq!(
        one(
            "schema.check(schema.object{ n = schema.int, xs = schema.array(schema.string) }, { n = 3, xs = {} })"
        ),
        "true"
    );
    let out = eval(
        "return schema.check(schema.object{ n = schema.int, opt = schema.optional(schema.bool) }, { n = 1.5, opt = 'yes' })",
    )
    .unwrap();
    assert_eq!(out[0], "false");
    assert_eq!(
        out[1],
        r#"[{"message":"expected an integer, got a number","path":"n"},{"message":"expected a boolean, got a string","path":"opt"}]"#
    );
    assert_eq!(
        one("schema.check(schema.optional(schema.string), json.null)"),
        "true"
    );
}

#[test]
fn builders_refuse_non_schemas() {
    let err = eval("schema.object{ a = 'string' }").unwrap_err().error;
    assert_eq!(err.kind, "bad_argument");
    assert_eq!(err.message, "field \"a\" must be a schema, not a string");
}

#[test]
fn builtin_contracts_load() {
    for name in [
        "review",
        "implementation",
        "investigation",
        "yes_no",
        "list",
    ] {
        assert_eq!(
            one(&format!("tostring(schema.contract('{name}'))")),
            "schema(object)"
        );
    }
    assert_eq!(
        one(
            "schema.check(schema.contract('review'), { verdict = 'approve', summary = 'ok', findings = { { severity = 'low', file = 'a.rs', title = 't', detail = 'd' } } })"
        ),
        "true"
    );
}

#[test]
fn layer_contracts_shadow_builtin_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let contracts = root.join("timon/config/contracts");
    std::fs::create_dir_all(&contracts).unwrap();
    std::fs::write(
        contracts.join("yes_no.lua"),
        "return schema.object{ answer = schema.bool }",
    )
    .unwrap();
    std::fs::write(
        contracts.join("score.fnl"),
        "(schema.object {:score schema.int})",
    )
    .unwrap();
    std::fs::write(
        contracts.join("bad.lua"),
        "fs.write('x', 'y')\nreturn schema.string",
    )
    .unwrap();
    let run = |code: &str| {
        let o = Command::new(env!("CARGO_BIN_EXE_timon"))
            .args(["eval", code])
            .current_dir(root)
            .env("TIMON_HOME", root.join("timon"))
            .env("HOME", root)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    };
    assert_eq!(
        run("json.encode(schema.json(schema.contract('yes_no')).required)").0,
        "[\"answer\"]\n"
    );
    assert_eq!(
        run("schema.check(schema.contract('score'), { score = 2 })").0,
        "true\n"
    );
    let (_, err) = run("schema.contract('bad')");
    assert!(err.contains("bad_config"), "{err}");
    assert!(!root.join("x").exists());
}
