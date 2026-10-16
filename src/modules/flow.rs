//! `flow`: workflow patterns over the runner, written in Lua (`flow.lua`).
//!
//! Rust keeps processes, cancellation, capacity, budgets and records; the patterns keep
//! sequence, rounds, quorum and verification. A pattern returns a record and never raises for
//! an outcome.

use std::time::Instant;

use mlua::{Function, Table};

use crate::docs::{Func, Module, Opt};
use crate::modules::{Installer, fail_function};
use crate::options;
use crate::runtime::INTERNAL_CHUNK;

const SOURCE: &str = include_str!("flow.lua");

const GATE: &str = "a gate: a function(value) returning true or false, { passed, reason }, or an agent record with a yes_no or review answer; a command string or list, which passes on exit 0; or an agent spec { prompt, ... }, which defaults to the yes_no contract";

pub const DOC: Module = Module {
    name: "flow",
    summary: "Workflow patterns over agent runs, commands and Lua. A pattern returns a record and never raises for an outcome: status says whether it ran to the end (ok, failed, timeout, invalid), and what it concluded is a separate field (winner, accepted, kept). Every record has pattern, runs (the agent records its steps returned), usage { runs, usd, unknown_cost } and ms. A step returns an agent record, a pattern's record, or a plain value.",
    functions: &[
        Func::new(
            "check",
            "flow.check(gate, value) -> { status, passed, reason }",
            "Runs one gate on a value: a Lua predicate, a command (passes on exit 0) or an agent spec (yes_no by default). status says whether the check ran; passed says what it concluded, and is nil when it did not run.",
            "local tests = flow.check({ \"cargo\", \"test\", \"--quiet\" })\nlocal short = flow.check(function(s) return #s < 80, \"at most 80 characters\" end, \"a summary\")\nreturn tests.passed, short.passed",
        ),
        Func::new(
            "pipeline",
            "flow.pipeline(items, stages, { limit }) -> { status, values, items }",
            "Runs each item through the stages, a function(value, item, i) or { name, run } each, with no barrier between items. An item stops at its first stage that is not ok; status is ok when every item finished.",
            "local r = flow.pipeline({ \"a.rs\", \"b.rs\" }, {\n  { name = \"read\", run = function(path) return \"fn main() {}\" end },\n  { name = \"measure\", run = function(text) return #text end },\n})\nreturn r.values[1]  -- 12",
        )
        .with_options(&[Opt::new("limit", "integer", "most items at once (default: all)")]),
        Func::new(
            "vote",
            "flow.vote{ run, n, quorum, key, limit } -> { status, winner, value, agreed, tally }",
            "Runs n samples, run(i) each, and counts them by key(value) (default: a yes_no answer, or the value). Stops as soon as one key reaches quorum (default: a majority of n). winner is the quorum's key, or the plurality when none reached it.",
            "local answers = { \"yes\", \"no\", \"yes\" }\nlocal r = flow.vote{ n = 3, run = function(i) return answers[i] end }\nreturn r.winner, r.agreed  -- yes, true",
        )
        .with_options(&[
            Opt::required("run", "function", "run(i) returns one sample"),
            Opt::new("n", "integer", "how many samples (default 3)"),
            Opt::new("quorum", "integer", "how many agreeing samples end the vote (default: a majority)"),
            Opt::new("key", "function", "key(value) is what samples agree on"),
            Opt::new("limit", "integer", "most samples at once (default: n)"),
        ]),
        Func::new(
            "best_of",
            "flow.best_of{ run, n, score | judge, limit } -> { status, winner, value, scores, candidates }",
            "Runs n candidates, run(i) each, and picks one: score(value, i) returns a number and the highest wins, or judge(values) returns the position of the best (it may run an agent).",
            "local r = flow.best_of{ n = 3, run = function(i) return (\"x\"):rep(i) end, score = function(s) return -#s end }\nreturn r.value  -- x",
        )
        .with_options(&[
            Opt::required("run", "function", "run(i) returns one candidate"),
            Opt::new("n", "integer", "how many candidates (default 3)"),
            Opt::new("score", "function", "score(value, i) returns a number; the highest wins"),
            Opt::new("judge", "function", "judge(values) returns the position of the best, or { index }"),
            Opt::new("limit", "integer", "most candidates at once (default: all)"),
        ]),
        Func::new(
            "refine",
            "flow.refine{ draft, check, revise, rounds } -> { status, value, accepted, rounds, verdicts }",
            "Drafts, checks the draft with a gate, and revises until it passes, at most rounds checks (default 3). revise(value, verdict, round, last) gets the failed verdict and the last step's agent record, for agent.continue. accepted says whether the last check passed.",
            "local r = flow.refine{\n  draft = function() return \"a long first line that runs on\" end,\n  check = function(s) return #s <= 12 end,\n  revise = function(s) return s:sub(1, 12) end,\n}\nreturn r.accepted, r.value  -- true, a long first",
        )
        .with_options(&[
            Opt::required("draft", "function", "draft() returns the first version"),
            Opt::required("check", "any", GATE),
            Opt::required("revise", "function", "revise(value, verdict, round, last) returns the next version"),
            Opt::new("rounds", "integer", "most checks (default 3)"),
        ]),
        Func::new(
            "verify",
            "flow.verify{ items, check | checks, keep, limit } -> { status, kept, rejected, items }",
            "Checks every item with each gate and keeps or rejects it: majority (default) keeps an item unless most gates reject it, all keeps one every gate passes, any keeps one at least one passes. A gate that did not run neither keeps nor rejects.",
            "local findings = { { line = 3, text = \"off by one\" }, { line = 0, text = \"style\" } }\nlocal r = flow.verify{ items = findings, check = function(f) return f.line > 0 end }\nreturn #r.kept, #r.rejected  -- 1, 1",
        )
        .with_options(&[
            Opt::required("items", "any[]", "the items to check"),
            Opt::new("check", "any", GATE),
            Opt::new("checks", "any[]", "several gates"),
            Opt::new("keep", "\"majority\" | \"all\" | \"any\"", "which items to keep (default majority)"),
            Opt::new("limit", "integer", "most items at once (default: all)"),
        ]),
        Func::new(
            "retry",
            "flow.retry(fn, { times, on }) -> { status, value, attempts }",
            "Calls fn(attempt, last) again while its outcome's status is in on (default failed, timeout and invalid), at most times more (default 2). last is the previous outcome, with error.data for an invalid answer to repair from.",
            "local r = flow.retry(function(attempt) if attempt < 2 then error(\"flaky\") end return \"done\" end)\nreturn r.value, r.attempts  -- done, 2",
        )
        .with_options(&[
            Opt::new("times", "integer", "most retries (default 2)"),
            Opt::new("on", "(\"failed\" | \"timeout\" | \"invalid\" | \"cancelled\")[]", "the statuses that retry"),
        ]),
        Func::new(
            "plan",
            "flow.plan{ steps, limit } -> { status, value, steps, order, stopped_at, skipped }",
            "Runs named steps, { run = function(deps), needs = { ... } } each, as soon as the steps they need succeed; deps holds those steps' values. The first step that is not ok stops new ones; the rest are skipped.",
            "local r = flow.plan{ steps = {\n  plan = { run = function() return { \"a\", \"b\" } end },\n  build = { needs = { \"plan\" }, run = function(deps) return #deps.plan end },\n} }\nreturn r.value.build  -- 2",
        )
        .with_options(&[
            Opt::required("steps", "table<string, { run: function, needs: string[]? }>", "the steps by name"),
            Opt::new("limit", "integer", "most steps at once (default: all)"),
        ]),
    ],
};

pub(crate) fn install(installer: &Installer) -> mlua::Result<()> {
    let lua = installer.lua();
    let globals = lua.globals();
    let scope: Function = globals.raw_get("scope")?;
    let try_fn: Function = globals.raw_get("try")?;
    let json: Table = globals.raw_get("json")?;
    let array: Function = json.raw_get("array")?;
    let par: Table = globals.raw_get("par")?;
    let first: Function = par.raw_get("first")?;
    let check = lua.create_function(|_, (path, opts): (String, Option<Table>)| {
        options::check_opt(&path, opts.as_ref())?;
        Ok(())
    })?;
    let started = Instant::now();
    let now = lua.create_function(move |_, ()| Ok(started.elapsed().as_millis() as i64))?;
    let functions: Table = lua.load(SOURCE).set_name(INTERNAL_CHUNK).call((
        scope,
        try_fn,
        array,
        installer.wrap(check)?,
        first,
        fail_function(lua)?,
        now,
    ))?;
    let flow = installer.module(&DOC)?;
    for func in DOC.functions {
        let f: Function = functions.raw_get(func.name)?;
        installer.lua_function(&DOC, Some(&flow), func.name, f)?;
    }
    Ok(())
}
