---
name: timon
description: Use timon when a task is easier as a few lines of code than as many tool calls - reading and reshaping files and JSON, querying with JMESPath, running programs and concurrent work, or scripting in Lua or Fennel. Run `timon eval 'CODE'` from the shell; `help()` documents every module.
---

# timon

One binary: Lua 5.4 and Fennel over a set of modules. Run code from the shell:

```sh
timon eval '1 + 1'                 # 2: an expression returns its value
timon eval '{ a = 1, b = { 2 } }'  # tables print as JSON: {"a":1,"b":[2]}
timon eval -f job.lua              # run a Lua file; read data with fs.read
timon job.lua 12 14                # a script; arguments in `arg` and `...`
timon eval --fnl '(+ 1 2)'         # Fennel
echo 'return 6 * 7' | timon eval - # stdin
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
| `sh`, `proc` | run programs: `sh` streams and raises on failure, `sh.out` captures, `proc.run` returns a record, `proc.spawn` runs in the background |
| `par` | `map`, `all`, `race`: concurrent work |
| `log`, `pp` | messages and pretty-printing on stderr |
| globals | `help`, `try`, `scope`, `spawn`, `sleep` |

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

## Rules

- Run `timon` as a command of its own, not inside a longer shell line with `;`, `&&`, `$?` or
  redirects: a host that allows only `timon` commands denies the whole line. Do the rest in Lua.
- An error is a table with `kind`, `message` and often `hint`. Uncaught, it prints all three and
  exits 1. Read the hint: it usually names the fix.
- Globals, module fields and options are strict: a typo raises with "did you mean".
- `try(f, ...)` returns `true, ...` or `false, err`.
- Lists from modules are arrays; `json.null` is JSON null; an empty `json.array{}` encodes as `[]`.
- `scope`, `spawn` and `par` run work concurrently; a failure cancels the rest and is raised.
- Write scripts and reusable code with the `timon-scripting` skill, run subagents with
  `timon-agents`, and find earlier work with `timon-sessions`.
