//! `gh`: GitHub's REST and GraphQL APIs, through `http`.
//!
//! The token comes from `GH_TOKEN` or `GITHUB_TOKEN`, then `gh auth token` when `gh` is
//! installed. `{owner}` and `{repo}` in a path come from the `repo` option or from `origin`'s
//! URL. `TIMON_GITHUB_API` replaces `https://api.github.com`, for GitHub Enterprise.

use std::rc::Rc;

use mlua::{Lua, Table, Value};
use serde_json::{Value as Json, json};

use crate::docs::{Func, Module, Opt};
use crate::error::Error;
use crate::modules::Installer;
use crate::modules::http::{Request, send};
use crate::modules::json::decode_result as to_lua;
use crate::options;

const REPO: Opt = Opt::new(
    "repo",
    "string",
    "\"owner/name\" (default: from origin's URL)",
);

pub const DOC: Module = Module {
    name: "gh",
    summary: "GitHub's APIs as data. Authenticates with GH_TOKEN or GITHUB_TOKEN, then gh auth token. {owner} and {repo} in a path come from the repo option or origin. Errors: not_authenticated, not_found, github_failed.",
    functions: &[
        Func::new(
            "repo",
            "gh.repo({ repo }) -> repository",
            "The repository: name, full_name, default_branch, private, html_url and the rest of GitHub's fields.",
            "local r = gh.repo()\nprint(r.full_name, r.default_branch)",
        )
        .with_options(&[REPO]),
        Func::new(
            "prs",
            "gh.prs({ state, base, limit, repo }) -> { pr }",
            "Pull requests, newest first: { number, title, state, draft, author, head, base, url, labels, created_at, updated_at }.",
            "for _, pr in ipairs(gh.prs{ state = \"open\", limit = 10 }) do print(pr.number, pr.title) end",
        )
        .with_options(&[
            Opt::new("state", "\"open\" | \"closed\" | \"all\"", "default open"),
            Opt::new("base", "string", "only pull requests into this branch"),
            Opt::new("limit", "integer", "most pull requests (default 30)"),
            REPO,
        ]),
        Func::new(
            "pr",
            "gh.pr(number, { repo }) -> pr",
            "One pull request with its body, merged and mergeable, files { path, status, additions, deletions }, reviews { author, state, body } and checks { name, status, conclusion }.",
            "local pr = gh.pr(12)\nfor _, c in ipairs(pr.checks) do print(c.name, c.conclusion) end",
        )
        .with_options(&[REPO]),
        Func::new(
            "issues",
            "gh.issues({ state, labels, limit, repo }) -> { issue }",
            "Issues, not pull requests, newest first: { number, title, state, author, labels, url, comments, created_at }.",
            "local bugs = gh.issues{ labels = { \"bug\" } }",
        )
        .with_options(&[
            Opt::new("state", "\"open\" | \"closed\" | \"all\"", "default open"),
            Opt::new("labels", "string[]", "only issues with every one of these labels"),
            Opt::new("limit", "integer", "most issues (default 30)"),
            REPO,
        ]),
        Func::new(
            "checks",
            "gh.checks(ref, { repo }) -> { { name, status, conclusion, url } }",
            "The check runs of a commit. ref is a sha, a branch or tag on GitHub, or HEAD for the local HEAD.",
            "local failing = tbl.filter(gh.checks(\"HEAD\"), function(c) return c.conclusion == \"failure\" end)",
        )
        .with_options(&[REPO]),
        Func::new(
            "api",
            "gh.api(path, { method, json, query, paginate, limit, repo }) -> value",
            "Any REST call. path is relative to the API (\"repos/{owner}/{repo}/releases\"). paginate follows the next links and joins the lists, up to limit items.",
            "local release = gh.api(\"repos/{owner}/{repo}/releases\", {\n  method = \"POST\",\n  json = { tag_name = \"v0.2.0\", name = \"0.2.0\", generate_release_notes = true },\n})",
        )
        .with_options(&[
            Opt::new("method", "string", "default GET"),
            Opt::new("json", "any", "the request body"),
            Opt::new("query", "table<string, string | number | boolean>", "query parameters"),
            Opt::new("paginate", "boolean", "follow next links and join the lists"),
            Opt::new("limit", "integer", "with paginate, most items"),
            REPO,
        ]),
        Func::new(
            "graphql",
            "gh.graphql(query, variables) -> data",
            "A GraphQL query; returns its data, or raises github_failed with GraphQL's errors.",
            "local d = gh.graphql(\"query($o: String!, $n: String!) { repository(owner: $o, name: $n) { stargazerCount } }\", { o = \"joshrotenberg\", n = \"timon\" })",
        ),
    ],
};

fn api_base() -> String {
    std::env::var("TIMON_GITHUB_API")
        .unwrap_or_else(|_| "https://api.github.com".into())
        .trim_end_matches('/')
        .to_owned()
}

/// The token, cached once found.
struct Token(String);

async fn token(lua: &Lua) -> Result<String, Error> {
    if let Some(t) = lua.app_data_ref::<Rc<Token>>() {
        return Ok(t.0.clone());
    }
    let mut found = ["GH_TOKEN", "GITHUB_TOKEN"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()));
    if found.is_none() {
        let argv = vec!["gh".to_owned(), "auth".into(), "token".into()];
        if let Ok(c) = crate::modules::proc::capture(lua, argv, None, Vec::new(), None).await
            && c.code == 0
            && !c.out.trim().is_empty()
        {
            found = Some(c.out.trim().to_owned());
        }
    }
    let Some(token) = found else {
        return Err(
            Error::new("not_authenticated", "no GitHub token").with_hint(
                "set GH_TOKEN or GITHUB_TOKEN, or log in with gh auth login; inside a sandbox gh \
                 auth token may not reach its credentials, and GH_TOKEN works there",
            ),
        );
    };
    lua.set_app_data(Rc::new(Token(token.clone())));
    Ok(token)
}

/// owner and name from a GitHub remote URL.
pub(crate) fn parse_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let rest = url
        .strip_prefix("git@")
        .and_then(|r| r.split_once(':').map(|(_, p)| p))
        .or_else(|| {
            let without_scheme = url.split_once("://").map(|(_, r)| r)?;
            without_scheme.split_once('/').map(|(_, p)| p)
        })?;
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (parts.next().is_none() && !owner.is_empty() && !name.is_empty())
        .then(|| (owner.to_owned(), name.to_owned()))
}

async fn repo(lua: &Lua, opts: Option<&Table>) -> Result<(String, String), Error> {
    if let Some(o) = opts
        && let Some(r) = o
            .raw_get::<Option<String>>("repo")
            .map_err(|e| Error::from_lua(&e))?
    {
        return r
            .split_once('/')
            .map(|(o, n)| (o.to_owned(), n.to_owned()))
            .ok_or_else(|| {
                Error::new(
                    "bad_option",
                    format!("repo must be owner/name, got \"{r}\""),
                )
            });
    }
    let argv = ["git", "remote", "get-url", "origin"]
        .map(str::to_owned)
        .to_vec();
    let c = crate::modules::proc::capture(lua, argv, None, Vec::new(), None).await?;
    if c.code != 0 {
        return Err(
            Error::new("not_found", "no origin remote to take the repository from")
                .with_hint("pass repo = \"owner/name\""),
        );
    }
    parse_remote(&c.out).ok_or_else(|| {
        Error::new(
            "not_found",
            format!("origin ({}) is not a GitHub repository URL", c.out.trim()),
        )
        .with_hint("pass repo = \"owner/name\"")
    })
}

/// Fills `{owner}` and `{repo}` in a path, looking the repository up only when needed.
async fn fill(lua: &Lua, path: &str, opts: Option<&Table>) -> Result<String, Error> {
    if !path.contains("{owner}") && !path.contains("{repo}") {
        return Ok(path.to_owned());
    }
    let (owner, name) = repo(lua, opts).await?;
    Ok(path.replace("{owner}", &owner).replace("{repo}", &name))
}

/// The next page's URL from a Link header.
fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        rel.contains("rel=\"next\"").then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_owned()
        })
    })
}

/// A REST call: one response's JSON, or every page's items joined when `paginate`.
async fn rest(
    lua: &Lua,
    method: &str,
    path: &str,
    body: Option<Json>,
    query: Vec<(String, String)>,
    paginate: Option<usize>,
) -> Result<Json, Error> {
    let token = token(lua).await?;
    let mut url = if path.starts_with("http://") || path.starts_with("https://") {
        path.to_owned()
    } else {
        format!("{}/{}", api_base(), path.trim_start_matches('/'))
    };
    let mut query = query;
    query.sort();
    let mut items: Vec<Json> = Vec::new();
    loop {
        let mut req = Request {
            method: method.to_owned(),
            url: url.clone(),
            headers: vec![
                ("accept".into(), "application/vnd.github+json".into()),
                ("x-github-api-version".into(), "2022-11-28".into()),
                ("authorization".into(), format!("Bearer {token}")),
            ],
            query: std::mem::take(&mut query),
            body: None,
            timeout: std::time::Duration::from_secs(30),
        };
        if let Some(b) = &body {
            req.body =
                Some(serde_json::to_vec(b).map_err(|e| Error::new("internal", e.to_string()))?);
            req.headers
                .push(("content-type".into(), "application/json".into()));
        }
        let r = send(lua, req).await?;
        let value: Json = if r.body.trim().is_empty() {
            Json::Null
        } else {
            serde_json::from_str(&r.body).unwrap_or(Json::String(r.body.clone()))
        };
        if !(200..300).contains(&r.status) {
            let message = value["message"]
                .as_str()
                .unwrap_or(r.body.trim())
                .to_owned();
            let kind = match r.status {
                401 => "not_authenticated",
                404 => "not_found",
                _ => "github_failed",
            };
            let err = Error::new(kind, format!("{method} {path}: {} {message}", r.status))
                .with_data(
                    json!({ "status": r.status, "documentation_url": value["documentation_url"] }),
                );
            return Err(match r.status {
                401 => {
                    err.with_hint("check GH_TOKEN or GITHUB_TOKEN, or log in with gh auth login")
                }
                403 | 404 => {
                    err.with_hint("check the path and that the token can see the repository")
                }
                _ => err,
            });
        }
        let Some(limit) = paginate else {
            return Ok(value);
        };
        match value {
            Json::Array(a) => items.extend(a),
            Json::Object(mut o) => match o.remove("items") {
                Some(Json::Array(a)) => items.extend(a),
                _ => return Ok(Json::Object(o)),
            },
            other => return Ok(other),
        }
        if items.len() >= limit {
            items.truncate(limit);
            break;
        }
        match r.header("link").and_then(next_link) {
            Some(next) => url = next,
            None => break,
        }
    }
    Ok(Json::Array(items))
}

fn login(v: &Json) -> Json {
    v["login"].clone()
}

fn labels(v: &Json) -> Json {
    Json::Array(
        v["labels"]
            .as_array()
            .map(|a| a.iter().map(|l| l["name"].clone()).collect())
            .unwrap_or_default(),
    )
}

fn pr_summary(p: &Json) -> Json {
    json!({
        "number": p["number"], "title": p["title"], "state": p["state"], "draft": p["draft"],
        "author": login(&p["user"]), "head": p["head"]["ref"], "base": p["base"]["ref"],
        "url": p["html_url"], "labels": labels(p),
        "created_at": p["created_at"], "updated_at": p["updated_at"],
    })
}

fn limit(opts: Option<&Table>) -> mlua::Result<usize> {
    Ok(match opts {
        Some(o) => o.raw_get::<Option<i64>>("limit")?.unwrap_or(30).max(1) as usize,
        None => 30,
    })
}

fn get_str(opts: Option<&Table>, key: &str) -> mlua::Result<Option<String>> {
    match opts {
        Some(o) => o.raw_get(key),
        None => Ok(None),
    }
}

fn checks_summary(v: &Json) -> Json {
    Json::Array(
        v["check_runs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| json!({ "name": c["name"], "status": c["status"], "conclusion": c["conclusion"], "url": c["html_url"] }))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// A ref the API understands: HEAD becomes the local HEAD's sha.
async fn api_ref(lua: &Lua, r: &str) -> Result<String, Error> {
    if r != "HEAD" {
        return Ok(r.to_owned());
    }
    let argv = ["git", "rev-parse", "HEAD"].map(str::to_owned).to_vec();
    let c = crate::modules::proc::capture(lua, argv, None, Vec::new(), None).await?;
    if c.code != 0 {
        return Err(Error::new(
            "git_failed",
            format!("cannot resolve HEAD: {}", c.err.trim()),
        ));
    }
    Ok(c.out.trim().to_owned())
}

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let module = installer.module(&DOC)?;
    let add = |name: &str, f: mlua::Function| installer.function(&DOC, Some(&module), name, f);

    add(
        "repo",
        lua.create_async_function(|lua, opts: Option<Table>| async move {
            options::check_opt("gh.repo", opts.as_ref())?;
            let path = fill(&lua, "repos/{owner}/{repo}", opts.as_ref()).await?;
            to_lua(
                &lua,
                &rest(&lua, "GET", &path, None, Vec::new(), None).await?,
            )
        })?,
    )?;
    add(
        "prs",
        lua.create_async_function(|lua, opts: Option<Table>| async move {
            options::check_opt("gh.prs", opts.as_ref())?;
            let o = opts.as_ref();
            let path = fill(&lua, "repos/{owner}/{repo}/pulls", o).await?;
            let mut query = vec![
                (
                    "state".into(),
                    get_str(o, "state")?.unwrap_or_else(|| "open".into()),
                ),
                ("per_page".into(), "100".into()),
            ];
            if let Some(base) = get_str(o, "base")? {
                query.push(("base".into(), base));
            }
            let list = rest(&lua, "GET", &path, None, query, Some(limit(o)?)).await?;
            let prs: Vec<Json> = list
                .as_array()
                .map(|a| a.iter().map(pr_summary).collect())
                .unwrap_or_default();
            to_lua(&lua, &Json::Array(prs))
        })?,
    )?;
    add(
        "pr",
        lua.create_async_function(|lua, (number, opts): (i64, Option<Table>)| async move {
            options::check_opt("gh.pr", opts.as_ref())?;
            let base = fill(
                &lua,
                &format!("repos/{{owner}}/{{repo}}/pulls/{number}"),
                opts.as_ref(),
            )
            .await?;
            let p = rest(&lua, "GET", &base, None, Vec::new(), None).await?;
            let files = rest(
                &lua,
                "GET",
                &format!("{base}/files"),
                None,
                vec![("per_page".into(), "100".into())],
                Some(3000),
            )
            .await?;
            let reviews = rest(
                &lua,
                "GET",
                &format!("{base}/reviews"),
                None,
                vec![("per_page".into(), "100".into())],
                Some(1000),
            )
            .await?;
            let sha = p["head"]["sha"].as_str().unwrap_or_default().to_owned();
            let checks_path = base.split("/pulls/").next().unwrap_or_default().to_owned();
            let checks = rest(
                &lua,
                "GET",
                &format!("{checks_path}/commits/{sha}/check-runs"),
                None,
                Vec::new(),
                None,
            )
            .await?;
            let mut out = pr_summary(&p);
            out["body"] = p["body"].clone();
            out["merged"] = p["merged"].clone();
            out["mergeable"] = p["mergeable"].clone();
            out["head_sha"] = Json::String(sha);
            out["files"] = Json::Array(
                files
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|f| {
                                json!({
                                    "path": f["filename"], "status": f["status"],
                                    "additions": f["additions"], "deletions": f["deletions"],
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            );
            out["reviews"] = Json::Array(
                reviews
                    .as_array()
                    .map(|a| {
                        a.iter().map(|r| json!({
                    "author": login(&r["user"]), "state": r["state"], "body": r["body"],
                })).collect()
                    })
                    .unwrap_or_default(),
            );
            out["checks"] = checks_summary(&checks);
            to_lua(&lua, &out)
        })?,
    )?;
    add(
        "issues",
        lua.create_async_function(|lua, opts: Option<Table>| async move {
            options::check_opt("gh.issues", opts.as_ref())?;
            let o = opts.as_ref();
            let path = fill(&lua, "repos/{owner}/{repo}/issues", o).await?;
            let mut query = vec![
                (
                    "state".into(),
                    get_str(o, "state")?.unwrap_or_else(|| "open".into()),
                ),
                ("per_page".into(), "100".into()),
            ];
            if let Some(o) = o
                && let Some(l) = o.raw_get::<Option<Vec<String>>>("labels")?
                && !l.is_empty()
            {
                query.push(("labels".into(), l.join(",")));
            }
            let limit = limit(o)?;
            // Pull requests come back as issues too; ask for extra and drop them.
            let list = rest(&lua, "GET", &path, None, query, Some(limit * 2)).await?;
            let issues: Vec<Json> = list
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|i| i.get("pull_request").is_none())
                        .take(limit)
                        .map(|i| json!({
                            "number": i["number"], "title": i["title"], "state": i["state"],
                            "author": login(&i["user"]), "labels": labels(i), "url": i["html_url"],
                            "comments": i["comments"], "created_at": i["created_at"],
                        }))
                        .collect()
                })
                .unwrap_or_default();
            to_lua(&lua, &Json::Array(issues))
        })?,
    )?;
    add(
        "checks",
        lua.create_async_function(|lua, (r, opts): (String, Option<Table>)| async move {
            options::check_opt("gh.checks", opts.as_ref())?;
            let sha = api_ref(&lua, &r).await?;
            let path = fill(
                &lua,
                &format!("repos/{{owner}}/{{repo}}/commits/{sha}/check-runs"),
                opts.as_ref(),
            )
            .await?;
            let v = rest(
                &lua,
                "GET",
                &path,
                None,
                vec![("per_page".into(), "100".into())],
                None,
            )
            .await?;
            to_lua(&lua, &checks_summary(&v))
        })?,
    )?;
    add(
        "api",
        lua.create_async_function(|lua, (path, opts): (String, Option<Table>)| async move {
            options::check_opt("gh.api", opts.as_ref())?;
            let o = opts.as_ref();
            let path = fill(&lua, &path, o).await?;
            let method = get_str(o, "method")?
                .unwrap_or_else(|| "GET".into())
                .to_uppercase();
            let body = match o.map(|o| o.raw_get::<Value>("json")).transpose()? {
                None | Some(Value::Nil) => None,
                Some(v) => Some(crate::value::to_json(&v, crate::value::Mode::Strict)?),
            };
            let mut query = Vec::new();
            if let Some(o) = o
                && let Some(q) = o.raw_get::<Option<Table>>("query")?
            {
                for pair in q.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    let v = match v {
                        Value::String(s) => s.to_string_lossy(),
                        Value::Integer(i) => i.to_string(),
                        Value::Number(n) => n.to_string(),
                        Value::Boolean(b) => b.to_string(),
                        other => {
                            return Err(Error::new(
                                "bad_option",
                                format!("query parameter \"{k}\" is a {}", other.type_name()),
                            )
                            .into());
                        }
                    };
                    query.push((k, v));
                }
                query.sort();
            }
            let paginate = match o {
                Some(o) if o.raw_get::<Option<bool>>("paginate")?.unwrap_or(false) => Some(
                    o.raw_get::<Option<i64>>("limit")?
                        .map_or(usize::MAX, |n| n.max(1) as usize),
                ),
                _ => None,
            };
            let value = rest(&lua, &method, &path, body, query, paginate).await?;
            to_lua(&lua, &value)
        })?,
    )?;
    add(
        "graphql",
        lua.create_async_function(|lua, (query, vars): (String, Option<Table>)| async move {
            let variables = match vars {
                Some(t) => crate::value::to_json(&Value::Table(t), crate::value::Mode::Strict)?,
                None => json!({}),
            };
            let base = api_base();
            let url = match base.strip_suffix("/api/v3") {
                Some(host) => format!("{host}/api/graphql"),
                None => format!("{base}/graphql"),
            };
            let v = rest(
                &lua,
                "POST",
                &url,
                Some(json!({ "query": query, "variables": variables })),
                Vec::new(),
                None,
            )
            .await?;
            if let Some(errors) = v["errors"].as_array().filter(|e| !e.is_empty()) {
                let messages: Vec<&str> = errors
                    .iter()
                    .filter_map(|e| e["message"].as_str())
                    .collect();
                return Err(Error::new(
                    "github_failed",
                    format!("GraphQL: {}", messages.join("; ")),
                )
                .with_data(json!({ "errors": errors }))
                .into());
            }
            to_lua(&lua, &v["data"])
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_urls_name_the_repository() {
        for url in [
            "git@github.com:joshrotenberg/timon.git",
            "https://github.com/joshrotenberg/timon",
            "https://github.com/joshrotenberg/timon.git\n",
            "ssh://git@github.com/joshrotenberg/timon.git",
            "https://token@github.com/joshrotenberg/timon/",
        ] {
            assert_eq!(
                parse_remote(url),
                Some(("joshrotenberg".into(), "timon".into())),
                "{url}"
            );
        }
        assert_eq!(parse_remote("/srv/git/repo.git"), None);
    }

    #[test]
    fn the_next_link_is_found() {
        let link = "<https://api.github.com/repos/o/r/pulls?page=2>; rel=\"next\", <https://api.github.com/repos/o/r/pulls?page=5>; rel=\"last\"";
        assert_eq!(
            next_link(link).as_deref(),
            Some("https://api.github.com/repos/o/r/pulls?page=2")
        );
        assert_eq!(next_link("<x>; rel=\"prev\""), None);
    }
}
