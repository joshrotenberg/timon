//! Type expressions for declared options, in the LuaLS annotation syntax, parsed into
//! [`Shape`]s.
//!
//! ```text
//! string  integer  number  boolean  any  function  schema
//! "read" | "write" | "full"          one of these strings
//! string[]                           an array
//! { usd: number?, turns?: integer }  a table with these fields; ? makes one optional
//! table<string, integer>             a table with any string keys
//! string | schema                    either
//! T?                                 T or nothing
//! ("user" | "assistant")[]           parentheses group
//! ```
//!
//! The text is what `help()` and the API reference print, and what LuaLS stubs will use, so a
//! declaration is written once.

use std::rc::Rc;

use crate::modules::schema::{Node, Shape};

/// Parses a type expression.
pub fn parse(text: &str) -> Result<Rc<Shape>, String> {
    let mut p = Parser {
        text,
        chars: text.char_indices().peekable(),
    };
    let shape = p.union()?;
    p.skip_space();
    match p.chars.peek() {
        None => Ok(shape),
        Some(&(at, _)) => Err(format!("{text}: unexpected \"{}\"", &text[at..])),
    }
}

struct Parser<'a> {
    text: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
}

impl Parser<'_> {
    fn skip_space(&mut self) {
        while self.chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
    }

    fn eat(&mut self, want: char) -> bool {
        self.skip_space();
        self.chars.next_if(|&(_, c)| c == want).is_some()
    }

    fn expect(&mut self, want: char) -> Result<(), String> {
        if self.eat(want) {
            Ok(())
        } else {
            Err(format!("{}: expected \"{want}\"", self.text))
        }
    }

    fn name(&mut self) -> Option<String> {
        self.skip_space();
        let mut out = String::new();
        while let Some((_, c)) = self
            .chars
            .next_if(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
        {
            out.push(c);
        }
        (!out.is_empty()).then_some(out)
    }

    fn union(&mut self) -> Result<Rc<Shape>, String> {
        let mut members = vec![self.postfix()?];
        while self.eat('|') {
            members.push(self.postfix()?);
        }
        Ok(join(members))
    }

    fn postfix(&mut self) -> Result<Rc<Shape>, String> {
        let mut shape = self.primary()?;
        loop {
            if self.eat('?') {
                shape = Shape::new(Node::Optional(shape));
            } else if self.eat('[') {
                self.expect(']')?;
                shape = Shape::new(Node::Array(shape));
            } else {
                return Ok(shape);
            }
        }
    }

    fn primary(&mut self) -> Result<Rc<Shape>, String> {
        self.skip_space();
        if self.eat('(') {
            let inner = self.union()?;
            self.expect(')')?;
            return Ok(inner);
        }
        if self.eat('"') {
            let mut value = String::new();
            loop {
                match self.chars.next() {
                    Some((_, '"')) => break,
                    Some((_, c)) => value.push(c),
                    None => return Err(format!("{}: unclosed string", self.text)),
                }
            }
            return Ok(Shape::new(Node::Enum(vec![value])));
        }
        if self.eat('{') {
            return self.fields();
        }
        let name = self
            .name()
            .ok_or_else(|| format!("{}: expected a type", self.text))?;
        let node = match name.as_str() {
            "string" => Node::String,
            "integer" => Node::Integer,
            "number" => Node::Number,
            "boolean" => Node::Boolean,
            "any" => Node::Any,
            "function" => Node::Function,
            "schema" => Node::Schema,
            "table" => {
                self.expect('<')?;
                if self.name().as_deref() != Some("string") {
                    return Err(format!("{}: table keys must be string", self.text));
                }
                self.expect(',')?;
                let value = self.union()?;
                self.expect('>')?;
                Node::Map(value)
            }
            other => return Err(format!("{}: unknown type \"{other}\"", self.text)),
        };
        Ok(Shape::new(node))
    }

    fn fields(&mut self) -> Result<Rc<Shape>, String> {
        let mut fields = Vec::new();
        while !self.eat('}') {
            let name = self
                .name()
                .ok_or_else(|| format!("{}: expected a field name", self.text))?;
            let optional = self.eat('?');
            self.expect(':')?;
            let mut shape = self.union()?;
            if optional && !matches!(shape.node, Node::Optional(_)) {
                shape = Shape::new(Node::Optional(shape));
            }
            fields.push((name, shape));
            if !self.eat(',') {
                self.expect('}')?;
                break;
            }
        }
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Shape::new(Node::Object(fields)))
    }
}

/// One member stays itself; string literals together become one enum.
fn join(members: Vec<Rc<Shape>>) -> Rc<Shape> {
    if members.len() == 1 {
        return members
            .into_iter()
            .next()
            .unwrap_or_else(|| Shape::new(Node::Any));
    }
    let mut literals = Vec::new();
    let mut rest = Vec::new();
    for m in members {
        match &m.node {
            Node::Enum(values) => literals.extend(values.iter().cloned()),
            _ => rest.push(m),
        }
    }
    if !literals.is_empty() {
        let shape = Shape::new(Node::Enum(literals));
        if rest.is_empty() {
            return shape;
        }
        rest.insert(0, shape);
    }
    Shape::new(Node::OneOf(rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(text: &str) -> serde_json::Value {
        parse(text).unwrap().json_schema()
    }

    #[test]
    fn literals_unions_arrays_and_tables_parse() {
        assert_eq!(
            schema(r#""read" | "write" | "full""#),
            json!({ "type": "string", "enum": ["read", "write", "full"] })
        );
        assert_eq!(
            schema("string[]"),
            json!({ "type": "array", "items": { "type": "string" } })
        );
        assert_eq!(
            schema(r#"("a" | "b")[]"#),
            json!({ "type": "array", "items": { "type": "string", "enum": ["a", "b"] } })
        );
        assert_eq!(
            schema("{ usd: number?, turns?: integer }"),
            json!({ "type": "object",
                "properties": {
                    "turns": { "anyOf": [{ "type": "integer" }, { "type": "null" }] },
                    "usd": { "anyOf": [{ "type": "number" }, { "type": "null" }] } },
                "required": ["turns", "usd"], "additionalProperties": false })
        );
        assert_eq!(
            schema("number | string"),
            json!({ "anyOf": [{ "type": "number" }, { "type": "string" }] })
        );
        assert_eq!(
            schema("table<string, integer>"),
            json!({ "type": "object", "additionalProperties": { "type": "integer" } })
        );
    }

    #[test]
    fn mistakes_say_where() {
        assert_eq!(parse("strin").unwrap_err(), "strin: unknown type \"strin\"");
        assert_eq!(parse("string]").unwrap_err(), "string]: unexpected \"]\"");
        assert_eq!(
            parse("{ a string }").unwrap_err(),
            "{ a string }: expected \":\""
        );
    }
}
