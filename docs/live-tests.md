# Live tests

`tests/live.rs` runs timon against the real Claude Code and Codex CLIs. Most tests spend
tokens, so every test is `#[ignore]` and also needs `TIMON_LIVE=1`. They never run in CI. The
plan and its scenarios are in #45.

```sh
TIMON_LIVE=1 cargo test --test live -- --ignored --test-threads 1 --nocapture live_0_
```

Test names carry their phase (`live_0_`, `live_1_` and so on), so a phase runs on its own.
`--nocapture` shows the versions, records and cost tables the tests print.

## Isolation

- Every command runs with a cleared environment that keeps only `HOME`, `PATH`, `USER` and
  `LANG`, as a terminal would. Variables of a host session, such as a Claude Code session
  running the tests, do not reach the CLIs.
- timon's state goes to a temporary `TIMON_HOME`. Agents work in a temporary git repository,
  never this checkout.
- Logins come from the real home directory.
- A test that should spend nothing checks first and refuses to run if it would spend. For
  example, the not-logged-in tests ask `claude auth status` and `codex login status` before
  running.

## Phases

| Phase | Proves | Cost |
|---|---|---|
| 0 | CLI versions against the wrappers' tested ranges; not logged in is `not_logged_in` on both providers | none |
| 1 | contracts, access levels, sessions, events, failure classification, cancellation, limits, records, on both providers | $0.01 of Claude usage; Codex reports none |
| 2 | timon-started sessions in the archives, transcripts, real summaries | $0.001 |
| 3 | the dogfood agents in `.timon/`: the reviewer finds a seeded bug, the namer returns a valid `naming` answer | $0.17 (sonnet) |
| 4 | timon as a plugin: an agent on each host uses the skills and `timon` for JSON, the API reference, a named script and sessions | $0.02 for Claude; Codex reports none |
| 5 | tasks through the plugin: a Claude Code agent with only the plugin finds a project's tasks and runs one with an option | $0.003 |
| 8 | `flow` on real runs: a review with two verifiers (`flow.verify`) over a seeded bug, and a vote of three (`flow.vote`) | $0.01 |
| 9 | `timon serve`: a Claude Code agent with only timon's MCP tools and no Bash runs a task through its tool, and does not confirm a task for the user | $0.003 |
| 10 | hooks through the plugin on each host: the session-start brief reaches the model, and the guard refuses an edit outside the project | $0.003 for Claude; Codex reports none |
| 11 | offering to keep what proves useful, in two turns of one session with no mention of tasks or agents: the agent does the job and offers, writing nothing; after a yes it keeps the definition. A pre-release link check is offered as a task, kept, run, and reused by a later session on another directory; an existing `links` task is offered an option, not a copy; a repeated read-only doc check is offered as a role; with a `reviewer` in reach, the offer builds on it. A one-off question, in one turn, offers and writes nothing | about $0.02 on haiku for the keep-unasked version; the two-turn version not yet run |

Phases 5 and 9 work in a crate at version 1.4.2 whose trusted project layer has three tasks:
`version`, `release-notes` (which depends on `version` and takes `--audience`) and `wipe` (which
asks before it runs). Phase 9 gives Claude Code `timon serve` as its only MCP server, allows only
its tools, and checks that the agent never sends `confirmed = true` on its own.

Phase 11 works in a project whose `docs/` and `guides/` each have one broken relative link, and
for the role cases a crate whose docs make one claim its code contradicts. The project layer is
trusted before the agent starts, as a person would have done: trust is a precondition, and no
test lets the agent grant it. The role cases cap the subagents with user limits (haiku, 6 runs,
$0.5). The prompts ask for the work before a release and for a report of what changed and what
ran; they never mention tasks or agents.

Each scenario but the one-off is two turns of one Claude Code session. Turn one keeps its
session; turn two resumes it with `--resume` and says only "Yes, keep it." These sessions are
saved in the real Claude Code home, under the temporary work directory's name, as a person's
would be. The assertions follow the acceptance list of #141 and the offer model of #152:
- Turn one does the job (the broken targets, the contradicted claim), changes nothing under
  `.timon/` (every file and its text, before and after), and offers the right thing: a task; an
  option on the existing `links` task; an agent or role; something built on `reviewer`.
- Turn one looked at `timon tasks`, `timon defs`, `timon agents` or `.timon/`. The brief alone
  does not count.
- After the yes, exactly one definition is added and `timon tasks --json` or `timon agents
  --json` lists it, and it ran: a whole word of a `timon` command for a task, the recorded
  runs' `agent` for a role, which asks for no more than read access. The extended task is
  still the one named `links`. With a `reviewer` in reach, any role added `extends` it and the
  reviewer is unchanged.
- In the first scenario, a later fresh session runs the kept task on `guides/` and adds none.
- The one-off control is one turn: it answers `3`, offers nothing and changes nothing.
- Text checks are loose (an offer word and the kind; a report that names what was added and
  mentions a run). Whether the offer and the report say truly what was meant and verified is
  judged by eye from the printed transcript. Filesystem and listing checks are strict.
- The `links` extension keeps `docs/` as the default, so the use the project had still works.
  This and the exact answers may prove strict for haiku; record what a first run shows before
  loosening them.

Phase 4 runs each host with only timon added:
- Claude Code runs with `--plugin-dir plugin`, `--setting-sources project` and an empty
  `--mcp-config`, and Bash allows only `timon` commands.
- Codex has no plugin directory flag. Its skills go in the test repository's `.agents/skills`,
  which Codex reads, and `timon` goes on `PATH`. Phase 4 installs no Codex plugin.

Phase 10 installs the plugin into each host:
- Claude Code loads it with `--plugin-dir plugin`, as in phase 4.
- Codex gets a temporary `CODEX_HOME`, and `codex plugin marketplace add` with this checkout
  then `codex plugin add timon@timon` install the plugin there, never into the real `~/.codex`.
  The temporary home's `auth.json` is a link to the real login, and `codex exec` runs with
  `--dangerously-bypass-hook-trust`. So phase 10 proves that the local marketplace install
  works and that its hooks run, but not a login from scratch, nor the `/hooks` review a
  person gives once on a first install.

## Compatibility

Phase 0 prints each CLI's version against the range its wrapper was tested on
(`claude-wrapper`'s and `codex-wrapper`'s `TESTED_CLI_VERSION_MIN` and `_MAX`). That range is
the wrapper's claim, and a passing timon run does not widen it: a result below is what timon's
suite observed on one machine with those versions, not upstream support.

## Recorded output

When a live test finds a difference from the fake CLIs, the real output is saved, with paths
replaced, under `tests/fixtures/recorded/`. The fakes replay it (`FAKE_REPLAY`, or a mode
built from it), so a regression test covers the bug without a live run.

`live_1_record_raw_streams` puts a shim in front of each CLI and runs timon through it, so it
records the exact arguments timon passes and the raw output, under `target/tmp/live-recorded/`.

## Results

| Date | Phase | Claude Code | Codex | Result |
|---|---|---|---|---|
| 2026-10-16 | 0 | 2.1.301 (tested range) | 0.162.0 (newer than codex-wrapper 0.4.4's tested 0.147.0) | pass, after two fixes: Claude's not-logged-in message was its raw result JSON, and in stream mode it was classified as `provider_failed`; Codex messages kept a tracing prefix |
| 2026-10-16 | 1 | 2.1.301 | 0.162.0 | 13 of 13 pass in 156 s. Fixed: an unknown model is now `unknown_model` with a readable message on both providers. Found: a killed timon leaves its provider running (#63) |
| 2026-10-16 | 2 | 2.1.301 | 0.162.0 | pass in 162 s, most of it scanning 6,600 real sessions with a debug build |
| 2026-10-16 | 3 | 2.1.301 | | pass in 57 s: the reviewer reported the seeded off-by-one as high; the namer's four findings are on #52 |
| 2026-10-16 | 4 | 2.1.301 | 0.162.0 | Claude 4 of 4 and Codex 4 of 4, after one skill fix: an agent ran `timon run NAME` inside a longer shell line, which a `timon`-only permission denies; the `timon` skill now says to run timon as a command of its own |
| 2026-10-16 | 1 | 2.1.301 | | A fan-out of 8 haiku runs at concurrency 4: 10 s wall for 35 s of runs, at most 4 at once, no rate-limit failures, $0.004. A killed timon's provider is now stopped by the next command (#63) |
| 2026-10-16 | 1 | 2.1.301 | | Resume: a script's third run failed with max_turns; `--resume` reused the first two and ran only the third, which passed. $0.0008 |
| 2026-10-16 | 5 | 2.1.301 | | pass, $0.0024 to $0.0035: the agent ran `timon tasks`, read `timon release-notes --help`, then ran `timon release-notes --audience developers` |
| 2026-10-16 | 9 | 2.1.301 | | pass, $0.0023 to $0.0025: the agent called `mcp__timon__release-notes` with `audience = "developers"`, called `mcp__timon__wipe` with `confirmed = false`, got the confirm error, and told the user nothing was deleted. The first assertion on the notes' exact wording failed when the agent reformatted them; the test now checks the version and audience |
| 2026-10-16 | 8 | 2.1.301 | | pass, $0.0093 for 8 haiku runs: the review found the seeded off-by-one (2 findings), both verifiers kept both, and the vote stopped at a quorum of 2 and cancelled its third run |
| 2026-10-16 | 4 | 2.1.301 | | Claude 2 of 4 on the first run, after the skills had changed for tasks. Found: agents wrapped timon in shell syntax a `timon`-only host denies (`timon eval -f /dev/stdin <<'EOF' 2>/dev/null`, `timon count-lines; echo "exit=$?"`). The `timon` skill's own one-liners piped into timon, and the rule against it was only in that skill. Fixed: the rule, with what to do instead, now opens every skill, and the pipe examples are marked as for a person's shell. Then 4 of 4: sessions $0.0046, reference $0.0037, reshape $0.0028, script $0.0034 |
| 2026-10-16 | 10 | 2.1.302 | 0.162.0 | Claude passed on the first run ($0.0023): the model named both tasks from the brief and the guard refused its Write. Codex ran no plugin hooks at first: it reads the root `plugin.json` before `.codex-plugin/plugin.json` and then lists no plugin hooks, with or without a `hooks` entry. Fixed: Codex's manifest moved to `.codex-plugin/plugin.json`. Then both pass; Codex used 33k input tokens (28k cached) and 199 output, and set `PLUGIN_ROOT` and `CLAUDE_PLUGIN_ROOT` for the hook |
| 2026-10-16 | 0 to 10, the whole suite | 2.1.302 | 0.162.0 | 34 of 34 pass in 651 s on main at ae3a7f1 (hooks v1). $0.24 of Claude usage: the namer $0.15 and the reviewer $0.05 (both sonnet), every other test under $0.01; Codex reports none. No regressions since the per-phase runs |
| 2026-10-16 | 0 to 10, the whole suite | 2.1.302 | 0.163.0 (`NewerUntested`: codex-wrapper 0.4.4 tested 0.145.0 to 0.147.0) | 33 of 34 pass in 563 s at 460b589, on Darwin arm64; the run exited 101. Failed: `live_5_claude_runs_tasks_through_the_plugin`, where the agent ran the default `users` audience when `developers` was asked for. Both phase 10 tests and the phase 9 task and confirmation test passed. Run once, not repeated until green, so no failure rate is claimed. Codex cost is unknown, not $0 (#84) |
| 2026-10-17 | 11, keep-unasked guidance | 2.1.302 | | Results for the earlier guidance, which told agents to keep definitions without asking; superseded by the offer model (#152). 1 of 5 on haiku ($0.022) and 1 of 5 on a sonnet host ($0.30). The one-off control passed. In the other four, both models did the work correctly and kept nothing: each said it changed nothing in the repository, and several did not look for existing definitions first (`timon tasks`, `timon agents`). With guidance alone, a request to check something is treated as read-only. Whether agents should write definitions unasked, or propose them in the report, is open on #141 |
