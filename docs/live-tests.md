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

Phases 5 and 9 work in a crate at version 1.4.2 whose trusted project layer has three tasks:
`version`, `release-notes` (which depends on `version` and takes `--audience`) and `wipe` (which
asks before it runs). Phase 9 gives Claude Code `timon serve` as its only MCP server, allows only
its tools, and checks that the agent never sends `confirmed = true` on its own.

Phase 4 runs each host with only timon added:
- Claude Code runs with `--plugin-dir plugin`, `--setting-sources project` and an empty
  `--mcp-config`, and Bash allows only `timon` commands.
- Codex has no plugin directory flag. Its skills go in the test repository's `.agents/skills`,
  which Codex reads, and `timon` goes on `PATH`.

So the Codex marketplace install itself is not exercised, because it would change the real
`~/.codex`.

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
