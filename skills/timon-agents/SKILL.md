---
name: timon-agents
description: Use when Claude or Codex subagents should do part of the work from timon - one run with a validated answer, fan-out over many items, author and reviewer loops, isolated git worktrees, budgets, and reading past runs. Runs spend tokens. Load the timon skill first for the basics.
---

# Agents with timon

`agent.run` runs one non-interactive turn of Claude Code (default) or Codex and returns a
record. It never raises for an outcome: check `status`. Runs spend tokens.

```lua
local r = agent.run{ prompt = "Summarize what this repository does in three bullets" }
if r.status ~= "ok" then error(r.error) end  -- error has kind, message, hint
return r.text
```

The record: `id`, `status` (`ok`, `failed`, `invalid`, `timeout`, `cancelled`), `answer`,
`text`, `error`, `provider`, `model`, `session`, `usage` (`input_tokens`, `output_tokens`,
`usd` for Claude), `ms`, `started_at`, and `worktree` when one was used.

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

`agent.continue(record, prompt, spec)` resumes the run's session. `fork = SESSION` branches
from any session instead (Claude only).

## Isolate writes in a worktree

`worktree = true` runs in a new git worktree on branch `timon/<run id>`. It is removed when
the run changed nothing and kept, with its path in `record.worktree`, when it did:

```lua fragment
local r = agent.run{ access = "write", worktree = true, prompt = "Fix the failing test" }
if r.worktree.kept then print("changes in " .. r.worktree.path .. " on " .. r.worktree.branch) end
```

## Access

| access | Claude | Codex |
|---|---|---|
| `read` (default) | Read, Glob, Grep | read-only sandbox |
| `write` | adds Edit, Write, NotebookEdit | workspace-write sandbox |
| `full` | no permission checks | no sandbox |

`full` needs `limits = { allow_full = true }` in `init.lua`. `tools` replaces Claude's list.

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

## Past runs

Every run is recorded, with secrets redacted:

```lua
local recent = agent.runs{ since = "1d", limit = 5 }
return tbl.map(recent, function(r) return r.status .. " " .. r.prompt end)
```

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
