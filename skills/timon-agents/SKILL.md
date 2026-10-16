---
name: timon-agents
description: Use when Claude or Codex subagents should do part of the work from timon - one run with a validated answer, fan-out over many items, author and reviewer loops, isolated git worktrees, budgets, and reading past runs. Runs spend tokens. Load the timon skill first for the basics.
---

# Agents with timon

Run `timon` as a command of its own. A host that allows only `timon` commands denies the whole
line when it has `;`, `&&`, `|`, `$?`, a redirect or a heredoc around timon. Pass multi-line Lua
as one single-quoted argument, which may span lines; read files with `fs.read` in Lua instead
of piping them in; and skip `$?`: a failure prints `timon: kind: message` and exits nonzero.

`agent.run` runs one non-interactive turn of Claude Code (default) or Codex and returns a
record. It never raises for an outcome: check `status`. Runs spend tokens.

```lua
local r = agent.run{ prompt = "Summarize what this repository does in three bullets" }
if r.status ~= "ok" then error(r.error) end  -- error has kind, message, hint
return r.text
```

The record: `id`, `status` (`ok`, `failed`, `invalid`, `timeout`, `cancelled`), `answer`,
`text`, `error`, `provider`, `model`, `session`, `usage` (`input_tokens`, `output_tokens`,
`usd` for Claude), `ms`, `started_at`, `label`, `phase`, `worktree` when one was used, and
`events` with `events = true`.

## Ask for a shape, get it validated

`output` names a contract or takes a schema. The provider is told the shape and timon checks
the answer again; a mismatch is `status = "invalid"` with the problems in `error.data`.

```lua
local reply = agent.ask{
  prompt = "Does `fn first(xs: &[i32]) -> i32 { xs[0] }` handle an empty slice?",
  output = "yes_no",
}
return reply.answer, reply.reason
```

`agent.ask` returns the answer (or the text, without `output`) and raises `agent_failed` with
the record as `error.data` when the run is not ok.

Built-in contracts: `review`, `implementation`, `investigation`, `yes_no`, `list`. Inline
schemas work too:

```lua
local Todo = schema.object{
  file = schema.string,
  line = schema.optional(schema.int),
  text = schema.string,
}
local r = agent.run{ prompt = "List the TODO comments in src/", output = schema.object{ todos = schema.array(Todo) } }
return r.status, #r.answer.todos
```

## Fan out

```lua
local files = { "src/main.rs", "src/lib.rs", "src/error.rs" }
local reviews = par.map(files, function(file)
  return agent.run{ prompt = "Review " .. file .. " for error handling", output = "review" }
end, { limit = 3 })
local requested = tbl.filter(reviews, function(r)
  return r.status == "ok" and r.answer.verdict == "request_changes"
end)
return #reviews, #requested
```

`limits.concurrency` in `init.lua` caps runs at once across the whole script (default 4).

Take results as they finish with `par.first`, which cancels nothing:

```lua
local tasks = tbl.map({ "a", "b", "c" }, function(x)
  return agent.spawn{ prompt = "Reply with the letter " .. x, label = x }
end)
local order = {}
while #tasks > 0 do
  local r = table.remove(tasks, par.first(tasks)):await()
  order[#order + 1] = r.label
end
return #order
```

## Phases and progress

`phase(name)` names what the current task is doing. Runs started in it, and in tasks it
spawns, record the phase; `label` names one run:

```lua
phase("review")
local reviews = par.map({ "src/a.rs", "src/b.rs" }, function(file)
  return agent.run{ prompt = "Review " .. file, label = file, output = "review" }
end, { limit = 2 })
phase("summary")
local summary = agent.run{ prompt = "Summarize: " .. json.encode(reviews), label = "summary" }
return summary.phase  -- "summary"
```

```sh
timon review.lua --progress text    # a line per run start and end, on stderr
timon review.lua --progress json    # the same as JSON lines, for an agent to read
```

Progress is on by default only when stderr is a terminal.

## Author, review, follow up

One provider writes, the other reviews read-only, findings go back to the author:

```lua
local change = agent.run{
  provider = "codex", access = "write",
  prompt = "Add a --verbose flag to the CLI", output = "implementation",
}
local review = agent.run{ prompt = "Review the uncommitted changes", output = "review" }
if review.status == "ok" and review.answer.verdict == "request_changes" then
  change = agent.continue(change, str.fmt("Address these findings:\n${f}", { f = review.answer.findings }))
end
return change.status, review.answer.verdict
```

`agent.continue(record, prompt, spec)` resumes the run's session with the earlier call's
model, access, output, tools, system prompt and directory (its kept worktree, if any); `spec`
overrides them. `fork = SESSION` branches from any session instead (Claude only).

## Isolate writes in a worktree

`worktree = true` runs in a new git worktree on branch `timon/<run id>`. It is removed when
the run changed nothing and kept, with its path in `record.worktree`, when it did:

```lua fragment
local r = agent.run{ access = "write", worktree = true, prompt = "Fix the failing test" }
if r.worktree.kept then print("changes in " .. r.worktree.path .. " on " .. r.worktree.branch) end
```

## See what the agent did

`events = true` lists the agent's text, reasoning, tool calls and tool results in
`record.events`, in the same shape for Claude and Codex. A failed run keeps the events it got
to, which shows where it went wrong:

```lua
local r = agent.run{ prompt = "Find where the config file is loaded", events = true }
for _, e in ipairs(r.events) do
  if e.kind == "tool_call" then print(e.name, json.encode(e.input)) end
  if e.kind == "tool_result" and e.error then print("  failed: " .. e.text) end
end
local calls = tbl.filter(r.events, function(e) return e.kind == "tool_call" end)
return r.status, #calls
```

Events: `{ kind = "text" | "reasoning", text }`, `{ kind = "tool_call", id, name, input }`,
`{ kind = "tool_result", id, error, text }`. Codex shell commands are `shell` calls. Long text
is cut at 2,000 characters.

## Access

| access | Claude | Codex |
|---|---|---|
| `read` (default) | Read, Glob, Grep | read-only sandbox |
| `write` | adds Edit, Write, NotebookEdit | workspace-write sandbox |
| `full` | no permission checks | no sandbox |

`full` needs `limits = { allow_full = true }` in `init.lua`. `tools` replaces Claude's list;
`tools = {}` allows none. `ephemeral = true` leaves no session behind to resume or list.

## Named agents

```lua
-- ~/.config/timon/agents/reviewer.lua
return {
  description = "Reviews one change against its issue",
  model = "opus", effort = "high", access = "read",
  system = "Report findings only. Do not edit files.",
  output = "review",
  budget = { usd = 2, turns = 20 },
}
```

```lua fragment
local r = agent.run{ agent = "reviewer", prompt = "Review PR 12" }
```

Fields merge: defaults < `init.lua` `defaults` < the definition (`extends = "NAME"` builds on
another) < the call. `timon agents` lists them.

## Budgets

```sh
timon review.lua --max-runs 10 --max-usd 5
```

Or `limits = { max_runs = 10, max_usd = 5 }` in `init.lua`. A run that would pass a limit
raises `budget_exceeded` before it starts. Codex reports no cost, so only runs count for it.
Per run: `budget = { usd = 1, turns = 15 }` (Claude).

A scope can have its own budget, on top of every enclosing one. Runs in the scope and its
tasks count against all of them, and no Claude run may spend more than the least any of them
has left:

```lua
local ok, err = try(scope, function()
  for i = 1, 3 do agent.run{ prompt = "Reply with OK" } end
end, { budget = { runs = 2 } })
return err.kind  -- budget_exceeded, at the third run
```

`reserve = true` takes the scope's runs from the enclosing budget up front, so a fan-out that
cannot finish fails before its first run. Unused runs go back when the scope ends:

```lua
scope(function(s)
  for i = 1, 3 do s:spawn(agent.run, { prompt = "Answer in one word: what is 2 + " .. i .. "?" }) end
end, { budget = { runs = 3 }, reserve = true })
```

## Past runs

Every run is recorded, with secrets redacted, including cancelled ones. A run whose timon
process died shows as `interrupted`; `effects_uncertain = true` means it could write, so check
its working directory or worktree:

```lua
local recent = agent.runs{ since = "1d", limit = 5 }
return tbl.map(recent, function(r) return r.status .. " " .. r.prompt end)
```

In a script run, each agent run records its script as `parent`
(`agent.runs{ parent = id }`), and `idempotent = true` lets a resumed script reuse a run that
could write. The `timon-scripting` skill covers script runs and resume.

```sh
timon runs --since 1d
timon runs show r_1a2b3c       # the request and result; a prefix of the id works
timon agent run --output yes_no "Is this repository using async Rust?"
```

## Rules

- Check `r.status`, or use `agent.ask` and `try`.
- Read-only is the default. Give write access in a worktree unless the change should land in
  the working directory.
- Stage large inputs as files in `cwd` and say where they are in the prompt.
- Codex has no system prompt flag (`system` goes before the prompt) and cannot fork.
