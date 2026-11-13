//! Parses the part of a JSON document that has arrived so far.
//!
//! Streaming tool calls deliver their arguments as JSON fragments, so a
//! truncated string or container yields the partial value instead of failing.

use serde_json::Value;

pub fn partial_json(text: &str) -> Option<Value> {
    let mut parser = PartialJson { text, pos: 0 };
    parser.value()
}

struct PartialJson<'a> {
    text: &'a str,
    pos: usize,
}

impl PartialJson<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(ch) if ch.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn value(&mut self) -> Option<Value> {
        self.skip_ws();
        match self.peek()? {
            '{' => self.object(),
            '[' => self.array(),
            '"' => self.string().map(Value::String),
            't' => self.literal("true", Value::Bool(true)),
            'f' => self.literal("false", Value::Bool(false)),
            'n' => self.literal("null", Value::Null),
            _ => self.number(),
        }
    }

    fn object(&mut self) -> Option<Value> {
        self.bump();
        let mut map = serde_json::Map::new();
        loop {
            self.skip_ws();
            if self.peek() == Some('}') {
                self.bump();
                break;
            }
            if self.peek() != Some('"') {
                break;
            }
            let Some(key) = self.string() else { break };
            self.skip_ws();
            if self.peek() != Some(':') {
                break;
            }
            self.bump();
            let Some(value) = self.value() else { break };
            map.insert(key, value);
            self.skip_ws();
            match self.peek() {
                Some(',') => {
                    self.bump();
                }
                Some('}') => {
                    self.bump();
                    break;
                }
                _ => break,
            }
        }
        Some(Value::Object(map))
    }

    fn array(&mut self) -> Option<Value> {
        self.bump();
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(']') {
                self.bump();
                break;
            }
            let Some(value) = self.value() else { break };
            items.push(value);
            self.skip_ws();
            match self.peek() {
                Some(',') => {
                    self.bump();
                }
                Some(']') => {
                    self.bump();
                    break;
                }
                _ => break,
            }
        }
        Some(Value::Array(items))
    }

    fn string(&mut self) -> Option<String> {
        if self.peek() != Some('"') {
            return None;
        }
        self.bump();
        let mut out = String::new();
        while let Some(ch) = self.bump() {
            match ch {
                '"' => return Some(out),
                '\\' => {
                    let Some(escape) = self.bump() else { break };
                    match escape {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let mut code = 0u32;
                            let mut complete = true;
                            for _ in 0..4 {
                                match self.bump().and_then(|ch| ch.to_digit(16)) {
                                    Some(digit) => code = code * 16 + digit,
                                    None => {
                                        complete = false;
                                        break;
                                    }
                                }
                            }
                            if !complete {
                                break;
                            }
                            if let Some(ch) = char::from_u32(code) {
                                out.push(ch);
                            }
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
        Some(out)
    }

    fn literal(&mut self, word: &str, value: Value) -> Option<Value> {
        let rest = &self.text[self.pos..];
        if rest.len() < word.len() && word.starts_with(rest) {
            self.pos = self.text.len();
            return Some(value);
        }
        if rest.starts_with(word) {
            self.pos += word.len();
            return Some(value);
        }
        None
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.pos;
        while matches!(self.peek(), Some(ch) if ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E'))
        {
            self.bump();
        }
        let raw = &self.text[start..self.pos];
        if let Ok(number) = raw.parse::<i64>() {
            return Some(Value::from(number));
        }
        if let Ok(number) = raw.parse::<u64>() {
            return Some(Value::from(number));
        }
        raw.parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_unfinished_arguments() {
        let value = partial_json(r#"{"path":"src/main.rs","content":"line one\nline "#)
            .expect("partial value");
        assert_eq!(value.get("path").and_then(Value::as_str), Some("src/main.rs"));
        assert_eq!(
            value.get("content").and_then(Value::as_str),
            Some("line one\nline ")
        );

        let value = partial_json(r#"{"edits":[{"oldText":"a"},{"newText":"b"}],"n":3}"#)
            .expect("value");
        assert_eq!(
            value.pointer("/edits/1/newText").and_then(Value::as_str),
            Some("b")
        );
        assert_eq!(value.get("n").and_then(Value::as_u64), Some(3));
    }
}
