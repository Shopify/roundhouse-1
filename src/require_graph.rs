//! The emitted Ruby tree's load-time require graph, and the cycles in it.
//!
//! Core's config/boot.rb loads strict_warnings.rb, which raises on every
//! Ruby warning, so a require of a file that is still loading (Ruby's
//! "loading in progress, circular require considered harmful") fails the
//! boot. Zeitwerk never makes one: the emitted tree must not either. This
//! walks the tree as written and names each cycle at compile time, so an
//! emit shows them all without a boot.
//!
//! An edge is a line that loads another file of the tree whenever it runs:
//! - an unguarded `require_relative "x"` or `require
//!   RoundhouseAutoload.path("models/x")`. A guarded one (`… unless
//!   defined?(::X)`, `… unless RoundhouseAutoload.loading?(…)`) is the
//!   compiler's own break of a cycle, and is no edge;
//! - a `module X` / `class X` header reopening a constant whose autoload
//!   (app/models.rb's `NAMESPACES`) is another file: Ruby runs that
//!   autoload as the header runs.

use std::collections::{BTreeMap, BTreeSet};

const REQUIRES: &str = "requires";
const THROUGH: &str = "requires through its program";

/// One edge: the line (1-based) of `from` that loads `to`, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub to: String,
    pub line: usize,
    pub via: String,
}

/// A cycle, as the edges that close it, in order: each edge's file is the
/// one before it (the first edge's file is the last edge's `to`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cycle {
    pub files: Vec<String>,
    pub edges: Vec<Edge>,
}

impl Cycle {
    /// `a.rb:5 requires b.rb; b.rb:2 opens A (its autoload is a.rb)`.
    pub fn describe(&self) -> String {
        let steps: Vec<String> = self
            .files
            .iter()
            .zip(&self.edges)
            .map(|(from, edge)| format!("{from}.rb:{} {} {}.rb", edge.line, edge.via, edge.to))
            .collect();
        steps.join("; ")
    }
}

fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// The file an unguarded require line loads (a tree path without `.rb`),
/// and whether it loads it through its program (`require_through`), or
/// None for any other line.
fn required(dir: &str, line: &str) -> Option<(String, bool)> {
    let text = line.trim();
    if let Some((rel, rest)) = text.strip_prefix("require_relative \"").and_then(|r| r.split_once('"')) {
        if !rest.trim().is_empty() || rel.contains(['#', '\\']) {
            return None;
        }
        let target = normalize(&format!("{dir}/{rel}"));
        return Some((target.strip_suffix(".rb").map(str::to_string).unwrap_or(target), false));
    }
    // A load-time need of a constant its source's program loads
    // (`project::load_declarations_through_their_program`): the program,
    // or its declarations file while the program is loading, so never a
    // circular require of the program.
    if let Some(rest) = text.strip_prefix("defined?(RoundhouseAutoload) ? RoundhouseAutoload.require_through(\"") {
        return rest.split_once('"').map(|(program, _)| (format!("app/{program}"), true));
    }
    let (anchor, rest) = text.strip_prefix("require RoundhouseAutoload.path(\"")?.split_once("\")")?;
    rest.trim().is_empty().then(|| (format!("app/{anchor}"), false))
}

/// app/models.rb's `NAMESPACES`: each constant path (`Plat::Artifact`)
/// mapped to the tree file its autoload loads (`app/models/plat/artifact`).
fn autoload_files(models_rb: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut inside = false;
    for line in models_rb.lines() {
        if line == "  NAMESPACES = {" {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line.starts_with("  }") {
            break;
        }
        let Some((namespace, rest)) = line.trim().strip_prefix('"').and_then(|l| l.split_once("\" => [")) else { continue };
        for entry in rest.split("[:").skip(1) {
            let Some((name, rest)) = entry.split_once(", ") else { continue };
            let Some(file) = rest.strip_prefix('"').and_then(|r| r.split_once('"')).map(|(f, _)| f) else { continue };
            let full = if namespace == "Object" { name.to_string() } else { format!("{namespace}::{name}") };
            out.insert(full, format!("app/{file}"));
        }
    }
    out
}

/// The constant paths a file's top-level headers open, outermost first
/// for each: `module Plat` / `  module Artifact` give `Plat` and
/// `Plat::Artifact`; `class A::B` gives `A` and `A::B`. Read from the
/// indentation the emitter writes (two spaces a level).
fn opened_namespaces(content: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let Some(rest) = trimmed.strip_prefix("module ").or_else(|| trimmed.strip_prefix("class ")) else { continue };
        if indent % 2 != 0 || indent / 2 > stack.len() || rest.starts_with("<<") {
            continue;
        }
        let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':').collect();
        let name = name.trim_start_matches("::");
        if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_uppercase()) {
            continue;
        }
        stack.truncate(indent / 2);
        // A rooted header (`class ::A`) names `A` from the top.
        if rest.starts_with("::") {
            stack.clear();
        }
        stack.push(name.to_string());
        let path = stack.join("::");
        let segments: Vec<&str> = path.split("::").collect();
        for n in 1..=segments.len() {
            let prefix = segments[..n].join("::");
            if seen.insert(prefix.clone()) {
                out.push((i + 1, prefix));
            }
        }
    }
    out
}

/// Each `.rb` file's load edges (`files`: tree path => content). Only
/// edges between files of `files` are kept.
pub fn edges(files: &[(String, String)]) -> BTreeMap<String, Vec<Edge>> {
    let present: BTreeSet<&str> = files.iter().filter_map(|(p, _)| p.strip_suffix(".rb")).collect();
    let autoloads = files
        .iter()
        .find(|(p, _)| p == "app/models.rb")
        .map(|(_, content)| autoload_files(content))
        .unwrap_or_default();
    let mut out: BTreeMap<String, Vec<Edge>> = BTreeMap::new();
    for (path, content) in files {
        let Some(own) = path.strip_suffix(".rb") else { continue };
        if path == "app/models.rb" {
            continue;
        }
        let dir = own.rsplit_once('/').map_or("", |(d, _)| d);
        let mut list: Vec<Edge> = Vec::new();
        if content.contains("require") {
            for (i, line) in content.lines().enumerate() {
                if let Some((to, through)) = required(dir, line) {
                    if to != own && present.contains(to.as_str()) {
                        let via = if through { THROUGH } else { REQUIRES };
                        list.push(Edge { to, line: i + 1, via: via.to_string() });
                    }
                }
            }
        }
        if path.starts_with("app/models/") && !autoloads.is_empty() {
            for (line, namespace) in opened_namespaces(content) {
                let Some(to) = autoloads.get(&namespace) else { continue };
                if to != own && present.contains(to.as_str()) {
                    list.push(Edge { to: to.clone(), line, via: format!("opens {namespace}, whose autoload loads") });
                }
            }
        }
        if !list.is_empty() {
            out.insert(own.to_string(), list);
        }
    }
    // A header's autoload loads its file only when nothing loaded that
    // file first. Zeitwerk loads a namespace before its children
    // (`plat/artifact/empty`'s `module Artifact` autoloads
    // `plat/artifact`), and a source's file program before the
    // declarations file it requires (whose `module X` autoloads the
    // program, `RoundhouseAutoload.entering_program`). So does the tree
    // where every file that requires the reopening file is the autoload's
    // own file, or requires it on an earlier line
    // (`project::require_namespace_files_first`,
    // `load_declarations_through_their_program`). Where one does not, the
    // reopening file loads first and its header runs the autoload's
    // file, which may require it back.
    let mut requirers: BTreeMap<&str, Vec<(&str, usize)>> = BTreeMap::new();
    for (from, list) in &out {
        for edge in list.iter().filter(|e| e.via == REQUIRES || e.via == THROUGH) {
            requirers.entry(edge.to.as_str()).or_default().push((from.as_str(), edge.line));
        }
    }
    // Whether every load of `file` runs once `namespace`'s file has
    // started: each file requiring it is that file, requires it on an
    // earlier line, or itself loads after it. A file nothing requires,
    // or one a constant autoloads, also loads through its constant's
    // autoload, which Ruby resolves through the namespace first only
    // when the file reopens it. While the
    // namespace's file loads, its autoload no longer fires: Ruby's own
    // while it loads through it, and `RoundhouseAutoload.entering_program`
    // removes a program's.
    fn loads_after<'a>(
        file: &'a str,
        namespace: &str,
        requirers: &BTreeMap<&'a str, Vec<(&'a str, usize)>>,
        out: &BTreeMap<String, Vec<Edge>>,
        entries: &BTreeSet<&str>,
        visiting: &mut BTreeSet<&'a str>,
    ) -> bool {
        if !visiting.insert(file) {
            return false;
        }
        let opens = || out.get(file).is_some_and(|l| l.iter().any(|e| e.to == namespace && e.via != REQUIRES && e.via != THROUGH));
        if entries.contains(file) && !opens() {
            return false;
        }
        let Some(loads) = requirers.get(file) else { return opens() };
        loads.iter().all(|(from, line)| {
            *from == namespace
                || out.get(*from).is_some_and(|l| {
                    l.iter().any(|e| e.to == namespace && e.line < *line && (e.via == REQUIRES || e.via == THROUGH))
                })
                || loads_after(from, namespace, requirers, out, entries, visiting)
        })
    }
    let entries: BTreeSet<&str> = autoloads.values().map(String::as_str).collect();
    let mut dropped: Vec<(String, usize)> = Vec::new();
    for (from, list) in &out {
        for (i, edge) in list.iter().enumerate() {
            if edge.via != REQUIRES && edge.via != THROUGH && loads_after(from, &edge.to, &requirers, &out, &entries, &mut BTreeSet::new()) {
                dropped.push((from.clone(), i));
            }
        }
    }
    for (from, i) in dropped.into_iter().rev() {
        out.get_mut(&from).unwrap().remove(i);
    }
    out
}

/// The cycles of the require graph: one per strongly connected component
/// of more than one file, as a shortest cycle through its first file.
pub fn cycles(files: &[(String, String)]) -> Vec<Cycle> {
    let graph = edges(files);
    let mut ids: BTreeMap<&str, usize> = BTreeMap::new();
    let mut names: Vec<&str> = Vec::new();
    for (from, list) in &graph {
        for name in std::iter::once(from.as_str()).chain(list.iter().map(|e| e.to.as_str())) {
            if !ids.contains_key(name) {
                ids.insert(name, names.len());
                names.push(name);
            }
        }
    }
    let adjacency: Vec<Vec<usize>> = names
        .iter()
        .map(|n| graph.get(*n).into_iter().flatten().filter(|e| e.via != THROUGH).map(|e| ids[e.to.as_str()]).collect())
        .collect();
    let component = crate::emit::ruby::strongly_connected(&adjacency);
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (v, c) in component.iter().enumerate() {
        members.entry(*c).or_default().push(v);
    }
    let mut out = Vec::new();
    for nodes in members.values().filter(|m| m.len() > 1) {
        let start = *nodes.iter().min_by_key(|v| names[**v]).unwrap();
        let inside: BTreeSet<usize> = nodes.iter().copied().collect();
        // Breadth first from `start` back to it, within the component.
        let mut previous: BTreeMap<usize, usize> = BTreeMap::new();
        let mut queue = std::collections::VecDeque::from([start]);
        let mut closing = None;
        'search: while let Some(v) = queue.pop_front() {
            for &w in &adjacency[v] {
                if w == start {
                    closing = Some(v);
                    break 'search;
                }
                if inside.contains(&w) && !previous.contains_key(&w) {
                    previous.insert(w, v);
                    queue.push_back(w);
                }
            }
        }
        let Some(mut at) = closing else { continue };
        let mut path = vec![at];
        while at != start {
            at = previous[&at];
            path.push(at);
        }
        path.reverse();
        let files: Vec<String> = path.iter().map(|v| names[*v].to_string()).collect();
        let edges: Vec<Edge> = path
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let to = names[path[(i + 1) % path.len()]];
                graph[names[*v]].iter().find(|e| e.to == to && e.via != THROUGH).cloned().unwrap()
            })
            .collect();
        out.push(Cycle { files, edges });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, &str)]) -> Vec<(String, String)> {
        files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect()
    }

    const MODELS: &str = "module RoundhouseAutoload\n  NAMESPACES = {\n    \"Object\" => [[:Plat, \"models/plat\", true], [:Terms, \"models/terms\", false]],\n    \"Plat\" => [[:Artifact, \"models/plat/artifact\", true]],\n  }.freeze\nend\n";

    #[test]
    fn a_header_reopening_an_autoloaded_namespace_closes_a_cycle() {
        let files = tree(&[
            ("app/models.rb", MODELS),
            ("app/models/plat.rb", "module Plat\nend\n"),
            ("app/models/plat/artifact.rb", "require_relative \"../terms\"\n\nmodule Plat\n  module Artifact\n  end\nend\n"),
            ("app/models/plat/artifact/empty.rb", "module Plat\n  module Artifact\n    module Empty\n    end\n  end\nend\n"),
            ("app/models/terms.rb", "require_relative \"plat/artifact/empty\"\n\nclass Terms\nend\n"),
        ]);
        let found = cycles(&files);
        assert_eq!(found.len(), 1, "{found:?}");
        let text = found[0].describe();
        assert!(text.contains("app/models/plat/artifact.rb:1 requires app/models/terms.rb"), "{text}");
        assert!(text.contains("app/models/plat/artifact/empty.rb:2 opens Plat::Artifact, whose autoload loads app/models/plat/artifact.rb"), "{text}");
    }

    #[test]
    fn a_guarded_require_is_no_edge() {
        let files = tree(&[
            ("app/models.rb", MODELS),
            ("app/models/plat/artifact.rb", "module Plat\n  module Artifact\n    require_relative \"../terms\" unless defined?(::Terms)\n  end\nend\n"),
            ("app/models/plat/artifact/empty.rb", "module Plat\n  module Artifact\n    module Empty\n    end\n  end\nend\n"),
            ("app/models/terms.rb", "require_relative \"plat/artifact/empty\"\n\nclass Terms\nend\n"),
        ]);
        assert!(cycles(&files).is_empty());
    }

    #[test]
    fn plain_requires_close_a_cycle() {
        let files = tree(&[
            ("app/models/a.rb", "require RoundhouseAutoload.path(\"models/b\")\n"),
            ("app/models/b.rb", "require_relative \"a\"\n"),
        ]);
        let found = cycles(&files);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].files, vec!["app/models/a", "app/models/b"]);
    }
}
