//! The repository's release tasks (#94), from `.timon/lib/release.lua`, run against a sandbox
//! crate, a bare remote and a local GitHub API. Nothing here reaches the network.

mod common;

use std::path::Path;
use std::process::{Command, Output};

fn isolated(cmd: &mut Command, home: &Path) {
    cmd.env("HOME", home)
        .env("TIMON_HOME", home.join("timon"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env_remove("GITHUB_TOKEN")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY");
}

fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    isolated(&mut cmd, home);
    let o = cmd.output().unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

fn timon(home: &Path, dir: &Path, api: &str, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_timon"));
    cmd.args(args)
        .current_dir(dir)
        .env("TIMON_GITHUB_API", api)
        .env("GH_TOKEN", "test-token")
        .env("CARGO_TARGET_DIR", home.join("target"));
    isolated(&mut cmd, home);
    cmd.output().unwrap()
}

#[test]
fn bump_opens_a_pull_request_and_release_tags_and_publishes() {
    let server = common::serve(|req, _| {
        let json = |status: u16, v: serde_json::Value| (status, vec![], v.to_string());
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/repos/o/r") => json(200, serde_json::json!({ "default_branch": "main" })),
            ("POST", "/repos/o/r/pulls") => json(
                201,
                serde_json::json!({ "html_url": "https://github.com/o/r/pull/7" }),
            ),
            ("POST", "/repos/o/r/releases") => json(
                201,
                serde_json::json!({ "html_url": "https://github.com/o/r/releases/tag/v0.2.0" }),
            ),
            _ => json(404, serde_json::json!({ "message": "Not Found" })),
        }
    });
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    let remote = home.join("remote.git");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join(".timon/lib")).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(".timon/lib/release.lua");
    std::fs::copy(source, repo.join(".timon/lib/release.lua")).unwrap();
    std::fs::write(
        repo.join(".timon/tasks.lua"),
        "local release = require('release')\nreturn { bump = release.bump, release = release.release }\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"sandbox\"\nversion = \"0.1.0\" # set by timon bump\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/lib.rs"), "").unwrap();
    std::fs::write(repo.join(".gitignore"), "target\n").unwrap();
    git(home, &repo, &["init", "-q", "-b", "main"]);
    let generate = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "--quiet"])
        .current_dir(&repo)
        .env("CARGO_TARGET_DIR", home.join("target"))
        .status()
        .unwrap();
    assert!(generate.success());
    git(home, &repo, &["add", "."]);
    git(home, &repo, &["commit", "-q", "-m", "start"]);
    git(
        home,
        home,
        &["init", "-q", "--bare", remote.to_str().unwrap()],
    );
    git(
        home,
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(home, &repo, &["push", "-q", "-u", "origin", "main"]);
    let o = timon(home, &repo, &server.base, &["trust"]);
    assert!(o.status.success());

    let run = |args: &[&str]| {
        let o = timon(home, &repo, &server.base, args);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()
    };
    let bumped = run(&["bump", "--version", "0.2.0", "--repo", "o/r"]);
    assert_eq!(bumped["from"], "0.1.0");
    assert_eq!(bumped["pr"], "https://github.com/o/r/pull/7");
    assert_eq!(
        git(home, &repo, &["branch", "--show-current"]),
        "release/v0.2.0"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("Cargo.toml")).unwrap(),
        "[package]\nname = \"sandbox\"\nversion = \"0.2.0\" # set by timon bump\nedition = \"2021\"\n"
    );
    assert!(
        std::fs::read_to_string(repo.join("Cargo.lock"))
            .unwrap()
            .contains("version = \"0.2.0\"")
    );
    assert_eq!(
        git(home, &repo, &["log", "-1", "--format=%s"]),
        "chore: release v0.2.0"
    );
    assert!(git(home, &remote, &["branch", "--list", "release/v0.2.0"]).contains("release/v0.2.0"));
    let pr = server
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.path == "/repos/o/r/pulls")
        .cloned()
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&pr.body).unwrap();
    assert_eq!(
        (&body["head"], &body["base"]),
        (&"release/v0.2.0".into(), &"main".into())
    );

    // Merging the pull request: main moves to the release commit.
    git(home, &repo, &["switch", "-q", "main"]);
    git(home, &repo, &["merge", "-q", "--ff-only", "release/v0.2.0"]);
    git(home, &repo, &["push", "-q", "origin", "main"]);

    let o = timon(home, &repo, &server.base, &["release", "--repo", "o/r"]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("confirm: task \"release\" asks"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let released = run(&["--yes", "release", "--repo", "o/r"]);
    assert_eq!(released["tag"], "v0.2.0");
    assert_eq!(
        released["url"],
        "https://github.com/o/r/releases/tag/v0.2.0"
    );
    assert_eq!(
        git(
            home,
            &remote,
            &[
                "for-each-ref",
                "--format=%(refname) %(objecttype)",
                "refs/tags"
            ]
        ),
        "refs/tags/v0.2.0 tag"
    );
    let rel = server
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.path == "/repos/o/r/releases")
        .cloned()
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&rel.body).unwrap();
    assert_eq!(body["tag_name"], "v0.2.0");
    assert_eq!(body["generate_release_notes"], true);
    assert_eq!(rel.header("authorization"), Some("Bearer test-token"));

    let o = timon(
        home,
        &repo,
        &server.base,
        &["--yes", "release", "--repo", "o/r"],
    );
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("release: v0.2.0 exists already"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    std::fs::write(repo.join("dirty.txt"), "x").unwrap();
    let o = timon(
        home,
        &repo,
        &server.base,
        &["bump", "--version", "0.3.0", "--repo", "o/r"],
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("the working tree has changes"));
    let o = timon(
        home,
        &repo,
        &server.base,
        &["bump", "--version", "two", "--repo", "o/r"],
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("\"two\" is not a version"));
}
