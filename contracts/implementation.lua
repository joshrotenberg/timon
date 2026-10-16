-- A change made by an agent: what changed, and whether it was tested.
return schema.object{
  summary = schema.string:describe("What changed and why, in a few sentences."),
  files_changed = schema.array(schema.string),
  tests_run = schema.bool:describe("Whether tests were run after the change."),
  tests_passed = schema.optional(schema.bool),
  notes = schema.optional(schema.string:describe("Anything left undone or uncertain.")),
}
