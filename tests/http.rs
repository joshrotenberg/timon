//! The http and gh modules (#90), against a server in the test process. Nothing here reaches
//! the network.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use common::{Server, serve};

fn timon(home: &Path, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_timon"));
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("TIMON_HOME", home.join("timon"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn ok(home: &Path, code: &str, env: &[(&str, &str)]) -> String {
    let o = timon(home, home, &["eval", code], env);
    assert!(
        o.status.success(),
        "{code}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(home: &Path, code: &str, env: &[(&str, &str)]) -> String {
    let o = timon(home, home, &["eval", code], env);
    assert!(!o.status.success(), "{code} should fail");
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn http_requests_and_responses_are_data() {
    let server = serve(|req, _| match req.path.as_str() {
        "/missing" => (404, vec![], "{\"message\":\"gone\"}".into()),
        "/slow" => {
            std::thread::sleep(std::time::Duration::from_secs(2));
            (200, vec![], "late".into())
        }
        _ => (
            200,
            vec![("x-seen".into(), "yes".into())],
            serde_json::json!({ "method": req.method, "path": req.path, "body": req.body })
                .to_string(),
        ),
    });
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let base = &server.base;

    assert_eq!(
        ok(
            home,
            &format!(
                "local r = http.get('{base}/items', {{ query = {{ q = 'a b', n = 2 }}, headers = {{ ['x-token'] = 't' }} }})
                 return r.status, r.ok, r.headers['x-seen'], r.json().path"
            ),
            &[]
        ),
        "200\ntrue\nyes\n/items?n=2&q=a+b\n"
    );
    let first = server.seen.lock().unwrap()[0].clone();
    assert_eq!(first.header("x-token"), Some("t"));
    assert!(first.header("user-agent").unwrap().starts_with("timon/"));

    assert_eq!(
        ok(
            home,
            &format!(
                "local r = http.post('{base}/new', {{ json = {{ name = 'timon' }} }}) return r.json().body"
            ),
            &[]
        ),
        "{\"name\":\"timon\"}\n"
    );
    let post = server.seen.lock().unwrap().last().unwrap().clone();
    assert_eq!(post.header("content-type"), Some("application/json"));
    assert_eq!(
        ok(
            home,
            &format!(
                "return http.request{{ method = 'put', url = '{base}/x', body = 'raw' }}.json().method"
            ),
            &[]
        ),
        "PUT\n"
    );

    assert_eq!(
        ok(
            home,
            &format!("local r = http.get('{base}/missing') return r.status, r.ok"),
            &[]
        ),
        "404\nfalse\n"
    );
    let e = err(
        home,
        &format!("http.get('{base}/missing', {{ check = true }})"),
        &[],
    );
    assert!(
        e.contains(&format!("http_status: GET {base}/missing returned 404")),
        "{e}"
    );
    let e = err(
        home,
        &format!("http.get('{base}/slow', {{ timeout = '300ms' }})"),
        &[],
    );
    assert!(e.contains("timeout: GET"), "{e}");
    let e = err(home, "http.get('http://127.0.0.1:1/')", &[]);
    assert!(e.contains("http_failed: GET http://127.0.0.1:1/"), "{e}");
    let e = err(
        home,
        &format!("http.post('{base}/', {{ body = 'a', json = {{}} }})"),
        &[],
    );
    assert!(e.contains("give body or json, not both"), "{e}");
    let e = err(home, &format!("http.get('{base}/', {{ timout = 1 }})"), &[]);
    assert!(e.contains("did you mean \"timeout\"?"), "{e}");
}

fn github() -> Server {
    serve(|req, me| {
        let path = req.path.as_str();
        if req.header("authorization") == Some("Bearer bad") {
            return (401, vec![], "{\"message\":\"Bad credentials\"}".into());
        }
        let pr = |n: i64| {
            serde_json::json!({
                "number": n, "title": format!("pr {n}"), "state": "open", "draft": false,
                "user": { "login": "ann" }, "head": { "ref": "feat", "sha": "abc" }, "base": { "ref": "main" },
                "html_url": format!("https://github.com/o/r/pull/{n}"), "labels": [{ "name": "bug" }],
                "body": "the body", "merged": false, "mergeable": true,
            })
        };
        let json = |v: serde_json::Value| (200, vec![], v.to_string());
        match (req.method.as_str(), path) {
            ("GET", "/repos/o/r") => json(
                serde_json::json!({ "full_name": "o/r", "default_branch": "main", "license": null }),
            ),
            ("GET", "/repos/o/r/pulls?per_page=100&state=open") => (
                200,
                vec![(
                    "link".into(),
                    format!("<{me}/repos/o/r/pulls?page=2>; rel=\"next\""),
                )],
                serde_json::json!([pr(3), pr(2)]).to_string(),
            ),
            ("GET", "/repos/o/r/pulls?page=2") => json(serde_json::json!([pr(1)])),
            ("GET", "/repos/o/r/pulls/7") => json(pr(7)),
            ("GET", "/repos/o/r/pulls/7/files?per_page=100") => json(serde_json::json!([
                { "filename": "src/a.rs", "status": "modified", "additions": 3, "deletions": 1 }
            ])),
            ("GET", "/repos/o/r/pulls/7/reviews?per_page=100") => json(serde_json::json!([
                { "user": { "login": "bob" }, "state": "APPROVED", "body": "" }
            ])),
            ("GET", "/repos/o/r/commits/abc/check-runs")
            | ("GET", "/repos/o/r/commits/abc/check-runs?per_page=100") => {
                json(serde_json::json!({
                    "check_runs": [{ "name": "test", "status": "completed", "conclusion": "success", "html_url": "u" }]
                }))
            }
            ("GET", "/repos/o/r/issues?labels=bug%2Cp1&per_page=100&state=open") => {
                json(serde_json::json!([
                    { "number": 9, "title": "an issue", "state": "open", "user": { "login": "ann" }, "labels": [], "html_url": "i", "comments": 2, "created_at": "t" },
                    { "number": 8, "title": "a pr", "pull_request": {}, "user": { "login": "ann" } }
                ]))
            }
            ("POST", "/repos/o/r/releases") => (201, vec![], req.body.clone()),
            ("POST", "/graphql") if req.body.contains("broken") => {
                json(serde_json::json!({ "errors": [{ "message": "Field 'x' doesn't exist" }] }))
            }
            ("POST", "/graphql") => {
                json(serde_json::json!({ "data": { "viewer": { "login": "ann" } } }))
            }
            _ => (
                404,
                vec![],
                "{\"message\":\"Not Found\",\"documentation_url\":\"d\"}".into(),
            ),
        }
    })
}

#[test]
fn gh_reads_and_writes_through_the_api() {
    let server = github();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let env = [
        ("TIMON_GITHUB_API", server.base.as_str()),
        ("GH_TOKEN", "secret"),
    ];
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(home)
                .env("HOME", home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["remote", "add", "origin", "git@github.com:o/r.git"]);

    assert_eq!(
        ok(
            home,
            "local r = gh.repo() return r.full_name, r.default_branch, r.license",
            &env
        ),
        "o/r\nmain\nnil\n"
    );
    assert_eq!(
        server.seen.lock().unwrap()[0].header("authorization"),
        Some("Bearer secret")
    );
    assert_eq!(
        ok(
            home,
            "local prs = gh.prs() return #prs, prs[1].number, prs[3].number, prs[1].author, prs[1].labels[1]",
            &env
        ),
        "3\n3\n1\nann\nbug\n"
    );
    assert_eq!(ok(home, "return #gh.prs{ limit = 2 }", &env), "2\n");
    assert_eq!(
        ok(
            home,
            "local p = gh.pr(7, { repo = 'o/r' }) return p.files[1].path, p.reviews[1].state, p.checks[1].conclusion, p.mergeable",
            &env
        ),
        "src/a.rs\nAPPROVED\nsuccess\ntrue\n"
    );
    assert_eq!(
        ok(
            home,
            "local i = gh.issues{ labels = { 'bug', 'p1' } } return #i, i[1].number",
            &env
        ),
        "1\n9\n"
    );
    assert_eq!(ok(home, "return gh.checks('abc')[1].name", &env), "test\n");
    assert_eq!(
        ok(
            home,
            "return gh.api('repos/{owner}/{repo}/releases', { method = 'POST', json = { tag_name = 'v0.2.0' } }).tag_name",
            &env
        ),
        "v0.2.0\n"
    );
    assert_eq!(
        ok(
            home,
            "return gh.graphql('query { viewer { login } }').viewer.login",
            &env
        ),
        "ann\n"
    );
    let e = err(home, "gh.graphql('query { broken }')", &env);
    assert!(
        e.contains("github_failed: GraphQL: Field 'x' doesn't exist"),
        "{e}"
    );
    let e = err(home, "gh.api('repos/{owner}/{repo}/nothing')", &env);
    assert!(
        e.contains("not_found: GET repos/o/r/nothing: 404 Not Found"),
        "{e}"
    );
    let bad = [
        ("TIMON_GITHUB_API", server.base.as_str()),
        ("GH_TOKEN", "bad"),
    ];
    let e = err(home, "gh.repo()", &bad);
    assert!(
        e.contains("not_authenticated") && e.contains("Bad credentials"),
        "{e}"
    );
}

#[test]
fn the_token_comes_from_the_environment_then_gh() {
    let server = github();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let api = server.base.as_str();
    let e = err(
        home,
        "gh.repo{ repo = 'o/r' }",
        &[("TIMON_GITHUB_API", api), ("PATH", bin.to_str().unwrap())],
    );
    assert!(e.contains("not_authenticated: no GitHub token"), "{e}");
    let fake = bin.join("gh");
    std::fs::write(&fake, "#!/bin/sh\necho from-gh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!("{}:/bin:/usr/bin", bin.display());
    ok(
        home,
        "gh.repo{ repo = 'o/r' }",
        &[("TIMON_GITHUB_API", api), ("PATH", &path)],
    );
    ok(
        home,
        "gh.repo{ repo = 'o/r' }",
        &[
            ("TIMON_GITHUB_API", api),
            ("PATH", &path),
            ("GITHUB_TOKEN", "from-env"),
        ],
    );
    let auth: Vec<String> = server
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|s| s.header("authorization").map(str::to_owned))
        .collect();
    assert_eq!(auth, ["Bearer from-gh", "Bearer from-env"]);
}

#[test]
fn http_is_refused_while_tasks_load() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let project = home.join("p");
    std::fs::create_dir_all(project.join(".timon")).unwrap();
    std::fs::write(
        project.join(".timon/tasks.lua"),
        "http.get('http://127.0.0.1:1/')\nreturn {}",
    )
    .unwrap();
    assert!(timon(home, &project, &["trust"], &[]).status.success());
    let o = timon(home, &project, &["tasks"], &[]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("an HTTP request cannot run while tasks load"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}
