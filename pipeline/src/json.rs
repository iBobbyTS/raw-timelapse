//! Minimal JSON value, parser and pretty writer.
//!
//! The pipeline consumes the numeric sidecar produced by `dng-linear-frame`
//! and writes its own run manifest. No external crates are used.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn obj(entries: Vec<(&str, Json)>) -> Json {
        Json::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries
                .iter()
                .find(|(entry_key, _)| entry_key == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Int(value) => Some(*value as f64),
            Json::Float(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Int(value) if *value >= 0 => Some(*value as u64),
            Json::Float(value)
                if value.is_finite()
                    && *value >= 0.0
                    && value.fract() == 0.0
                    && *value <= u64::MAX as f64 =>
            {
                Some(*value as u64)
            }
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        self.as_u64().and_then(|value| u32::try_from(value).ok())
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(values) => Some(values),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

pub fn parse(text: &str) -> std::result::Result<Json, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        pos: 0,
        depth: 0,
    };
    parser.skip_whitespace();
    let value = parser.value()?;
    parser.skip_whitespace();
    if parser.pos != parser.bytes.len() {
        return Err(format!("trailing characters at byte {}", parser.pos));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, expected: u8) -> std::result::Result<(), String> {
        match self.bump() {
            Some(byte) if byte == expected => Ok(()),
            Some(byte) => Err(format!(
                "expected '{}' at byte {}, found '{}'",
                expected as char,
                self.pos - 1,
                byte as char
            )),
            None => Err(format!("expected '{}' at end of input", expected as char)),
        }
    }

    fn value(&mut self) -> std::result::Result<Json, String> {
        if self.depth >= 128 {
            return Err("JSON nesting limit exceeded".to_string());
        }
        self.depth += 1;
        let result = self.value_inner();
        self.depth -= 1;
        result
    }

    fn value_inner(&mut self) -> std::result::Result<Json, String> {
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => {
                self.literal("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.literal("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.literal("null")?;
                Ok(Json::Null)
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(byte) => Err(format!(
                "unexpected byte '{}' at {}",
                byte as char, self.pos
            )),
            None => Err("unexpected end of input".to_string()),
        }
    }

    fn literal(&mut self, literal: &str) -> std::result::Result<(), String> {
        let end = self.pos + literal.len();
        if end <= self.bytes.len() && &self.bytes[self.pos..end] == literal.as_bytes() {
            self.pos = end;
            Ok(())
        } else {
            Err(format!("invalid literal at byte {}", self.pos))
        }
    }

    fn object(&mut self) -> std::result::Result<Json, String> {
        self.expect(b'{')?;
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            let value = self.value()?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.bump() {
                Some(b',') => continue,
                Some(b'}') => break,
                Some(byte) => {
                    return Err(format!(
                        "expected ',' or '}}' at byte {}, found '{}'",
                        self.pos - 1,
                        byte as char
                    ))
                }
                None => return Err("unterminated object".to_string()),
            }
        }
        Ok(Json::Object(entries))
    }

    fn array(&mut self) -> std::result::Result<Json, String> {
        self.expect(b'[')?;
        let mut values = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(values));
        }
        loop {
            self.skip_whitespace();
            values.push(self.value()?);
            self.skip_whitespace();
            match self.bump() {
                Some(b',') => continue,
                Some(b']') => break,
                Some(byte) => {
                    return Err(format!(
                        "expected ',' or ']' at byte {}, found '{}'",
                        self.pos - 1,
                        byte as char
                    ))
                }
                None => return Err("unterminated array".to_string()),
            }
        }
        Ok(Json::Array(values))
    }

    fn string(&mut self) -> std::result::Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        let mut run_start = self.pos;
        loop {
            let byte = match self.peek() {
                Some(byte) => byte,
                None => return Err("unterminated string".to_string()),
            };
            match byte {
                b'"' => {
                    push_utf8_slice(&mut out, &self.bytes[run_start..self.pos])?;
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    push_utf8_slice(&mut out, &self.bytes[run_start..self.pos])?;
                    self.pos += 1;
                    let escape = self
                        .bump()
                        .ok_or_else(|| "unterminated escape".to_string())?;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let first = self.hex4()?;
                            let code = if (0xD800..=0xDBFF).contains(&first) {
                                if self.peek() != Some(b'\\') {
                                    return Err(format!(
                                        "unpaired high surrogate at byte {}",
                                        self.pos
                                    ));
                                }
                                self.pos += 1;
                                if self.bump() != Some(b'u') {
                                    return Err("expected low surrogate escape".to_string());
                                }
                                let second = self.hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&second) {
                                    return Err(format!(
                                        "invalid low surrogate at byte {}",
                                        self.pos
                                    ));
                                }
                                0x10000
                                    + (((first as u32 - 0xD800) << 10) | (second as u32 - 0xDC00))
                            } else if (0xDC00..=0xDFFF).contains(&first) {
                                return Err(format!("unpaired low surrogate at byte {}", self.pos));
                            } else {
                                first as u32
                            };
                            let ch = char::from_u32(code)
                                .ok_or_else(|| format!("invalid code point {code:#x}"))?;
                            out.push(ch);
                        }
                        other => {
                            return Err(format!(
                                "invalid escape '\\{}' at byte {}",
                                other as char,
                                self.pos - 1
                            ))
                        }
                    }
                    run_start = self.pos;
                }
                0x00..=0x1F => {
                    return Err(format!(
                        "unescaped control byte {:#04x} in string at byte {}",
                        byte, self.pos
                    ))
                }
                _ => self.pos += 1,
            }
        }
    }

    fn hex4(&mut self) -> std::result::Result<u16, String> {
        let mut value: u16 = 0;
        for _ in 0..4 {
            let byte = self
                .bump()
                .ok_or_else(|| "truncated \\u escape".to_string())?;
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => {
                    return Err(format!(
                        "invalid hex digit '{}' in \\u escape at byte {}",
                        byte as char,
                        self.pos - 1
                    ))
                }
            };
            value = (value << 4) | digit as u16;
        }
        Ok(value)
    }

    fn number(&mut self) -> std::result::Result<Json, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(format!("leading zero in number at byte {start}"));
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(format!("invalid number at byte {start}")),
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(format!("missing fraction digits at byte {}", self.pos));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(format!("missing exponent digits at byte {}", self.pos));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| format!("invalid number bytes at {start}"))?;
        if !is_float {
            if let Ok(value) = text.parse::<i64>() {
                return Ok(Json::Int(value));
            }
        }
        text.parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(Json::Float)
            .ok_or_else(|| format!("invalid or nonfinite number '{text}'"))
    }
}

fn push_utf8_slice(out: &mut String, bytes: &[u8]) -> std::result::Result<(), String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "invalid UTF-8 inside JSON string".to_string())?;
    out.push_str(text);
    Ok(())
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

pub fn to_pretty_string(value: &Json) -> String {
    let mut out = String::new();
    write_value(value, 0, &mut out);
    out.push('\n');
    out
}

fn write_value(value: &Json, indent: usize, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Int(number) => {
            let _ = write!(out, "{number}");
        }
        Json::Float(number) => {
            if !number.is_finite() {
                out.push_str("null");
            } else {
                let _ = write!(out, "{number}");
            }
        }
        Json::Str(text) => write_string(text, out),
        Json::Array(values) => {
            if values.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in values.iter().enumerate() {
                write_indent(indent + 2, out);
                write_value(item, indent + 2, out);
                if index + 1 != values.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            write_indent(indent, out);
            out.push(']');
        }
        Json::Object(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (index, (key, item)) in entries.iter().enumerate() {
                write_indent(indent + 2, out);
                write_string(key, out);
                out.push_str(": ");
                write_value(item, indent + 2, out);
                if index + 1 != entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            write_indent(indent, out);
            out.push('}');
        }
    }
}

fn write_indent(indent: usize, out: &mut String) {
    for _ in 0..indent {
        out.push(' ');
    }
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_native_sidecar_shape() {
        let text = r#"{
            "schema_version": 1,
            "source_sha256": "92b3f0",
            "width": 4000,
            "height": 2256,
            "applied_white_balance": [0.605080831408776, 1, 0.48250460405156537],
            "white_balance_source": "as-shot",
            "observed_scene_min": -0.15037427842617035,
            "quantization": {"scale": 16384, "negative_samples": 310759},
            "color": {"color_matrix3": null, "forward_matrix_used": false}
        }"#;
        let value = parse(text).expect("parse");
        assert_eq!(value.get("schema_version").unwrap().as_u64(), Some(1));
        assert_eq!(
            value.get("white_balance_source").unwrap().as_str(),
            Some("as-shot")
        );
        let wb = value
            .get("applied_white_balance")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(wb.len(), 3);
        assert!((wb[1].as_f64().unwrap() - 1.0).abs() < f64::EPSILON);
        assert_eq!(
            value
                .get("quantization")
                .unwrap()
                .get("negative_samples")
                .unwrap()
                .as_u64(),
            Some(310759)
        );
        assert!(matches!(
            value.get("color").unwrap().get("color_matrix3").unwrap(),
            Json::Null
        ));
    }

    #[test]
    fn parses_escapes_and_surrogates() {
        let value = parse(r#""a\"b\\c\/d\n\t\u00e9\uD83D\uDE00""#).expect("parse");
        assert_eq!(value.as_str().unwrap(), "a\"b\\c/d\n\t\u{00e9}\u{1F600}");
    }

    #[test]
    fn rejects_invalid_documents() {
        assert!(parse("{\"a\":1,}").is_err());
        assert!(parse("[1 2]").is_err());
        assert!(parse("01").is_err());
        assert!(parse("\"unterminated").is_err());
        assert!(parse("{} trailing").is_err());
    }

    #[test]
    fn writer_round_trips() {
        let value = Json::obj(vec![
            ("name", Json::Str("a\n\"b\"".to_string())),
            ("values", Json::Array(vec![Json::Int(1), Json::Float(0.5)])),
            ("nested", Json::obj(vec![("ok", Json::Bool(true))])),
            ("nothing", Json::Null),
        ]);
        let text = to_pretty_string(&value);
        let reparsed = parse(&text).expect("reparse");
        assert_eq!(reparsed, value);
    }
}
