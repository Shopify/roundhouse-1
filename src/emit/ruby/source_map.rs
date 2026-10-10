//! `runtime/roundhouse_source_paths.rb`: where each emitted file and
//! constant came from in the source app.
//!
//! The emitted tree moves code. A test class written in
//! `components/checkouts/test/models/x_test.rb` runs from
//! `test/models/x_test.rb`, a class from
//! `components/checkouts/app/models/y.rb` from `app/models/y.rb`, and a
//! required file program from `app/models/roundhouse_files/<path>`. App
//! code that reads its own source paths (Core's MochaStrictStubbing picks
//! strict or legacy Mocha stubbing from `Method#source_location` and
//! `Object.const_source_location` under `Rails.root`) then sees the
//! emitted layout and decides differently than it does natively.
//!
//! This table is the source side of those answers; the test runtime
//! (`runtime/spinel/cruby/test/source_locations.rb`) consults it:
//! - `FILES`: emitted path → authored path, for the test files (from
//!   the emitted test index).
//! - `CONSTANTS`: constant → `[authored path, line, one_file]`, the file
//!   and `class`/`module` line of the declaration Ruby would have opened
//!   it with (the one naming the superclass, else the first ingested), and
//!   whether every declaration of it is in that one file (only then does
//!   a method's owner name its file).
//!
//! Paths are relative to the app root. Vendored gem sources
//! (`vendor/…`) are left out: natively they live outside `Rails.root`.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::App;
use crate::span::Span;

pub const PATH: &str = "runtime/roundhouse_source_paths.rb";

/// The authored path of `span`'s file, relative to the app root, or
/// `None` for a synthetic span, an unregistered file or a vendored one.
fn authored_path<'a>(app: &'a App, span: &Span) -> Option<&'a str> {
    if span.is_synthetic() || span.file.0 == 0 {
        return None;
    }
    let source = app.sources.get(span.file.0 as usize - 1)?;
    relative(app, &source.path)
}

fn relative<'a>(app: &App, path: &'a str) -> Option<&'a str> {
    let root = app.root.trim_end_matches('/');
    let rel = path.strip_prefix(root).map(|r| r.trim_start_matches('/')).unwrap_or(path);
    (!rel.is_empty() && !rel.starts_with('/') && !rel.starts_with("vendor/")).then_some(rel)
}

/// The line of the `class`/`module` keyword that opens `name`, at or
/// before `offset` in `path`; else the line `offset` is on.
fn declaration_line(app: &App, path: &str, name: &str, offset: u32) -> u32 {
    let Some(source) = app.sources.iter().find(|s| relative(app, &s.path) == Some(path)) else {
        return 1;
    };
    let last = name.rsplit("::").next().unwrap_or(name);
    let mut upto = (offset as usize).min(source.text.len());
    while !source.text.is_char_boundary(upto) {
        upto -= 1;
    }
    let lines: Vec<&str> = source.text[..upto].split('\n').collect();
    let opens = |line: &str| {
        let t = line.trim_start();
        let rest = t.strip_prefix("class ").or_else(|| t.strip_prefix("module "));
        rest.is_some_and(|r| {
            let declared = r.trim_start().split(|c: char| c.is_whitespace() || c == '<' || c == ';').next().unwrap_or("");
            declared.rsplit("::").next() == Some(last)
        })
    };
    match lines.iter().rposition(|l| opens(l)) {
        Some(i) => i as u32 + 1,
        None => lines.len().max(1) as u32,
    }
}

/// The earliest authored position among a declaration's own spans.
fn first_span(spans: impl Iterator<Item = Span>) -> Option<Span> {
    spans.filter(|s| !s.is_synthetic() && s.file.0 != 0).min_by_key(|s| (s.file.0, s.start))
}

fn constants(app: &App) -> BTreeMap<String, (String, u32, bool)> {
    // name → (the creating declaration's span, the files of all of them)
    let mut decls: BTreeMap<String, (Option<(bool, Span)>, Vec<String>)> = BTreeMap::new();
    for lc in &app.library_classes {
        if lc.origin.is_some() && !matches!(lc.origin, Some(crate::dialect::LibraryClassOrigin::FactoryClass { .. })) {
            continue;
        }
        let name = lc.name.0.as_str().trim_start_matches("::").to_string();
        let names_parent = !lc.parent_span.is_synthetic();
        let span = if names_parent {
            Some(lc.parent_span)
        } else {
            first_span(
                lc.methods
                    .iter()
                    .map(|m| m.name_span)
                    .chain(lc.constants.iter().map(|(_, e)| e.span)),
            )
        };
        let Some(span) = span.filter(|s| authored_path(app, s).is_some()) else {
            // A declaration with no authored span of its own (a bare
            // namespace reopen) says nothing about its file.
            if let Some(entry) = decls.get_mut(&name) {
                entry.1.push(String::new());
            }
            continue;
        };
        let entry = decls.entry(name).or_default();
        entry.1.push(authored_path(app, &span).unwrap_or_default().to_string());
        // The declaration naming the superclass is the one that creates a
        // class; among the others, the first ingested.
        if entry.0.is_none() || (names_parent && !entry.0.unwrap().0) {
            entry.0 = Some((names_parent, span));
        }
    }
    let mut out = BTreeMap::new();
    for (name, (first, files)) in decls {
        let Some((_, span)) = first else { continue };
        let path = authored_path(app, &span).unwrap_or_default().to_string();
        let line = declaration_line(app, &path, &name, span.start + 1);
        let one_file = files.iter().all(|f| *f == path);
        out.insert(name, (path, line, one_file));
    }
    for module in &app.test_modules {
        let Some(path) = module.source_file.as_deref() else { continue };
        let name = module.name.0.as_str().trim_start_matches("::").to_string();
        if out.contains_key(&name) {
            continue;
        }
        let at = module.parent_span.map(|s| s.start + 1).unwrap_or(u32::MAX);
        let line = declaration_line(app, path, &name, at);
        out.insert(name, (path.to_string(), line, true));
    }
    out
}

/// Emitted test file → authored test file, from the emitted test index.
fn test_files(files: &[(String, String)]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some((_, index)) = files.iter().find(|(p, _)| p == "test/roundhouse_tests.json") else {
        return out;
    };
    let Ok(index) = serde_json::from_str::<serde_json::Value>(index) else {
        return out;
    };
    for source in index["sources"].as_array().into_iter().flatten() {
        let Some(authored) = source["source"].as_str().filter(|s| !s.is_empty()) else { continue };
        for class in source["classes"].as_array().into_iter().flatten() {
            if let Some(emitted) = class["file"].as_str() {
                out.insert(emitted.to_string(), authored.to_string());
            }
        }
    }
    out
}

fn literal(s: &str) -> String {
    serde_json::to_string(s).expect("string").replace('#', "\\#")
}

/// The table, or `None` when every test file runs from where it was
/// written (an app laid out as the emitted tree is: nothing moved for
/// the runtime to answer differently).
pub fn table_file(app: &App, files: &[(String, String)]) -> Option<(String, String)> {
    let tests: BTreeMap<String, String> = test_files(files).into_iter().filter(|(e, a)| e != a).collect();
    if tests.is_empty() {
        return None;
    }
    let constants = constants(app);
    let mut s = String::from(
        "# Generated by roundhouse (emit/ruby/source_map.rs): the authored\n\
         # path of each emitted test file and constant, relative to the app\n\
         # root. Read by runtime/source_locations.rb.\n\
         module RoundhouseSourcePaths\n  FILES = {\n",
    );
    for (emitted, authored) in &tests {
        writeln!(s, "    {} => {},", literal(emitted), literal(authored)).unwrap();
    }
    s.push_str("  }.freeze\n\n  CONSTANTS = {\n");
    for (name, (path, line, one_file)) in &constants {
        writeln!(s, "    {} => [{}, {line}, {one_file}].freeze,", literal(name), literal(path)).unwrap();
    }
    s.push_str("  }.freeze\nend\n");
    Some((PATH.to_string(), s))
}

/// Carry the table and the runtime that reads it into a CRuby test tree
/// whose test files moved, and require them at the end of the test
/// helper, before any app file loads.
pub fn apply(files: &mut Vec<(String, String)>, app: &App) -> Result<(), String> {
    let Some(table) = table_file(app, files) else {
        return Ok(());
    };
    let runtime = crate::runtime_files::read_to_string("runtime/spinel/cruby/test/source_locations.rb")?;
    let Some((_, helper)) = files.iter_mut().find(|(p, _)| p == "test/test_helper.rb") else {
        return Ok(());
    };
    if !helper.ends_with('\n') {
        helper.push('\n');
    }
    helper.push_str("require_relative \"../runtime/source_locations\"\n");
    files.retain(|(p, _)| p != PATH && p != "runtime/source_locations.rb");
    files.push(table);
    files.push(("runtime/source_locations.rb".to_string(), runtime));
    Ok(())
}
