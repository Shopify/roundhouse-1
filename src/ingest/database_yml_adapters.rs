//! The adapter each `test` configuration of `config/database.yml` names,
//! where the file states it without its ERB having to run.
//!
//! Core keeps `core_general_1` on trilogy (MySQL) even in a PostgreSQL
//! test run: its `test:` entry merges `*<%= "test_core_general_1" %>`,
//! whose chain ends in `adapter: trilogy`. Models routed there never
//! reach the PostgreSQL database, and `analyze::model_table` says so
//! instead of calling their tables schema gaps.
//!
//! Read line by line, following `<<: *anchor` merges (an ERB anchor
//! name only as a plain string literal). Anything whose value would
//! depend on running the ERB yields no entry: a key under an ERB
//! conditional or loop, an ERB value, a merge list, two merges. An
//! explicit `adapter:` beats a merged one, as YAML merge keys do.

use crate::det::HashMap;

pub(super) fn test_adapters(text: &str) -> Vec<(String, String)> {
    let lines = scan(text);
    let Some(env) = lines.iter().position(|l| l.indent == 0 && l.depth == 0 && key_of(&l.text) == Some("test")) else {
        return Vec::new();
    };
    // An anchor defined twice (or under ERB control) resolves to nothing.
    let mut anchors: HashMap<&str, Option<usize>> = HashMap::default();
    for (i, l) in lines.iter().enumerate() {
        if let Some(a) = anchor_of(&l.text) {
            let unique = l.depth == 0 && !anchors.contains_key(a);
            anchors.insert(a, unique.then_some(i));
        }
    }
    let anchors: HashMap<&str, usize> = anchors.into_iter().filter_map(|(a, i)| Some((a, i?))).collect();
    let mut out = Vec::new();
    for child in children(&lines, env) {
        let Some(name) = key_of(&lines[child].text) else { continue };
        if lines[child].depth != 0 {
            continue;
        }
        if let Some(adapter) = adapter_of(&lines, child, &anchors, 0) {
            out.push((name.to_string(), adapter));
        }
    }
    out
}

struct Line {
    indent: usize,
    /// ERB control nesting (`<% if %>`, `<% x.each do %>`) before the line.
    depth: usize,
    text: String,
}

fn scan(text: &str) -> Vec<Line> {
    let mut depth: usize = 0;
    let mut out = Vec::new();
    for raw in text.lines() {
        let start = depth;
        let mut rest = raw;
        let mut control_only = true;
        let mut outside = String::new();
        while let Some(i) = rest.find("<%") {
            outside.push_str(&rest[..i]);
            let Some(j) = rest[i..].find("%>") else { break };
            let tag = &rest[i + 2..i + j];
            if tag.starts_with('=') {
                control_only = false;
            } else if !tag.starts_with('#') {
                let code = tag.trim_start_matches('-').trim().trim_end_matches('-').trim();
                let first = code.split_whitespace().next().unwrap_or("");
                if matches!(first, "if" | "unless" | "case" | "while" | "until" | "for" | "begin")
                    || code.ends_with(" do")
                    || code.contains(" do |")
                {
                    depth += 1;
                } else if first == "end" {
                    depth = depth.saturating_sub(1);
                }
            }
            outside.push_str(&rest[i..i + j + 2]);
            rest = &rest[i + j + 2..];
        }
        outside.push_str(rest);
        let trimmed = outside.trim_start();
        let body = trimmed.trim();
        let erb_only = body.starts_with("<%") && body.ends_with("%>") && control_only && body.matches("<%").count() == 1;
        if body.is_empty() || body.starts_with('#') || erb_only {
            continue;
        }
        out.push(Line { indent: outside.len() - trimmed.len(), depth: start.min(depth), text: body.to_string() });
    }
    out
}

/// The mapping key of `key: ...`, `key:` or `key: &anchor`.
fn key_of(text: &str) -> Option<&str> {
    let (key, _) = text.split_once(':')?;
    (!key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(key)
}

fn value_of(text: &str) -> &str {
    text.split_once(':').map(|(_, v)| v.trim()).unwrap_or("")
}

fn anchor_of(text: &str) -> Option<&str> {
    key_of(text)?;
    let a = value_of(text).strip_prefix('&')?;
    (!a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(a)
}

/// The lines directly under `parent`: the next line's indent, until a
/// line at or left of the parent's.
fn children(lines: &[Line], parent: usize) -> Vec<usize> {
    let base = lines[parent].indent;
    let Some(first) = lines.get(parent + 1).filter(|l| l.indent > base) else { return Vec::new() };
    let indent = first.indent;
    (parent + 1..lines.len())
        .take_while(|&i| lines[i].indent > base)
        .filter(|&i| lines[i].indent == indent)
        .collect()
}

fn adapter_of(lines: &[Line], at: usize, anchors: &HashMap<&str, usize>, hops: usize) -> Option<String> {
    if hops > 16 {
        return None;
    }
    let kids = children(lines, at);
    let mut merges = Vec::new();
    for &k in &kids {
        let text = &lines[k].text;
        match key_of(text) {
            Some("adapter") => {
                let v = value_of(text);
                let literal = !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                return (lines[k].depth == 0 && literal).then(|| v.to_string());
            }
            _ if text.starts_with("<<:") => merges.push(k),
            _ => {}
        }
    }
    let [m] = merges[..] else { return None };
    if lines[m].depth != 0 {
        return None;
    }
    let target = value_of(&lines[m].text).strip_prefix('*')?.trim();
    let name = match target.strip_prefix("<%=").and_then(|t| t.strip_suffix("%>")) {
        Some(code) => {
            let code = code.trim();
            let lit = code.strip_prefix('"').and_then(|c| c.strip_suffix('"'))?;
            if lit.contains(['"', '\\', '#']) {
                return None;
            }
            lit
        }
        None => target,
    };
    adapter_of(lines, *anchors.get(name)?, anchors, hops + 1)
}

#[cfg(test)]
mod tests {
    use super::test_adapters;

    const YML: &str = r#"
mysql_config: &mysql_config
  adapter: trilogy
  database_tasks: <%= !Rails.env.test? %>

pg_config: &pg_config
  adapter: postgresql

test_mysql: &test_mysql
  database: x
  <<: *mysql_config

test: &test
  primary:
    <<: *pg_config
  side:
    <<: *<%= "test_mysql" %>
    username: <%= user("x") %>
  computed:
    <<: *<%= "test_#{kind}" %>
  <% if ENV["YB"] %>
  yb:
    adapter: yugabyte
  <% end %>
  <% [1, 2].each do |i| %>
  shard_<%= i %>:
    adapter: postgresql
  <% end %>
  direct:
    <<: *mysql_config
    adapter: postgresql
"#;

    #[test]
    fn literal_chains_resolve_and_erb_dependent_entries_do_not() {
        assert_eq!(
            test_adapters(YML),
            vec![
                ("primary".to_string(), "postgresql".to_string()),
                ("side".to_string(), "trilogy".to_string()),
                ("direct".to_string(), "postgresql".to_string()),
            ]
        );
    }
}
