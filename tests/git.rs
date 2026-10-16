//! The git module (#88), against temporary repositories and a local bare remote.

use std::path::Path;
use std::process::{Command, Output};

fn isolated(cmd: &mut Command, home: &Path) {
    cmd.env("HOME", home)
        .env("TIMON_HOME", home.join("timon"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com");
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

fn timon(home: &Path, dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_timon"));
    cmd.args(args).current_dir(dir);
    isolated(&mut cmd, home);
    cmd.output().unwrap()
}

fn eval(home: &Path, dir: &Path, code: &str) -> String {
    let o = timon(home, dir, &["eval", code]);
    assert!(
        o.status.success(),
        "{code}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn reads_and_writes_a_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(home, &repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    std::fs::write(repo.join("old.txt"), "moved\n").unwrap();
    git(home, &repo, &["add", "."]);
    git(
        home,
        &repo,
        &["commit", "-q", "-m", "feat: start", "-m", "the body"],
    );
    let head = git(home, &repo, &["rev-parse", "HEAD"]);

    assert_eq!(
        eval(
            home,
            &repo,
            "local s = git.status() return git.branch(), s.clean, #s.files, git.rev('HEAD')"
        ),
        format!("main\ntrue\n0\n{head}\n")
    );
    let root = eval(home, &repo, "return git.root()");
    assert_eq!(
        std::fs::canonicalize(root.trim()).unwrap(),
        std::fs::canonicalize(&repo).unwrap()
    );

    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(repo.join("new.txt"), "untracked\n").unwrap();
    std::fs::write(repo.join("staged.txt"), "staged\n").unwrap();
    git(home, &repo, &["add", "staged.txt"]);
    git(home, &repo, &["mv", "old.txt", "renamed.txt"]);
    let status = eval(
        home,
        &repo,
        "local out = {}
         for _, f in ipairs(git.status().files) do
           out[#out + 1] = table.concat({ f.kind, f.path, f.index or '-', f.worktree or '-', f.from or '-' }, ' ')
         end
         table.sort(out)
         return table.concat(out, '\\n')",
    );
    assert_eq!(
        status,
        "changed a.txt - modified -\n\
         changed staged.txt added - -\n\
         renamed renamed.txt renamed - old.txt\n\
         untracked new.txt - - -\n"
    );
    assert_eq!(
        eval(
            home,
            &repo,
            "local d = git.diff{ stat = true } return d[1].path, d[1].added, d[1].removed"
        ),
        "a.txt\n1\n0\n"
    );
    assert!(eval(home, &repo, "return git.diff()").contains("+two"));
    assert!(
        eval(
            home,
            &repo,
            "return git.diff{ staged = true, paths = { 'staged.txt' } }"
        )
        .contains("+staged")
    );

    let sha = eval(
        home,
        &repo,
        "git.add{ 'new.txt' } return git.commit{ message = 'feat: more', all = true }",
    );
    assert_eq!(sha.trim(), git(home, &repo, &["rev-parse", "HEAD"]));
    assert_eq!(
        eval(
            home,
            &repo,
            "local l = git.log{ n = 2 } return l[1].subject, l[2].subject, l[2].body, #l[1].sha"
        ),
        "feat: more\nfeat: start\nthe body\n40\n"
    );
    assert_eq!(
        eval(
            home,
            &repo,
            "return #git.log{ paths = { 'a.txt' }, range = 'HEAD~1..HEAD' }"
        ),
        "1\n"
    );
    assert_eq!(
        eval(home, &repo, "return git.run{ 'log', '-1', '--format=%s' }"),
        "feat: more\n"
    );

    eval(
        home,
        &repo,
        "git.tag('v0.1.0', { message = 'first', ref = 'HEAD~1' }) git.tag('v0.10.0') git.tag('v0.2.0')",
    );
    assert_eq!(
        eval(
            home,
            &repo,
            "return table.concat(git.tags{ pattern = 'v*' }, ' ')"
        ),
        "v0.10.0 v0.2.0 v0.1.0\n"
    );

    let remote = home.join("remote.git");
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
    eval(
        home,
        &repo,
        "git.push{ ref = 'main', tags = true, set_upstream = true }",
    );
    let refs = git(home, &remote, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(
        refs, "refs/heads/main\nrefs/tags/v0.1.0",
        "an annotated tag follows; lightweight ones do not"
    );
    assert_eq!(
        eval(
            home,
            &repo,
            "local s = git.status() return s.upstream, s.ahead, s.behind"
        ),
        "origin/main\n0\n0\n"
    );

    let wt = home.join("wt");
    let wt = wt.to_str().unwrap();
    let listed = eval(
        home,
        &repo,
        &format!(
            "git.worktree_add{{ path = '{wt}', branch = 'try' }}
             local w = git.worktrees()
             return #w, w[2].branch"
        ),
    );
    assert_eq!(listed, "2\ntry\n");
    assert_eq!(eval(home, Path::new(wt), "return git.branch()"), "try\n");
    eval(home, &repo, &format!("git.worktree_remove('{wt}')"));
    assert!(!Path::new(wt).exists());
}

#[test]
fn failures_raise_git_failed_with_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(home, &repo, &["init", "-q", "-b", "main"]);
    let o = timon(home, &repo, &["eval", "git.rev('nope')"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("git_failed: `git rev-parse --verify nope^{commit}` exited 128: fatal: Needed a single revision"),
        "{err}"
    );
    let o = timon(
        home,
        &repo,
        &[
            "eval",
            "local ok, e = try(git.status, { cwd = '/' }) return e.kind, e.data.code",
        ],
    );
    assert_eq!(String::from_utf8_lossy(&o.stdout), "git_failed\n128\n");
    let o = timon(home, &repo, &["eval", "git.commit{ messge = 'x' }"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("did you mean \"message\"?"));
    assert_eq!(eval(home, &repo, "return git.branch()"), "main\n");
    git(home, &repo, &["commit", "-q", "--allow-empty", "-m", "x"]);
    git(home, &repo, &["checkout", "-q", "--detach"]);
    assert_eq!(eval(home, &repo, "return git.branch()"), "nil\n");
}

#[test]
fn git_is_refused_while_tasks_load() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let project = home.join("p");
    std::fs::create_dir_all(project.join(".timon")).unwrap();
    std::fs::write(
        project.join(".timon/tasks.lua"),
        "local b = git.branch()\nreturn {}",
    )
    .unwrap();
    assert!(timon(home, &project, &["trust"]).status.success());
    let o = timon(home, &project, &["tasks"]);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("git cannot run while tasks load"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}
