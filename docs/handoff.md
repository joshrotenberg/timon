# Handoff

timon is paused as of 2026-10-17. Work on its ideas continues in babagent, an agent layer on
babashka, which Josh chose over timon after comparing the two. This repository stays as a
reference design: the APIs, the edge cases the tests pin down, and the issues are the record.
This page is for picking timon back up.

## Why it paused

timon was built to answer whether a self-contained runtime (Rust, with Lua 5.4 and Fennel) beats
an agent layer over a tool Josh already knows. The comparison:

- **The task runner** is babashka's model on purpose (`docs/prior-art.md`, "Decisions shaped by
  prior art"). babashka already is that runner, with packages, a REPL and EDN.
- **Agents writing Lua against Clojure.** A separate trial ran identical tasks with deterministic
  oracles in timon Lua, timon Fennel, babashka Clojure and plain Elixir, on haiku, sonnet and
  Codex. It found no meaningful difference between Lua, Clojure and Elixir. Fennel trailed only
  on haiku, on one construct (`&until`). Clojure was even with Lua or ahead on every model, so
  the language did not justify a second tool.
- **What timon adds** is the agent half: contracts, `flow`, budgets, records, hooks, sessions.
  That half can be carried as a library on babashka.

## Where it stands

- **Built:** phases 0 to 9 and host hooks (README, "Status"). That covers:
  - the runtime and toolbox
  - tasks with typed options
  - agents, with contracts, budgets, worktrees, records and `--resume`
  - sessions, with an FTS5 index
  - `flow`
  - the MCP client and `timon serve`
  - hooks, with the session-start brief and the pre-tool guard
  - the Claude Code and Codex plugin
  - an mdbook site generated from the sources (`timon book`)
- **Tested:** 380 tests in `cargo test`, never starting a real CLI. 11 live phases in
  `tests/live.rs` run against the real Claude Code and Codex CLIs, with results logged in
  `docs/live-tests.md`.
- **Last changes:**
  - Codex runs isolated from the user's global `AGENTS.md` (#172, #175)
  - `timon scripts` removed (#171)
  - "look first" in the skills (#170)
  - the book (#169)
  - failed Claude runs keeping their cost, and USD holds (#151)

## Picking it back up

1. **Install from a checkout or from main:**

   ```sh
   cargo install --locked --path .
   timon update            # or: from main, plus the Claude Code and Codex plugin updates
   ```

2. **Trust the project layer once, then run the gates before every push.** The gates are fmt,
   clippy, test, rustdoc and the plugin check:

   ```sh
   timon trust && timon check
   ```

   In a sandboxed host, set `TIMON_HOME` to a writable directory with the repository trusted
   there.
3. **Live tests** are opt-in and spend tokens. `TIMON_LIVE_MODEL=sonnet` picks the Claude host's
   model:

   ```sh
   TIMON_LIVE=1 cargo test --test live -- --ignored --test-threads 1 --nocapture live_11_
   ```

4. **The book** builds with `timon book` (needs mdbook). CI builds it on every PR. To deploy it,
   turn on GitHub Pages with GitHub Actions as the source, then set the repository variable:

   ```sh
   gh variable set PAGES_ENABLED --body true
   ```

## The issue-fixing loop, as run

Most fixes in the last stretch went through the project layer's own workflow:

```sh
timon --max-usd 12 --progress text fix-issues 171 172      # one argument per group of issues
```

1. **The fix:** each group gets the `fixer` agent (opus) in its own worktree, then the gate
   (fmt, clippy, test).
2. **The review:** then the `reviewer` (sonnet), which is given the issues, the commits and the
   diff.
3. **Read the result JSON:** apply small follow-ups by hand, or resume the fixer's own session
   with a decision on each review finding:

   ```lua fragment
   agent.run{ agent = "fixer", resume = SESSION, cwd = WORKTREE, prompt = DECISIONS }
   ```

4. **Ship it:** run `timon check` in the worktree, push the branch, open the PR, and turn on
   squash auto-merge for implementation changes. Design and positioning changes go to Josh.

A single PR for a group cost about $0.3 to $5 in fixer and reviewer runs. Things learned
running it:

- **Squashing:** squash a branch with `git reset --soft origin/main` only when its merge-base is
  origin/main. Otherwise rebase first. A reset after main moved once reverted a merged PR.
- **Shell traps:**
  - macOS has no `timeout`.
  - In zsh, an unquoted word starting with `=` is expanded (`echo ====` fails).
  - Put Lua with apostrophes in a file and run it with `timon eval -f FILE`.

## Open threads

The open issues, by theme:

| Theme | Issues |
|---|---|
| Runner and toolbox | #11 REPL, #73 fs and path, #74 sys, #76 pipes and waits, #36 runs rerun and prune, #37 script logs, #51 edit (maybe), #52 naming review |
| Agents and models | #33 nested budgets, #38 agent dry run, #84 cost summaries, #83 declared effects, #160 to #162 model choice, #174 user instructions on request |
| Workflows | #99 more flow patterns |
| Long-running work | #35 schedules and a worker, with its notes (2026-10-17) |
| Reuse | #39 promote, #141 offer to keep proven work |
| Evidence | #85 reference workflows against just, mise and babashka |

Lessons not yet in issues:

- **Content-hash trust.** timon trusts a directory, so anything changed after `timon trust`
  runs without approval. babagent's trust is a SHA-256 over every file of the layer. #35's notes
  call for this before anything runs unattended.
- **Edit with the host's tool.** In the trial, Codex edited files through `fs.write` inside
  `timon eval` instead of its patch tool, and that is where its errors came from. The `timon`
  skill should say to edit with the host's edit or patch tool and keep `timon eval` for
  computing.
- **Fennel traps for agents:**
  - `&until` read as "add, then stop"
  - `local` where `var` is needed
  - multiple values without parentheses
  - `cond`, which Fennel lacks
  - Lua method-call syntax in place of `(: obj :method)`

  They belong in the scripting skill.
