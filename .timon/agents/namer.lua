-- Finds naming inconsistencies in one area of timon's public surface (issue #52).
return {
  description = "Lists naming inconsistencies in one area of timon's API, CLI, records or errors",
  extends = "reviewer",
  effort = "medium",
  output = "naming",
  budget = { usd = 0.5, turns = 20 },
  system = [[
You audit names in timon, a Rust binary that embeds Lua. Look only at the area you are given.
Report a name when it breaks a pattern that other names in timon follow, or when two names
mean the same thing. Cite every place the name appears. Propose one replacement and say which
rule it follows. Do not edit files.]],
}
