//! Fixture YAML values with last-key-wins maps and fresh anchor identities.
//! Parse structure before resolving aliases: text inside scalars is never syntax.

use crate::det::HashMap;

use serde_yaml_ng::{Mapping, Value};
use yaml_rust2::parser::{Event, Parser, Tag};
use yaml_rust2::scanner::TScalarStyle;

pub(super) fn parse(source: &str) -> Result<Value, String> {
    let mut reader = Reader {
        parser: Parser::new_from_str(source),
        anchors: HashMap::default(),
    };
    reader.expect(Event::StreamStart)?;
    match reader.next()? {
        Event::StreamEnd => return Ok(Value::Null),
        Event::DocumentStart => {}
        other => return Err(format!("expected document start, got {other:?}")),
    }
    let value = reader.value(None, 0)?;
    reader.expect(Event::DocumentEnd)?;
    reader.expect(Event::StreamEnd)?;
    Ok(value)
}

struct Reader<'a> {
    parser: Parser<std::str::Chars<'a>>,
    anchors: HashMap<usize, Value>,
}

impl Reader<'_> {
    fn next(&mut self) -> Result<Event, String> {
        self.parser
            .next_token()
            .map(|(event, _)| event)
            .map_err(|e| e.to_string())
    }

    fn expect(&mut self, expected: Event) -> Result<(), String> {
        let actual = self.next()?;
        if actual == expected {
            Ok(())
        } else {
            Err(format!("expected {expected:?}, got {actual:?}"))
        }
    }

    fn value(&mut self, merge_owner: Option<usize>, depth: usize) -> Result<Value, String> {
        if depth > 128 {
            return Err("fixture YAML nesting limit exceeded".to_string());
        }
        let (value, anchor) = match self.next()? {
            Event::Scalar(text, style, anchor, tag) => {
                let is_binary = tag
                    .as_ref()
                    .is_some_and(|t| matches!(t.handle.as_str(), "tag:yaml.org,2002:" | "!") && t.suffix == "binary");
                let binary = Some(())
                    .filter(|_| is_binary)
                    .and_then(|_| base64_decode(&text))
                    .and_then(|bytes| String::from_utf8(bytes).ok());
                let value = if let Some(decoded) = binary {
                    // `!!binary` / `!binary` (yaml.org/type/binary) is base64
                    // for bytes, which Psych decodes before the row is typed.
                    // UTF-8 bytes ride as the string they spell; others stay
                    // a tagged scalar of their base64 text, which the fixture
                    // ingest turns into a runtime decode.
                    Value::String(decoded)
                } else if let Some(tag) = tag {
                    // Let the existing scalar resolver retain its tag and number
                    // semantics; JSON string escaping is also valid YAML quoting.
                    let quoted = serde_json::to_string(&text).map_err(|e| e.to_string())?;
                    // serde_yaml_ng drops a verbatim `tag:yaml.org,2002:` tag
                    // as its own core type; `!binary` keeps its tag, which
                    // the fixture ingest reads to decode the text at load.
                    let (handle, suffix) = if is_binary { ("!", "binary") } else { (tag.handle.as_str(), tag.suffix.as_str()) };
                    serde_yaml_ng::from_str(&format!("!<{handle}{suffix}> {quoted}"))
                        .map_err(|e| e.to_string())?
                } else if style == TScalarStyle::Plain {
                    plain_scalar(text)
                } else {
                    Value::String(text)
                };
                (value, anchor)
            }
            Event::MappingStart(anchor, tag) => {
                let mut map = Mapping::new();
                while self.parser.peek().map_err(|e| e.to_string())?.0 != Event::MappingEnd {
                    let key = self.value(None, depth + 1)?;
                    let owner = (anchor != 0 && key.as_str() == Some("<<")).then_some(anchor);
                    let value = self.value(owner, depth + 1)?;
                    map.insert(key, value);
                }
                self.expect(Event::MappingEnd)?;
                (tagged(Value::Mapping(map), tag), anchor)
            }
            Event::SequenceStart(anchor, tag) => {
                let mut sequence = Vec::new();
                while self.parser.peek().map_err(|e| e.to_string())?.0 != Event::SequenceEnd {
                    sequence.push(self.value(merge_owner, depth + 1)?);
                }
                self.expect(Event::SequenceEnd)?;
                (tagged(Value::Sequence(sequence), tag), anchor)
            }
            Event::Alias(id) => {
                if merge_owner == Some(id) {
                    // Merging a mapping into itself adds no keys. Other cycles
                    // cannot be represented as fixture values, so reject them.
                    return Ok(Value::Mapping(Mapping::new()));
                }
                return self.anchors.get(&id).cloned().ok_or_else(|| {
                    "recursive fixture YAML alias outside a self-merge".to_string()
                });
            }
            other => return Err(format!("expected fixture value, got {other:?}")),
        };
        if anchor != 0 {
            self.anchors.insert(anchor, value.clone());
        }
        Ok(value)
    }
}

fn plain_scalar(text: String) -> Value {
    // Resolve scalar types without parsing another YAML document: `---` and
    // `...` can be ordinary mapping values, and must remain strings here.
    match text.as_str() {
        "" | "~" | "null" | "Null" | "NULL" => Value::Null,
        "true" | "True" | "TRUE" => Value::Bool(true),
        "false" | "False" | "FALSE" => Value::Bool(false),
        _ => text
            .parse::<serde_yaml_ng::Number>()
            .map(Value::Number)
            .unwrap_or(Value::String(text)),
    }
}

fn tagged(value: Value, tag: Option<Tag>) -> Value {
    match tag {
        None => value,
        Some(tag) if tag.handle == "tag:yaml.org,2002:" => value,
        Some(tag) => Value::Tagged(Box::new(serde_yaml_ng::value::TaggedValue {
            tag: serde_yaml_ng::value::Tag::new(format!("{}{}", tag.handle, tag.suffix)),
            value,
        })),
    }
}

/// Standard-alphabet base64 as `!!binary` writes it (whitespace between
/// groups allowed, `=` padding optional). `None` for anything else.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.chars().filter(|c| !c.is_whitespace()).take_while(|c| *c != '=') {
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}
