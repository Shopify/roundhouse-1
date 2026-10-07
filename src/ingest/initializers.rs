//! Initializer coverage: what of `config/initializers/` the emitted app
//! actually runs.
//!
//! Rails runs every `config/initializers/**/*.rb` of every engine and of
//! the application at boot. Roundhouse does not evaluate initializers;
//! a dozen independent readers in [`super::app`] each recognize one
//! shape (a time format, a mixin, a Kaminari page size, an app config
//! key …) and lift it into the IR. Everything else used to vanish with
//! nothing said. This module closes that hole without changing what is
//! emitted:
//!
//! * A per-thread registry of CONSUMED byte ranges per file. Every
//!   reader calls [`consume`] for exactly the node it recognized and
//!   applies, under the same path string the survey uses
//!   (`entry.display().to_string()`).
//! * [`initializer_files`] lists the files Rails would run, in its load
//!   order: each in-repo engine's, in [`engines_in_initializer_order`],
//!   then the application's.
//! * [`survey`], run once after every reader, walks each file's
//!   top-level statements. A statement a consumed range covers is fine.
//!   One with a consumed range inside one of its child statement lists
//!   (a block body, an `if` branch, a `begin` body, a class or module
//!   body) is a container a reader descended into: its child statements
//!   are judged by the same rule and the container itself is not
//!   reported. Anything else is ONE [`InitializerGap`], which
//!   `analyze::initializer_coverage` reports as an
//!   `initializer_unsupported` error.
//! * A reader that recognizes a statement's shape but cannot apply one
//!   part of it [`refuse`]s the statement with a reason. The survey names
//!   that reason as the gap's detail, so the error says what was not
//!   applied rather than only quoting the line. A refusal in a file Rails
//!   loads that is not an initializer (`config/application.rb`'s class
//!   body) is a gap of its own: no survey walks that file.

use std::cell::RefCell;
use crate::det::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use ruby_prism::Node;

use crate::App;
use crate::app::InitializerGap;
use crate::span::{FileId, Span};
use crate::vfs::Vfs;

use super::util::{class_name_path, constant_id_str, flatten_statements, module_name_path};

thread_local! {
    static CONSUMED: RefCell<HashMap<String, Vec<(usize, usize)>>> = RefCell::new(HashMap::default());
    static REFUSED: RefCell<Vec<Refusal>> = const { RefCell::new(Vec::new()) };
}

/// A statement a reader recognized and did not apply, with the reason.
#[derive(Clone)]
struct Refusal {
    file: String,
    start: usize,
    end: usize,
    reason: String,
}

/// Clear the registry for a fresh whole-app ingest.
pub fn reset() {
    CONSUMED.with(|c| c.borrow_mut().clear());
    REFUSED.with(|r| r.borrow_mut().clear());
}

/// Record that a reader recognized the statement at `start..end` of
/// `file` and does NOT apply it, because of `reason`. The statement stays
/// unconsumed; the reason becomes its gap's detail.
pub fn refuse(file: &str, start: usize, end: usize, reason: impl Into<String>) {
    REFUSED.with(|r| {
        r.borrow_mut().push(Refusal { file: file.to_string(), start, end, reason: reason.into() })
    });
}

/// Record that a reader applied the source range `start..end` of `file`
/// to the emitted app.
pub fn consume(file: &str, start: usize, end: usize) {
    CONSUMED.with(|c| c.borrow_mut().entry(file.to_string()).or_default().push((start, end)));
}

/// The ranges consumed in `file` so far.
pub fn consumed(file: &str) -> Vec<(usize, usize)> {
    CONSUMED.with(|c| c.borrow().get(file).cloned().unwrap_or_default())
}

/// [`consume`] a prism node's whole location.
pub(super) fn consume_node(file: &str, node: &Node<'_>) {
    let loc = node.location();
    consume(file, loc.start_offset(), loc.end_offset());
}

/// The statement lists nested directly in `node` that the survey and
/// [`consume_statement_at`] descend through: a call's block body, `if`
/// / `unless` / `else` branches, `begin` / `rescue` / `else` / `ensure`
/// bodies, `def` bodies (an adapter patch whose registrations a reader
/// lifts, `sql_functions`), class / module / singleton-class bodies and
/// a parenthesized body. An `elsif` is its own one-statement list.
pub(super) fn child_statement_lists<'pr>(node: &Node<'pr>) -> Vec<Vec<Node<'pr>>> {
    fn stmts<'pr>(s: Option<ruby_prism::StatementsNode<'pr>>) -> Option<Vec<Node<'pr>>> {
        s.map(|s| s.body().iter().collect())
    }
    let mut out = Vec::new();
    if let Some(call) = node.as_call_node() {
        if let Some(body) = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()) {
            out.push(flatten_statements(body));
        }
    } else if let Some(n) = node.as_if_node() {
        out.extend(stmts(n.statements()));
        if let Some(sub) = n.subsequent() {
            match sub.as_else_node() {
                Some(e) => out.extend(stmts(e.statements())),
                None => out.push(vec![sub]),
            }
        }
    } else if let Some(n) = node.as_unless_node() {
        out.extend(stmts(n.statements()));
        if let Some(e) = n.else_clause() {
            out.extend(stmts(e.statements()));
        }
    } else if let Some(n) = node.as_begin_node() {
        out.extend(stmts(n.statements()));
        let mut rescue = n.rescue_clause();
        while let Some(r) = rescue {
            out.extend(stmts(r.statements()));
            rescue = r.subsequent();
        }
        if let Some(e) = n.else_clause() {
            out.extend(stmts(e.statements()));
        }
        if let Some(e) = n.ensure_clause() {
            out.extend(stmts(e.statements()));
        }
    } else if let Some(n) = node.as_def_node() {
        out.extend(n.body().map(flatten_statements));
    } else if let Some(n) = node.as_class_node() {
        out.extend(n.body().map(flatten_statements));
    } else if let Some(n) = node.as_module_node() {
        out.extend(n.body().map(flatten_statements));
    } else if let Some(n) = node.as_singleton_class_node() {
        out.extend(n.body().map(flatten_statements));
    } else if let Some(n) = node.as_parentheses_node() {
        out.extend(n.body().map(flatten_statements));
    }
    out.retain(|list| !list.is_empty());
    out
}

/// The innermost statement (through [`child_statement_lists`]) whose
/// range contains `offset`.
fn deepest_statement<'pr>(stmts: Vec<Node<'pr>>, offset: usize) -> Option<Node<'pr>> {
    for stmt in stmts {
        let loc = stmt.location();
        if loc.start_offset() <= offset && offset < loc.end_offset() {
            for list in child_statement_lists(&stmt) {
                if let Some(inner) = deepest_statement(list, offset) {
                    return Some(inner);
                }
            }
            return Some(stmt);
        }
    }
    None
}

/// For the line-scanned readers, which match source text rather than a
/// node: consume the innermost statement containing the byte `offset`
/// of their match, when `accept` agrees it is the shape they applied.
pub(super) fn consume_statement_at(
    source: &[u8],
    file: &str,
    offset: usize,
    accept: impl Fn(&Node<'_>, &str) -> bool,
) {
    let result = ruby_prism::parse(source);
    let src = String::from_utf8_lossy(source);
    let root = result.node();
    let Some(program) = root.as_program_node() else { return };
    if let Some(stmt) = deepest_statement(program.statements().body().iter().collect(), offset) {
        if accept(&stmt, &src) {
            consume_node(file, &stmt);
        }
    }
}

/// The class and module declarations of a file, by the qualified name
/// `ingest_library_classes` gives the `LibraryClass` it builds from
/// each (enclosing class/module path plus the declared path).
pub(super) fn declaration_ranges(source: &[u8]) -> Vec<(String, usize, usize)> {
    fn walk(stmts: Vec<Node<'_>>, scope: &[String], out: &mut Vec<(String, usize, usize)>) {
        for stmt in stmts {
            let (path, body) = if let Some(c) = stmt.as_class_node() {
                (class_name_path(&c), c.body())
            } else if let Some(m) = stmt.as_module_node() {
                (module_name_path(&m), m.body())
            } else {
                continue;
            };
            let mut inner = scope.to_vec();
            inner.extend(path.unwrap_or_default());
            let loc = stmt.location();
            out.push((inner.join("::"), loc.start_offset(), loc.end_offset()));
            if let Some(body) = body {
                walk(flatten_statements(body), &inner, out);
            }
        }
    }
    let result = ruby_prism::parse(source);
    let root = result.node();
    let mut out = Vec::new();
    if let Some(program) = root.as_program_node() {
        walk(program.statements().body().iter().collect(), &[], &mut out);
    }
    out
}

/// `NAME = …` at the top level of a file: the constants
/// `ingest_library_classes` hoists into the file's first library class.
pub(super) fn top_level_constant_ranges(source: &[u8]) -> Vec<(usize, usize)> {
    let result = ruby_prism::parse(source);
    let root = result.node();
    let Some(program) = root.as_program_node() else { return Vec::new() };
    program
        .statements()
        .body()
        .iter()
        .filter(|s| s.as_constant_write_node().is_some())
        .map(|s| (s.location().start_offset(), s.location().end_offset()))
        .collect()
}

/// Every initializer Rails runs at boot, in its order: each engine's
/// `config/initializers/**/*.rb` (engines in `engines` order), then the
/// application's. Within one railtie Rails runs
/// `paths["config/initializers"].existent.sort` (railties engine.rb,
/// `load_config_initializers`) — recursive, sorted by full path STRING,
/// which is not `PathBuf`'s component order (`a-b/` sorts before `a/`
/// as a string).
pub(super) fn initializer_files<V: Vfs + ?Sized>(
    vfs: &V,
    dir: &Path,
    engines: &[PathBuf],
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let app_root = dir.to_path_buf();
    for root in engines.iter().chain(std::iter::once(&app_root)) {
        let d = root.join("config/initializers");
        if !vfs.is_dir(&d) {
            continue;
        }
        let mut files = super::app::read_rb_files(vfs, &d).unwrap_or_default();
        files.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
        out.extend(files);
    }
    out
}

/// The in-repo engines (path gems declaring a `Rails::Engine` subclass
/// under their `lib/`), in the order Rails runs their initializers.
///
/// THE RULE. Rails runs each railtie's `load_config_initializers` in
/// `Railtie` subclass load order — the order the engine classes are
/// DEFINED (railties `railtie.rb` `subclasses` / `load_index`; plain
/// railties before engines, `engine/railties.rb`), with the application
/// last (`Application#ordered_railties`, default `railties_order
/// [:all]`). An engine class is defined when the file declaring it is
/// first required, so the order is the static require graph walked
/// depth-first, first load wins:
///
/// * Seeded with `config/application.rb`, statement by statement
///   (descending into class and module bodies). Each `require "lit"`
///   resolves against the path gems' `lib/` directories, then the app's
///   `lib/`; each `require_relative "lit"` against the requiring file.
///   Unresolvable names (gems outside the tree) are skipped.
/// * `Rails.load_embedded_engines` (the gurgel gem,
///   `lib/gurgel/rails_context.rb`) reads `config/contexts.yml` and
///   `require`s each name of the context `ENV.fetch("RAILS_CONTEXT",
///   "all")` selects, in order; it expands here to the requires of the
///   `all:` list. Without a `contexts.yml` the call expands to nothing.
/// * Each `class X < Rails::Engine` / `< ::Rails::Engine` reached is
///   recorded for the engine directory containing its file.
///
/// NOT MODELED: `Bundler.require` (it requires the Gemfile's gems that
/// are not `require: false`, before anything after it), conditional or
/// block-nested requires, and `$LOAD_PATH` order beyond "path gems,
/// then app `lib/`".
///
/// UNMEASURED FALLBACK: engines the walk never reaches follow the
/// reached ones, sorted by directory. Their real position is unknown.
pub(super) fn engines_in_initializer_order<V: Vfs + ?Sized>(
    vfs: &V,
    dir: &Path,
    path_gems: &[PathBuf],
) -> Vec<PathBuf> {
    let engines: Vec<PathBuf> = path_gems
        .iter()
        .filter(|gem| super::app::declares_rails_engine(vfs, &gem.join("lib")))
        .cloned()
        .collect();
    if engines.is_empty() {
        return engines;
    }
    let mut load_path: Vec<PathBuf> =
        path_gems.iter().map(|gem| gem.join("lib")).filter(|lib| vfs.is_dir(lib)).collect();
    load_path.push(dir.join("lib"));
    let mut walk = RequireWalk {
        vfs,
        dir,
        load_path,
        engines: &engines,
        visited: HashSet::default(),
        reached: Vec::new(),
    };
    walk.file(&dir.join("config/application.rb"));
    let mut order = walk.reached;
    let mut rest: Vec<PathBuf> = engines.iter().filter(|e| !order.contains(e)).cloned().collect();
    rest.sort();
    order.extend(rest);
    order
}

struct RequireWalk<'a, V: Vfs + ?Sized> {
    vfs: &'a V,
    dir: &'a Path,
    load_path: Vec<PathBuf>,
    engines: &'a [PathBuf],
    visited: HashSet<PathBuf>,
    reached: Vec<PathBuf>,
}

impl<V: Vfs + ?Sized> RequireWalk<'_, V> {
    fn file(&mut self, path: &Path) {
        let path = normalize(path);
        if !self.visited.insert(path.clone()) {
            return;
        }
        let Ok(source) = self.vfs.read(&path) else { return };
        let result = ruby_prism::parse(&source);
        let root = result.node();
        let Some(program) = root.as_program_node() else { return };
        self.statements(program.statements().body().iter().collect(), &path);
    }

    fn statements(&mut self, stmts: Vec<Node<'_>>, file: &Path) {
        for stmt in stmts {
            if let Some(call) = stmt.as_call_node() {
                let name = constant_id_str(&call.name());
                let receiver = call.receiver();
                if receiver.is_none() && matches!(name, "require" | "require_relative") {
                    let arg = call.arguments().and_then(|a| {
                        let args: Vec<_> = a.arguments().iter().collect();
                        match args.as_slice() {
                            [one] => one
                                .as_string_node()
                                .map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()),
                            _ => None,
                        }
                    });
                    let Some(arg) = arg else { continue };
                    let target = if name == "require" {
                        self.resolve(&arg)
                    } else {
                        file.parent().map(|p| p.join(with_rb(&arg))).filter(|p| self.vfs.exists(p))
                    };
                    if let Some(target) = target {
                        self.file(&target);
                    }
                    continue;
                }
                let on_rails = receiver.as_ref().is_some_and(|r| {
                    r.as_constant_read_node().is_some_and(|c| constant_id_str(&c.name()) == "Rails")
                        || r.as_constant_path_node().is_some_and(|p| {
                            p.parent().is_none()
                                && p.name().is_some_and(|n| constant_id_str(&n) == "Rails")
                        })
                });
                if name == "load_embedded_engines" && on_rails {
                    for engine in self.embedded_engine_names() {
                        if let Some(target) = self.resolve(&engine) {
                            self.file(&target);
                        }
                    }
                }
                continue;
            }
            if let Some(class) = stmt.as_class_node() {
                if super::app::is_rails_engine_class(&class) {
                    self.record(file);
                }
                if let Some(body) = class.body() {
                    self.statements(flatten_statements(body), file);
                }
                continue;
            }
            if let Some(module) = stmt.as_module_node() {
                if let Some(body) = module.body() {
                    self.statements(flatten_statements(body), file);
                }
            }
        }
    }

    /// `require "name"` against the load path, first match.
    fn resolve(&self, name: &str) -> Option<PathBuf> {
        let file = with_rb(name);
        self.load_path
            .iter()
            .map(|lib| lib.join(&file))
            .find(|p| self.vfs.exists(p) && !self.vfs.is_dir(p))
    }

    /// The `all:` list of `config/contexts.yml` (gurgel's
    /// `Rails.load_embedded_engines` with the default context).
    fn embedded_engine_names(&self) -> Vec<String> {
        let Ok(text) = self.vfs.read_to_string(&self.dir.join("config/contexts.yml")) else {
            return Vec::new();
        };
        let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text) else {
            return Vec::new();
        };
        value
            .get("all")
            .and_then(|all| all.as_sequence())
            .map(|seq| seq.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    }

    /// An engine class defined in `file` belongs to the innermost engine
    /// directory containing it.
    fn record(&mut self, file: &Path) {
        let engine = self
            .engines
            .iter()
            .filter(|e| file.starts_with(e))
            .max_by_key(|e| e.components().count());
        if let Some(engine) = engine {
            if !self.reached.contains(engine) {
                self.reached.push(engine.clone());
            }
        }
    }
}

fn with_rb(name: &str) -> String {
    if name.ends_with(".rb") { name.to_string() } else { format!("{name}.rb") }
}

/// Lexical `.`/`..` folding, so a `require_relative "../x"` names the
/// same file as a direct require of it.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
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

/// Walk every initializer in [`App::initializer_files`] against the
/// consumed ranges and record each statement nothing applied as an
/// [`InitializerGap`]. Runs after every reader, on the drained
/// `App::sources` (every initializer file is registered before the
/// Rubydex snapshot).
pub(super) fn survey(app: &mut App) {
    let mut gaps = Vec::new();
    let refusals = REFUSED.with(|r| r.borrow().clone());
    for path in &app.initializer_files {
        let Some(index) = app.sources.iter().position(|s| &s.path == path) else { continue };
        let file = FileId(index as u32 + 1);
        let text = &app.sources[index].text;
        let ranges = consumed(path);
        let refused: Vec<&Refusal> = refusals.iter().filter(|r| &r.file == path).collect();
        let result = ruby_prism::parse(text.as_bytes());
        let root = result.node();
        let Some(program) = root.as_program_node() else { continue };
        survey_statements(program.statements().body().iter().collect(), &ranges, &refused, text, file, &mut gaps);
    }
    // Refusals outside the initializer files: their statement is the gap.
    for refusal in refusals.iter().filter(|r| !app.initializer_files.contains(&r.file)) {
        let Some(index) = app.sources.iter().position(|s| s.path == refusal.file) else { continue };
        let text = &app.sources[index].text;
        let Some(stmt_text) = text.get(refusal.start..refusal.end) else { continue };
        let result = ruby_prism::parse(stmt_text.as_bytes());
        let root = result.node();
        let construct = root
            .as_program_node()
            .and_then(|p| p.statements().body().iter().next())
            .map(|n| construct_of(&n, stmt_text))
            .unwrap_or_else(|| "expression".to_string());
        gaps.push(InitializerGap {
            span: Span { file: FileId(index as u32 + 1), start: refusal.start as u32, end: refusal.end as u32 },
            construct,
            detail: cap(&refusal.reason, DETAIL_CAP),
        });
    }
    app.initializer_gaps = gaps;
}

fn survey_statements(
    stmts: Vec<Node<'_>>,
    ranges: &[(usize, usize)],
    refused: &[&Refusal],
    src: &str,
    file: FileId,
    out: &mut Vec<InitializerGap>,
) {
    for stmt in stmts {
        let loc = stmt.location();
        let (start, end) = (loc.start_offset(), loc.end_offset());
        if ranges.iter().any(|&(a, b)| a <= start && end <= b) {
            continue;
        }
        let lists = child_statement_lists(&stmt);
        let descended = ranges.iter().any(|&(a, b)| {
            start <= a
                && b <= end
                && lists.iter().any(|list| {
                    let first = list.first().map(|n| n.location().start_offset()).unwrap_or(0);
                    let last = list.last().map(|n| n.location().end_offset()).unwrap_or(0);
                    first <= a && b <= last
                })
        });
        if descended {
            for list in lists {
                survey_statements(list, ranges, refused, src, file, out);
            }
            continue;
        }
        let detail = match refused.iter().find(|r| start <= r.start && r.end <= end) {
            Some(r) => cap(&r.reason, DETAIL_CAP),
            None => first_line(&src[start..end]),
        };
        out.push(InitializerGap {
            span: Span { file, start: start as u32, end: end as u32 },
            construct: construct_of(&stmt, src),
            detail,
        });
    }
}

const CONSTRUCT_CAP: usize = 60;
const RECEIVER_CAP: usize = 48;
const DETAIL_CAP: usize = 100;

fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn first_line(text: &str) -> String {
    cap(text.lines().next().unwrap_or("").trim(), DETAIL_CAP)
}

/// A short, stable name to group gaps by: `<receiver>.<method>` for a
/// call whose receiver is a constant or a short argument-free call chain
/// (`Rails.application.config.middleware.use`), the bare method for a
/// receiverless call (`require`), `<expr>.<method>` for any other
/// receiver, and the statement kind otherwise (`if`, `class`, `constant
/// assignment`).
fn construct_of(node: &Node<'_>, src: &str) -> String {
    let with_receiver = |receiver: Option<Node<'_>>, member: String| match receiver {
        None => member,
        Some(r) => format!("{}.{member}", receiver_label(&r, src)),
    };
    let name = if let Some(call) = node.as_call_node() {
        with_receiver(call.receiver(), constant_id_str(&call.name()).to_string())
    } else if let Some(w) = node.as_call_operator_write_node() {
        let member = format!(
            "{}{}=",
            constant_id_str(&w.read_name()),
            constant_id_str(&w.binary_operator())
        );
        with_receiver(w.receiver(), member)
    } else if let Some(w) = node.as_call_or_write_node() {
        with_receiver(w.receiver(), format!("{}||=", constant_id_str(&w.read_name())))
    } else if let Some(w) = node.as_call_and_write_node() {
        with_receiver(w.receiver(), format!("{}&&=", constant_id_str(&w.read_name())))
    } else {
        statement_kind(node).to_string()
    };
    cap(&name, CONSTRUCT_CAP)
}

fn statement_kind(node: &Node<'_>) -> &'static str {
    if node.as_if_node().is_some() {
        "if"
    } else if node.as_unless_node().is_some() {
        "unless"
    } else if node.as_class_node().is_some() {
        "class"
    } else if node.as_module_node().is_some() {
        "module"
    } else if node.as_singleton_class_node().is_some() {
        "class <<"
    } else if node.as_def_node().is_some() {
        "def"
    } else if node.as_constant_write_node().is_some()
        || node.as_constant_path_write_node().is_some()
        || node.as_constant_or_write_node().is_some()
        || node.as_constant_path_or_write_node().is_some()
        || node.as_constant_operator_write_node().is_some()
        || node.as_constant_path_operator_write_node().is_some()
    {
        "constant assignment"
    } else if node.as_local_variable_write_node().is_some()
        || node.as_local_variable_or_write_node().is_some()
        || node.as_local_variable_operator_write_node().is_some()
    {
        "local assignment"
    } else if node.as_instance_variable_write_node().is_some()
        || node.as_instance_variable_or_write_node().is_some()
    {
        "instance variable assignment"
    } else if node.as_global_variable_write_node().is_some()
        || node.as_global_variable_or_write_node().is_some()
        || node.as_global_variable_operator_write_node().is_some()
    {
        "global assignment"
    } else if node.as_class_variable_write_node().is_some() {
        "class variable assignment"
    } else if node.as_index_operator_write_node().is_some()
        || node.as_index_or_write_node().is_some()
        || node.as_index_and_write_node().is_some()
    {
        "index assignment"
    } else if node.as_multi_write_node().is_some() {
        "multiple assignment"
    } else if node.as_begin_node().is_some() {
        "begin"
    } else if node.as_case_node().is_some() || node.as_case_match_node().is_some() {
        "case"
    } else if node.as_while_node().is_some() {
        "while"
    } else if node.as_until_node().is_some() {
        "until"
    } else if node.as_for_node().is_some() {
        "for"
    } else if node.as_alias_method_node().is_some() || node.as_alias_global_variable_node().is_some() {
        "alias"
    } else if node.as_and_node().is_some() || node.as_or_node().is_some() {
        "boolean expression"
    } else if node.as_parentheses_node().is_some() {
        "parentheses"
    } else {
        "expression"
    }
}

/// A constant (as written) or an argument-free, block-free call chain
/// down to a constant, a local, or a receiverless reader (`Rails.application
/// .config`, `config.action_mailer`); `<expr>` for anything else or
/// anything longer than [`RECEIVER_CAP`].
fn receiver_label(node: &Node<'_>, src: &str) -> String {
    fn chain(node: &Node<'_>, src: &str, depth: usize) -> Option<String> {
        if depth > 6 {
            return None;
        }
        if node.as_constant_read_node().is_some() || node.as_constant_path_node().is_some() {
            let loc = node.location();
            let text: String = src[loc.start_offset()..loc.end_offset()]
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            return Some(text);
        }
        // A block parameter (`config` in `Kaminari.configure do |config|`).
        if let Some(local) = node.as_local_variable_read_node() {
            return Some(constant_id_str(&local.name()).to_string());
        }
        let call = node.as_call_node()?;
        if call.block().is_some() || call.arguments().is_some_and(|a| a.arguments().iter().count() > 0) {
            return None;
        }
        let name = constant_id_str(&call.name());
        match call.receiver() {
            None => Some(name.to_string()),
            Some(r) => Some(format!("{}.{name}", chain(&r, src, depth + 1)?)),
        }
    }
    chain(node, src, 0)
        .filter(|label| label.chars().count() <= RECEIVER_CAP)
        .unwrap_or_else(|| "<expr>".to_string())
}
