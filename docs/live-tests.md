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
| 3 | the dogfood agents in `.timon/` | about $0.50 |
| 4 | timon as a plugin (#46) | |

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
