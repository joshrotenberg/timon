# Recorded provider output

Real CLI output, kept so the fake CLIs can replay it. Paths are replaced with `/work`.

| File | Recorded from |
|---|---|
| `claude-not-logged-in.json` | Claude Code 2.1.301, `claude -p --output-format json` with an empty `CLAUDE_CONFIG_DIR` (exit 1, empty stderr), 2026-10-16 |
| `claude-not-logged-in.stream.jsonl` | the same with `--output-format stream-json --verbose` |
| `claude-read.stream.jsonl` | Claude Code 2.1.301 through timon (`events = true`, haiku, read access): read README.md and reply with its first line, 2026-10-16 |
| `codex-read.jsonl` | Codex 0.162.0 through timon (`events = true`, low effort, read-only): the same task, 2026-10-16 |

The fakes replay a recording with `FAKE_REPLAY=<file>`.
