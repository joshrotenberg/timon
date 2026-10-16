//! `http`: an HTTP client on the async runtime, so a request is cancellable and times out.
//!
//! A response is data: `{ status, ok, headers, body, url, json }`. A non-2xx status raises
//! only with `check = true`; a request that cannot complete raises `http_failed` or `timeout`.

use std::rc::Rc;
use std::time::Duration;

use mlua::{Lua, Table, Value};
use serde_json::Value as Json;

use crate::docs::{Func, Module, Opt};
use crate::error::Error;
use crate::modules::Installer;
use crate::options;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

const COMMON: &[Opt] = &[
    Opt::new("headers", "table<string, string>", "request headers"),
    Opt::new(
        "query",
        "table<string, string | number | boolean>",
        "query parameters, added to the URL",
    ),
    Opt::new("body", "string", "the request body"),
    Opt::new(
        "json",
        "any",
        "a value sent as a JSON body, with content-type application/json",
    ),
    Opt::new(
        "timeout",
        "number | string",
        "seconds, or a duration such as \"10s\" (default 30s)",
    ),
    Opt::new(
        "check",
        "boolean",
        "raise http_status for a status that is not 2xx",
    ),
];

pub const DOC: Module = Module {
    name: "http",
    summary: "HTTP requests. A response is { status, ok, headers, body, url, json }: headers have lowercase names, and json() decodes the body. A status that is not 2xx is data unless check = true.",
    functions: &[
        Func::new(
            "get",
            "http.get(url, opts) -> response",
            "A GET request.",
            "local r = http.get(\"https://api.github.com/zen\", { timeout = \"10s\" })\nif r.ok then print(r.body) end",
        )
        .with_options(COMMON),
        Func::new(
            "post",
            "http.post(url, opts) -> response",
            "A POST request, usually with json or body.",
            "local r = http.post(\"https://httpbin.org/post\", { json = { name = \"timon\" }, check = true })\nlocal echoed = r.json().json",
        )
        .with_options(COMMON),
        Func::new(
            "request",
            "http.request{ method, url, ... } -> response",
            "Any request: the method and URL with the same options as http.get.",
            "local r = http.request{ method = \"DELETE\", url = \"https://example.com/items/1\" }",
        )
        .with_options(&[
            Opt::required("method", "string", "GET, POST, PUT, PATCH, DELETE, HEAD or another method"),
            Opt::required("url", "string", "the URL"),
            Opt::new("headers", "table<string, string>", "request headers"),
            Opt::new("query", "table<string, string | number | boolean>", "query parameters, added to the URL"),
            Opt::new("body", "string", "the request body"),
            Opt::new("json", "any", "a value sent as a JSON body, with content-type application/json"),
            Opt::new("timeout", "number | string", "seconds, or a duration such as \"10s\" (default 30s)"),
            Opt::new("check", "boolean", "raise http_status for a status that is not 2xx"),
        ]),
    ],
};

/// A request, as the modules that use HTTP build it.
pub(crate) struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub query: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout: Duration,
}

/// A response: the status, headers with lowercase names, the body and the final URL.
pub(crate) struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub url: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn client(lua: &Lua) -> Result<Rc<reqwest::Client>, Error> {
    if let Some(c) = lua.app_data_ref::<Rc<reqwest::Client>>() {
        return Ok(c.clone());
    }
    // rustls on ring rather than aws-lc-rs: the same TLS 1.2 and 1.3, 1.4 MB less binary
    // (#96). The first install wins, so a provider installed earlier stays.
    static PROVIDER: std::sync::Once = std::sync::Once::new();
    PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    let client = reqwest::Client::builder()
        .user_agent(concat!("timon/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Error::new("http_failed", format!("cannot start the HTTP client: {e}")))?;
    let client = Rc::new(client);
    lua.set_app_data(client.clone());
    Ok(client)
}

/// Sends a request. Refused while tasks load; a failure to connect or read raises
/// `http_failed`, running out of time `timeout`.
pub(crate) async fn send(lua: &Lua, req: Request) -> Result<Response, Error> {
    crate::tasks::refuse_while_loading(lua, "an HTTP request")?;
    let method =
        reqwest::Method::from_bytes(req.method.to_uppercase().as_bytes()).map_err(|_| {
            Error::new(
                "bad_argument",
                format!("\"{}\" is not an HTTP method", req.method),
            )
        })?;
    let mut url = reqwest::Url::parse(&req.url)
        .map_err(|e| Error::new("bad_argument", format!("\"{}\" is not a URL: {e}", req.url)))?;
    if !req.query.is_empty() {
        url.query_pairs_mut().extend_pairs(&req.query);
    }
    let shown = url.to_string();
    let mut builder = client(lua)?
        .request(method.clone(), url)
        .timeout(req.timeout);
    for (k, v) in &req.headers {
        builder = builder.header(k, v);
    }
    if let Some(body) = req.body {
        builder = builder.body(body);
    }
    let failed = |e: reqwest::Error| {
        if e.is_timeout() {
            Error::new(
                "timeout",
                format!("{method} {shown} took longer than {:?}", req.timeout),
            )
            .with_hint("raise timeout")
        } else {
            let mut message = e.to_string();
            let mut source = std::error::Error::source(&e);
            while let Some(s) = source {
                message = format!("{message}: {s}");
                source = s.source();
            }
            Error::new("http_failed", format!("{method} {shown}: {message}"))
        }
    };
    let response = builder.send().await.map_err(failed)?;
    let status = response.status().as_u16();
    let final_url = response.url().to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    for (k, v) in response.headers() {
        let v = String::from_utf8_lossy(v.as_bytes()).into_owned();
        match headers.iter_mut().find(|(name, _)| name == k.as_str()) {
            Some((_, existing)) => {
                existing.push_str(", ");
                existing.push_str(&v);
            }
            None => headers.push((k.as_str().to_owned(), v)),
        }
    }
    let bytes = response.bytes().await.map_err(failed)?;
    Ok(Response {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
        url: final_url,
    })
}

/// The timeout option: seconds or a duration string.
pub(crate) fn timeout(opts: Option<&Table>) -> mlua::Result<Duration> {
    match opts.map(|o| o.raw_get::<Value>("timeout")).transpose()? {
        None | Some(Value::Nil) => Ok(DEFAULT_TIMEOUT),
        Some(v) => Ok(crate::modules::concurrency::duration(&v)?),
    }
}

/// Reads headers, query, body or json, and timeout from an options table into `req`.
fn read_options(req: &mut Request, opts: Option<&Table>) -> mlua::Result<bool> {
    let Some(o) = opts else {
        return Ok(false);
    };
    if let Some(h) = o.raw_get::<Option<Table>>("headers")? {
        for pair in h.pairs::<String, String>() {
            req.headers.push(pair?);
        }
    }
    if let Some(q) = o.raw_get::<Option<Table>>("query")? {
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
            req.query.push((k, v));
        }
        req.query.sort();
    }
    let body: Option<mlua::LuaString> = o.raw_get("body")?;
    let json: Value = o.raw_get("json")?;
    match (body, json.is_nil()) {
        (Some(_), false) => {
            return Err(Error::new("bad_option", "give body or json, not both").into());
        }
        (Some(b), true) => req.body = Some(b.as_bytes().to_vec()),
        (None, false) => {
            let value = crate::value::to_json(&json, crate::value::Mode::Strict)?;
            req.body = Some(
                serde_json::to_vec(&value).map_err(|e| Error::new("internal", e.to_string()))?,
            );
            if !req
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            {
                req.headers
                    .push(("content-type".into(), "application/json".into()));
            }
        }
        (None, true) => {}
    }
    req.timeout = timeout(Some(o))?;
    Ok(o.raw_get::<Option<bool>>("check")?.unwrap_or(false))
}

/// A response as Lua: a table with a json() that decodes the body.
fn response_table(lua: &Lua, r: Response) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.raw_set("status", r.status)?;
    t.raw_set("ok", (200..300).contains(&r.status))?;
    let headers = lua.create_table()?;
    for (k, v) in &r.headers {
        headers.raw_set(k.as_str(), v.as_str())?;
    }
    t.raw_set("headers", headers)?;
    t.raw_set("url", r.url.as_str())?;
    let body = lua.create_string(&r.body)?;
    t.raw_set("body", &body)?;
    let json = lua.create_function(move |lua, ()| {
        let text = body.to_string_lossy();
        let value: Json = serde_json::from_str(&text)
            .map_err(|e| Error::new("bad_json", format!("the response body is not JSON: {e}")))?;
        crate::modules::json::decode(lua, &value)
    })?;
    t.raw_set("json", json)?;
    Ok(t)
}

async fn call(
    lua: Lua,
    path: &str,
    method: String,
    url: String,
    opts: Option<Table>,
) -> mlua::Result<Table> {
    let mut req = Request {
        method,
        url,
        headers: Vec::new(),
        query: Vec::new(),
        body: None,
        timeout: DEFAULT_TIMEOUT,
    };
    let check = read_options(&mut req, opts.as_ref())?;
    let shown = format!("{} {}", req.method.to_uppercase(), req.url);
    let r = send(&lua, req).await?;
    if check && !(200..300).contains(&r.status) {
        let first: String = r.body.chars().take(200).collect();
        return Err(
            Error::new("http_status", format!("{shown} returned {}", r.status))
                .with_data(serde_json::json!({ "status": r.status, "body": first }))
                .with_hint(format!(
                    "{path} with check = false returns the response instead"
                ))
                .into(),
        );
    }
    response_table(&lua, r)
}

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let module = installer.module(&DOC)?;
    installer.function(
        &DOC,
        Some(&module),
        "get",
        lua.create_async_function(|lua, (url, opts): (String, Option<Table>)| async move {
            options::check_opt("http.get", opts.as_ref())?;
            call(lua, "http.get", "GET".into(), url, opts).await
        })?,
    )?;
    installer.function(
        &DOC,
        Some(&module),
        "post",
        lua.create_async_function(|lua, (url, opts): (String, Option<Table>)| async move {
            options::check_opt("http.post", opts.as_ref())?;
            call(lua, "http.post", "POST".into(), url, opts).await
        })?,
    )?;
    installer.function(
        &DOC,
        Some(&module),
        "request",
        lua.create_async_function(|lua, opts: Table| async move {
            options::check("http.request", &opts)?;
            let method: String = opts.raw_get("method")?;
            let url: String = opts.raw_get("url")?;
            call(lua, "http.request", method, url, Some(opts)).await
        })?,
    )?;
    Ok(())
}
