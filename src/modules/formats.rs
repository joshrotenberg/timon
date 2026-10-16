//! Data formats: `toml`, `yaml`, `csv` and `markdown`, and `term` for the terminal.
//!
//! Every decoder returns tables with `json`'s array and object markers, so a decoded value
//! encodes back with `json.encode` the way it was read.

use std::borrow::Cow;
use std::io::IsTerminal;

use mlua::{Lua, Table, Value};
use serde_json::{Map, Value as Json};

use crate::docs::{Func, Module, Opt};
use crate::error::Error;
use crate::modules::Installer;
use crate::modules::json::decode as to_lua;
use crate::options;
use crate::value::{Mode, to_json};

pub const TOML_DOC: Module = Module {
    name: "toml",
    summary: "TOML: decode and encode, and edits that keep a file's comments and formatting.",
    functions: &[
        Func::new(
            "decode",
            "toml.decode(text) -> table",
            "Parses TOML into tables. Dates and times become strings.",
            "local cargo = toml.decode('[package]\\nname = \"timon\"\\nversion = \"0.1.0\"')\nreturn cargo.package.version  -- 0.1.0",
        ),
        Func::new(
            "encode",
            "toml.encode(table) -> string",
            "Writes a table as TOML. Nested tables become sections; nil and json.null have no TOML form.",
            "return toml.encode({ package = { name = \"x\", version = \"1.0.0\" } })",
        ),
        Func::new(
            "set",
            "toml.set(text, key, value) -> text, old",
            "Sets one value in TOML text and returns the new text and the old value. Comments, order and spacing outside the value stay as they were. key is dotted (\"package.version\") or a list of parts ({ \"dependencies\", \"serde.json\" }); missing tables are created; a nil value removes the key.",
            "local text = '[package]\\nversion = \"0.1.0\" # bumped by release\\n'\nlocal new, old = toml.set(text, \"package.version\", \"0.2.0\")\nreturn new, old  -- the comment stays; old is 0.1.0",
        ),
        Func::new(
            "edit",
            "toml.edit(path, key, value) -> old",
            "toml.set on a file: reads it, sets the value and writes it back, keeping everything else byte for byte. Returns the old value.",
            "local old = toml.edit(\"Cargo.toml\", \"package.version\", \"0.2.0\")",
        ),
    ],
};

pub const YAML_DOC: Module = Module {
    name: "yaml",
    summary: "YAML 1.2: decode and encode.",
    functions: &[
        Func::new(
            "decode",
            "yaml.decode(text, { all }) -> value",
            "Parses YAML. Text with several documents (---) needs all = true, which returns a list of them. An alias reads as its anchor's value.",
            "local ci = yaml.decode(\"on: [push]\\njobs:\\n  test:\\n    runs-on: ubuntu-latest\\n\")\nreturn ci.jobs.test[\"runs-on\"]  -- ubuntu-latest",
        )
        .with_options(&[Opt::new(
            "all",
            "boolean",
            "return every document as a list",
        )]),
        Func::new(
            "encode",
            "yaml.encode(value) -> string",
            "Writes a value as YAML, quoting strings that would read back as something else.",
            "return yaml.encode({ name = \"timon\", tags = json.array{ \"lua\", \"yes\" } })",
        ),
    ],
};

pub const CSV_DOC: Module = Module {
    name: "csv",
    summary: "CSV: rows as tables keyed by the header, or as lists.",
    functions: &[
        Func::new(
            "decode",
            "csv.decode(text, { header, separator }) -> rows",
            "Parses CSV. With a header (the default), each row is a table keyed by the header's names; without, a list of fields. Fields are strings.",
            "local rows = csv.decode(\"name,lines\\nmain.rs,120\\nlib.rs,80\\n\")\nreturn rows[2].name, tonumber(rows[2].lines)  -- lib.rs, 80",
        )
        .with_options(&[
            Opt::new("header", "boolean", "the first row names the fields (default true)"),
            Opt::new("separator", "string", "one character (default \",\")"),
        ]),
        Func::new(
            "encode",
            "csv.encode(rows, { columns, header, separator }) -> string",
            "Writes rows: tables keyed by column name, or lists. columns sets which names and their order (default: every name, sorted); header = false leaves the header row out.",
            "return csv.encode({ { name = \"a\", n = 1 }, { name = \"b,c\", n = 2 } }, { columns = { \"name\", \"n\" } })",
        )
        .with_options(&[
            Opt::new("columns", "string[]", "the column names, in order"),
            Opt::new("header", "boolean", "write a header row (default true)"),
            Opt::new("separator", "string", "one character (default \",\")"),
        ]),
    ],
};

pub const MARKDOWN_DOC: Module = Module {
    name: "markdown",
    summary: "Markdown structure: headings and fenced code blocks, with line numbers.",
    functions: &[
        Func::new(
            "headings",
            "markdown.headings(text) -> { { level, text, line } }",
            "The headings, in order. Lines that look like headings inside code blocks are not headings.",
            "local h = markdown.headings(\"# timon\\n\\n```sh\\n# not a heading\\n```\\n## Install\\n\")\nreturn #h, h[2].text, h[2].line  -- 2, Install, 6",
        ),
        Func::new(
            "code",
            "markdown.code(text, { lang }) -> { { lang, code, line } }",
            "The fenced code blocks, in order: the first word of the info string, the code, and the line the fence starts on.",
            "local blocks = markdown.code(\"text\\n```lua\\nreturn 1\\n```\\n\", { lang = \"lua\" })\nreturn blocks[1].code  -- return 1",
        )
        .with_options(&[Opt::new(
            "lang",
            "string",
            "only blocks in this language",
        )]),
    ],
};

pub const TERM_DOC: Module = Module {
    name: "term",
    summary: "The terminal: whether there is one, its width, and color that appears only on one.",
    functions: &[
        Func::new(
            "tty",
            "term.tty(stream) -> boolean",
            "Whether \"stdout\" (the default), \"stderr\" or \"stdin\" is a terminal.",
            "if not term.tty() then log.info(\"output is piped\") end",
        ),
        Func::new(
            "color",
            "term.color(name, text) -> string",
            "text in a color or style: red, green, yellow, blue, magenta, cyan, gray, bold, dim. Plain text when stdout is not a terminal or NO_COLOR is set.",
            "print(term.color(\"green\", \"all gates pass\"))",
        ),
        Func::new(
            "width",
            "term.width() -> integer | nil",
            "The terminal's width in columns, from the terminal or COLUMNS; nil when there is no terminal.",
            "local w = term.width() or 80",
        ),
    ],
};

fn bad(kind: &str, message: impl Into<String>) -> Error {
    Error::new(kind, message.into())
}

// ---- TOML ----

fn toml_json(v: &toml::Value) -> Json {
    match v {
        toml::Value::String(s) => Json::String(s.clone()),
        toml::Value::Integer(i) => Json::from(*i),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f).map_or(Json::Null, Json::Number),
        toml::Value::Boolean(b) => Json::Bool(*b),
        toml::Value::Datetime(d) => Json::String(d.to_string()),
        toml::Value::Array(a) => Json::Array(a.iter().map(toml_json).collect()),
        toml::Value::Table(t) => {
            Json::Object(t.iter().map(|(k, v)| (k.clone(), toml_json(v))).collect())
        }
    }
}

fn edit_json(item: &toml_edit::Item) -> Json {
    use toml_edit::{Item, Value as V};
    fn value(v: &V) -> Json {
        match v {
            V::String(s) => Json::String(s.value().clone()),
            V::Integer(i) => Json::from(*i.value()),
            V::Float(f) => {
                serde_json::Number::from_f64(*f.value()).map_or(Json::Null, Json::Number)
            }
            V::Boolean(b) => Json::Bool(*b.value()),
            V::Datetime(d) => Json::String(d.value().to_string()),
            V::Array(a) => Json::Array(a.iter().map(value).collect()),
            V::InlineTable(t) => {
                Json::Object(t.iter().map(|(k, v)| (k.to_owned(), value(v))).collect())
            }
        }
    }
    match item {
        Item::None => Json::Null,
        Item::Value(v) => value(v),
        Item::Table(t) => Json::Object(
            t.iter()
                .map(|(k, v)| (k.to_owned(), edit_json(v)))
                .collect(),
        ),
        Item::ArrayOfTables(a) => Json::Array(
            a.iter()
                .map(|t| {
                    Json::Object(
                        t.iter()
                            .map(|(k, v)| (k.to_owned(), edit_json(v)))
                            .collect(),
                    )
                })
                .collect(),
        ),
    }
}

fn edit_value(json: &Json) -> Result<toml_edit::Value, Error> {
    Ok(match json {
        Json::String(s) => s.as_str().into(),
        Json::Bool(b) => (*b).into(),
        Json::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64().unwrap_or(f64::NAN).into(),
        },
        Json::Array(items) => {
            let mut a = toml_edit::Array::new();
            for v in items {
                a.push(edit_value(v)?);
            }
            a.into()
        }
        Json::Object(map) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, v) in map {
                t.insert(k, edit_value(v)?);
            }
            t.into()
        }
        Json::Null => return Err(bad("bad_argument", "null has no TOML form")),
    })
}

/// A key: dotted text or a list of parts.
fn key_parts(key: &Value) -> Result<Vec<String>, Error> {
    let parts: Vec<String> = match key {
        Value::String(s) => s.to_string_lossy().split('.').map(str::to_owned).collect(),
        Value::Table(t) => t
            .sequence_values::<String>()
            .collect::<mlua::Result<_>>()
            .map_err(|e| Error::from_lua(&e))?,
        other => {
            return Err(bad(
                "bad_argument",
                format!(
                    "a TOML key is a string or a list of parts, got {}",
                    other.type_name()
                ),
            ));
        }
    };
    if parts.is_empty() || parts.iter().any(String::is_empty) {
        return Err(bad("bad_argument", "a TOML key needs every part"));
    }
    Ok(parts)
}

/// Sets `keys` in TOML `text` to `value`, or removes it for `None`.
fn toml_set(text: &str, keys: &[String], value: Option<&Json>) -> Result<(String, Json), Error> {
    use toml_edit::{Item, TableLike};
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| bad("bad_toml", e.to_string()))?;
    let value = value.map(edit_value).transpose()?;
    let (last, parents) = keys.split_last().expect("keys are not empty");
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for (i, k) in parents.iter().enumerate() {
        if !table.contains_key(k) {
            if value.is_none() {
                return Ok((text.to_owned(), Json::Null));
            }
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            table.insert(k, Item::Table(t));
        }
        table = table
            .get_mut(k)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| {
                bad(
                    "bad_argument",
                    format!("\"{}\" is not a table", keys[..=i].join(".")),
                )
            })?;
    }
    let old = table.get(last).map(edit_json).unwrap_or(Json::Null);
    match value {
        None => {
            table.remove(last);
        }
        Some(mut v) => match table.get_mut(last) {
            Some(Item::Value(existing)) => {
                *v.decor_mut() = existing.decor().clone();
                *existing = v;
            }
            Some(item) => *item = Item::Value(v),
            None => {
                table.insert(last, Item::Value(v));
            }
        },
    }
    Ok((doc.to_string(), old))
}

// ---- YAML ----

fn yaml_json(y: &saphyr::Yaml) -> Result<Json, Error> {
    use saphyr::{Scalar, Yaml};
    Ok(match y {
        Yaml::Value(Scalar::Null) => Json::Null,
        Yaml::Value(Scalar::Boolean(b)) => Json::Bool(*b),
        Yaml::Value(Scalar::Integer(i)) => Json::from(*i),
        Yaml::Value(Scalar::FloatingPoint(f)) => {
            serde_json::Number::from_f64(f.into_inner()).map_or(Json::Null, Json::Number)
        }
        Yaml::Value(Scalar::String(s)) => Json::String(s.to_string()),
        Yaml::Representation(s, _, _) => Json::String(s.to_string()),
        Yaml::Sequence(items) => {
            Json::Array(items.iter().map(yaml_json).collect::<Result<_, _>>()?)
        }
        Yaml::Mapping(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                let key = match yaml_json(k)? {
                    Json::String(s) => s,
                    Json::Null => "null".to_owned(),
                    Json::Bool(b) => b.to_string(),
                    Json::Number(n) => n.to_string(),
                    _ => return Err(bad("bad_yaml", "a mapping key is a list or a mapping")),
                };
                out.insert(key, yaml_json(v)?);
            }
            Json::Object(out)
        }
        Yaml::Tagged(_, node) => yaml_json(node)?,
        Yaml::Alias(_) => {
            return Err(bad("bad_yaml", "an alias cannot be resolved")
                .with_hint("write the value out where the alias is"));
        }
        Yaml::BadValue => return Err(bad("bad_yaml", "a value cannot be read")),
    })
}

fn json_yaml(json: &Json) -> saphyr::Yaml<'static> {
    use saphyr::{Scalar, ScalarStyle, Yaml};
    match json {
        Json::Null => Yaml::Value(Scalar::Null),
        Json::Bool(b) => Yaml::Value(Scalar::Boolean(*b)),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Yaml::Value(Scalar::Integer(i)),
            None => {
                let f = n.as_f64().unwrap_or(f64::NAN);
                let text = if f.fract() == 0.0 && f.is_finite() {
                    format!("{f:.1}")
                } else {
                    f.to_string()
                };
                Yaml::Representation(Cow::Owned(text), ScalarStyle::Plain, None)
            }
        },
        Json::String(s) => Yaml::Value(Scalar::String(Cow::Owned(s.clone()))),
        Json::Array(items) => Yaml::Sequence(items.iter().map(json_yaml).collect()),
        Json::Object(map) => {
            let mut m = saphyr::Mapping::new();
            for (k, v) in map {
                m.insert(
                    Yaml::Value(Scalar::String(Cow::Owned(k.clone()))),
                    json_yaml(v),
                );
            }
            Yaml::Mapping(m)
        }
    }
}

// ---- CSV ----

fn separator(opts: Option<&Table>) -> mlua::Result<u8> {
    let sep = match opts {
        Some(o) => o.raw_get::<Option<String>>("separator")?,
        None => None,
    };
    match sep.as_deref() {
        None => Ok(b','),
        Some(s) if s.len() == 1 => Ok(s.as_bytes()[0]),
        Some(s) => Err(bad(
            "bad_option",
            format!("separator must be one ASCII character, got \"{s}\""),
        )
        .into()),
    }
}

fn csv_field(v: &Json) -> String {
    match v {
        Json::Null => String::new(),
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---- Markdown ----

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

// ---- term ----

fn width() -> Option<u16> {
    #[cfg(unix)]
    {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO, libc::STDIN_FILENO] {
            if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
                return Some(ws.ws_col);
            }
        }
    }
    std::env::var("COLUMNS").ok()?.parse().ok()
}

fn color_code(name: &str) -> Option<&'static str> {
    Some(match name {
        "red" => "31",
        "green" => "32",
        "yellow" => "33",
        "blue" => "34",
        "magenta" => "35",
        "cyan" => "36",
        "gray" => "90",
        "bold" => "1",
        "dim" => "2",
        _ => return None,
    })
}

const COLORS: &[&str] = &[
    "red", "green", "yellow", "blue", "magenta", "cyan", "gray", "bold", "dim",
];

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();

    let toml = installer.module(&TOML_DOC)?;
    installer.function(
        &TOML_DOC,
        Some(&toml),
        "decode",
        lua.create_function(|lua, text: String| {
            let table: toml::Table = toml::from_str(&text)
                .map_err(|e: toml::de::Error| bad("bad_toml", e.to_string()))?;
            to_lua(lua, &toml_json(&toml::Value::Table(table)))
        })?,
    )?;
    installer.function(
        &TOML_DOC,
        Some(&toml),
        "encode",
        lua.create_function(|_, value: Value| {
            let json = to_json(&value, Mode::Strict)?;
            if !json.is_object() {
                return Err(bad("bad_argument", "TOML encodes a table of named values").into());
            }
            Ok(toml::to_string(&json).map_err(|e| bad("bad_argument", e.to_string()))?)
        })?,
    )?;
    installer.function(
        &TOML_DOC,
        Some(&toml),
        "set",
        lua.create_function(|lua, (text, key, value): (String, Value, Value)| {
            let keys = key_parts(&key)?;
            let value = if value.is_nil() {
                None
            } else {
                Some(to_json(&value, Mode::Strict)?)
            };
            let (text, old) = toml_set(&text, &keys, value.as_ref())?;
            Ok((text, to_lua(lua, &old)?))
        })?,
    )?;
    installer.function(
        &TOML_DOC,
        Some(&toml),
        "edit",
        lua.create_function(|lua, (path, key, value): (String, Value, Value)| {
            crate::tasks::refuse_while_loading(lua, "toml.edit")?;
            let keys = key_parts(&key)?;
            let value = if value.is_nil() {
                None
            } else {
                Some(to_json(&value, Mode::Strict)?)
            };
            let text = std::fs::read_to_string(&path).map_err(|e| {
                let kind = if e.kind() == std::io::ErrorKind::NotFound {
                    "not_found"
                } else {
                    "io"
                };
                bad(kind, format!("cannot read {path}: {e}"))
            })?;
            let (new, old) = toml_set(&text, &keys, value.as_ref())
                .map_err(|e| Error::new(e.kind.clone(), format!("{path}: {}", e.message)))?;
            if new != text {
                std::fs::write(&path, new)
                    .map_err(|e| bad("io", format!("cannot write {path}: {e}")))?;
            }
            to_lua(lua, &old)
        })?,
    )?;

    let yaml = installer.module(&YAML_DOC)?;
    installer.function(
        &YAML_DOC,
        Some(&yaml),
        "decode",
        lua.create_function(|lua, (text, opts): (String, Option<Table>)| {
            options::check_opt("yaml.decode", opts.as_ref())?;
            let all = match &opts {
                Some(o) => o.raw_get::<Option<bool>>("all")?.unwrap_or(false),
                None => false,
            };
            use saphyr::LoadableYamlNode;
            let docs =
                saphyr::Yaml::load_from_str(&text).map_err(|e| bad("bad_yaml", e.to_string()))?;
            let docs: Vec<Json> = docs.iter().map(yaml_json).collect::<Result<_, _>>()?;
            if all {
                return to_lua(lua, &Json::Array(docs));
            }
            match docs.len() {
                0 => Ok(Value::Nil),
                1 => to_lua(lua, &docs[0]),
                n => Err(bad("bad_yaml", format!("the text has {n} documents"))
                    .with_hint("pass { all = true } for a list of them")
                    .into()),
            }
        })?,
    )?;
    installer.function(
        &YAML_DOC,
        Some(&yaml),
        "encode",
        lua.create_function(|_, value: Value| {
            let json = to_json(&value, Mode::Strict)?;
            let mut out = String::new();
            saphyr::YamlEmitter::new(&mut out)
                .dump(&json_yaml(&json))
                .map_err(|e| bad("bad_argument", format!("{e:?}")))?;
            let body = out
                .strip_prefix("---\n")
                .unwrap_or(out.trim_start_matches("---"));
            let body = body.trim_start_matches(' ');
            Ok(format!("{}\n", body.trim_end()))
        })?,
    )?;

    let csv = installer.module(&CSV_DOC)?;
    installer.function(
        &CSV_DOC,
        Some(&csv),
        "decode",
        lua.create_function(|lua, (text, opts): (String, Option<Table>)| {
            options::check_opt("csv.decode", opts.as_ref())?;
            let header = match &opts {
                Some(o) => o.raw_get::<Option<bool>>("header")?.unwrap_or(true),
                None => true,
            };
            let mut reader = ::csv::ReaderBuilder::new()
                .has_headers(header)
                .flexible(true)
                .delimiter(separator(opts.as_ref())?)
                .from_reader(text.as_bytes());
            let names: Vec<String> = if header {
                reader
                    .headers()
                    .map_err(|e| bad("bad_csv", e.to_string()))?
                    .iter()
                    .map(str::to_owned)
                    .collect()
            } else {
                Vec::new()
            };
            let mut rows = Vec::new();
            for record in reader.records() {
                let record = record.map_err(|e| bad("bad_csv", e.to_string()))?;
                rows.push(if header {
                    Json::Object(
                        names
                            .iter()
                            .zip(record.iter())
                            .map(|(k, v)| (k.clone(), Json::String(v.to_owned())))
                            .collect(),
                    )
                } else {
                    Json::Array(record.iter().map(|v| Json::String(v.to_owned())).collect())
                });
            }
            to_lua(lua, &Json::Array(rows))
        })?,
    )?;
    installer.function(
        &CSV_DOC,
        Some(&csv),
        "encode",
        lua.create_function(|_, (rows, opts): (Value, Option<Table>)| {
            options::check_opt("csv.encode", opts.as_ref())?;
            let rows = match to_json(&rows, Mode::Strict)? {
                Json::Array(rows) => rows,
                Json::Object(m) if m.is_empty() => Vec::new(),
                _ => return Err(bad("bad_argument", "csv.encode takes a list of rows").into()),
            };
            let (columns, header) = match &opts {
                Some(o) => (
                    o.raw_get::<Option<Vec<String>>>("columns")?,
                    o.raw_get::<Option<bool>>("header")?.unwrap_or(true),
                ),
                None => (None, true),
            };
            let keyed = rows.iter().any(Json::is_object);
            let columns = match columns {
                Some(c) => c,
                None if keyed => {
                    let mut names: Vec<String> = rows
                        .iter()
                        .filter_map(Json::as_object)
                        .flat_map(|m| m.keys().cloned())
                        .collect();
                    names.sort();
                    names.dedup();
                    names
                }
                None => Vec::new(),
            };
            let mut writer = ::csv::WriterBuilder::new()
                .flexible(true)
                .delimiter(separator(opts.as_ref())?)
                .from_writer(Vec::new());
            let io = |e: ::csv::Error| bad("io", e.to_string());
            if header && !columns.is_empty() {
                writer.write_record(&columns).map_err(io)?;
            }
            for row in &rows {
                let fields: Vec<String> = match row {
                    Json::Object(m) => columns
                        .iter()
                        .map(|c| m.get(c).map(csv_field).unwrap_or_default())
                        .collect(),
                    Json::Array(items) => items.iter().map(csv_field).collect(),
                    other => vec![csv_field(other)],
                };
                writer.write_record(&fields).map_err(io)?;
            }
            let bytes = writer.into_inner().map_err(|e| bad("io", e.to_string()))?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        })?,
    )?;

    let markdown = installer.module(&MARKDOWN_DOC)?;
    installer.function(
        &MARKDOWN_DOC,
        Some(&markdown),
        "headings",
        lua.create_function(|lua, text: String| {
            use pulldown_cmark::{Event, Parser, Tag, TagEnd};
            let mut out = Vec::new();
            let mut current: Option<(u32, String, usize)> = None;
            for (event, range) in Parser::new(&text).into_offset_iter() {
                match event {
                    Event::Start(Tag::Heading { level, .. }) => {
                        current = Some((level as u32, String::new(), line_of(&text, range.start)));
                    }
                    Event::Text(t) | Event::Code(t) => {
                        if let Some((_, s, _)) = current.as_mut() {
                            s.push_str(&t);
                        }
                    }
                    Event::End(TagEnd::Heading(_)) => {
                        if let Some((level, s, line)) = current.take() {
                            out.push(serde_json::json!({ "level": level, "text": s.trim(), "line": line }));
                        }
                    }
                    _ => {}
                }
            }
            to_lua(lua, &Json::Array(out))
        })?,
    )?;
    installer.function(
        &MARKDOWN_DOC,
        Some(&markdown),
        "code",
        lua.create_function(|lua, (text, opts): (String, Option<Table>)| {
            use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
            options::check_opt("markdown.code", opts.as_ref())?;
            let want = match &opts {
                Some(o) => o.raw_get::<Option<String>>("lang")?,
                None => None,
            };
            let mut out = Vec::new();
            let mut current: Option<(String, String, usize)> = None;
            for (event, range) in Parser::new(&text).into_offset_iter() {
                match event {
                    Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                        let lang = info.split_whitespace().next().unwrap_or("").to_owned();
                        current = Some((lang, String::new(), line_of(&text, range.start)));
                    }
                    Event::Text(t) => {
                        if let Some((_, code, _)) = current.as_mut() {
                            code.push_str(&t);
                        }
                    }
                    Event::End(TagEnd::CodeBlock) => {
                        if let Some((lang, code, line)) = current.take()
                            && want.as_deref().is_none_or(|w| w == lang)
                        {
                            out.push(
                                serde_json::json!({ "lang": lang, "code": code, "line": line }),
                            );
                        }
                    }
                    _ => {}
                }
            }
            to_lua(lua, &Json::Array(out))
        })?,
    )?;

    let term = installer.module(&TERM_DOC)?;
    installer.function(
        &TERM_DOC,
        Some(&term),
        "tty",
        lua.create_function(|_, stream: Option<String>| {
            Ok(match stream.as_deref().unwrap_or("stdout") {
                "stdout" => std::io::stdout().is_terminal(),
                "stderr" => std::io::stderr().is_terminal(),
                "stdin" => std::io::stdin().is_terminal(),
                other => {
                    return Err(bad(
                        "bad_argument",
                        format!(
                            "term.tty takes \"stdout\", \"stderr\" or \"stdin\", got \"{other}\""
                        ),
                    )
                    .into());
                }
            })
        })?,
    )?;
    installer.function(
        &TERM_DOC,
        Some(&term),
        "color",
        lua.create_function(|_, (name, text): (String, String)| {
            let Some(code) = color_code(&name) else {
                let err = bad("bad_argument", format!("unknown color \"{name}\""));
                return Err(
                    match crate::suggest::closest(&name, COLORS.iter().copied()) {
                        Some(c) => err.with_hint(format!("did you mean \"{c}\"?")),
                        None => err.with_hint(format!("colors are {}", COLORS.join(", "))),
                    }
                    .into(),
                );
            };
            let plain = !std::io::stdout().is_terminal()
                || std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
            Ok(if plain {
                text
            } else {
                format!("\x1b[{code}m{text}\x1b[0m")
            })
        })?,
    )?;
    installer.function(
        &TERM_DOC,
        Some(&term),
        "width",
        lua.create_function(|_, ()| Ok(width()))?,
    )?;
    let _: &Lua = lua;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toml_edit_changes_only_the_value() {
        let text = "# The package\n[package]\nname    = \"timon\"   # aligned\nversion = \"0.1.0\" # bumped by release\n\n[dependencies]\nserde = \"1\"\n";
        let (new, old) = toml_set(
            text,
            &["package".into(), "version".into()],
            Some(&Json::from("0.2.0")),
        )
        .unwrap();
        assert_eq!(old, Json::from("0.1.0"));
        assert_eq!(new, text.replace("0.1.0", "0.2.0"));
        let (new, _) = toml_set(
            text,
            &["dependencies".into(), "serde.json".into()],
            Some(&Json::from("2")),
        )
        .unwrap();
        assert!(
            new.ends_with("serde = \"1\"\n\"serde.json\" = \"2\"\n"),
            "{new}"
        );
        let (new, old) = toml_set(text, &["dependencies".into(), "serde".into()], None).unwrap();
        assert_eq!(old, Json::from("1"));
        assert!(!new.contains("serde"), "{new}");
        let (new, _) = toml_set(
            text,
            &["profile".into(), "release".into(), "lto".into()],
            Some(&Json::Bool(true)),
        )
        .unwrap();
        assert!(new.ends_with("[profile.release]\nlto = true\n"), "{new}");
        let err = toml_set(
            text,
            &["package".into(), "name".into(), "x".into()],
            Some(&Json::from(1)),
        )
        .unwrap_err();
        assert_eq!(err.message, "\"package.name\" is not a table");
    }
}
