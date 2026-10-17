---
name: timon
description: Use timon when a task is easier as a few lines of code than as many tool calls - reading and reshaping files and JSON, querying with JMESPath, running programs and concurrent work, or scripting in Lua or Fennel - and to run a project's tasks (timon tasks, timon NAME). Run `timon eval 'CODE'` from the shell; `help()` documents every module.
---

# timon

One binary: Lua 5.4 and Fennel over a set of modules. Run code from the shell:

Run `timon` as a command of its own. A host that allows only `timon` commands denies the whole
line when it has `;`, `&&`, `|`, `$?`, a redirect or a heredoc around timon. Pass multi-line Lua
as one single-quoted argument, which may span lines and cannot contain `'`: use `"` for Lua
strings, and put code whose text needs an apostrophe in a file run with `timon eval -f FILE`.
Read files with `fs.read` in Lua instead of piping them in, and skip `$?`: a failure prints
`timon: kind: message` and exits nonzero.

```sh
timon eval '1 + 1'                 # 2: an expression returns its value
timon eval '{ a = 1, b = { 2 } }'  # tables print as JSON: {"a":1,"b":[2]}
timon eval -f job.lua              # run a Lua file with no words; read data with fs.read
timon job.lua 12 14                # a script; words in `arg` and `...`; eval -f takes none
timon tasks                        # the tasks in reach: tasks.lua and scripts/
timon check                        # run a task, after its dependencies
timon serve                        # the tasks as MCP tools over stdio, for hosts without a shell
timon eval --fnl '(+ 1 2)'         # Fennel
```

In a shell of your own, where pipes are allowed, timon also reads stdin:

```sh
echo 'return 6 * 7' | timon eval -                             # code from stdin
git log --oneline | timon eval -i 'return #input'              # input: the lines of stdin
cat events.jsonl | timon eval -I --stream 'return input.kind'  # once per JSON value
```

Strings print raw, tables print as JSON, and logs go to stderr, so stdout is data you can pipe.

## Look it up

```sh
timon eval 'help()'                 # the modules
timon eval 'help(json)'             # one module
timon eval 'help(par.map)'          # one function: signature, options, example
timon skill show timon --ref api    # every function with an example
```

| Module | For |
|---|---|
| `json` | `encode`, `decode`, `query` (JMESPath with jpx's functions), `null`, `array`, `object` |
| `str` | `split`, `trim`, `lines`, `starts`, `ends`, `fmt`; also string methods: `s:trim()` |
| `tbl` | `map`, `filter`, `reduce`, `find`, `keys`, `values`, `group_by`, `sort_by`, `count`, `merge`, `pick`, `omit` |
| `fs`, `path` | files, directories, path names |
| `git` | `status`, `log`, `diff`, `tags`, `branch`, `rev`, `add`, `commit`, `tag`, `push`, worktrees, `run`: git's output as data |
| `http`, `gh` | `http.get`, `post`, `request`: responses as data; `gh.repo`, `prs`, `pr`, `issues`, `checks`, `api`, `graphql` with GH_TOKEN or gh's login |
| `mcp` | `connect` to an MCP server: `c:tools()`, `c:call(tool, args)`, `c:resources()`, `c:read(uri)`; `servers()` |
| `flow` | workflow patterns: `check` (a gate), `verify`, `vote`, `best_of`, `refine`, `pipeline`, `retry`, `plan`; records with status and conclusion apart |
| `sqlite` | `sqlite.open(path)`: `db:exec`, `db:query` (rows keyed by column), `db:one`, `db:transaction` |
| `toml`, `yaml`, `csv` | `decode`, `encode`; `toml.set` and `toml.edit` change one value and keep the rest of the file |
| `markdown`, `term` | `headings` and `code` blocks with line numbers; `tty`, `color`, `width` |
| `sh`, `proc` | run programs: `sh` streams and raises on failure, `sh.out` captures, `proc.run` returns a record, `proc.spawn` runs in the background |
| `cli` | `parse` a script's flags against typed options, as tasks declare them; `help` |
| `task` | `run` another task from inside a task |
| `agent`, `schema` | Claude and Codex runs with answers checked against contracts (the `timon-agents` skill) |
| `sessions` | every Claude Code and Codex session: list, search, read, summarize (the `timon-sessions` skill) |
| `hook` | `input()`: the host event a hook task runs for (the `timon-tasks` skill) |
| `par` | `map`, `all`, `race`: concurrent work |
| `log`, `pp` | messages and pretty-printing on stderr |
| globals | `help`, `try`, `scope`, `spawn`, `phase`, `sleep` |

## Examples

Reshape JSON:

```lua
local prs = json.decode([[
  [{"number": 12, "title": "fix: parser", "draft": false, "labels": ["bug"]},
   {"number": 14, "title": "feat: cache", "draft": true, "labels": []}]
]])
local ready = tbl.filter(prs, function(pr) return not pr.draft end)
return tbl.map(ready, function(pr) return str.fmt("#${n} ${t}", { n = pr.number, t = pr.title }) end)
```

Keep identifiers exact: an integer outside signed 64-bit range decodes to a float, and the lost
bits do not come back, so ask for identifiers as strings:

```lua
local lossy = json.decode('[9223372036854775808, 9223372036854775809]')
assert(lossy[1] == lossy[2])  -- two ids, one float
local exact = json.decode('{"id": "9223372036854775809"}')
return exact.id, json.encode(exact)  -- 9223372036854775809, {"id":"9223372036854775809"}
```

Query with JMESPath:

```lua
local data = json.decode('{"items": [{"name": "b", "ok": true}, {"name": "a", "ok": true}, {"name": "c", "ok": false}]}')
return json.query(data, "items[?ok].name | sort(@) | join(', ', @)")  -- "a, b"
```

Files:

```lua
local dir = fs.tmpdir()
fs.write(path.join(dir, "notes", "todo.md"), "- one\n- two\n", { mkdir = true })
local counts = {}
for _, file in ipairs(fs.glob(path.join(dir, "**", "*.md"))) do
  counts[path.base(file)] = #str.lines(fs.read(file))
end
return counts  -- {"todo.md":2}
```

Bump a version and read a changelog, keeping the file's comments:

```lua
local cargo = '[package]\nname = "app"\nversion = "0.1.0" # bumped by release\n'
local bumped, old = toml.set(cargo, "package.version", "0.2.0")
local changelog = "# Changelog\n\n## Unreleased\n\n- feat: tasks\n"
local next = markdown.headings(changelog)[2]
return old, bumped:match("version = [^\n]*"), next.text, next.line
-- 0.1.0, version = "0.2.0" # bumped by release, Unreleased, 3
```

`-i` binds the lines of stdin to `input`, `-I` its JSON values (one document or JSON lines),
`-o` prints a returned list one element per line, `-O` as JSON lines, and `--stream` runs the
code once per line or value without reading all of stdin first.

A database, in memory or in a file:

```lua
local db = sqlite.open()
db:exec("create table runs (id text primary key, usd real)")
db:transaction(function()
  for i, usd in ipairs({ 0.01, 0.03 }) do
    db:exec("insert into runs values (:id, :usd)", { id = "r" .. i, usd = usd })
  end
end)
return db:one("select count(*) as n, sum(usd) as total from runs").total  -- 0.04
```

Run programs:

```lua
sh("echo building")                         -- streams to the terminal; a nonzero exit raises
local who = sh.out{ "printf", "%s", "timon" }   -- captured and trimmed
local r = proc.run({ "sh", "-c", "echo oops >&2; exit 3" })   -- a record; never raises for the code
return who, r.code, r.err                    -- timon, 3, "oops\n"
```

A command string is split into words without a shell. Pipes, redirects, globs and `$VAR` need
`shell = true`; a string that uses them without it raises and says so. A failed `sh` ends a
script with the program's exit code. Every process stops if its task is cancelled or times out
(`timeout = "10m"`), children included.

Concurrent work, at most 4 at a time, results in input order:

```lua
local sizes = par.map({ "a", "bb", "ccc" }, function(s)
  sleep("10ms")
  return #s
end, { limit = 4 })
return sizes  -- [1,2,3]
```

Errors as data:

```lua
local ok, err = try(fs.read, "/no/such/file")
if not ok then
  return err.kind, err.message  -- not_found, cannot read /no/such/file: ...
end
```

## Keep what proves useful

While you work, notice the parts worth running again: a check before every release, a reshaping
you repeat, a delegation you keep phrasing the same way. Finish the job, then keep the smallest
thing that will be used again. Worked examples are in the tasks, agents and workflows skills:

| You did | Keep | Read |
|---|---|---|
| a command or check worth rerunning | a task with a description and typed options | `timon skill show timon-tasks`, From work to a task |
| several commands or data transformations | a named script; `lib/` only for code two scripts share | `timon skill show timon-scripting`, Named scripts and layers (no worked example) |
| the same bounded delegation, again | a named agent: instructions, access, answer contract, budget | `timon skill show timon-agents`, From delegation to a role |
| a repeated sequence of agent work and checks | a script over `flow` with its success criteria and a readable result | `timon skill show timon-workflows`, From a sequence to a workflow |

- Look first: `timon tasks --all`, `timon agents`, `timon defs`. Reuse or extend what is there
  rather than add a near duplicate.
- Keep what belongs to this job (the directory, the issue, the file) at the call site, as an
  option or in the prompt.
- Project knowledge goes in the project's `.timon/`; your own habits in `~/.config/timon/`.
- A one-off stays a one-off unless it will likely be used again. No count of repeats or quota
  of new definitions makes it worth keeping.
- Run what you kept on the input you just used, check it gives the same answer, then use it.
  Say what you added and what you actually ran.
- A project's `.timon/` loads only once a person has run `timon trust`. In a project that is
  not trusted, check the logic with `timon eval` on the same input, and say the definition is
  saved but has not run as a task; trusting it is the person's decision.
- Keeping a definition changes no policy: do not run `timon trust`, widen access, or move
  delegation off the runner the workspace requires to show a definition off.

## Rules

- Run `timon` as a command of its own, not inside a longer shell line with `;`, `&&`, `|`,
  `$?`, redirects or heredocs: a host that allows only `timon` commands denies the whole line.
  Do the rest in Lua.
- An error is a table with `kind`, `message` and often `hint`. Uncaught, it prints all three and
  exits 1, or the error's `exit` (`usage` exits 2, a failed `sh` with the program's code). Read
  the hint: it usually names the fix.
- Globals, module fields and options are strict: a typo raises with "did you mean".
- `try(f, ...)` returns `true, ...` or `false, err`.
- Lists from modules are arrays; `json.null` is JSON null; an empty `json.array{}` encodes as `[]`.
- `scope`, `spawn` and `par` run work concurrently; a failure cancels the rest and is raised.
- Define tasks with the `timon-tasks` skill, write scripts and reusable code with
  `timon-scripting`, run subagents with `timon-agents`, combine and check their work with
  `timon-workflows`, and find earlier work with `timon-sessions`.
