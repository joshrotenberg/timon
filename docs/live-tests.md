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
