//! `timon serve` (#34): a project's tasks as MCP tools, driven by tower-mcp's client.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tower_mcp::client::{McpClient, StdioClientTransport};

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new(tasks: &str) -> Self {
        let s = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(s.project().join(".timon")).unwrap();
        std::fs::write(s.project().join(".timon/tasks.lua"), tasks).unwrap();
        let o = std::process::Command::new(env!("CARGO_BIN_EXE_timon"))
            .arg("trust")
            .current_dir(s.project())
            .env("TIMON_HOME", s.dir.path().join("timon"))
            .env("HOME", s.dir.path().join("home"))
            .output()
            .unwrap();
        assert!(o.status.success());
        s
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("home/proj")
    }

    async fn connect(&self) -> McpClient {
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_timon"));
        cmd.arg("serve")
            .current_dir(self.project())
            .env("TIMON_HOME", self.dir.path().join("timon"))
            .env("HOME", self.dir.path().join("home"))
            .stderr(std::process::Stdio::null());
        let transport = StdioClientTransport::spawn_command(&mut cmd).await.unwrap();
        let client = McpClient::connect(transport).await.unwrap();
        let init = client.initialize("test", "1").await.unwrap();
        assert_eq!(init.server_info.name, "timon");
        client
    }
}

const TASKS: &str = r#"return {
  greet = {
    description = "Greet someone",
    options = { name = { type = "string", default = "you", doc = "who to greet" }, loud = "boolean" },
    run = function(opts)
      print("noise from print")
      sh("echo noise from sh")
      return (opts.loud and "HELLO " or "hello ") .. opts.name
    end,
  },
  shout = { depends = { "greet" }, run = function(opts, deps) return deps.greet:upper() end },
  drop = { description = "Drop the table", confirm = "Drop the table?", run = function() return "dropped" end },
  wait = {
    run = function()
      local p = proc.spawn{ "sleep", "30" }
      fs.write("sleeper.pid", tostring(p:pid()))
      p:wait()
    end,
  },
  hidden = { private = true, run = "true" },
  ["lint:fix"] = "true",
}
"#;

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .unwrap()
        .success()
}

#[tokio::test]
async fn tasks_are_tools_with_typed_inputs() {
    let s = Sandbox::new(TASKS);
    let client = s.connect().await;
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    assert_eq!(
        names,
        ["drop", "eval", "greet", "lint.fix", "shout", "wait"]
    );
    let greet = serde_json::to_value(tools.iter().find(|t| t.name == "greet").unwrap()).unwrap();
    assert_eq!(greet["description"], "Greet someone.");
    let props = &greet["inputSchema"]["properties"];
    assert_eq!(props["name"]["type"], "string");
    assert_eq!(props["name"]["default"], "you");
    assert_eq!(props["name"]["description"], "who to greet");
    assert_eq!(props["loud"]["type"], "boolean");
    assert_eq!(props["args"]["type"], "array");
    assert_eq!(greet["inputSchema"]["additionalProperties"], false);
    let drop = serde_json::to_value(tools.iter().find(|t| t.name == "drop").unwrap()).unwrap();
    assert_eq!(
        drop["inputSchema"]["required"],
        serde_json::json!(["confirmed"])
    );
    assert!(
        drop["description"]
            .as_str()
            .unwrap()
            .contains("Asks first: \"Drop the table?\"")
    );

    let r = client
        .call_tool("greet", serde_json::json!({ "name": "Ann", "loud": true }))
        .await
        .unwrap();
    assert!(!r.is_error);
    assert_eq!(
        r.structured_content,
        Some(serde_json::json!({ "value": "HELLO Ann" }))
    );
    assert_eq!(
        r.all_text(),
        "HELLO Ann",
        "print and sh went to stderr, not the protocol"
    );
    let r = client
        .call_tool("shout", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(
        r.structured_content,
        Some(serde_json::json!({ "value": "HELLO YOU" }))
    );
    let r = client
        .call_tool(
            "eval",
            serde_json::json!({ "code": "return { 1 + 41, 'x' }" }),
        )
        .await
        .unwrap();
    assert_eq!(
        r.structured_content,
        Some(serde_json::json!({ "value": [42, "x"] }))
    );
    let r = client
        .call_tool(
            "eval",
            serde_json::json!({ "code": "(+ 1 2)", "fennel": true }),
        )
        .await
        .unwrap();
    assert_eq!(
        r.structured_content,
        Some(serde_json::json!({ "value": 3 }))
    );
}

#[tokio::test]
async fn confirm_needs_the_users_answer_and_failures_are_error_results() {
    let s = Sandbox::new(TASKS);
    let client = s.connect().await;
    let r = client
        .call_tool("drop", serde_json::json!({}))
        .await
        .unwrap();
    assert!(r.is_error);
    assert!(
        r.all_text()
            .contains("confirm: task \"drop\" asks: Drop the table?"),
        "{}",
        r.all_text()
    );
    let r = client
        .call_tool("drop", serde_json::json!({ "confirmed": true }))
        .await
        .unwrap();
    assert_eq!(
        r.structured_content,
        Some(serde_json::json!({ "value": "dropped" }))
    );
    let r = client
        .call_tool("eval", serde_json::json!({ "code": "error{ kind = 'lint', message = 'two findings', hint = 'fix them' }" }))
        .await
        .unwrap();
    assert!(r.is_error);
    let e = &r.structured_content.unwrap()["error"];
    assert_eq!(
        (&e["kind"], &e["message"], &e["hint"]),
        (&"lint".into(), &"two findings".into(), &"fix them".into())
    );
}

#[tokio::test]
async fn a_cancelled_call_stops_its_processes() {
    let s = Sandbox::new(TASKS);
    let client = s.connect().await;
    let call = client.call_tool("wait", serde_json::json!({}));
    assert!(
        tokio::time::timeout(Duration::from_millis(1500), call)
            .await
            .is_err()
    );
    let pid_file = s.project().join("sleeper.pid");
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    let mut gone = false;
    for _ in 0..30 {
        if !alive(&pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        gone,
        "the task's process stopped when the call was cancelled"
    );
    let r = client
        .call_tool("greet", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(
        r.structured_content,
        Some(serde_json::json!({ "value": "hello you" }))
    );
}

#[tokio::test]
async fn the_skills_are_resources() {
    let s = Sandbox::new(TASKS);
    let client = s.connect().await;
    let list = client.list_all_resources().await.unwrap();
    let uris: Vec<String> = list.iter().map(|r| r.uri.clone()).collect();
    assert!(
        uris.contains(&"timon://skills/timon-tasks".to_owned()),
        "{uris:?}"
    );
    let read = client.read_resource("timon://skills/timon").await.unwrap();
    let v = serde_json::to_value(&read).unwrap();
    assert!(
        v["contents"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("---\nname: timon")
    );
    let _ = Path::new("");
}
