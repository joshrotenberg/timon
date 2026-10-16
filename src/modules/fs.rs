//! `fs` and `path`: files, directories and path names.

use std::io::Write;
use std::path::{Path, PathBuf};

use mlua::{Table, Variadic};

use crate::docs::{Func, Module, Opt};
use crate::error::Error;
use crate::modules::{Installer, json};
use crate::options;

pub const FS_DOC: Module = Module {
    name: "fs",
    summary: "Files and directories. Paths are relative to the working directory.",
    functions: &[
        Func::new(
            "read",
            "fs.read(path) -> string",
            "The file's contents. A missing file raises not_found.",
            "local text = fs.read(\"README.md\")",
        ),
        Func::new(
            "write",
            "fs.write(path, data, { append, mkdir })",
            "Writes data to the file, replacing it unless append is set.",
            "fs.write(\"out/report.json\", json.encode(report), { mkdir = true })",
        )
        .with_options(&[
            Opt::new("append", "boolean", "add to the end instead of replacing"),
            Opt::new("mkdir", "boolean", "create missing parent directories"),
        ]),
        Func::new(
            "exists",
            "fs.exists(path) -> boolean",
            "Whether anything exists at path.",
            "if not fs.exists(\".timon\") then fs.mkdir(\".timon\") end",
        ),
        Func::new(
            "ls",
            "fs.ls(dir, { hidden }) -> array",
            "Entry names in dir (default \".\"), sorted. Names starting with a dot are left out unless hidden is set.",
            "for _, name in ipairs(fs.ls(\"src\")) do print(name) end",
        )
        .with_options(&[Opt::new("hidden", "boolean", "include names starting with a dot")]),
        Func::new(
            "glob",
            "fs.glob(pattern) -> array",
            "Paths matching a shell pattern (*, ?, [ab], ** for any depth), sorted.",
            "fs.glob(\"src/**/*.rs\")",
        ),
        Func::new(
            "mkdir",
            "fs.mkdir(path)",
            "Creates a directory and any missing parents. An existing directory is fine.",
            "fs.mkdir(\"out/logs\")",
        ),
        Func::new(
            "tmpdir",
            "fs.tmpdir() -> string",
            "Creates a new empty directory under the system temp directory and returns its path. It is not removed automatically.",
            "local dir = fs.tmpdir()\nfs.write(path.join(dir, \"x.txt\"), \"hi\")",
        ),
        Func::new(
            "cwd",
            "fs.cwd() -> string",
            "The working directory.",
            "fs.cwd()",
        ),
        Func::new(
            "home",
            "fs.home() -> string",
            "The home directory.",
            "path.join(fs.home(), \".config\", \"timon\")",
        ),
    ],
};

pub const PATH_DOC: Module = Module {
    name: "path",
    summary: "Path names, without touching the filesystem.",
    functions: &[
        Func::new(
            "join",
            "path.join(...) -> string",
            "Joins parts with the platform separator. An absolute part replaces what came before.",
            "path.join(\"src\", \"modules\", \"fs.rs\")  --> \"src/modules/fs.rs\"",
        ),
        Func::new(
            "base",
            "path.base(p) -> string",
            "The last component, or \"\" when there is none.",
            "path.base(\"src/main.rs\")  --> \"main.rs\"",
        ),
        Func::new(
            "dir",
            "path.dir(p) -> string",
            "Everything before the last component, or \".\" when there is nothing before it.",
            "path.dir(\"src/main.rs\")  --> \"src\"",
        ),
        Func::new(
            "ext",
            "path.ext(p) -> string",
            "The extension without the dot, or \"\".",
            "path.ext(\"review.lua\")  --> \"lua\"",
        ),
    ],
};

fn io_error(action: &str, path: &Path, err: std::io::Error) -> Error {
    let kind = match err.kind() {
        std::io::ErrorKind::NotFound => "not_found",
        std::io::ErrorKind::PermissionDenied => "permission_denied",
        _ => "io",
    };
    Error::new(kind, format!("cannot {action} {}: {err}", path.display()))
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let fs = installer.module(&FS_DOC)?;

    let functions = [
        (
            "read",
            lua.create_function(|lua, path: String| {
                let path = PathBuf::from(path);
                let bytes = std::fs::read(&path).map_err(|e| io_error("read", &path, e))?;
                lua.create_string(bytes)
            })?,
        ),
        (
            "write",
            lua.create_function(
                |_, (path, data, opts): (String, mlua::LuaString, Option<Table>)| {
                    options::check_opt("fs.write", opts.as_ref())?;
                    let flag = |name| -> mlua::Result<bool> {
                        Ok(match &opts {
                            Some(o) => o.raw_get::<Option<bool>>(name)?.unwrap_or(false),
                            None => false,
                        })
                    };
                    let (append, mkdir) = (flag("append")?, flag("mkdir")?);
                    let path = PathBuf::from(path);
                    if mkdir
                        && let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
                    {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| io_error("create", parent, e))?;
                    }
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .write(true)
                        .append(append)
                        .truncate(!append)
                        .open(&path)
                        .map_err(|e| io_error("open", &path, e))?;
                    file.write_all(&data.as_bytes())
                        .map_err(|e| io_error("write", &path, e))?;
                    Ok(())
                },
            )?,
        ),
        (
            "exists",
            lua.create_function(|_, path: String| Ok(Path::new(&path).exists()))?,
        ),
        (
            "ls",
            lua.create_function(|lua, (dir, opts): (Option<String>, Option<Table>)| {
                options::check_opt("fs.ls", opts.as_ref())?;
                let hidden = match &opts {
                    Some(o) => o.raw_get::<Option<bool>>("hidden")?.unwrap_or(false),
                    None => false,
                };
                let dir = PathBuf::from(dir.unwrap_or_else(|| ".".to_owned()));
                let mut names = Vec::new();
                for entry in std::fs::read_dir(&dir).map_err(|e| io_error("list", &dir, e))? {
                    let entry = entry.map_err(|e| io_error("list", &dir, e))?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if hidden || !name.starts_with('.') {
                        names.push(name);
                    }
                }
                names.sort();
                json::array_from(lua, names)
            })?,
        ),
        (
            "glob",
            lua.create_function(|lua, pattern: String| {
                let paths = glob::glob(&pattern).map_err(|e| {
                    Error::new("bad_argument", format!("bad glob pattern {pattern:?}: {e}"))
                })?;
                let mut found = Vec::new();
                for path in paths {
                    let path = path.map_err(|e| {
                        Error::new("io", format!("cannot read {}: {e}", e.path().display()))
                    })?;
                    found.push(text(&path));
                }
                found.sort();
                json::array_from(lua, found)
            })?,
        ),
        (
            "mkdir",
            lua.create_function(|_, path: String| {
                let path = PathBuf::from(path);
                std::fs::create_dir_all(&path).map_err(|e| io_error("create", &path, e))?;
                Ok(())
            })?,
        ),
        (
            "tmpdir",
            lua.create_function(|_, ()| {
                let dir = tempfile::Builder::new()
                    .prefix("timon-")
                    .tempdir()
                    .map_err(|e| {
                        Error::new("io", format!("cannot create a temp directory: {e}"))
                    })?;
                Ok(text(&dir.keep()))
            })?,
        ),
        (
            "cwd",
            lua.create_function(|_, ()| {
                let cwd = std::env::current_dir().map_err(|e| {
                    Error::new("io", format!("cannot read the working directory: {e}"))
                })?;
                Ok(text(&cwd))
            })?,
        ),
        (
            "home",
            lua.create_function(|_, ()| {
                std::env::home_dir()
                    .map(|h| text(&h))
                    .ok_or_else(|| Error::new("not_found", "no home directory is set").into())
            })?,
        ),
    ];
    for (name, f) in functions {
        installer.function(&FS_DOC, Some(&fs), name, f)?;
    }

    let path = installer.module(&PATH_DOC)?;
    let functions = [
        (
            "join",
            lua.create_function(|_, parts: Variadic<String>| {
                if parts.is_empty() {
                    return Err(
                        Error::new("bad_argument", "path.join needs at least one part").into(),
                    );
                }
                let joined: PathBuf = parts.iter().collect();
                Ok(text(&joined))
            })?,
        ),
        (
            "base",
            lua.create_function(|_, p: String| {
                Ok(Path::new(&p)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default())
            })?,
        ),
        (
            "dir",
            lua.create_function(|_, p: String| {
                Ok(match Path::new(&p).parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => text(parent),
                    _ => ".".to_owned(),
                })
            })?,
        ),
        (
            "ext",
            lua.create_function(|_, p: String| {
                Ok(Path::new(&p)
                    .extension()
                    .map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_default())
            })?,
        ),
    ];
    for (name, f) in functions {
        installer.function(&PATH_DOC, Some(&path), name, f)?;
    }
    Ok(())
}
