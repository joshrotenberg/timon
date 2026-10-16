//! `sqlite`: SQLite databases through the bundled rusqlite.
//!
//! A database is a userdata with `exec`, `query`, `one`, `last_id`, `transaction` and `close`.
//! Values map to Lua directly: integers, reals, text, blobs as Lua strings, and NULL as nil.

use std::cell::RefCell;

use mlua::{Lua, MultiValue, Table, UserData, UserDataMethods, Value};
use rusqlite::types::{Value as Sql, ValueRef};

use crate::docs::{Func, Module, Opt};
use crate::error::Error;
use crate::modules::Installer;
use crate::options;

pub const DOC: Module = Module {
    name: "sqlite",
    summary: "SQLite databases. sqlite.open returns a database: db:exec(sql, params) runs statements and returns the rows changed; db:query(sql, params) returns rows as tables keyed by column; db:one returns the first row or nil; db:last_id(); db:transaction(fn) commits, or rolls back and raises; db:close(). params is a list for ?, or a table for :name. NULL reads as nil. Errors raise sqlite_failed.",
    functions: &[Func::new(
        "open",
        "sqlite.open(path, { readonly, create }) -> db",
        "Opens a database file, or an in-memory database without a path. A missing file is created unless readonly or create = false.",
        "local db = sqlite.open()\ndb:exec(\"create table seen (id text primary key, n integer)\")\ndb:exec(\"insert into seen values (?, ?)\", { \"a\", 1 })\ndb:exec(\"insert into seen values (:id, :n)\", { id = \"b\", n = 2 })\nreturn db:one(\"select sum(n) as total from seen\").total  -- 3",
    )
    .with_options(&[
        Opt::new("readonly", "boolean", "open for reading only"),
        Opt::new("create", "boolean", "create a missing file (default true)"),
    ])],
};

struct Db {
    conn: RefCell<Option<rusqlite::Connection>>,
    name: String,
}

fn failed(sql: &str, e: rusqlite::Error) -> mlua::Error {
    Error::new("sqlite_failed", e.to_string())
        .with_data(serde_json::json!({ "sql": sql }))
        .into()
}

impl Db {
    fn with<T>(&self, f: impl FnOnce(&rusqlite::Connection) -> mlua::Result<T>) -> mlua::Result<T> {
        match self.conn.borrow().as_ref() {
            Some(c) => f(c),
            None => {
                Err(Error::new("closed", format!("the database {} is closed", self.name)).into())
            }
        }
    }
}

fn to_sql(v: Value) -> mlua::Result<Sql> {
    Ok(match v {
        Value::Nil => Sql::Null,
        Value::Boolean(b) => Sql::Integer(b.into()),
        Value::Integer(i) => Sql::Integer(i),
        Value::Number(n) => Sql::Real(n),
        Value::String(s) => match std::str::from_utf8(&s.as_bytes()) {
            Ok(text) => Sql::Text(text.to_owned()),
            Err(_) => Sql::Blob(s.as_bytes().to_vec()),
        },
        Value::Table(t)
            if t.metatable()
                .and_then(|m| m.raw_get::<Option<String>>("__name").ok().flatten())
                .as_deref()
                == Some(crate::value::NULL_NAME) =>
        {
            Sql::Null
        }
        other => {
            return Err(Error::new(
                "bad_argument",
                format!("a {} cannot be an SQL parameter", other.type_name()),
            )
            .into());
        }
    })
}

/// Binds params: a list for `?` placeholders, or a table of names for `:name`, `@name`, `$name`.
fn bind(stmt: &mut rusqlite::Statement, params: Option<Table>, sql: &str) -> mlua::Result<()> {
    let Some(params) = params else {
        return Ok(());
    };
    if params.raw_len() > 0 {
        for (i, v) in params.sequence_values::<Value>().enumerate() {
            stmt.raw_bind_parameter(i + 1, to_sql(v?)?)
                .map_err(|e| failed(sql, e))?;
        }
        return Ok(());
    }
    for pair in params.pairs::<String, Value>() {
        let (name, v) = pair?;
        let index = ["", ":", "@", "$"]
            .iter()
            .find_map(|p| stmt.parameter_index(&format!("{p}{name}")).ok().flatten())
            .ok_or_else(|| {
                mlua::Error::from(
                    Error::new("bad_argument", format!("the SQL has no parameter :{name}"))
                        .with_data(serde_json::json!({ "sql": sql })),
                )
            })?;
        stmt.raw_bind_parameter(index, to_sql(v)?)
            .map_err(|e| failed(sql, e))?;
    }
    Ok(())
}

fn to_lua(lua: &Lua, v: ValueRef) -> mlua::Result<Value> {
    Ok(match v {
        ValueRef::Null => Value::Nil,
        ValueRef::Integer(i) => Value::Integer(i),
        ValueRef::Real(f) => Value::Number(f),
        ValueRef::Text(t) | ValueRef::Blob(t) => Value::String(lua.create_string(t)?),
    })
}

/// Runs a query and returns up to `limit` rows as tables keyed by column.
fn rows(
    lua: &Lua,
    conn: &rusqlite::Connection,
    sql: &str,
    params: Option<Table>,
    limit: Option<usize>,
) -> mlua::Result<Table> {
    let mut stmt = conn.prepare(sql).map_err(|e| failed(sql, e))?;
    bind(&mut stmt, params, sql)?;
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let out = crate::modules::json::array(lua, None)?;
    let mut rows = stmt.raw_query();
    let mut n = 0;
    while let Some(row) = rows.next().map_err(|e| failed(sql, e))? {
        let t = crate::modules::json::object(lua, None)?;
        for (i, name) in names.iter().enumerate() {
            let v = row.get_ref(i).map_err(|e| failed(sql, e))?;
            t.raw_set(name.as_str(), to_lua(lua, v)?)?;
        }
        n += 1;
        out.raw_set(n, t)?;
        if limit.is_some_and(|l| n >= l) {
            break;
        }
    }
    Ok(out)
}

impl UserData for Db {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("exec", |_, this, (sql, params): (String, Option<Table>)| {
            this.with(|conn| {
                if params.is_none() {
                    let before = conn.total_changes();
                    conn.execute_batch(&sql).map_err(|e| failed(&sql, e))?;
                    return Ok((conn.total_changes() - before) as i64);
                }
                let mut stmt = conn.prepare(&sql).map_err(|e| failed(&sql, e))?;
                bind(&mut stmt, params, &sql)?;
                Ok(stmt.raw_execute().map_err(|e| failed(&sql, e))? as i64)
            })
        });
        methods.add_method(
            "query",
            |lua, this, (sql, params): (String, Option<Table>)| {
                this.with(|conn| rows(lua, conn, &sql, params, None))
            },
        );
        methods.add_method(
            "one",
            |lua, this, (sql, params): (String, Option<Table>)| {
                this.with(|conn| {
                    let t = rows(lua, conn, &sql, params, Some(1))?;
                    t.raw_get::<Value>(1)
                })
            },
        );
        methods.add_method("last_id", |_, this, ()| {
            this.with(|conn| Ok(conn.last_insert_rowid()))
        });
        methods.add_async_method("transaction", |lua, this, f: mlua::Function| async move {
            this.with(|conn| conn.execute_batch("begin").map_err(|e| failed("begin", e)))?;
            // Through the runner, so an error table comes back as a value and is raised again
            // as it was.
            let run: mlua::Function = lua.named_registry_value(crate::runtime::RUN)?;
            let mut out = run.call_async::<MultiValue>(f).await?.into_iter();
            if matches!(out.next(), Some(Value::Boolean(true))) {
                this.with(|conn| {
                    conn.execute_batch("commit")
                        .map_err(|e| failed("commit", e))
                })?;
                return Ok(out.collect::<MultiValue>());
            }
            let _ = this.with(|conn| {
                conn.execute_batch("rollback")
                    .map_err(|e| failed("rollback", e))
            });
            Err(crate::error::raise(&lua, out.next().unwrap_or(Value::Nil)))
        });
        methods.add_method("close", |_, this, ()| {
            if let Some(conn) = this.conn.borrow_mut().take() {
                conn.close().map_err(|(_, e)| failed("close", e))?;
            }
            Ok(())
        });
        methods.add_meta_method("__tostring", |_, this, ()| {
            Ok(format!("sqlite database {}", this.name))
        });
    }
}

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let module = installer.module(&DOC)?;
    installer.function(
        &DOC,
        Some(&module),
        "open",
        lua.create_function(|lua, (path, opts): (Option<String>, Option<Table>)| {
            options::check_opt("sqlite.open", opts.as_ref())?;
            crate::tasks::refuse_while_loading(lua, "sqlite.open")?;
            let (readonly, create) = match &opts {
                Some(o) => (
                    o.raw_get::<Option<bool>>("readonly")?.unwrap_or(false),
                    o.raw_get::<Option<bool>>("create")?.unwrap_or(true),
                ),
                None => (false, true),
            };
            use rusqlite::OpenFlags as F;
            let conn = match &path {
                None => rusqlite::Connection::open_in_memory(),
                Some(p) => {
                    let mut flags = F::SQLITE_OPEN_URI | F::SQLITE_OPEN_NO_MUTEX;
                    flags |= if readonly {
                        F::SQLITE_OPEN_READ_ONLY
                    } else if create {
                        F::SQLITE_OPEN_READ_WRITE | F::SQLITE_OPEN_CREATE
                    } else {
                        F::SQLITE_OPEN_READ_WRITE
                    };
                    rusqlite::Connection::open_with_flags(p, flags)
                }
            }
            .map_err(|e| {
                let name = path.as_deref().unwrap_or(":memory:");
                Error::new("sqlite_failed", format!("cannot open {name}: {e}"))
            })?;
            Ok(Db {
                conn: RefCell::new(Some(conn)),
                name: path.unwrap_or_else(|| ":memory:".into()),
            })
        })?,
    )?;
    Ok(())
}
