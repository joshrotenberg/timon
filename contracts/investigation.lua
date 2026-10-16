-- An answer to a question, with the evidence for it.
return schema.object{
  answer = schema.string,
  confidence = schema.enum{ "high", "medium", "low" },
  evidence = schema.array(schema.object{
    source = schema.string:describe("A file, command, URL or session."),
    detail = schema.string,
  }),
  open_questions = schema.array(schema.string),
}
