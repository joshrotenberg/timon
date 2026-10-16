//! `git`: the `git` program, with its output as data.
//!
//! Every call runs `git` the way `proc.run` runs a program (through `proc::capture`): in its
//! own process group, cancellable, never prompting (`GIT_TERMINAL_PROMPT=0`), and refused while
//! tasks load. A nonzero exit raises `git_failed`.

use mlua::{Lua, Table, Value};
use serde_json::{Value as Json, json};

use crate::docs::{Func, Module, Opt};
use crate::error::Error;
use crate::modules::Installer;
use crate::modules::json::decode_result as to_lua;
use crate::options;

const CWD: Opt = Opt::new(
    "cwd",
    "string",
    "the repository directory (default: the working directory)",
);

pub const DOC: Module = Module {
    name: "git",
    summary: "The git program, with its output as data. Each call takes cwd; a failure raises git_failed with git's stderr.",
    functions: &[
        Func::new(
            "root",
            "git.root({ cwd }) -> path",
            "The repository's top directory.",
            "local root = git.root()",
        )
        .with_options(&[CWD]),
        Func::new(
            "branch",
            "git.branch({ cwd }) -> name | nil",
            "The current branch, or nil on a detached HEAD.",
            "local b = git.branch() or \"(detached)\"",
        )
        .with_options(&[CWD]),
        Func::new(
            "rev",
            "git.rev(ref, { cwd }) -> sha",
            "The full commit id a ref names: HEAD, a branch, a tag, HEAD~2.",
            "local head = git.rev(\"HEAD\")",
        )
        .with_options(&[CWD]),
        Func::new(
            "status",
            "git.status({ cwd }) -> { branch, upstream, ahead, behind, clean, files }",
            "The working tree's state. Each file is { path, index, worktree, kind, from }: index and worktree are what changed in each (modified, added, deleted, renamed, copied, type_changed, unmerged) or nil; kind is changed, renamed, copied, conflict or untracked; from is a rename's old path.",
            "local s = git.status()\nif not s.clean then\n  for _, f in ipairs(s.files) do print(f.kind, f.path) end\nend",
        )
        .with_options(&[CWD]),
        Func::new(
            "log",
            "git.log({ n, paths, since, range, cwd }) -> { { sha, short, author, email, date, subject, body } }",
            "Commits, newest first. date is ISO 8601.",
            "for _, c in ipairs(git.log{ n = 5, paths = { \"src\" } }) do print(c.short, c.subject) end",
        )
        .with_options(&[
            Opt::new("n", "integer", "most commits"),
            Opt::new("paths", "string[]", "only commits that touch these paths"),
            Opt::new("since", "string", "a date or a duration git understands: \"2 weeks ago\", \"2026-10-01\""),
            Opt::new("range", "string", "a revision range such as \"v0.1.0..HEAD\""),
            CWD,
        ]),
        Func::new(
            "diff",
            "git.diff({ base, staged, stat, paths, cwd }) -> text | { { path, added, removed } }",
            "The diff of the working tree (or the index with staged) against base (default: the index, or HEAD with staged). With stat, a list of files with the lines added and removed; a binary file has nil counts.",
            "local files = git.diff{ base = \"main\", stat = true }\nprint(#files .. \" files changed\")",
        )
        .with_options(&[
            Opt::new("base", "string", "a ref to diff against"),
            Opt::new("staged", "boolean", "the staged changes"),
            Opt::new("stat", "boolean", "a list of files and line counts instead of text"),
            Opt::new("paths", "string[]", "only these paths"),
            CWD,
        ]),
        Func::new(
            "tags",
            "git.tags({ pattern, cwd }) -> { name }",
            "Tag names, highest version first.",
            "local latest = git.tags{ pattern = \"v*\" }[1]",
        )
        .with_options(&[Opt::new("pattern", "string", "a glob such as \"v*\""), CWD]),
        Func::new(
            "tag",
            "git.tag(name, { message, ref, cwd })",
            "Creates a tag: annotated with message, lightweight without, at ref (default HEAD).",
            "git.tag(\"v0.2.0\", { message = \"timon 0.2.0\" })",
        )
        .with_options(&[
            Opt::new("message", "string", "the annotation; makes an annotated tag"),
            Opt::new("ref", "string", "what to tag (default HEAD)"),
            CWD,
        ]),
        Func::new(
            "add",
            "git.add(paths, { cwd })",
            "Stages paths: a string or a list.",
            "git.add{ \"Cargo.toml\", \"CHANGELOG.md\" }",
        )
        .with_options(&[CWD]),
        Func::new(
            "commit",
            "git.commit({ message, all, paths, allow_empty, cwd }) -> sha",
            "Commits and returns the new commit's id. all stages tracked changes first; paths commits only those.",
            "local sha = git.commit{ message = \"chore: release 0.2.0\", all = true }",
        )
        .with_options(&[
            Opt::required("message", "string", "the commit message"),
            Opt::new("all", "boolean", "stage every tracked change first"),
            Opt::new("paths", "string[]", "commit only these paths"),
            Opt::new("allow_empty", "boolean", "commit even with nothing to commit"),
            CWD,
        ]),
        Func::new(
            "push",
            "git.push({ remote, ref, tags, set_upstream, cwd })",
            "Pushes to remote (default origin). ref is a branch or tag (default: git's choice); tags also pushes annotated tags that point into what is pushed.",
            "git.push{ ref = \"main\", tags = true }",
        )
        .with_options(&[
            Opt::new("remote", "string", "the remote (default origin)"),
            Opt::new("ref", "string", "the branch or tag to push"),
            Opt::new("tags", "boolean", "push annotated tags that point into what is pushed (--follow-tags)"),
            Opt::new("set_upstream", "boolean", "make the remote branch the upstream"),
            CWD,
        ]),
        Func::new(
            "worktree_add",
            "git.worktree_add({ path, branch, base, cwd }) -> path",
            "Adds a worktree at path, on a new branch when branch is given, starting from base (default HEAD).",
            "local dir = git.worktree_add{ path = fs.tmpdir() .. \"/wt\", branch = \"try-it\" }",
        )
        .with_options(&[
            Opt::required("path", "string", "where the worktree goes"),
            Opt::new("branch", "string", "a new branch for it"),
            Opt::new("base", "string", "the commit to start from"),
            CWD,
        ]),
        Func::new(
            "worktrees",
            "git.worktrees({ cwd }) -> { { path, head, branch } }",
            "The repository's worktrees, the main one first. branch is nil when detached.",
            "for _, w in ipairs(git.worktrees()) do print(w.path, w.branch) end",
        )
        .with_options(&[CWD]),
        Func::new(
            "worktree_remove",
            "git.worktree_remove(path, { force, cwd })",
            "Removes a worktree; force removes one with changes.",
            "git.worktree_remove(dir, { force = true })",
        )
        .with_options(&[Opt::new("force", "boolean", "remove it even with changes"), CWD]),
        Func::new(
            "run",
            "git.run(args, { cwd, input }) -> stdout",
            "Runs git with these arguments and returns stdout without its last newline, for anything the other functions do not cover.",
            "local subject = git.run{ \"log\", \"-1\", \"--format=%s\" }",
        )
        .with_options(&[Opt::new("input", "string", "text for stdin"), CWD]),
    ],
};

fn string_opt(opts: &Option<Table>, key: &str) -> mlua::Result<Option<String>> {
    match opts {
        Some(o) => o.raw_get(key),
        None => Ok(None),
    }
}

fn bool_opt(opts: &Option<Table>, key: &str) -> mlua::Result<bool> {
    match opts {
        Some(o) => Ok(o.raw_get::<Option<bool>>(key)?.unwrap_or(false)),
        None => Ok(false),
    }
}

fn list_opt(opts: &Option<Table>, key: &str) -> mlua::Result<Vec<String>> {
    match opts {
        Some(o) => Ok(o.raw_get::<Option<Vec<String>>>(key)?.unwrap_or_default()),
        None => Ok(Vec::new()),
    }
}

/// Runs git and returns stdout, or raises `git_failed`.
async fn git(
    lua: &Lua,
    args: Vec<String>,
    cwd: Option<String>,
    input: Option<String>,
) -> Result<String, Error> {
    let mut argv = vec!["git".to_owned()];
    argv.extend(args);
    let display = shell_words::join(&argv);
    let env = vec![("GIT_TERMINAL_PROMPT".to_owned(), Some("0".to_owned()))];
    let c = crate::modules::proc::capture(lua, argv, cwd, env, input).await?;
    if c.code != 0 {
        let err = c.err.trim();
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(Error::new(
            "git_failed",
            format!("`{display}` exited {}: {first}", c.code),
        )
        .with_data(json!({ "code": c.code, "stderr": err, "stdout": c.out })));
    }
    Ok(c.out)
}

/// Runs git and returns its exit code and stdout, without raising for the code.
async fn git_code(
    lua: &Lua,
    args: Vec<String>,
    cwd: Option<String>,
) -> Result<(i64, String), Error> {
    let mut argv = vec!["git".to_owned()];
    argv.extend(args);
    let env = vec![("GIT_TERMINAL_PROMPT".to_owned(), Some("0".to_owned()))];
    let c = crate::modules::proc::capture(lua, argv, cwd, env, None).await?;
    Ok((c.code, c.out))
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn change(c: char) -> Json {
    match c {
        'M' => "modified".into(),
        'T' => "type_changed".into(),
        'A' => "added".into(),
        'D' => "deleted".into(),
        'R' => "renamed".into(),
        'C' => "copied".into(),
        'U' => "unmerged".into(),
        _ => Json::Null,
    }
}

/// Parses `git status --porcelain=v2 --branch -z`.
pub(crate) fn parse_status(out: &str) -> Json {
    let mut branch = Json::Null;
    let mut upstream = Json::Null;
    let (mut ahead, mut behind) = (0i64, 0i64);
    let mut files = Vec::new();
    let mut entries = out.split('\0').filter(|e| !e.is_empty());
    while let Some(e) = entries.next() {
        if let Some(header) = e.strip_prefix("# ") {
            if let Some(v) = header.strip_prefix("branch.head ") {
                branch = if v == "(detached)" {
                    Json::Null
                } else {
                    v.into()
                };
            } else if let Some(v) = header.strip_prefix("branch.upstream ") {
                upstream = v.into();
            } else if let Some(v) = header.strip_prefix("branch.ab ") {
                let mut parts = v.split_whitespace();
                ahead = parts
                    .next()
                    .and_then(|a| a.trim_start_matches('+').parse().ok())
                    .unwrap_or(0);
                behind = parts
                    .next()
                    .and_then(|b| b.trim_start_matches('-').parse().ok())
                    .unwrap_or(0);
            }
            continue;
        }
        let (kind, fields, path_at) = match e.as_bytes().first() {
            Some(b'1') => ("changed", 9, 8),
            Some(b'2') => ("renamed", 10, 9),
            Some(b'u') => ("conflict", 11, 10),
            Some(b'?') => {
                files.push(json!({ "path": &e[2..], "kind": "untracked" }));
                continue;
            }
            _ => continue,
        };
        let parts: Vec<&str> = e.splitn(fields, ' ').collect();
        let (Some(xy), Some(path)) = (parts.get(1), parts.get(path_at)) else {
            continue;
        };
        let mut xy = xy.chars();
        let (x, y) = (xy.next().unwrap_or('.'), xy.next().unwrap_or('.'));
        let mut file = json!({
            "path": path,
            "index": change(x),
            "worktree": change(y),
            "kind": kind,
        });
        if kind == "renamed" {
            if parts.get(8).is_some_and(|s| s.starts_with('C')) {
                file["kind"] = "copied".into();
            }
            file["from"] = entries.next().map_or(Json::Null, Json::from);
        }
        files.push(file);
    }
    json!({
        "branch": branch,
        "upstream": upstream,
        "ahead": ahead,
        "behind": behind,
        "clean": files.is_empty(),
        "files": files,
    })
}

const LOG_FORMAT: &str = "--format=%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%s%x1f%b%x1e";

pub(crate) fn parse_log(out: &str) -> Json {
    Json::Array(
        out.split('\x1e')
            .map(|r| r.trim_start_matches('\n'))
            .filter(|r| !r.is_empty())
            .filter_map(|r| {
                let f: Vec<&str> = r.split('\x1f').collect();
                (f.len() == 7).then(|| {
                    json!({
                        "sha": f[0], "short": f[1], "author": f[2], "email": f[3],
                        "date": f[4], "subject": f[5], "body": f[6].trim(),
                    })
                })
            })
            .collect(),
    )
}

pub(crate) fn parse_numstat(out: &str) -> Json {
    Json::Array(
        out.lines()
            .filter_map(|l| {
                let mut parts = l.splitn(3, '\t');
                let (a, r, p) = (parts.next()?, parts.next()?, parts.next()?);
                Some(json!({
                    "path": p,
                    "added": a.parse::<i64>().ok(),
                    "removed": r.parse::<i64>().ok(),
                }))
            })
            .collect(),
    )
}

pub(crate) fn parse_worktrees(out: &str) -> Json {
    let mut list = Vec::new();
    for block in out.split("\n\n").filter(|b| !b.trim().is_empty()) {
        let mut w = json!({ "path": null, "head": null, "branch": null });
        for line in block.lines() {
            if let Some(v) = line.strip_prefix("worktree ") {
                w["path"] = v.into();
            } else if let Some(v) = line.strip_prefix("HEAD ") {
                w["head"] = v.into();
            } else if let Some(v) = line.strip_prefix("branch ") {
                w["branch"] = v.trim_start_matches("refs/heads/").into();
            }
        }
        list.push(w);
    }
    Json::Array(list)
}

/// A path argument or a list of them.
fn paths_arg(v: Value) -> mlua::Result<Vec<String>> {
    match v {
        Value::String(s) => Ok(vec![s.to_string_lossy()]),
        Value::Table(t) => t.sequence_values::<String>().collect(),
        other => Err(Error::new(
            "bad_argument",
            format!(
                "git.add takes a path or a list of paths, not a {}",
                other.type_name()
            ),
        )
        .into()),
    }
}

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let module = installer.module(&DOC)?;
    let add = |name: &str, f: mlua::Function| installer.function(&DOC, Some(&module), name, f);

    add(
        "root",
        lua.create_async_function(|lua, opts: Option<Table>| async move {
            options::check_opt("git.root", opts.as_ref())?;
            let out = git(
                &lua,
                words(&["rev-parse", "--show-toplevel"]),
                string_opt(&opts, "cwd")?,
                None,
            )
            .await?;
            Ok(out.trim_end().to_owned())
        })?,
    )?;
    add(
        "branch",
        lua.create_async_function(|lua, opts: Option<Table>| async move {
            options::check_opt("git.branch", opts.as_ref())?;
            let (code, out) = git_code(
                &lua,
                words(&["symbolic-ref", "--short", "-q", "HEAD"]),
                string_opt(&opts, "cwd")?,
            )
            .await?;
            Ok((code == 0).then(|| out.trim_end().to_owned()))
        })?,
    )?;
    add(
        "rev",
        lua.create_async_function(|lua, (rev, opts): (String, Option<Table>)| async move {
            options::check_opt("git.rev", opts.as_ref())?;
            let out = git(
                &lua,
                vec![
                    "rev-parse".into(),
                    "--verify".into(),
                    format!("{rev}^{{commit}}"),
                ],
                string_opt(&opts, "cwd")?,
                None,
            )
            .await?;
            Ok(out.trim_end().to_owned())
        })?,
    )?;
    add(
        "status",
        lua.create_async_function(|lua, opts: Option<Table>| async move {
            options::check_opt("git.status", opts.as_ref())?;
            let out = git(
                &lua,
                words(&["status", "--porcelain=v2", "--branch", "-z"]),
                string_opt(&opts, "cwd")?,
                None,
            )
            .await?;
            to_lua(&lua, &parse_status(&out))
        })?,
    )?;
    add(
        "log",
        lua.create_async_function(|lua, opts: Option<Table>| async move {
            options::check_opt("git.log", opts.as_ref())?;
            let mut args = words(&["log", LOG_FORMAT]);
            if let Some(o) = &opts
                && let Some(n) = o.raw_get::<Option<i64>>("n")?
            {
                args.push(format!("-n{n}"));
            }
            if let Some(since) = string_opt(&opts, "since")? {
                args.push(format!("--since={since}"));
            }
            if let Some(range) = string_opt(&opts, "range")? {
                args.push(range);
            }
            let paths = list_opt(&opts, "paths")?;
            if !paths.is_empty() {
                args.push("--".into());
                args.extend(paths);
            }
            let out = git(&lua, args, string_opt(&opts, "cwd")?, None).await?;
            to_lua(&lua, &parse_log(&out))
        })?,
    )?;
    add(
        "diff",
        lua.create_async_function(|lua, opts: Option<Table>| async move {
            options::check_opt("git.diff", opts.as_ref())?;
            let stat = bool_opt(&opts, "stat")?;
            let mut args = words(&["diff", "--no-color"]);
            if stat {
                args.push("--numstat".into());
            }
            if bool_opt(&opts, "staged")? {
                args.push("--cached".into());
            }
            if let Some(base) = string_opt(&opts, "base")? {
                args.push(base);
            }
            let paths = list_opt(&opts, "paths")?;
            if !paths.is_empty() {
                args.push("--".into());
                args.extend(paths);
            }
            let out = git(&lua, args, string_opt(&opts, "cwd")?, None).await?;
            if stat {
                to_lua(&lua, &parse_numstat(&out))
            } else {
                Ok(Value::String(lua.create_string(&out)?))
            }
        })?,
    )?;
    add(
        "tags",
        lua.create_async_function(|lua, opts: Option<Table>| async move {
            options::check_opt("git.tags", opts.as_ref())?;
            let mut args = words(&["tag", "--list", "--sort=-v:refname"]);
            if let Some(p) = string_opt(&opts, "pattern")? {
                args.push(p);
            }
            let out = git(&lua, args, string_opt(&opts, "cwd")?, None).await?;
            let tags: Vec<Json> = out
                .lines()
                .filter(|l| !l.is_empty())
                .map(Json::from)
                .collect();
            to_lua(&lua, &Json::Array(tags))
        })?,
    )?;
    add(
        "tag",
        lua.create_async_function(|lua, (name, opts): (String, Option<Table>)| async move {
            options::check_opt("git.tag", opts.as_ref())?;
            let mut args = words(&["tag"]);
            if let Some(m) = string_opt(&opts, "message")? {
                args.extend(["-a".into(), "-m".into(), m]);
            }
            args.push(name);
            if let Some(r) = string_opt(&opts, "ref")? {
                args.push(r);
            }
            git(&lua, args, string_opt(&opts, "cwd")?, None).await?;
            Ok(())
        })?,
    )?;
    add(
        "add",
        lua.create_async_function(|lua, (paths, opts): (Value, Option<Table>)| async move {
            options::check_opt("git.add", opts.as_ref())?;
            let mut args = words(&["add", "--"]);
            args.extend(paths_arg(paths)?);
            git(&lua, args, string_opt(&opts, "cwd")?, None).await?;
            Ok(())
        })?,
    )?;
    add(
        "commit",
        lua.create_async_function(|lua, opts: Table| async move {
            options::check("git.commit", &opts)?;
            let opts = Some(opts);
            let message = string_opt(&opts, "message")?.unwrap_or_default();
            let cwd = string_opt(&opts, "cwd")?;
            let mut args = vec!["commit".into(), "-m".into(), message];
            if bool_opt(&opts, "all")? {
                args.push("--all".into());
            }
            if bool_opt(&opts, "allow_empty")? {
                args.push("--allow-empty".into());
            }
            let paths = list_opt(&opts, "paths")?;
            if !paths.is_empty() {
                args.push("--".into());
                args.extend(paths);
            }
            git(&lua, args, cwd.clone(), None).await?;
            let sha = git(&lua, words(&["rev-parse", "HEAD"]), cwd, None).await?;
            Ok(sha.trim_end().to_owned())
        })?,
    )?;
    add(
        "push",
        lua.create_async_function(|lua, opts: Option<Table>| async move {
            options::check_opt("git.push", opts.as_ref())?;
            let mut args = words(&["push"]);
            if bool_opt(&opts, "set_upstream")? {
                args.push("--set-upstream".into());
            }
            if bool_opt(&opts, "tags")? {
                args.push("--follow-tags".into());
            }
            args.push(string_opt(&opts, "remote")?.unwrap_or_else(|| "origin".into()));
            if let Some(r) = string_opt(&opts, "ref")? {
                args.push(r);
            }
            git(&lua, args, string_opt(&opts, "cwd")?, None).await?;
            Ok(())
        })?,
    )?;
    add(
        "worktree_add",
        lua.create_async_function(|lua, opts: Table| async move {
            options::check("git.worktree_add", &opts)?;
            let opts = Some(opts);
            let path = string_opt(&opts, "path")?.unwrap_or_default();
            let mut args = words(&["worktree", "add"]);
            if let Some(b) = string_opt(&opts, "branch")? {
                args.extend(["-b".into(), b]);
            }
            args.push(path.clone());
            if let Some(base) = string_opt(&opts, "base")? {
                args.push(base);
            }
            git(&lua, args, string_opt(&opts, "cwd")?, None).await?;
            Ok(path)
        })?,
    )?;
    add(
        "worktrees",
        lua.create_async_function(|lua, opts: Option<Table>| async move {
            options::check_opt("git.worktrees", opts.as_ref())?;
            let out = git(
                &lua,
                words(&["worktree", "list", "--porcelain"]),
                string_opt(&opts, "cwd")?,
                None,
            )
            .await?;
            to_lua(&lua, &parse_worktrees(&out))
        })?,
    )?;
    add(
        "worktree_remove",
        lua.create_async_function(|lua, (path, opts): (String, Option<Table>)| async move {
            options::check_opt("git.worktree_remove", opts.as_ref())?;
            let mut args = words(&["worktree", "remove"]);
            if bool_opt(&opts, "force")? {
                args.push("--force".into());
            }
            args.push(path);
            git(&lua, args, string_opt(&opts, "cwd")?, None).await?;
            Ok(())
        })?,
    )?;
    add(
        "run",
        lua.create_async_function(
            |lua, (args, opts): (Vec<String>, Option<Table>)| async move {
                options::check_opt("git.run", opts.as_ref())?;
                let out = git(
                    &lua,
                    args,
                    string_opt(&opts, "cwd")?,
                    string_opt(&opts, "input")?,
                )
                .await?;
                Ok(out.strip_suffix('\n').unwrap_or(&out).to_owned())
            },
        )?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reads_every_entry_kind() {
        let out = "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
                   1 .M N... 100644 100644 100644 a b src/my file.rs\0\
                   1 A. N... 000000 100644 100644 0 b new.rs\0\
                   2 R. N... 100644 100644 100644 a b R100 renamed.rs\0old.rs\0\
                   u UU N... 100644 100644 100644 100644 a b c both.rs\0\
                   ? notes.txt\0";
        let s = parse_status(out);
        assert_eq!(s["branch"], "main");
        assert_eq!(s["upstream"], "origin/main");
        assert_eq!(
            (s["ahead"].as_i64(), s["behind"].as_i64()),
            (Some(2), Some(1))
        );
        assert_eq!(s["clean"], false);
        let f = s["files"].as_array().unwrap();
        assert_eq!(
            f[0],
            json!({ "path": "src/my file.rs", "index": null, "worktree": "modified", "kind": "changed" })
        );
        assert_eq!(f[1]["index"], "added");
        assert_eq!(
            (&f[2]["kind"], &f[2]["from"], &f[2]["path"]),
            (&json!("renamed"), &json!("old.rs"), &json!("renamed.rs"))
        );
        assert_eq!(
            (&f[3]["kind"], &f[3]["path"]),
            (&json!("conflict"), &json!("both.rs"))
        );
        assert_eq!(f[4], json!({ "path": "notes.txt", "kind": "untracked" }));
        assert_eq!(
            parse_status("# branch.head (detached)\0")["branch"],
            Json::Null
        );
    }

    #[test]
    fn log_numstat_and_worktrees_parse() {
        let log = parse_log(
            "a1\x1fa\x1fAnn\x1fann@x\x1f2026-10-16T10:00:00+00:00\x1ffeat: one\x1fbody line\n\x1e\nb2\x1fb\x1fBob\x1fbob@x\x1f2026-10-15T10:00:00+00:00\x1ffix: two\x1f\x1e\n",
        );
        assert_eq!(log[0]["subject"], "feat: one");
        assert_eq!(log[0]["body"], "body line");
        assert_eq!(log[1]["short"], "b");
        let stat = parse_numstat("3\t1\tsrc/a.rs\n-\t-\timg.png\n");
        assert_eq!(
            stat[0],
            json!({ "path": "src/a.rs", "added": 3, "removed": 1 })
        );
        assert_eq!(stat[1]["added"], Json::Null);
        let wt = parse_worktrees(
            "worktree /r\nHEAD abc\nbranch refs/heads/main\n\nworktree /r/wt\nHEAD def\ndetached\n\n",
        );
        assert_eq!(
            wt[0],
            json!({ "path": "/r", "head": "abc", "branch": "main" })
        );
        assert_eq!(wt[1]["branch"], Json::Null);
    }
}
