//! The mcp client (#100), against a fake MCP server (tests/fixtures/fake-mcp).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fake() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-mcp")
        .display()
        .to_string()
}

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let s = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(s.project().join(".timon/mcp")).unwrap();
        s
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("home/proj")
    }

    fn timon(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_timon"))
            .args(args)
            .current_dir(self.project())
            .env("TIMON_HOME", self.dir.path().join("timon"))
            .env("HOME", self.dir.path().join("home"))
            .output()
            .unwrap()
    }

    fn eval(&self, code: &str) -> String {
        let o = self.timon(&["eval", code]);
        assert!(
            o.status.success(),
            "{code}\n{}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn fails(&self, code: &str) -> String {
        let o = self.timon(&["eval", code]);
        assert!(!o.status.success(), "{code} should fail");
        String::from_utf8_lossy(&o.stderr).into_owned()
    }

    fn server(&self, name: &str, text: &str) {
        std::fs::write(self.project().join(format!(".timon/mcp/{name}.lua")), text).unwrap();
    }
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .unwrap()
        .success()
}

#[test]
fn a_connection_lists_calls_and_reads() {
    let s = Sandbox::new();
    let f = fake();
    assert_eq!(
        s.eval(&format!(
            r#"local c = mcp.connect{{ command = {{ "{f}" }} }}
               local tools = c:tools()
               return c.name, c.server.name, c.server.version, #tools, tools[1].name, tools[1].input_schema.required[1],
                 c:call("echo", {{ text = "hi" }}), c:call("add", {{ a = 2, b = 3 }}).sum,
                 c:resources()[1].uri, c:read("fake://readme")"#
        )),
        "fake-mcp\nfake-mcp\n1.2.3\n5\necho\ntext\nhi\n5\nfake://readme\n# fake\nhello\n"
    );
    assert_eq!(
        s.eval(&format!(
            r#"local c = mcp.connect{{ command = {{ "{f}" }} }}
               local ok, err = try(c.call, c, "fail")
               return ok, err.kind, err.message, err.data.content[1].text"#
        )),
        "false\nmcp_tool_failed\nfake-mcp: fail: the tool broke\nthe tool broke\n"
    );
    assert!(
        s.fails(&format!(
            r#"local c = mcp.connect{{ command = {{ "{f}" }} }} c:close() c:call("echo", {{ text = "x" }})"#
        ))
        .contains("closed: the connection to fake-mcp is closed")
    );
}

#[test]
fn a_server_runs_in_its_own_group_and_stops_with_its_scope() {
    let s = Sandbox::new();
    let f = fake();
    let out = s.eval(&format!(
        r#"local who
           scope(function()
             who = mcp.connect{{ command = {{ "{f}" }} }}:call("whoami")
           end)
           return who.pid, who.pgid"#
    ));
    let mut lines = out.lines();
    let (pid, pgid) = (lines.next().unwrap(), lines.next().unwrap());
    assert_eq!(pid, pgid, "the server leads its own process group");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(!alive(pid), "the server stopped when its scope ended");
}

#[test]
fn starting_fails_clearly() {
    let s = Sandbox::new();
    assert!(
        s.fails(r#"mcp.connect{ command = { "no-such-mcp-server" } }"#)
            .contains("not_found: cannot start `no-such-mcp-server`")
    );
    let start = std::time::Instant::now();
    let e = s.fails(r#"mcp.connect({ command = { "sleep", "30" } }, { timeout = "500ms" })"#);
    assert!(
        e.contains("timeout: `sleep 30` did not start and initialize"),
        "{e}"
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
    assert!(
        s.fails("mcp.connect(42)")
            .contains("takes a server name or")
    );
    assert!(
        s.fails(r#"mcp.connect{ comand = { "x" } }"#)
            .contains("did you mean \"command\"?")
    );
}

#[test]
fn named_servers_come_from_the_layers() {
    let s = Sandbox::new();
    let f = fake();
    s.server(
        "fake",
        &format!("return {{ description = 'The fake server', command = {{ '{f}' }}, env = {{ FAKE = '1' }} }}\n"),
    );
    let o = s.timon(&[
        "eval",
        "return mcp.connect('fake'):call('echo', { text = 'x' })",
    ]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("no MCP server named \"fake\""),
        "an untrusted project's servers are not loaded: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(s.timon(&["trust"]).status.success());
    assert_eq!(
        s.eval("return mcp.connect('fake'):call('echo', { text = 'named' })"),
        "named\n"
    );
    assert_eq!(
        s.eval("local l = mcp.servers() return #l, l[1].name, l[1].layer, l[1].description"),
        "1\nfake\nproject\nThe fake server\n"
    );
    assert!(
        s.fails("mcp.connect('fak')")
            .contains("did you mean \"fake\"?")
    );
    s.server("typo", "return { descripton = 'x', command = { 'x' } }\n");
    assert!(
        s.fails("mcp.connect('typo')")
            .contains("did you mean \"description\"?")
    );
    s.server(
        "effects",
        "fs.write('x', 'y')\nreturn { command = { 'x' } }\n",
    );
    assert!(
        s.fails("mcp.connect('effects')").contains("bad_mcp_server"),
        "definitions load without effects"
    );
}

#[test]
fn the_cli_lists_and_calls() {
    let s = Sandbox::new();
    let f = fake();
    s.server(
        "fake",
        &format!("return {{ description = 'The fake server', command = {{ '{f}' }} }}\n"),
    );
    assert!(s.timon(&["trust"]).status.success());
    let out = |args: &[&str]| {
        let o = s.timon(args);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    assert_eq!(out(&["mcp", "servers"]), "fake  project  The fake server\n");
    assert!(
        out(&["mcp", "tools", "fake"])
            .starts_with("echo  Echo the text back\nadd  Add two numbers\n")
    );
    let tools: serde_json::Value =
        serde_json::from_str(&out(&["mcp", "tools", "fake", "--json"])).unwrap();
    assert_eq!(tools[1]["name"], "add");
    assert_eq!(
        out(&["mcp", "call", "fake", "add", "a=2", "b=40"]),
        "{\"sum\":42}\n"
    );
    assert_eq!(
        out(&["mcp", "call", "fake", "echo", "text=hello world"]),
        "hello world\n"
    );
    let o = s.timon(&["mcp", "call", "fake", "echo", "nokey"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn connecting_is_refused_while_tasks_load() {
    let s = Sandbox::new();
    std::fs::write(
        s.project().join(".timon/tasks.lua"),
        format!(
            "mcp.connect{{ command = {{ '{}' }} }}\nreturn {{}}\n",
            fake()
        ),
    )
    .unwrap();
    assert!(s.timon(&["trust"]).status.success());
    let o = s.timon(&["tasks"]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("mcp.connect cannot run while tasks load"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}
