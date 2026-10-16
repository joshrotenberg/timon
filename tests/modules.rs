use timon::{Failure, Runtime};

/// Runs `code` in a fresh runtime and returns each value as `timon eval` prints it.
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
    assert_eq!(out.len(), 1, "{code} returned {out:?}");
    out.remove(0)
}

fn fails(code: &str) -> timon::Error {
    eval(code).expect_err(code).error
}

// Options and help (#5)

#[test]
fn an_unknown_option_suggests_the_declared_one() {
    let err = fails("json.encode({}, { prety = true })");
    assert_eq!(err.kind, "bad_option");
    assert_eq!(err.message, "unknown option \"prety\" to json.encode");
    assert_eq!(err.hint.as_deref(), Some("did you mean \"pretty\"?"));
}

#[test]
fn an_option_of_the_wrong_type_names_both_types() {
    let err = fails("fs.ls('.', { hidden = 'yes' })");
    assert_eq!(
        err.message,
        "option \"hidden\" to fs.ls must be a boolean, got a string"
    );
}

#[test]
fn positional_options_are_refused() {
    let err = fails("fs.ls('.', { true })");
    assert_eq!(err.kind, "bad_option");
    assert!(err.message.contains("must be named"), "{err}");
    assert_eq!(err.hint.as_deref(), Some("fs.ls takes hidden"));
}

#[test]
fn help_describes_modules_and_functions() {
    assert!(one("help()").contains("json"));
    assert!(one("help(json)").contains("json.encode(value, { pretty }) -> string"));
    let encode = one("help(json.encode)");
    assert!(encode.starts_with("json.encode(value, { pretty }) -> string"));
    assert!(encode.contains("Options:\n  pretty  boolean  indent with two spaces"));
    assert!(encode.contains("Example:"));
    assert!(one("help('str.fmt')").starts_with("str.fmt(template, vars)"));
    assert!(one("help(try)").starts_with("try(f, ...)"));
    assert!(one("help(('x').trim)").starts_with("str.trim"));
}

#[test]
fn help_for_an_unknown_name_suggests_one() {
    let err = fails("help('jsno')");
    assert_eq!(err.kind, "not_found");
    assert_eq!(err.hint.as_deref(), Some("did you mean \"json\"?"));
}

// json (#6)

#[test]
fn json_encodes_sequences_as_arrays_and_sorts_keys() {
    assert_eq!(
        one("json.encode({ b = { 1, 2 }, a = 'x' })"),
        r#"{"a":"x","b":[1,2]}"#
    );
    assert_eq!(
        one("json.encode({ a = 1 }, { pretty = true })"),
        "{\n  \"a\": 1\n}"
    );
}

#[test]
fn json_shapes_survive_a_round_trip() {
    let text = r#"{"empty_a":[],"empty_o":{},"n":null,"xs":[1,null,3]}"#;
    assert_eq!(one(&format!("json.encode(json.decode('{text}'))")), text);
    assert_eq!(
        one("json.encode({ items = json.array{} })"),
        r#"{"items":[]}"#
    );
    assert_eq!(one("json.encode(json.object{})"), "{}");
}

#[test]
fn json_null_is_a_sentinel() {
    assert_eq!(one("json.decode('null') == json.null"), "true");
    assert_eq!(one("tostring(json.null)"), "null");
    assert_eq!(one("json.encode({ json.null })"), "[null]");
}

#[test]
fn json_refuses_values_it_cannot_represent() {
    assert_eq!(fails("json.encode({ f = print })").kind, "not_json");
    let hole = fails("json.encode({ [1] = 'a', [3] = 'c' })");
    assert_eq!(hole.message, "an array has a hole at index 2");
    assert_eq!(fails("json.decode('{nope')").kind, "bad_json");
}

// str (#6)

#[test]
fn str_splits_trims_and_tests() {
    assert_eq!(one("str.split('a,b,,c', ',')"), r#"["a","b","","c"]"#);
    assert_eq!(one("('  one   two '):split()"), r#"["one","two"]"#);
    assert_eq!(one("json.encode(str.split('', ','))"), r#"[""]"#);
    assert_eq!(one("('  hi \\n'):trim()"), "hi");
    assert_eq!(one("str.lines('a\\r\\nb\\n')"), r#"["a","b"]"#);
    assert_eq!(one("('feat: x'):starts('feat:')"), "true");
    assert_eq!(one("('x.lua'):ends('.rs')"), "false");
    assert_eq!(fails("str.split('a', '')").kind, "bad_argument");
}

#[test]
fn str_fmt_fills_placeholders() {
    assert_eq!(
        one("str.fmt('PR ${n} touches ${files}', { n = 12, files = { 'a.rs', 'b.rs' } })"),
        r#"PR 12 touches ["a.rs","b.rs"]"#
    );
    assert_eq!(one("str.fmt('${1}-${2}', { 'a', 'b' })"), "a-b");
    assert_eq!(
        one("str.fmt('cost: $5, literal $${x}', {})"),
        "cost: $5, literal ${x}"
    );
    assert_eq!(
        fails("str.fmt('${missing}', {})").message,
        "str.fmt: no value for ${missing}"
    );
}

// tbl (#6)

#[test]
fn tbl_transforms_arrays() {
    assert_eq!(
        one("tbl.map({ 1, 2, 3, 4 }, function(n) if n % 2 == 0 then return n * 10 end end)"),
        "[20,40]"
    );
    assert_eq!(one("json.encode(tbl.map({}, tostring))"), "[]");
    assert_eq!(
        one("tbl.filter({ 1, 2, 3 }, function(n) return n > 1 end)"),
        "[2,3]"
    );
    assert_eq!(
        one("tbl.reduce({ 1, 2, 3 }, function(a, n) return a + n end, 10)"),
        "16"
    );
    assert_eq!(
        one("tbl.reduce({ 1, 2, 3 }, function(a, n) return a * n end)"),
        "6"
    );
    assert_eq!(
        eval("return tbl.find({ 'a', 'b' }, function(v) return v == 'b' end)").unwrap(),
        ["b", "2"]
    );
}

#[test]
fn tbl_keys_values_and_grouping_are_ordered() {
    assert_eq!(one("tbl.keys({ b = 1, a = 2, [1] = 3 })"), r#"[1,"a","b"]"#);
    assert_eq!(one("tbl.values({ b = 1, a = 2 })"), "[2,1]");
    assert_eq!(
        one("tbl.group_by({ 'apple', 'avocado', 'banana' }, function(s) return s:sub(1, 1) end)"),
        r#"{"a":["apple","avocado"],"b":["banana"]}"#
    );
    assert_eq!(
        one(
            "tbl.map(tbl.sort_by({ { k = 2, n = 'x' }, { k = 1, n = 'y' }, { k = 2, n = 'z' } }, function(v) return v.k end), function(v) return v.n end)"
        ),
        r#"["y","x","z"]"#
    );
    assert_eq!(one("tbl.count({ a = 1, b = 2 })"), "2");
    assert_eq!(
        one("tbl.count({ 1, 2, 3 }, function(n) return n > 1 end)"),
        "2"
    );
}

#[test]
fn tbl_merges_and_picks() {
    assert_eq!(
        one("tbl.merge({ a = 1, b = 1 }, nil, { b = 2 })"),
        r#"{"a":1,"b":2}"#
    );
    assert_eq!(
        one("tbl.pick({ a = 1, b = 2, c = 3 }, { 'a', 'c' })"),
        r#"{"a":1,"c":3}"#
    );
    assert_eq!(one("tbl.omit({ a = 1, b = 2 }, { 'a' })"), r#"{"b":2}"#);
}

#[test]
fn tbl_argument_errors_are_tables() {
    let err = fails("tbl.map('nope', print)");
    assert_eq!(err.kind, "bad_argument");
    assert_eq!(
        err.message,
        "bad argument #1 to tbl.map: expected a table, got a string"
    );
}

// fs and path (#6)

#[test]
fn fs_reads_writes_and_lists() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().display().to_string();
    let code = format!(
        r#"
        local d = '{d}'
        fs.write(path.join(d, 'a.txt'), 'one')
        fs.write(path.join(d, 'a.txt'), ' two', {{ append = true }})
        fs.write(path.join(d, 'sub', 'b.lua'), 'return 1', {{ mkdir = true }})
        fs.write(path.join(d, '.hidden'), '')
        fs.mkdir(path.join(d, 'x', 'y'))
        return fs.read(path.join(d, 'a.txt')), fs.ls(d), fs.ls(d, {{ hidden = true }}),
          #fs.glob(path.join(d, '**', '*.lua')), fs.exists(path.join(d, 'x', 'y')),
          fs.exists(path.join(d, 'nope'))
        "#
    );
    assert_eq!(
        eval(&code).unwrap(),
        [
            "one two",
            r#"["a.txt","sub","x"]"#,
            r#"[".hidden","a.txt","sub","x"]"#,
            "1",
            "true",
            "false"
        ]
    );
}

#[test]
fn fs_missing_files_are_not_found() {
    let err = fails("fs.read('/definitely/not/here.txt')");
    assert_eq!(err.kind, "not_found");
    assert!(
        err.message
            .starts_with("cannot read /definitely/not/here.txt")
    );
}

#[test]
fn fs_tmpdir_makes_a_fresh_directory() {
    assert_eq!(
        one("local d = fs.tmpdir(); local ok = fs.exists(d) and #fs.ls(d) == 0; return ok"),
        "true"
    );
}

#[test]
fn path_names() {
    assert_eq!(
        eval("return path.join('src', 'main.rs'), path.base('src/main.rs'), path.dir('src/main.rs'), path.dir('main.rs'), path.ext('a/b.lua'), path.ext('Makefile')")
            .unwrap(),
        ["src/main.rs", "main.rs", "src", ".", "lua", ""]
    );
}
