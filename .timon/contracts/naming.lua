-- Naming findings for one area of timon's public surface.
return schema.object{
  area = schema.string,
  findings = schema.array(schema.object{
    current = schema.string:describe("The name as it is today."),
    proposed = schema.string:describe("One replacement."),
    rule = schema.string:describe("The pattern the replacement follows, in one line."),
    places = schema.array(schema.string):describe("file:line for every use."),
  }),
}
