-- Settings for working on timon itself. Small limits: dogfood scripts should not spend much.
return {
  defaults = { provider = "claude", model = "sonnet" },
  limits = { concurrency = 2, max_usd = 2 },
}
