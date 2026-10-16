-- Audits names across timon's public surface with the namer agent (#52). Spends tokens.
-- Usage: timon run naming-audit [OUT], OUT defaults to target/naming-audit.json
local areas = {
  "Lua modules, functions and options: src/modules/*.rs (the DOC declarations)",
  "CLI subcommands and flags: src/main.rs",
  "Run records, events and their files: src/agent/*.rs",
  "Session rows, transcript items and search results: src/sessions/*.rs",
  "Error kinds: every Error::new(\"kind\", ...) in src/",
  "Settings, definition fields and directories: src/settings.rs, src/agents.rs, src/layers.rs, src/dirs.rs",
}
local out = arg[1] or "target/naming-audit.json"

local reports = par.map(areas, function(area)
  return agent.run{ agent = "namer", prompt = "Audit the names in this area: " .. area }
end)

local findings, failed = json.array{}, json.array{}
for i, r in ipairs(reports) do
  if r.status == "ok" then
    for _, f in ipairs(r.answer.findings) do
      f.area = r.answer.area
      findings[#findings + 1] = f
    end
  else
    failed[#failed + 1] = { area = areas[i], error = r.error }
  end
end
fs.write(out, json.encode({ findings = findings, failed = failed }, { pretty = true }), { mkdir = true })
log.info(#findings .. " findings, " .. #failed .. " failed areas, written to " .. out)
return #findings
