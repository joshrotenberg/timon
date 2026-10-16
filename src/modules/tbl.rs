//! `tbl`: table helpers. Written in Lua (`tbl.lua`), because they mostly call Lua functions.

use mlua::{Function, Table};

use crate::docs::{Func, Module};
use crate::modules::{Installer, fail_function};
use crate::runtime::INTERNAL_CHUNK;

const SOURCE: &str = include_str!("tbl.lua");

pub const DOC: Module = Module {
    name: "tbl",
    summary: "Table helpers. Results that are lists are arrays; callbacks get (value, index).",
    functions: &[
        Func::new(
            "map",
            "tbl.map(t, f) -> array",
            "Calls f(v, i) for each element and collects the results, skipping nils.",
            "tbl.map({ 1, 2, 3 }, function(n) return n * n end)  --> { 1, 4, 9 }",
        ),
        Func::new(
            "filter",
            "tbl.filter(t, f) -> array",
            "The elements for which f(v, i) is truthy.",
            "tbl.filter(prs, function(pr) return pr.draft == false end)",
        ),
        Func::new(
            "reduce",
            "tbl.reduce(t, f, init) -> value",
            "Folds left with f(acc, v, i). Without init, starts from the first element.",
            "tbl.reduce({ 1, 2, 3 }, function(sum, n) return sum + n end, 0)  --> 6",
        ),
        Func::new(
            "find",
            "tbl.find(t, f) -> v, i",
            "The first element for which f(v, i) is truthy, and its index; nil when none is.",
            "local pr = tbl.find(prs, function(p) return p.number == 12 end)",
        ),
        Func::new(
            "keys",
            "tbl.keys(t) -> array",
            "The keys, sorted (numbers before strings).",
            "tbl.keys({ b = 1, a = 2 })  --> { \"a\", \"b\" }",
        ),
        Func::new(
            "values",
            "tbl.values(t) -> array",
            "The values, in the order of tbl.keys.",
            "tbl.values({ b = 1, a = 2 })  --> { 2, 1 }",
        ),
        Func::new(
            "group_by",
            "tbl.group_by(t, f) -> table",
            "Groups elements by f(v, i); each group is an array. Elements whose key is nil are dropped.",
            "tbl.group_by(findings, function(f) return f.severity end).high",
        ),
        Func::new(
            "sort_by",
            "tbl.sort_by(t, f) -> array",
            "A new array sorted by f(v, i), stable for equal keys. t is not changed.",
            "tbl.sort_by(prs, function(p) return p.updated_at end)",
        ),
        Func::new(
            "count",
            "tbl.count(t, f) -> integer",
            "How many elements satisfy f(v, i); without f, how many entries t has.",
            "tbl.count(reviews, function(r) return r.status == \"ok\" end)",
        ),
        Func::new(
            "merge",
            "tbl.merge(...) -> table",
            "A new table with the entries of each argument; later ones win. nil arguments are skipped.",
            "tbl.merge(defaults, { model = \"opus\" })",
        ),
        Func::new(
            "pick",
            "tbl.pick(t, keys) -> table",
            "A new table with only the given keys.",
            "tbl.pick(pr, { \"number\", \"title\" })",
        ),
        Func::new(
            "omit",
            "tbl.omit(t, keys) -> table",
            "A new table without the given keys.",
            "tbl.omit(run, { \"events\" })",
        ),
    ],
};

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let json: Table = lua.globals().raw_get("json")?;
    let array: Function = json.raw_get("array")?;
    let functions: Table = lua
        .load(SOURCE)
        .set_name(INTERNAL_CHUNK)
        .call((array, fail_function(lua)?))?;
    let module = installer.module(&DOC)?;
    for func in DOC.functions {
        let f: Function = functions.raw_get(func.name)?;
        installer.lua_function(&DOC, Some(&module), func.name, f)?;
    }
    Ok(())
}
