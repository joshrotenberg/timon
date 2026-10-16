# Recorded provider output

Real CLI output, kept so the fake CLIs can replay it. Paths are replaced with `/work`.

| File | Recorded from |
|---|---|
| `claude-not-logged-in.json` | Claude Code 2.1.301, `claude -p --output-format json` with an empty `CLAUDE_CONFIG_DIR` (exit 1, empty stderr), 2026-10-16 |
| `claude-not-logged-in.stream.jsonl` | the same with `--output-format stream-json --verbose` |
