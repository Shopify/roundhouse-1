//! Fixture YAML values with last-key-wins maps and fresh anchor identities.
//! Parse structure before resolving aliases: text inside scalars is never syntax.

use crate::det::HashMap;

use serde_yaml_ng::{Mapping, Value};
use yaml_rust2::parser::{Event, Parser, Tag};
use yaml_rust2::scanner::TScalarStyle;

/// A document yaml-rust2 rejects for indentation inside a flow collection or
/// a multi-line quoted scalar is read again with those continuation lines
/// indented (`relax_continuation_indentation`): Psych, which parses Rails
/// fixtures, takes the text as written.
pub(super) fn parse(source: &str) -> Result<Value, String> {
    match parse_strict(source) {
        Err(error) if error.contains("invalid indentation") => {
            let relaxed = relax_continuation_indentation(source);
            if relaxed == source {
                return Err(error);
            }
            parse_strict(&relaxed).map_err(|_| error)
        }
        other => other,
    }
}

fn parse_strict(source: &str) -> Result<Value, String> {
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

/// Indentation means nothing on the continuation lines of a flow collection
/// or a quoted scalar (line folding trims it, and libyaml ignores it inside
/// flow context), but yaml-rust2 requires it to exceed the enclosing block
/// key's. Each such line that does not is pushed two columns past that key.
/// Block scalars, comments and plain text are left alone.
fn relax_continuation_indentation(source: &str) -> String {
    let mut out = String::with_capacity(source.len() + 64);
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut base = 0usize;
    let mut block_threshold: Option<usize> = None;
    for raw in source.split_inclusive('\n') {
        let (text, newline) = match raw.strip_suffix('\n') {
            Some(text) => (text, "\n"),
            None => (raw, ""),
        };
        let blank = text.trim().is_empty();
        let indent = text.chars().take_while(|c| *c == ' ').count();
        if let Some(threshold) = block_threshold {
            if blank || indent > threshold {
                out.push_str(raw);
                continue;
            }
            block_threshold = None;
        }
        let inside = depth > 0 || quote.is_some();
        let mut chars: Vec<char> = text.chars().collect();
        if inside && !blank && indent <= base {
            let mut widened = vec![' '; base + 2];
            widened.extend_from_slice(&chars[indent..]);
            chars = widened;
        }
        // The key column of a block line: past its leading `- ` entries.
        let lead = chars.iter().take_while(|c| **c == ' ').count();
        let mut key_col = lead;
        let mut last_dash = None;
        while chars.get(key_col) == Some(&'-') && chars.get(key_col + 1).is_none_or(|c| *c == ' ') {
            last_dash = Some(key_col);
            key_col += 1;
            while chars.get(key_col) == Some(&' ') {
                key_col += 1;
            }
        }
        let mut i = if inside { 0 } else { lead };
        while i < chars.len() {
            let c = chars[i];
            if let Some(open) = quote {
                if open == '"' && c == '\\' {
                    i += 2;
                    continue;
                }
                if c == open {
                    if open == '\'' && chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    quote = None;
                }
                i += 1;
                continue;
            }
            if c == '#' && (i == 0 || chars[i - 1].is_whitespace()) {
                break;
            }
            let node_start = {
                let mut j = i;
                while j > 0 && chars[j - 1] == ' ' {
                    j -= 1;
                }
                match j.checked_sub(1).map(|k| chars[k]) {
                    None => true,
                    Some(':' | '?' | ',' | '[' | '{') if j < i || depth > 0 => true,
                    Some('-') if j < i && (j == 1 || chars[j - 2] == ' ') => true,
                    Some(_) => {
                        let mut k = j;
                        while k > 0 && chars[k - 1] != ' ' {
                            k -= 1;
                        }
                        matches!(chars.get(k), Some('&' | '!'))
                    }
                }
            };
            match c {
                '"' | '\'' if node_start => {
                    if depth == 0 {
                        base = key_col;
                    }
                    quote = Some(c);
                }
                '[' | '{' if depth > 0 || node_start => {
                    if depth == 0 {
                        base = key_col;
                    }
                    depth += 1;
                }
                ']' | '}' if depth > 0 => depth -= 1,
                '|' | '>' if depth == 0 && node_start => {
                    let mut rest = chars[i + 1..].iter().skip_while(|c| matches!(c, '0'..='9' | '+' | '-'));
                    let header = rest.clone().all(|c| c.is_whitespace())
                        || rest.find(|c| !c.is_whitespace()) == Some(&'#');
                    if header {
                        block_threshold = Some(if i == key_col { last_dash.unwrap_or(lead) } else { key_col });
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        out.extend(chars);
        out.push_str(newline);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::parse;

    /// Psych (libyaml) ignores indentation inside a flow collection, so a
    /// closing `}` or `]` back at the enclosing key's column is accepted.
    #[test]
    fn flow_collection_may_close_at_the_enclosing_key_column() {
        let source = "first:\n  metadata: {\n    \"a\": {\n      \"b\": \"c\"\n    }\n  }\n\nsecond:\n  amount: 1\n";
        let value = parse(source).expect("flow mapping closing at the key's indentation parses");
        let first = value.get("first").and_then(|f| f.get("metadata")).and_then(|m| m.get("a")).and_then(|a| a.get("b"));
        assert_eq!(first.and_then(|b| b.as_str()), Some("c"));
        assert!(value.get("second").is_some());
    }

    #[test]
    fn flow_sequence_may_close_at_the_enclosing_key_column() {
        let source = "first:\n  tags: [\n    \"a\",\n    \"b\"\n  ]\nsecond:\n  amount: 1\n";
        let value = parse(source).expect("flow sequence closing at the key's indentation parses");
        assert_eq!(value.get("first").and_then(|f| f.get("tags")).and_then(|t| t.as_sequence()).map(|s| s.len()), Some(2));
    }

    #[test]
    fn multi_line_quoted_scalar_may_continue_at_the_key_column() {
        let source = "first:\n  body: \"Confirm\n\n  Click below.\"\n  name: x\nsecond:\n  definition: &definition '{\n    \"status\": \"published\"\n}'\n  name: y\n";
        let value = parse(source).expect("quoted scalars continuing at the key column parse");
        let body = value.get("first").and_then(|f| f.get("body")).and_then(|b| b.as_str()).unwrap_or_default();
        assert!(body.starts_with("Confirm") && body.ends_with("Click below."), "{body:?}");
        assert_eq!(value.get("first").and_then(|f| f.get("name")).and_then(|n| n.as_str()), Some("x"));
        assert_eq!(value.get("second").and_then(|s| s.get("name")).and_then(|n| n.as_str()), Some("y"));
    }

    #[test]
    fn block_scalar_text_is_not_read_as_flow_or_quotes() {
        let source = "first:\n  note: |\n    {\n    it's \"odd\n  flag: true\nsecond:\n  tags: [\n    a\n  ]\n";
        let value = parse(source).expect("a block scalar holding brackets and quotes does not confuse the relaxation");
        assert_eq!(value.get("first").and_then(|f| f.get("flag")).and_then(|n| n.as_bool()), Some(true));
        assert_eq!(value.get("second").and_then(|s| s.get("tags")).and_then(|t| t.as_sequence()).map(|s| s.len()), Some(1));
    }

    #[test]
    fn misaligned_block_keys_stay_rejected() {
        assert!(parse("a:\n    b: 1\n  c: 2\n").is_err());
    }
}
