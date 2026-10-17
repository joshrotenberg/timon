-- What the fixer agent did with a group of issues.
return schema.object{
  issues = schema.array(schema.object{
    number = schema.int,
    status = schema.enum{ "fixed", "partial", "left" }:describe("left: unchanged, with the reason in what"),
    what = schema.string:describe("The cause and the change, in two or three sentences."),
    tests = schema.array(schema.string):describe("The tests added or changed, as file::name."),
  }),
  commits = schema.array(schema.string):describe("Each commit's short sha and subject."),
  checks = schema.string:describe("The checks run before finishing, each with its result."),
  notes = schema.optional(schema.string):describe("Anything a reviewer should know."),
}
