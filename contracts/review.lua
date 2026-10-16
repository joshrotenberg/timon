-- A code review: a verdict, a short summary, and findings a person can act on.
return schema.object{
  verdict = schema.enum{ "approve", "request_changes" },
  summary = schema.string:describe("Two sentences at most."),
  findings = schema.array(schema.object{
    severity = schema.enum{ "high", "medium", "low" },
    file = schema.string,
    line = schema.optional(schema.int),
    title = schema.string:describe("One line."),
    detail = schema.string:describe("What is wrong, why it matters, and the fix."),
  }),
}
