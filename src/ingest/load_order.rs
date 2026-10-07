//! Ruby's load order over the app's source files, as far as the source
//! states it: each file runs top to bottom, and a literal
//! `require_relative "x"` (or a `require "a/b"` naming a carried
//! `lib/a/b.rb`) runs the required file, whole, at that line — unless it
//! already ran. Files nothing requires load in path order with `x.rb`
//! before `x/y.rb`, which is the order an autoloader reaches them: the
//! file that owns `X` loads when `X` is first named, and `X::Y`'s file
//! later.
//!
//! The answer is a rank for a source position, (file, byte offset): a
//! later rank runs later. `merge_library_class_reopens` orders the
//! reopens of one module by it, because the walk's order is a sorted
//! path list and a path component `cardinal` sorts before `cardinal.rb`.
//! graphql-cardinal's `cardinal.rb` defines `EMPTY_ARRAY` and then
//! requires `cardinal/errors`, whose reopen of the module builds
//! `UNREPORTED_ERROR = ExecutionError.new(...)` — and `initialize`
//! reads `EMPTY_ARRAY` through a default argument. Merged in walk order
//! the reopen's constant came first and the module died with
//! `NameError` at load.
//!
//! A rank is coarse on purpose: one per stretch of a file between two
//! of its requires. Positions inside one stretch keep their byte order.

use crate::det::HashMap;
use crate::span::{FileId, SourceFile, Span};
use std::path::{Component, Path, PathBuf};

pub(super) struct LoadOrder {
    /// Per file (index = `FileId - 1`): the start offset of each stretch
    /// and its global rank, in offset order.
    stretches: Vec<Vec<(u32, u32)>>,
}

impl LoadOrder {
    pub(super) fn new(sources: &[SourceFile]) -> Self {
        let mut by_path: HashMap<PathBuf, usize> = HashMap::default();
        for (i, f) in sources.iter().enumerate() {
            by_path.entry(normalize(Path::new(&f.path))).or_insert(i);
        }
        // `require "a/b"` resolves to the one carried `.../lib/a/b.rb`.
        let mut lib_suffix: HashMap<String, Option<usize>> = HashMap::default();
        for (i, f) in sources.iter().enumerate() {
            let Some(pos) = f.path.rfind("/lib/") else { continue };
            let Some(rel) = f.path[pos + 5..].strip_suffix(".rb") else { continue };
            lib_suffix
                .entry(rel.to_string())
                .and_modify(|slot| *slot = None)
                .or_insert(Some(i));
        }
        let edges: Vec<Vec<(u32, usize)>> = sources
            .iter()
            .map(|f| {
                if !f.path.ends_with(".rb") {
                    return Vec::new();
                }
                let dir = Path::new(&f.path).parent().unwrap_or(Path::new(""));
                requires(&f.text)
                    .into_iter()
                    .filter_map(|(offset, relative, arg)| {
                        let target = if relative {
                            let arg = if arg.ends_with(".rb") { arg } else { format!("{arg}.rb") };
                            by_path.get(&normalize(&dir.join(arg))).copied()
                        } else {
                            lib_suffix.get(arg.trim_end_matches(".rb")).copied().flatten()
                        }?;
                        Some((offset, target))
                    })
                    .collect()
            })
            .collect();
        let mut required = vec![false; sources.len()];
        for list in &edges {
            for &(_, t) in list {
                required[t] = true;
            }
        }
        LoadOrder { stretches: traverse(sources, &edges, &required) }
    }

    /// Where `span` runs in load order: (stretch rank, byte offset), or
    /// None for a synthetic span or an unregistered file.
    pub(super) fn key(&self, span: Span) -> Option<(u32, u32)> {
        if span.is_synthetic() || span.file == FileId(0) {
            return None;
        }
        let list = self.stretches.get(span.file.0 as usize - 1)?;
        let at = list.partition_point(|&(start, _)| start <= span.start);
        let &(_, rank) = list.get(at.checked_sub(1)?)?;
        Some((rank, span.start))
    }
}

/// Every stretch's rank in execution order: a file's stretch up to a
/// require, then the required file (recursively), then the file's next
/// stretch. Roots are the files nothing requires, in byte order of the
/// path ('.' sorts before '/', so `x.rb` before `x/y.rb`); a file only a
/// require cycle reaches starts once every root is done.
fn traverse(sources: &[SourceFile], edges: &[Vec<(u32, usize)>], required: &[bool]) -> Vec<Vec<(u32, u32)>> {
    let n = sources.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| required[a].cmp(&required[b]).then_with(|| sources[a].path.cmp(&sources[b].path)));
    let mut stretches: Vec<Vec<(u32, u32)>> = vec![Vec::new(); n];
    let mut visited = vec![false; n];
    let mut rank: u32 = 0;
    for root in order {
        if visited[root] {
            continue;
        }
        visited[root] = true;
        stretches[root].push((0, rank));
        rank += 1;
        // (file, index of its next require)
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(top) = stack.last_mut() {
            let (file, next) = *top;
            let Some(&(_, target)) = edges[file].get(next) else {
                stack.pop();
                // The parent resumes after the require that ran this file.
                if let Some(&(parent, done)) = stack.last() {
                    stretches[parent].push((edges[parent][done - 1].0, rank));
                    rank += 1;
                }
                continue;
            };
            top.1 += 1;
            if visited[target] {
                continue;
            }
            visited[target] = true;
            stretches[target].push((0, rank));
            rank += 1;
            stack.push((target, 0));
        }
    }
    stretches
}

/// Lexical normalization (`a/b/../c` → `a/c`); no filesystem access.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Every statement-position `require_relative "x"` / `require "x"` with a
/// literal argument: (byte offset just past the statement, is relative,
/// argument). A line scan, not a parse: the statement must start its
/// line, which a comment or a string body does not.
fn requires(text: &str) -> Vec<(u32, bool, String)> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let end = offset + line.len();
        let trimmed = line.trim_start();
        let (relative, rest) = if let Some(rest) = trimmed.strip_prefix("require_relative") {
            (true, rest)
        } else if let Some(rest) = trimmed.strip_prefix("require") {
            (false, rest)
        } else {
            offset = end;
            continue;
        };
        if !rest.starts_with([' ', '\t', '(']) {
            offset = end;
            continue;
        }
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('(').unwrap_or(rest).trim_start();
        let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            offset = end;
            continue;
        };
        let body = &rest[1..];
        if let Some(close) = body.find(quote) {
            let arg = &body[..close];
            if !arg.contains("#{") && !arg.is_empty() {
                out.push((end as u32, relative, arg.to_string()));
            }
        }
        offset = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, text: &str) -> SourceFile {
        SourceFile { path: path.to_string(), text: text.to_string() }
    }

    fn span(file: u32, start: u32) -> Span {
        Span { file: FileId(file), start, end: start + 1 }
    }

    #[test]
    fn a_trailing_require_runs_after_the_body_and_a_leading_one_before() {
        let card = "module Card\n  A = 1\nend\nrequire_relative \"card/errors\"\nB = 2\n";
        let lead = "require_relative 'lead/base'\nmodule Lead\nend\n";
        let sources = vec![
            file("app/models/card/errors.rb", "module Card\n  E = 1\nend\n"),
            file("app/models/card.rb", card),
            file("app/models/lead/base.rb", "module Lead\nend\n"),
            file("app/models/lead.rb", lead),
        ];
        let lo = LoadOrder::new(&sources);
        let body = lo.key(span(2, 14)).unwrap();
        let errors = lo.key(span(1, 14)).unwrap();
        let after = lo.key(span(2, card.find("B =").unwrap() as u32)).unwrap();
        assert!(body < errors && errors < after, "{body:?} {errors:?} {after:?}");
        let base = lo.key(span(3, 0)).unwrap();
        let lead_body = lo.key(span(4, 30)).unwrap();
        assert!(base < lead_body, "{base:?} {lead_body:?}");
    }

    #[test]
    fn a_lib_require_names_the_carried_file() {
        let sources = vec![
            file("gems/g/lib/g/a.rb", "A = 1\n"),
            file("gems/g/lib/g.rb", "require \"g/a\"\nB = 1\n"),
        ];
        let lo = LoadOrder::new(&sources);
        assert!(lo.key(span(1, 0)).unwrap() < lo.key(span(2, 15)).unwrap());
    }
}
