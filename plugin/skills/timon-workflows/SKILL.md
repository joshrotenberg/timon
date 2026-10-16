---
name: timon-workflows
description: Use when a task needs more than one agent run or a check on an agent's work - verify findings, vote, pick the best of several, revise until a gate passes, retry, or run steps in dependency order with timon's flow patterns. Load timon-agents first for agent.run.
---

# Workflows with flow

Run `timon` as a command of its own. A host that allows only `timon` commands denies the whole
line when it has `;`, `&&`, `|`, `$?`, a redirect or a heredoc around timon. Pass multi-line Lua
as one single-quoted argument, which may span lines and cannot contain `'`: use `"` for Lua
strings, and put code whose text needs an apostrophe in a file run with `timon eval -f FILE`.
Read files with `fs.read` in Lua instead of piping them in, and skip `$?`: a failure prints
`timon: kind: message` and exits nonzero.

`flow` holds patterns over agent runs, commands and plain Lua. A pattern returns a record and
never raises for an outcome:

- `status` says whether the pattern ran to the end: `ok`, `failed`, `timeout`, `invalid`.
- What it concluded is a separate field: `winner`, `accepted`, `kept`. A review that requests
  changes, a run that failed and an answer that broke its contract stay three different things.
- `runs` holds every agent record the steps returned, and `usage` is `{ runs, usd,
  unknown_cost }`; Codex reports no cost, so its runs count as unknown, not as $0.

A step returns an agent record (its `answer`, or its `text`, becomes the value), another
pattern's record, or a plain value. Wrap a pattern in `scope(fn, { budget = { runs = N } })`
to cap what it spends.

## Gates

A gate checks a value. Prefer a deterministic gate when one exists; ask a model when only
judgment will do.

```lua
local tests = flow.check({ "true" })                                -- a command: passes on exit 0
local short = flow.check(function(s) return #s <= 72, "at most 72 characters" end, "fix: a subject")
local real = flow.check({ prompt = "Is this finding real?" }, { file = "a.rs", line = 3 })  -- yes_no
return tests.passed, short.passed, real.status
```

A gate function returns `true` or `false` (and a reason), `{ passed, reason }`, or an agent
record with a `yes_no` or `review` answer. `passed` is nil when the check did not run.

## Verify, then keep what holds

```lua
local findings = { { title = "off by one", line = 3 }, { title = "style nit", line = 0 } }
local r = flow.verify{
  items = findings,
  checks = {
    function(f) return f.line > 0, "points at a line" end,
    { prompt = "Is this finding a real bug? Answer no for style." },
  },
  keep = "majority",            -- or "all", "any"
}
return #r.kept, r.usage.runs
```

## Vote

```lua
local r = flow.vote{
  n = 3,
  run = function(i) return agent.run{ prompt = "Is the release ready? Voter " .. i, output = "yes_no" } end,
}
return r.winner, r.agreed, r.tally
```

The vote stops as soon as one key reaches `quorum` (default: a majority of `n`). `key` decides
what counts as the same answer; the default reads a `yes_no` answer.

## Best of n

```lua
local r = flow.best_of{
  n = 3,
  run = function(i) return ("candidate %d: "):format(i) .. ("x"):rep(i) end,
  score = function(text) return -#text end,          -- deterministic: the shortest wins
}
return r.winner, r.value
```

`judge = function(values) return index end` picks instead of `score`; it may run an agent.

## Refine until a gate passes

```lua
local r = flow.refine{
  draft = function() return "feat: add the thing that does the stuff and more besides that" end,
  check = function(subject) return #subject <= 50, "subjects are at most 50 characters" end,
  revise = function(subject, verdict, round) return subject:sub(1, 50 - round) end,
  rounds = 3,
}
return r.accepted, r.value
```

`revise(value, verdict, round, last)` gets the failed verdict and the last step's agent record,
so an agent can revise in its own session with `agent.continue(last, ...)`.

## Pipeline, retry and plan

```lua
local p = flow.pipeline({ "src/a.rs", "src/b.rs" }, {
  { name = "size", run = function(path) return #path end },
  { name = "double", run = function(n) return n * 2 end },
})

local attempts = 0
local r = flow.retry(function(attempt, last)
  attempts = attempt
  if attempt == 1 then error("flaky", 0) end
  return "done after " .. last.error.message
end, { times = 2 })

local plan = flow.plan{ steps = {
  find = { run = function() return { "a", "b" } end },
  fix = { needs = { "find" }, run = function(deps) return #deps.find end },
} }
return p.values[2], r.value, plan.value.fix
```

- `pipeline` runs every item through the stages with no barrier; an item stops at its first
  stage that is not ok.
- `retry` calls again on `failed`, `timeout` or `invalid`; `last.error.data` holds an invalid
  answer to repair from.
- `plan` starts each step when the steps it `needs` succeed; the first failure stops new steps.

## As tasks

A workflow is a task or a script. The review a team runs every day:

```lua fragment
-- .timon/tasks.lua
review = {
  description = "Review the branch, then keep the findings both verifiers think are real",
  budget = { runs = 8, usd = 2 },
  run = function()
    local review = agent.run{ agent = "reviewer", prompt = "Review the diff against main", output = "review" }
    if review.status ~= "ok" then return review end
    local r = flow.verify{ items = review.answer.findings, keep = "all", checks = {
      { prompt = "Is this finding real? Read the code it names." },
      function(f) return git.diff{ paths = { f.file } } ~= "", "the file changed on this branch" end,
    } }
    return { kept = r.kept, dropped = #r.rejected, usd = r.usage.usd + review.usage.usd }
  end,
},
```
